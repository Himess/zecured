//! `zecured-ops`: the signers' operator messages (O-3), from one operator's request to the
//! transaction on Base.
//!
//! 1. `prepare`: an operator reads the deployment (the verifying contract and its current nonce)
//!    and writes a request file: chain, contract, message and digest. The contract's own hash
//!    function must give the same digest, or nothing is written.
//! 2. `sign`: each signer checks the request on its own machine (the digest is recomputed from the
//!    message, never taken from the file) and signs it with its attestation key, read from its
//!    node's identity file or from an environment variable. A key is never printed, logged or put
//!    in an error.
//! 3. `collect`: the signature files are checked against the network config's signer set (one per
//!    signer, sorted by address as the contracts require) and merged into a bundle; a bundle may
//!    be collected into again, so the five signatures a `ScheduleVersion` needs can come in rounds.
//! 4. `submit`: the bundle is checked against Base (the Registry's signer set, the deployment's
//!    contract, its digest and nonce, the signatures the message needs now), simulated, then sent
//!    from any funded key: the signatures authorize the call, not the sender. `--dry-run` stops
//!    after the simulation.
//!
//! The messages, their digests, calldata and thresholds are `zecured_base::operator`'s: the same
//! signing layer the nodes' liveness receipts (R-5) use.

use std::path::Path;

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, Bytes, FixedBytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::{SignerSync, local::PrivateKeySigner},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use zecured_base::{
    events::{IZecuredIntake, IZecuredRegistry, IZecuredVersions},
    gas_with_headroom,
    operator::{
        OperatorMessage, Verifier, VersionKind, recover_digest, removal_threshold, signature_encoding_ok,
        sorted_signatures,
    },
};
use zecured_policy::treasury::TreasuryKeys;

/// The parts of the network config (`signer/data/network.json`) this tool reads.
#[derive(Clone, Debug, Deserialize)]
pub struct Network {
    pub registry: Address,
    pub chain_id: u64,
    /// May carry a provider's API key: never printed (see [`scrub`]).
    pub base_rpc_url: String,
    #[serde(default)]
    pub nodes: Vec<NetworkNode>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NetworkNode {
    pub index: u16,
    pub eth: Address,
}

impl Network {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// The config's signer set: each node's attestation address.
    pub fn signers(&self) -> Vec<Address> {
        self.nodes.iter().map(|n| n.eth).collect()
    }
}

/// One message for one contract on one chain, with its digest (recomputed at every step; the
/// file's own only detects an altered file) and the signatures collected so far (none in a
/// request, at least the threshold in a bundle ready to submit).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub chain_id: u64,
    pub contract: Address,
    pub message: OperatorMessage,
    pub digest: B256,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<Signed>,
}

/// A signer's signature, as a bundle keeps it (sorted by `signer`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    pub signer: Address,
    pub signature: Bytes,
}

/// What `sign` writes: the digest signed, the signer's address and the signature. No key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureFile {
    pub digest: B256,
    pub signer: Address,
    pub signature: Bytes,
}

impl Request {
    pub fn new(chain_id: u64, contract: Address, message: OperatorMessage) -> Self {
        let digest = message.signing_hash(chain_id, contract);
        Self {
            chain_id,
            contract,
            message,
            digest,
            signatures: vec![],
        }
    }

    /// The message's own digest; a file whose recorded digest differs was altered.
    pub fn checked_digest(&self) -> Result<B256> {
        let digest = self.message.signing_hash(self.chain_id, self.contract);
        ensure!(digest == self.digest, "the file's digest is not its message's: the file was altered");
        Ok(digest)
    }

    /// What is signed, for whom: one paragraph for the operator to check before signing.
    pub fn describe(&self) -> String {
        format!(
            "{}\n  verified by the {:?} contract {} on chain {}\n  digest {}",
            self.message.describe(),
            self.message.verifier(),
            self.contract,
            self.chain_id,
            self.digest
        )
    }
}

// ------------------------------------------------------------------------------------------ keys

/// A signing key from a node's identity file (its `eth_private_key`) or from an environment
/// variable, exactly one of the two. The key never appears in an error.
pub fn load_key(identity: Option<&Path>, env: Option<&str>) -> Result<PrivateKeySigner> {
    load_key_with(identity, env, |var| std::env::var(var).ok())
}

fn load_key_with(
    identity: Option<&Path>,
    env: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<PrivateKeySigner> {
    match (identity, env) {
        (Some(path), None) => {
            #[derive(Deserialize)]
            struct IdentityKey {
                eth_private_key: String,
            }
            let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let id: IdentityKey =
                serde_json::from_slice(&raw).map_err(|_| anyhow!("{} is not a node identity file", path.display()))?;
            parse_key(&id.eth_private_key).with_context(|| format!("the key in {}", path.display()))
        }
        (None, Some(var)) => {
            let raw = lookup(var).ok_or_else(|| anyhow!("{var} is not set"))?;
            parse_key(&raw).with_context(|| format!("the key in {var}"))
        }
        _ => bail!("give exactly one key source: an identity file or an environment variable"),
    }
}

fn parse_key(raw: &str) -> Result<PrivateKeySigner> {
    raw.trim().parse::<PrivateKeySigner>().map_err(|_| anyhow!("not a private key (64 hex digits)"))
}

/// `text` with the RPC URL (which may carry an API key) replaced, for anything printed.
pub fn scrub(text: &str, rpc: &str) -> String {
    let mut forms = vec![rpc.to_owned(), format!("{}/", rpc.trim_end_matches('/'))];
    if let Some((base, _)) = rpc.split_once('?') {
        forms.push(base.to_owned());
    }
    forms.retain(|f| f.len() > 1);
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.iter().fold(text.to_owned(), |out, form| out.replace(form.as_str(), "<Base RPC>"))
}

// --------------------------------------------------------------------------------- sign, collect

/// W-6: a `ReimburseDestination` names the claim's own treasury destination, as the treasury keys
/// derive it; any other message passes.
pub fn check_reimbursement(message: &OperatorMessage, treasury: Option<&TreasuryKeys>) -> Result<()> {
    let OperatorMessage::ReimburseDestination {
        claim_id, commitment, ..
    } = message
    else {
        return Ok(());
    };
    let keys = treasury.context("a reimbursement destination is signed only with the treasury keys (--treasury)")?;
    ensure!(
        B256::from(keys.reimbursement(&claim_id.0).commitment()) == *commitment,
        "not the claim's treasury destination: not signed (W-6)"
    );
    Ok(())
}

/// Signs the request after checking the file, its deadline and, for a reimbursement destination,
/// that it is the claim's derived treasury destination (W-6).
pub fn sign(
    request: &Request,
    key: &PrivateKeySigner,
    now: u64,
    treasury: Option<&TreasuryKeys>,
) -> Result<SignatureFile> {
    let digest = request.checked_digest()?;
    ensure!(request.message.deadline() >= now, "the message's deadline has passed: prepare it again");
    check_reimbursement(&request.message, treasury)?;
    let signature = key.sign_hash_sync(&digest).map_err(|_| anyhow!("signing failed"))?;
    let signature = Bytes::from(signature.as_bytes().to_vec());
    ensure!(signature_encoding_ok(&signature), "the signature's encoding would be refused on Base");
    Ok(SignatureFile {
        digest,
        signer: key.address(),
        signature,
    })
}

/// Merges signature files into the request: each over this request's digest, by the signer it
/// names, a member of `signers`; one per signer, sorted by address.
pub fn collect(request: &Request, files: &[SignatureFile], signers: &[Address], now: u64) -> Result<Request> {
    let digest = request.checked_digest()?;
    ensure!(request.message.deadline() >= now, "the message's deadline has passed: prepare it again");
    for (i, f) in files.iter().enumerate() {
        ensure!(f.digest == digest, "signature file {} is for another message", i + 1);
        let by = recover_digest(digest, &f.signature).with_context(|| format!("signature file {}", i + 1))?;
        ensure!(by == f.signer, "signature file {} is not by the signer it names", i + 1);
    }
    let all: Vec<Bytes> = request
        .signatures
        .iter()
        .map(|s| s.signature.clone())
        .chain(files.iter().map(|f| f.signature.clone()))
        .collect();
    let sorted = sorted_signatures(digest, &all, signers)?;
    let mut bundle = request.clone();
    bundle.signatures = sorted.into_iter().map(|(signer, signature)| Signed { signer, signature }).collect();
    Ok(bundle)
}

// ----------------------------------------------------------------------------------------- Base

/// The deployment's contracts and signer set, as the Registry names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub registry: Address,
    pub intake: Address,
    pub versions: Address,
    pub signers: Vec<Address>,
}

impl Deployment {
    pub async fn read<P: Provider>(provider: &P, registry: Address) -> Result<Self> {
        let r = IZecuredRegistry::new(registry, provider);
        let signers = r.signers().call().await.context("Registry.signers")?.to_vec();
        let intake = r.intake().call().await.context("Registry.intake")?;
        ensure!(intake != Address::ZERO, "the Registry names no Intake");
        let versions = IZecuredIntake::new(intake, provider).versions().call().await.context("Intake.versions")?;
        Ok(Self {
            registry,
            intake,
            versions,
            signers,
        })
    }

    /// The contract that verifies messages of `verifier`'s kind.
    pub fn contract(&self, verifier: Verifier) -> Address {
        match verifier {
            Verifier::Intake => self.intake,
            Verifier::Versions => self.versions,
            Verifier::Registry => self.registry,
        }
    }
}

/// The nonce a message must carry now (`None`: the message has none).
async fn current_nonce<P: Provider>(provider: &P, d: &Deployment, verifier: Verifier) -> Result<Option<U256>> {
    Ok(match verifier {
        Verifier::Intake => {
            Some(IZecuredIntake::new(d.intake, provider).guardNonce().call().await.context("Intake.guardNonce")?)
        }
        Verifier::Versions => {
            Some(IZecuredVersions::new(d.versions, provider).nonce().call().await.context("ZecuredVersions.nonce")?)
        }
        Verifier::Registry => None,
    })
}

fn message_nonce(m: &OperatorMessage) -> Option<U256> {
    match m {
        OperatorMessage::PauseMinting { nonce, .. }
        | OperatorMessage::UnpauseMinting { nonce, .. }
        | OperatorMessage::ScheduleVersion { nonce, .. }
        | OperatorMessage::RemoveVersion { nonce, .. } => Some(*nonce),
        OperatorMessage::ReimburseDestination { .. } => None,
    }
}

/// The digest the verifying contract computes for `m` (its `hash...` view).
async fn contract_digest<P: Provider>(provider: &P, contract: Address, m: &OperatorMessage) -> Result<B256> {
    Ok(match m {
        OperatorMessage::PauseMinting { until, nonce, deadline } => IZecuredIntake::new(contract, provider)
            .hashPauseMinting(*until, *nonce, *deadline)
            .call()
            .await
            .context("Intake.hashPauseMinting")?,
        OperatorMessage::UnpauseMinting { nonce, deadline } => IZecuredIntake::new(contract, provider)
            .hashUnpauseMinting(*nonce, *deadline)
            .call()
            .await
            .context("Intake.hashUnpauseMinting")?,
        OperatorMessage::ScheduleVersion {
            kind,
            selector,
            image_id,
            branches,
            nonce,
            deadline,
        } => IZecuredVersions::new(contract, provider)
            .hashScheduleVersion(kind.code(), *selector, *image_id, *branches, *nonce, *deadline)
            .call()
            .await
            .context("ZecuredVersions.hashScheduleVersion")?,
        OperatorMessage::RemoveVersion {
            version_id,
            nonce,
            deadline,
        } => IZecuredVersions::new(contract, provider)
            .hashRemoveVersion(*version_id, *nonce, *deadline)
            .call()
            .await
            .context("ZecuredVersions.hashRemoveVersion")?,
        OperatorMessage::ReimburseDestination {
            claim_id,
            commitment,
            deadline,
        } => IZecuredRegistry::new(contract, provider)
            .hashReimburseDestination(*claim_id, *commitment, *deadline)
            .call()
            .await
            .context("Registry.hashReimburseDestination")?,
    })
}

/// Distinct signer signatures the contract needs for `m` now: its fixed threshold, or for a
/// `RemoveVersion` the version registry's rules as it reads them (`isWorking`: active, and its
/// router still verifies its seals): three for a version that is not working; for a working one,
/// R-22 and R-23 over the other working versions of its kind.
pub async fn signatures_needed<P: Provider>(provider: &P, d: &Deployment, m: &OperatorMessage) -> Result<usize> {
    let OperatorMessage::RemoveVersion { version_id, .. } = m else {
        return Ok(m.threshold());
    };
    let versions = IZecuredVersions::new(d.versions, provider);
    let v = versions.version(*version_id).call().await.context("ZecuredVersions.version")?;
    ensure!(v.activeAt != 0 && !v.removed, "no such version, or it is removed already");
    let kind = VersionKind::from_code(v.kind)?;
    if !versions.isWorking(*version_id).call().await.context("ZecuredVersions.isWorking")? {
        return Ok(3); // stopped, or not active yet
    }
    let ids = versions.versionIds(v.kind).call().await.context("ZecuredVersions.versionIds")?;
    let mut others = 0;
    for id in ids.iter().filter(|id| *id != version_id) {
        if versions.isWorking(*id).call().await.context("ZecuredVersions.isWorking")? {
            others += 1;
        }
    }
    removal_threshold(kind, others).ok_or_else(|| {
        anyhow!(
            "the last working violation version cannot be removed (R-22): schedule its successor and wait until it works"
        )
    })
}

/// One version of the deployment's version registry, as `zecured-ops versions` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub version_id: B256,
    pub kind: VersionKind,
    pub selector: FixedBytes<4>,
    pub image_id: B256,
    pub branches: [u32; 4],
    /// When it starts (or started) verifying; zero: never scheduled.
    pub active_at: u64,
    pub removed: bool,
    /// Active, and its router still verifies its seals.
    pub working: bool,
}

/// Every version of the deployment's registry, by kind (a registry from before v4 has no Swap
/// kind: none are listed for it).
pub async fn list_versions<P: Provider>(provider: &P, d: &Deployment) -> Result<Vec<VersionInfo>> {
    let versions = IZecuredVersions::new(d.versions, provider);
    let mut out = vec![];
    for kind in [VersionKind::Deposit, VersionKind::Violation, VersionKind::Swap] {
        let ids = match versions.versionIds(kind.code()).call().await {
            Ok(ids) => ids,
            Err(_) if kind == VersionKind::Swap => continue,
            Err(e) => return Err(e).context("ZecuredVersions.versionIds"),
        };
        for id in ids {
            let v = versions.version(id).call().await.context("ZecuredVersions.version")?;
            let working = versions.isWorking(id).call().await.context("ZecuredVersions.isWorking")?;
            out.push(VersionInfo {
                version_id: id,
                kind,
                selector: v.selector,
                image_id: v.imageId,
                branches: v.branches,
                active_at: v.activeAt,
                removed: v.removed,
                working,
            });
        }
    }
    Ok(out)
}

/// What `prepare` is asked for, before Base gives the contract and the nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    PauseMinting {
        until: u64,
    },
    UnpauseMinting,
    ScheduleVersion {
        kind: VersionKind,
        selector: FixedBytes<4>,
        image_id: B256,
        branches: [u32; 4],
    },
    RemoveVersion {
        version_id: B256,
    },
    ReimburseDestination {
        claim_id: B256,
        commitment: B256,
    },
}

/// W-6: the contents of a new treasury keys file for the treasury whose unified full viewing key is
/// `ufvk` (Zcash `network`): its Ironwood external incoming viewing key and a fresh random salt key.
/// One operator makes it and shares it privately with the others and the coordinator; nothing of it is
/// printed.
pub fn new_treasury_keys(network: zecured_zcash_ops::network::ZcashNetwork, ufvk: &str) -> Result<serde_json::Value> {
    let fvk = zecured_zcash_ops::keys::decode_ufvk(&network.params(), ufvk.trim())?;
    let ivk = fvk.to_ivk(orchard::keys::Scope::External).to_bytes();
    let mut salt_key = [0u8; 32];
    rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut salt_key);
    TreasuryKeys::from_bytes(&ivk, salt_key)?;
    Ok(serde_json::json!({ "ivk": hex::encode(ivk), "saltKey": hex::encode(salt_key) }))
}

/// W-6: reads the treasury keys file (`{"ivk": <128 hex>, "saltKey": <64 hex>}`, private to the
/// signers; never printed).
pub fn read_treasury(path: &Path) -> Result<TreasuryKeys> {
    TreasuryKeys::read_file(path)
}

/// A request and the signatures its contract needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub request: Request,
    pub needed: usize,
}

/// The request for `spec`, valid for `ttl` seconds from `now`, for the deployment of `net`.
pub async fn prepare<P: Provider>(provider: &P, net: &Network, spec: &Spec, ttl: u64, now: u64) -> Result<Prepared> {
    let chain = provider.get_chain_id().await.context("Base RPC")?;
    ensure!(
        chain == net.chain_id,
        "the RPC serves chain {chain}, the network config is for chain {}",
        net.chain_id
    );
    let d = Deployment::read(provider, net.registry).await?;
    let deadline = now.checked_add(ttl).context("the deadline overflows")?;
    let verifier = match spec {
        Spec::PauseMinting { .. } | Spec::UnpauseMinting => Verifier::Intake,
        Spec::ScheduleVersion { .. } | Spec::RemoveVersion { .. } => Verifier::Versions,
        Spec::ReimburseDestination { .. } => Verifier::Registry,
    };
    let nonce = current_nonce(provider, &d, verifier).await?.unwrap_or_default();
    let message = match *spec {
        Spec::PauseMinting { until } => {
            ensure!(until > now, "the pause must end in the future");
            OperatorMessage::PauseMinting { until, nonce, deadline }
        }
        Spec::UnpauseMinting => OperatorMessage::UnpauseMinting { nonce, deadline },
        Spec::ScheduleVersion {
            kind,
            selector,
            image_id,
            branches,
        } => {
            ensure!(
                image_id != B256::ZERO && selector != FixedBytes::ZERO,
                "a version needs its selector and image id"
            );
            OperatorMessage::ScheduleVersion {
                kind,
                selector,
                image_id,
                branches,
                nonce,
                deadline,
            }
        }
        Spec::RemoveVersion { version_id } => OperatorMessage::RemoveVersion {
            version_id,
            nonce,
            deadline,
        },
        Spec::ReimburseDestination { claim_id, commitment } => {
            ensure!(commitment != B256::ZERO, "the destination's commitment is zero");
            let c = IZecuredRegistry::new(d.registry, provider)
                .claimDetails(claim_id)
                .call()
                .await
                .context("Registry.claimDetails")?;
            ensure!(c.bondPaid, "the bonds have not paid this claim: there is nothing to reimburse");
            OperatorMessage::ReimburseDestination {
                claim_id,
                commitment,
                deadline,
            }
        }
    };
    let request = Request::new(net.chain_id, d.contract(verifier), message);
    let on_chain = contract_digest(provider, request.contract, &request.message).await?;
    ensure!(
        on_chain == request.digest,
        "the contract hashes this message differently ({on_chain}): do not sign it"
    );
    let needed = signatures_needed(provider, &d, &request.message).await?;
    Ok(Prepared { request, needed })
}

/// A bundle checked against Base: the call to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub contract: Address,
    pub calldata: Bytes,
    pub signatures: usize,
    pub needed: usize,
}

/// Checks a bundle against the deployment of `registry` as it is now: chain, contract, digest,
/// nonce, deadline, and enough distinct signatures by the Registry's signer set.
pub async fn check_submission<P: Provider>(
    provider: &P,
    registry: Address,
    bundle: &Request,
    now: u64,
) -> Result<Checked> {
    let digest = bundle.checked_digest()?;
    ensure!(
        bundle.message.deadline() >= now,
        "the signatures expired at {}: prepare and sign again",
        bundle.message.deadline()
    );
    let chain = provider.get_chain_id().await.context("Base RPC")?;
    ensure!(
        chain == bundle.chain_id,
        "the RPC serves chain {chain}, the bundle is for chain {}",
        bundle.chain_id
    );
    let d = Deployment::read(provider, registry).await?;
    let verifier = bundle.message.verifier();
    let contract = d.contract(verifier);
    ensure!(
        bundle.contract == contract,
        "the bundle is for {}, but this deployment's {verifier:?} contract is {contract}",
        bundle.contract
    );
    if let (Some(signed), Some(current)) =
        (message_nonce(&bundle.message), current_nonce(provider, &d, verifier).await?)
    {
        ensure!(
            signed == current,
            "the bundle's nonce is {signed}, the contract's is now {current}: another message went first; prepare and sign again"
        );
    }
    let on_chain = contract_digest(provider, contract, &bundle.message).await?;
    ensure!(on_chain == digest, "the contract hashes this message differently: not sending it");
    let sigs: Vec<Bytes> = bundle.signatures.iter().map(|s| s.signature.clone()).collect();
    let sorted = sorted_signatures(digest, &sigs, &d.signers)?;
    let needed = signatures_needed(provider, &d, &bundle.message).await?;
    ensure!(
        sorted.len() >= needed,
        "{} distinct signer signatures, the contract needs {needed}: collect more",
        sorted.len()
    );
    Ok(Checked {
        contract,
        signatures: sorted.len(),
        needed,
        calldata: bundle.message.calldata(sorted.into_iter().map(|(_, s)| s).collect()),
    })
}

fn call_of(checked: &Checked, from: Address) -> TransactionRequest {
    TransactionRequest::default()
        .with_from(from)
        .with_to(checked.contract)
        .with_input(checked.calldata.clone())
}

/// Runs the call on Base without sending it (`eth_call` from `from`).
pub async fn simulate<P: Provider>(provider: &P, checked: &Checked, from: Address) -> Result<()> {
    provider.call(call_of(checked, from)).await.map_err(|e| anyhow!("the call would revert: {e}"))?;
    Ok(())
}

/// Sends the call from `sender` and waits for its receipt: the transaction's hash.
pub async fn send(rpc: &str, checked: &Checked, sender: PrivateKeySigner) -> Result<B256> {
    let from = sender.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(sender))
        .connect_http(rpc.parse().context("Base RPC URL")?);
    let tx = call_of(checked, from);
    let gas = provider.estimate_gas(tx.clone()).await.map_err(|e| anyhow!("the call would revert: {e}"))?;
    let receipt = provider
        .send_transaction(tx.with_gas_limit(gas_with_headroom(gas)))
        .await
        .context("send the transaction")?
        .get_receipt()
        .await
        .context("wait for the receipt")?;
    ensure!(receipt.status(), "the transaction {} reverted", receipt.transaction_hash);
    Ok(receipt.transaction_hash)
}

/// A time from the command line: Unix seconds, or `+N` with an optional unit (`s`, `m`, `h`, `d`)
/// from `now`.
pub fn parse_time(text: &str, now: u64) -> Result<u64> {
    let Some(rel) = text.strip_prefix('+') else {
        return text.parse().map_err(|_| anyhow!("not a time: {text} (Unix seconds or +N[s|m|h|d])"));
    };
    let (digits, unit) = match rel.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&rel[..i], c),
        _ => (rel, 's'),
    };
    let n: u64 = digits.parse().map_err(|_| anyhow!("not a duration: {text}"))?;
    let secs = match unit {
        's' => n,
        'm' => n * 60,
        'h' => n * 3_600,
        'd' => n * 86_400,
        _ => bail!("unknown unit in {text}: s, m, h or d"),
    };
    now.checked_add(secs).context("the time overflows")
}

/// A consensus branch id from the command line (`0x77190ad9`).
pub fn parse_branch(text: &str) -> Result<u32> {
    u32::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|_| anyhow!("not a branch id: {text}"))
}

#[cfg(test)]
mod tests {

    /// W-6: the treasury keys file is made from the treasury's viewing key, with a fresh salt key
    /// each time, and reads back as the keys it holds.
    #[test]
    fn w6_the_treasury_keys_file_is_made_from_the_viewing_key() {
        use orchard::keys::{FullViewingKey, Scope, SpendingKey};
        use zecured_zcash_ops::network::ZcashNetwork;
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes([7; 32])).unwrap();
        let fvk = FullViewingKey::from(&sk);
        let ufvk = zecured_zcash_ops::keys::encode_ufvk(&ZcashNetwork::Testnet.params(), &fvk).unwrap();
        let a = new_treasury_keys(ZcashNetwork::Testnet, &ufvk).unwrap();
        let b = new_treasury_keys(ZcashNetwork::Testnet, &ufvk).unwrap();
        assert_eq!(a["ivk"], hex::encode(fvk.to_ivk(Scope::External).to_bytes()));
        assert_eq!(a["ivk"], b["ivk"]);
        assert_ne!(a["saltKey"], b["saltKey"], "a fresh salt key");
        let keys = TreasuryKeys::from_hex(a["ivk"].as_str().unwrap(), a["saltKey"].as_str().unwrap()).unwrap();
        assert_eq!(keys.reimbursement(&[1; 32]).receiver.len(), 43);
        assert!(new_treasury_keys(ZcashNetwork::Mainnet, &ufvk).is_err(), "a Testnet key on Mainnet");
        assert!(new_treasury_keys(ZcashNetwork::Testnet, "uviewtest1nope").is_err());
    }

    /// W-6: a signer signs a reimbursement destination only if it is the claim's treasury
    /// destination as its own treasury keys derive it, and never without the keys.
    #[test]
    fn w6_only_the_derived_treasury_destination_is_signed() {
        use orchard::keys::{FullViewingKey, Scope, SpendingKey};
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes([7; 32])).unwrap();
        let ivk = FullViewingKey::from(&sk).to_ivk(Scope::External).to_bytes();
        let treasury = TreasuryKeys::from_bytes(&ivk, [9; 32]).unwrap();
        let claim = B256::repeat_byte(0xc1);
        let derived = B256::from(treasury.reimbursement(&claim.0).commitment());
        let key = PrivateKeySigner::random();
        let message = |commitment| OperatorMessage::ReimburseDestination {
            claim_id: claim,
            commitment,
            deadline: NOW + 60,
        };
        let request = |commitment| Request::new(CHAIN, Address::repeat_byte(0x61), message(commitment));
        assert!(sign(&request(derived), &key, NOW, Some(&treasury)).is_ok());
        let e = sign(&request(B256::repeat_byte(0xbb)), &key, NOW, Some(&treasury)).unwrap_err();
        assert!(e.to_string().contains("W-6"), "{e}");
        assert!(sign(&request(derived), &key, NOW, None).is_err(), "never without the treasury keys");
        assert!(check_reimbursement(&pause(1), None).is_ok(), "other messages need no keys");
    }
    use std::sync::{Arc, Mutex};

    use alloy::sol_types::SolCall;

    use super::*;

    const CHAIN: u64 = 84_532;
    const REGISTRY: Address = Address::repeat_byte(0x11);
    const INTAKE: Address = Address::repeat_byte(0x66);
    const VERSIONS: Address = Address::repeat_byte(0x99);
    const NOW: u64 = 1_800_000_000;

    fn keys() -> Vec<PrivateKeySigner> {
        (0..5).map(|_| PrivateKeySigner::random()).collect()
    }

    fn pause(nonce: u64) -> OperatorMessage {
        OperatorMessage::PauseMinting {
            until: NOW + 86_400,
            nonce: U256::from(nonce),
            deadline: NOW + 3_600,
        }
    }

    /// Three of five signers sign a pause; the bundle holds each once, sorted by address; nothing
    /// written names a key.
    #[test]
    fn a_request_is_signed_and_collected() {
        let keys = keys();
        let set: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let request = Request::new(CHAIN, INTAKE, pause(3));
        let files: Vec<SignatureFile> = keys[..3].iter().map(|k| sign(&request, k, NOW, None).unwrap()).collect();
        let bundle = collect(&request, &files, &set, NOW).unwrap();
        assert_eq!(bundle.signatures.len(), 3);
        assert!(bundle.signatures.windows(2).all(|w| w[0].signer < w[1].signer));
        // Collected again with a signer already in: counted once; a fourth comes in another round.
        let again =
            collect(&bundle, &[files[0].clone(), sign(&request, &keys[3], NOW, None).unwrap()], &set, NOW).unwrap();
        assert_eq!(again.signatures.len(), 4);
        // The files round-trip and carry addresses and signatures only.
        let written = serde_json::to_string(&again).unwrap() + &serde_json::to_string(&files).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&again).unwrap()).unwrap(), again);
        for k in &keys {
            assert!(!written.contains(&hex::encode(k.to_bytes())), "a key was written");
        }
        let request_json = serde_json::to_value(&request).unwrap();
        assert_eq!(request_json["message"]["type"], "pause-minting");
        assert!(request_json.get("signatures").is_none(), "a request has no signatures yet");
    }

    /// Altered files, strangers, other messages, a signer field that is not the signer, and
    /// expired messages are refused.
    #[test]
    fn collect_refuses_what_the_contract_would() {
        let keys = keys();
        let set: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let request = Request::new(CHAIN, INTAKE, pause(3));
        // A message changed after preparation (its digest no longer matches) is never signed.
        let mut altered = request.clone();
        altered.message = pause(4);
        assert!(sign(&altered, &keys[0], NOW, None).is_err());
        // Expired.
        assert!(sign(&request, &keys[0], NOW + 3_601, None).is_err());
        let good = sign(&request, &keys[0], NOW, None).unwrap();
        assert!(collect(&request, std::slice::from_ref(&good), &set, NOW + 3_601).is_err());
        // Another message's signature.
        let other = sign(&Request::new(CHAIN, INTAKE, pause(4)), &keys[1], NOW, None).unwrap();
        assert!(collect(&request, &[other], &set, NOW).is_err());
        // A stranger.
        let stranger = sign(&request, &PrivateKeySigner::random(), NOW, None).unwrap();
        assert!(collect(&request, &[stranger], &set, NOW).is_err());
        // A file naming another signer than its signature's.
        let mut misnamed = sign(&request, &keys[1], NOW, None).unwrap();
        misnamed.signer = keys[2].address();
        assert!(collect(&request, &[misnamed], &set, NOW).is_err());
        // The same message under another contract's or chain's domain is another digest.
        assert_ne!(Request::new(CHAIN, VERSIONS, pause(3)).digest, request.digest);
        assert_ne!(Request::new(8_453, INTAKE, pause(3)).digest, request.digest);
    }

    /// Keys come from a node identity file or the environment, and never show in an error.
    #[test]
    fn keys_come_from_identity_files_or_the_environment() {
        let dir = std::env::temp_dir().join(format!("zecured-ops-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = PrivateKeySigner::random();
        let path = dir.join("identity.json");
        let identity =
            serde_json::json!({ "index": 2, "eth_private_key": hex::encode(key.to_bytes()), "x25519_secret": "00" });
        std::fs::write(&path, identity.to_string()).unwrap();
        assert_eq!(load_key(Some(&path), None).unwrap().address(), key.address());
        let var = "ZECURED_OPS_KEY";
        let env = |value: String| move |name: &str| (name == var).then(|| value.clone());
        let with_prefix = format!("0x{}", hex::encode(key.to_bytes()));
        assert_eq!(load_key_with(None, Some(var), env(with_prefix.clone())).unwrap().address(), key.address());
        assert!(load_key_with(Some(&path), Some(var), env(with_prefix)).is_err(), "one source only");
        assert!(load_key(None, None).is_err());
        assert!(load_key_with(None, Some(var), |_| None).is_err(), "unset");
        // A malformed key: the error names the source, never the value.
        let bad = "zz".repeat(32);
        std::fs::write(&path, serde_json::json!({ "eth_private_key": bad }).to_string()).unwrap();
        let e = format!("{:#}", load_key(Some(&path), None).unwrap_err());
        assert!(!e.contains(&bad), "{e}");
        let e = format!("{:#}", load_key_with(None, Some(var), env(bad.clone())).unwrap_err());
        assert!(!e.contains(&bad) && e.contains(var), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn times_branches_and_scrubbing() {
        assert_eq!(parse_time("1800000000", 5).unwrap(), 1_800_000_000);
        assert_eq!(parse_time("+90", 100).unwrap(), 190);
        assert_eq!(parse_time("+90m", 100).unwrap(), 100 + 5_400);
        assert_eq!(parse_time("+6h", 0).unwrap(), 21_600);
        assert_eq!(parse_time("+2d", 0).unwrap(), 172_800);
        assert!(parse_time("+2w", 0).is_err() && parse_time("soon", 0).is_err());
        assert_eq!(parse_branch("0x77190ad9").unwrap(), 0x7719_0ad9);
        assert!(parse_branch("0xg").is_err());
        let rpc = "https://base-sepolia.example/v2/s3cretkey?x=1";
        let text =
            format!("error sending request for url ({rpc}): timed out; also https://base-sepolia.example/v2/s3cretkey");
        let out = scrub(&text, rpc);
        assert!(!out.contains("s3cretkey"), "{out}");
        assert_eq!(scrub("http://127.0.0.1:8545/ refused", "http://127.0.0.1:8545"), "<Base RPC> refused");
    }

    // ------------------------------------------------------------------- against a stub of Base

    /// Answers by calldata prefix: a function selector, or a selector and its first argument.
    type Answers = Arc<Mutex<Vec<(Vec<u8>, Vec<u8>)>>>;

    /// A Base RPC stub answering `eth_call` with the answer of the longest matching calldata prefix
    /// (`answers`, changeable while it runs; no match reverts), `eth_chainId` with Base Sepolia's.
    async fn base_stub(answers: Answers) -> String {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| {
                let answers = answers.clone();
                async move {
                    let result = match req["method"].as_str() {
                        Some("eth_chainId") => serde_json::json!("0x14a34"),
                        Some("eth_call") => {
                            let call = &req["params"][0];
                            let data = call["input"].as_str().or(call["data"].as_str()).unwrap_or_default();
                            let data = hex::decode(data.trim_start_matches("0x")).unwrap_or_default();
                            let found = answers
                                .lock()
                                .unwrap()
                                .iter()
                                .filter(|(prefix, _)| data.starts_with(prefix))
                                .max_by_key(|(prefix, _)| prefix.len())
                                .cloned();
                            match found {
                                Some((_, ret)) => serde_json::json!(format!("0x{}", hex::encode(ret))),
                                None => {
                                    return Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"],
                                        "error": { "code": 3, "message": "execution reverted" } }));
                                }
                            }
                        }
                        _ => serde_json::Value::Null,
                    };
                    Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn set(answers: &Answers, prefix: impl AsRef<[u8]>, ret: Vec<u8>) {
        let prefix = prefix.as_ref().to_vec();
        let mut a = answers.lock().unwrap();
        a.retain(|(p, _)| *p != prefix);
        a.push((prefix, ret));
    }

    /// The calldata prefix of `selector` called with `arg` first.
    fn with_arg(selector: [u8; 4], arg: B256) -> Vec<u8> {
        [selector.as_slice(), arg.as_slice()].concat()
    }

    fn network(rpc: &str, signers: &[Address]) -> Network {
        Network {
            registry: REGISTRY,
            chain_id: CHAIN,
            base_rpc_url: rpc.to_owned(),
            nodes: signers.iter().zip(1u16..).map(|(eth, index)| NetworkNode { index, eth: *eth }).collect(),
        }
    }

    /// A pause from preparation to a simulated submission, and every check `submit` makes
    /// against Base before sending: the deployment's contract, the current nonce, the contract's
    /// own digest and the number of distinct signer signatures.
    #[tokio::test]
    async fn a_pause_is_checked_against_base_before_it_is_sent() {
        let keys = keys();
        let mut signers: [Address; 5] = std::array::from_fn(|i| keys[i].address());
        signers.sort();
        let answers: Answers = Arc::default();
        let a = &answers;
        set(
            a,
            IZecuredRegistry::signersCall::SELECTOR,
            IZecuredRegistry::signersCall::abi_encode_returns(&signers),
        );
        set(a, IZecuredRegistry::intakeCall::SELECTOR, IZecuredRegistry::intakeCall::abi_encode_returns(&INTAKE));
        set(
            a,
            IZecuredIntake::versionsCall::SELECTOR,
            IZecuredIntake::versionsCall::abi_encode_returns(&VERSIONS),
        );
        set(
            a,
            IZecuredIntake::guardNonceCall::SELECTOR,
            IZecuredIntake::guardNonceCall::abi_encode_returns(&U256::from(3)),
        );
        set(a, IZecuredIntake::pauseMintingCall::SELECTOR, vec![]);
        let expected = Request::new(CHAIN, INTAKE, pause(3)).digest;
        let hash = IZecuredIntake::hashPauseMintingCall::SELECTOR;
        set(a, hash, expected.to_vec());
        let rpc = base_stub(answers.clone()).await;
        let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap());
        let net = network(&rpc, &signers);

        let prepared = prepare(&provider, &net, &Spec::PauseMinting { until: NOW + 86_400 }, 3_600, NOW).await.unwrap();
        assert_eq!(prepared.request, Request::new(CHAIN, INTAKE, pause(3)));
        assert_eq!(prepared.needed, 3);
        // Two signatures are not enough; three are.
        let files: Vec<SignatureFile> =
            keys[..3].iter().map(|k| sign(&prepared.request, k, NOW, None).unwrap()).collect();
        let two = collect(&prepared.request, &files[..2], &net.signers(), NOW).unwrap();
        let e = check_submission(&provider, REGISTRY, &two, NOW).await.unwrap_err();
        assert!(e.to_string().contains("needs 3"), "{e}");
        let bundle = collect(&prepared.request, &files, &net.signers(), NOW).unwrap();
        let checked = check_submission(&provider, REGISTRY, &bundle, NOW).await.unwrap();
        assert_eq!((checked.contract, checked.signatures, checked.needed), (INTAKE, 3, 3));
        let call = IZecuredIntake::pauseMintingCall::abi_decode(&checked.calldata).unwrap();
        assert_eq!(call.sigs.len(), 3);
        simulate(&provider, &checked, Address::ZERO).await.unwrap();

        // Expired by the time it is submitted.
        assert!(check_submission(&provider, REGISTRY, &bundle, NOW + 3_601).await.is_err());
        // Another message went first: the nonce moved on.
        set(
            a,
            IZecuredIntake::guardNonceCall::SELECTOR,
            IZecuredIntake::guardNonceCall::abi_encode_returns(&U256::from(4)),
        );
        let e = check_submission(&provider, REGISTRY, &bundle, NOW).await.unwrap_err();
        assert!(e.to_string().contains("nonce"), "{e}");
        set(
            a,
            IZecuredIntake::guardNonceCall::SELECTOR,
            IZecuredIntake::guardNonceCall::abi_encode_returns(&U256::from(3)),
        );
        // A bundle for another contract than the deployment's.
        let mut elsewhere = collect(
            &Request::new(CHAIN, Address::repeat_byte(0x67), pause(3)),
            &keys[..3]
                .iter()
                .map(|k| sign(&Request::new(CHAIN, Address::repeat_byte(0x67), pause(3)), k, NOW, None).unwrap())
                .collect::<Vec<_>>(),
            &net.signers(),
            NOW,
        )
        .unwrap();
        let e = check_submission(&provider, REGISTRY, &elsewhere, NOW).await.unwrap_err();
        assert!(e.to_string().contains("deployment"), "{e}");
        elsewhere.contract = INTAKE; // and its digest no longer matches the message
        assert!(check_submission(&provider, REGISTRY, &elsewhere, NOW).await.is_err());
        // The contract hashes the message differently: never signed, never sent.
        set(a, hash, B256::repeat_byte(1).to_vec());
        assert!(prepare(&provider, &net, &Spec::PauseMinting { until: NOW + 86_400 }, 3_600, NOW).await.is_err());
        assert!(check_submission(&provider, REGISTRY, &bundle, NOW).await.is_err());
        set(a, hash, expected.to_vec());
        // Signers the Registry does not know (the config's set is stale).
        let others: [Address; 5] = std::array::from_fn(|i| Address::repeat_byte(0xe0 + i as u8));
        set(
            a,
            IZecuredRegistry::signersCall::SELECTOR,
            IZecuredRegistry::signersCall::abi_encode_returns(&others),
        );
        assert!(check_submission(&provider, REGISTRY, &bundle, NOW).await.is_err());
        set(
            a,
            IZecuredRegistry::signersCall::SELECTOR,
            IZecuredRegistry::signersCall::abi_encode_returns(&signers),
        );
        // A call that reverts on Base fails the simulation.
        answers.lock().unwrap().retain(|(s, _)| *s != IZecuredIntake::pauseMintingCall::SELECTOR);
        assert!(simulate(&provider, &checked, Address::ZERO).await.is_err());
    }

    /// The version registry's removal rules as it reads them (`isWorking`): removing the last
    /// working deposit version needs all five signatures (R-23), with another working one three;
    /// the last working violation version cannot go (R-22); a version that is not working goes
    /// with three; swap versions have no last-version rule.
    #[tokio::test]
    async fn removal_thresholds_follow_the_versions_on_base() {
        let answers: Answers = Arc::default();
        let rpc = base_stub(answers.clone()).await;
        let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap());
        let d = Deployment {
            registry: REGISTRY,
            intake: INTAKE,
            versions: VERSIONS,
            signers: vec![],
        };
        let (target, other) = (B256::repeat_byte(0xa1), B256::repeat_byte(0xa2));
        let version = |kind: u8| IZecuredVersions::Version {
            kind,
            selector: FixedBytes([1, 2, 3, 4]),
            imageId: B256::repeat_byte(7),
            branches: [0; 4],
            activeAt: 1,
            removed: false,
        };
        let remove = OperatorMessage::RemoveVersion {
            version_id: target,
            nonce: U256::ZERO,
            deadline: NOW,
        };
        let a = &answers;
        let working = |id: B256, yes: bool| {
            set(
                a,
                with_arg(IZecuredVersions::isWorkingCall::SELECTOR, id),
                IZecuredVersions::isWorkingCall::abi_encode_returns(&yes),
            )
        };
        set(
            a,
            IZecuredVersions::versionCall::SELECTOR,
            IZecuredVersions::versionCall::abi_encode_returns(&version(0)),
        );
        set(
            a,
            IZecuredVersions::versionIdsCall::SELECTOR,
            IZecuredVersions::versionIdsCall::abi_encode_returns(&vec![target, other]),
        );
        working(target, true);
        working(other, true);
        assert_eq!(signatures_needed(&provider, &d, &remove).await.unwrap(), 3, "another deposit version works");
        working(other, false);
        assert_eq!(signatures_needed(&provider, &d, &remove).await.unwrap(), 5, "the last working deposit version");
        set(
            a,
            IZecuredVersions::versionCall::SELECTOR,
            IZecuredVersions::versionCall::abi_encode_returns(&version(1)),
        );
        assert!(signatures_needed(&provider, &d, &remove).await.is_err(), "the last working violation version stays");
        working(target, false);
        assert_eq!(signatures_needed(&provider, &d, &remove).await.unwrap(), 3, "a stopped version goes with three");
        working(target, true);
        set(
            a,
            IZecuredVersions::versionCall::SELECTOR,
            IZecuredVersions::versionCall::abi_encode_returns(&version(2)),
        );
        assert_eq!(
            signatures_needed(&provider, &d, &remove).await.unwrap(),
            3,
            "swap versions have no last-version rule"
        );
        // The listing an operator removes from: each version with its kind and whether it works; a
        // registry without the Swap kind lists none.
        let kinds = Answers::default();
        let listing = base_stub(kinds.clone()).await;
        let listed = ProviderBuilder::new().connect_http(listing.parse().unwrap());
        let k = &kinds;
        let ids_of = |kind: u8| with_arg(IZecuredVersions::versionIdsCall::SELECTOR, B256::with_last_byte(kind));
        set(k, ids_of(0), IZecuredVersions::versionIdsCall::abi_encode_returns(&vec![target]));
        set(k, ids_of(1), IZecuredVersions::versionIdsCall::abi_encode_returns(&vec![other]));
        for (id, kind, works) in [(target, 0, true), (other, 1, false)] {
            set(
                k,
                with_arg(IZecuredVersions::versionCall::SELECTOR, id),
                IZecuredVersions::versionCall::abi_encode_returns(&version(kind)),
            );
            set(
                k,
                with_arg(IZecuredVersions::isWorkingCall::SELECTOR, id),
                IZecuredVersions::isWorkingCall::abi_encode_returns(&works),
            );
        }
        let all = list_versions(&listed, &d).await.unwrap();
        let summary: Vec<(B256, VersionKind, bool)> = all.iter().map(|v| (v.version_id, v.kind, v.working)).collect();
        assert_eq!(
            summary,
            [
                (target, VersionKind::Deposit, true),
                (other, VersionKind::Violation, false)
            ]
        );
        assert_eq!(serde_json::to_value(&all[0]).unwrap()["kind"], "deposit");
        // Other messages have their fixed thresholds.
        assert_eq!(signatures_needed(&provider, &d, &pause(0)).await.unwrap(), 3);
        let reimburse = OperatorMessage::ReimburseDestination {
            claim_id: B256::ZERO,
            commitment: B256::ZERO,
            deadline: NOW,
        };
        assert_eq!(signatures_needed(&provider, &d, &reimburse).await.unwrap(), 3, "W-6");
    }
}
