//! Node side of vault creation: FROST DKG plus agreement on the ZIP 2005 `sk`.
//!
//! Round 1: check the owner's `OpenVault` request (P0), run DKG part 1, commit to a random
//! contribution `r_i` to the vault secret.
//! Round 2: run DKG part 2; seal each recipient's round-2 package together with `r_i`.
//! Round 3: open the packages sealed to us, check every `r_j` against its commitment,
//! finish the DKG, derive `sk = H(vault_id, r_1..r_5)` and the vault's viewing keys, and sign
//! `VaultOpened` for the address we derived and the tag key of the viewing key we derived (C-4;
//! v2 signs `tagKeyHash = keccak256(K)`, which binds every deposit proof to the vault's own K).
//!
//! The coordinator relays every message but only ever sees public packages, commitments
//! and ciphertexts. Sealed messages are bound to the round-1 transcript (M-4): if the
//! coordinator shows two signers different round-1 outputs, they cannot open each other's
//! messages and the DKG fails instead of producing two different vaults. The owner accepts the
//! vault's address only with all five `VaultOpened` signatures, so the coordinator cannot hand
//! out an address (or viewing key) of its own.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use alloy::{
    primitives::{Address, B256, Bytes},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rand_core_06::{OsRng, RngCore};
use reddsa::frost::redpallas::{
    Identifier,
    keys::{
        EvenY,
        dkg::{self, round1, round2},
    },
};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;
use zecured_base::{OpenVault, recover_typed, registry_domain, sign_typed, vault_opened};
use zecured_policy::derive::{sk_commitment, vault_sk};
use zecured_threshold::{MAX_SIGNERS, MIN_SIGNERS, ak_bytes, signer_id};
use zecured_zcash_ops::keys::{VaultSecret, encode_ironwood_ua, encode_ufvk, vault_address};

pub use crate::envelope::Sealed;

use crate::{
    config::NetworkConfig,
    envelope,
    store::{Store, VaultRecord},
};

/// The owner's request to open a vault, plus the coordinator's chain parameters.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultOpen {
    pub vault_id: B256,
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub request_id: B256,
    pub deadline: u64,
    /// `OpenVault` EIP-712 signature by `owner`.
    pub signature: Bytes,
    pub birthday: u32,
    pub expiry_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round1Out {
    pub index: u16,
    pub package: serde_json::Value,
    pub sk_commitment: B256,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round2In {
    pub vault_id: B256,
    pub round1: Vec<Round1Out>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round2Out {
    pub index: u16,
    /// Sealed message per recipient index.
    pub sealed: BTreeMap<u16, Sealed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round3In {
    pub vault_id: B256,
    pub round1: Vec<Round1Out>,
    /// Sealed messages addressed to this node, by sender index.
    pub sealed: BTreeMap<u16, Sealed>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round3Out {
    pub index: u16,
    pub ak: String,
    pub ufvk: String,
    pub address: String,
    /// Public: group key + verifying shares (needed by the coordinator to aggregate).
    pub public_key_package: serde_json::Value,
    /// `VaultOpened` (EIP-712, Registry domain) signed with this node's attestation key, for
    /// the owner's checked request, the `address` above and the tag key of the `ufvk` above (C-4).
    pub signature: Bytes,
}

/// What travels inside a sealed round-2 message.
#[derive(Serialize, Deserialize)]
struct Round2Payload {
    package: serde_json::Value,
    contribution: B256,
}

/// DKG state kept in memory between rounds.
pub struct Pending {
    open: VaultOpen,
    contribution: [u8; 32],
    round1_secret: Option<round1::SecretPackage>,
    round2_secret: Option<round2::SecretPackage>,
    /// Our own round-1 output, which must be relayed unchanged.
    round1_out: Round1Out,
    /// Hash of the round-1 transcript seen in round 2 (M-4).
    transcript: Option<[u8; 32]>,
    created: Instant,
}

impl Pending {
    /// Time since round 1 (pending DKGs expire, L-7).
    pub fn age(&self) -> Duration {
        self.created.elapsed()
    }

    /// L-9: whether this opening is for the same account or owner as `open`.
    pub fn same_parties(&self, open: &VaultOpen) -> bool {
        self.open.account == open.account || self.open.owner == open.owner
    }
}

/// Hash of a DKG attempt's round-1 transcript: the vault id and every signer's round-1 output
/// in index order. The coordinator draws a fresh vault id for every opening attempt and every
/// round-1 output carries fresh randomness, so this also identifies the attempt (M-4).
pub fn transcript_hash(vault_id: &B256, round1: &[Round1Out]) -> Result<[u8; 32]> {
    let mut outputs: Vec<&Round1Out> = round1.iter().collect();
    outputs.sort_by_key(|r| r.index);
    ensure!(outputs.windows(2).all(|w| w[0].index != w[1].index), "duplicate round-1 output");
    let mut h = blake2b_simd::Params::new().hash_length(32).personal(b"Zecured_DkgTrans").to_state();
    h.update(vault_id.as_slice());
    for r in outputs {
        // serde_json maps are ordered, so this encoding is canonical for a given value.
        let package = serde_json::to_vec(&r.package)?;
        h.update(&r.index.to_be_bytes());
        h.update(&u64::try_from(package.len())?.to_be_bytes());
        h.update(&package);
        h.update(r.sk_commitment.as_slice());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(h.finalize().as_bytes());
    Ok(out)
}

/// Associated data of the sealed round-2 messages of one DKG attempt: the vault id and the
/// round-1 transcript hash.
fn context(vault_id: &B256, transcript: &[u8; 32]) -> Vec<u8> {
    let mut c = b"zecured/dkg2/".to_vec();
    c.extend_from_slice(vault_id.as_slice());
    c.extend_from_slice(transcript);
    c
}

/// P0 checks on an `OpenVault` request.
pub fn check_open(cfg: &NetworkConfig, store: &Store, open: &VaultOpen, now: u64, zcash_tip: u32) -> Result<()> {
    let msg = OpenVault {
        owner: open.owner,
        account: open.account,
        amount: open.amount,
        requestId: open.request_id,
        deadline: open.deadline,
    };
    let signer = recover_typed(&registry_domain(cfg.chain_id, cfg.registry), &msg, &open.signature)?;
    ensure!(signer == open.owner, "OpenVault not signed by the owner");
    ensure!(open.deadline >= now, "request expired");
    ensure!(cfg.denominations.contains(&open.amount), "not a standard denomination");
    ensure!(!store.vaults.contains_key(&open.vault_id), "vault id already used");
    ensure!(!store.is_bound(open.account, open.owner), "account or owner already has a vault");
    // M-2: the vault is scanned from its birthday, so a birthday ahead of our own tip could
    // miss a deposit mined below it (never attested, never refundable).
    ensure!(open.birthday <= zcash_tip, "birthday {} is ahead of our tip {zcash_tip}", open.birthday);
    ensure!(
        zcash_tip - open.birthday <= MAX_TIP_SKEW,
        "birthday {} too far behind our tip {zcash_tip}",
        open.birthday
    );
    let expiry = open.birthday.checked_add(cfg.vault_expiry_blocks).context("expiry height overflow")?;
    ensure!(open.expiry_height == expiry, "unexpected expiry height");
    Ok(())
}

/// Blocks the coordinator's view of the Zcash tip may differ from ours.
const MAX_TIP_SKEW: u32 = 20;

pub fn round1(index: u16, open: VaultOpen) -> Result<(Pending, Round1Out)> {
    let id = signer_id(index)?;
    let (secret, package) = dkg::part1(id, MAX_SIGNERS, MIN_SIGNERS, OsRng).map_err(|e| anyhow!("dkg part1: {e}"))?;
    let mut contribution = [0u8; 32];
    OsRng.fill_bytes(&mut contribution);
    let out = Round1Out {
        index,
        package: serde_json::to_value(&package)?,
        sk_commitment: B256::from(sk_commitment(&open.vault_id.0, &contribution)),
    };
    Ok((
        Pending {
            open,
            contribution,
            round1_secret: Some(secret),
            round2_secret: None,
            round1_out: out.clone(),
            transcript: None,
            created: Instant::now(),
        },
        out,
    ))
}

fn others_round1(index: u16, round1: &[Round1Out]) -> Result<BTreeMap<Identifier, round1::Package>> {
    let mut map = BTreeMap::new();
    for r in round1.iter().filter(|r| r.index != index) {
        map.insert(signer_id(r.index)?, serde_json::from_value(r.package.clone())?);
    }
    ensure!(map.len() == usize::from(MAX_SIGNERS) - 1, "need round-1 packages from all other signers");
    Ok(map)
}

pub fn round2(
    cfg: &NetworkConfig,
    me: &x25519_dalek::StaticSecret,
    index: u16,
    pending: &mut Pending,
    input: &Round2In,
) -> Result<Round2Out> {
    ensure!(input.vault_id == pending.open.vault_id, "vault id mismatch");
    let signers: BTreeSet<u16> = cfg.nodes.iter().map(|n| n.index).collect();
    let senders: BTreeSet<u16> = input.round1.iter().map(|r| r.index).collect();
    ensure!(
        input.round1.len() == signers.len() && senders == signers,
        "need exactly one round-1 output from every signer"
    );
    let mine = input.round1.iter().find(|r| r.index == index).context("our round-1 output is missing")?;
    ensure!(*mine == pending.round1_out, "our round-1 output was altered");
    let transcript = transcript_hash(&input.vault_id, &input.round1)?;
    let secret = pending.round1_secret.take().context("round 1 not run or round 2 repeated")?;
    let (secret2, packages) =
        dkg::part2(secret, &others_round1(index, &input.round1)?).map_err(|e| anyhow!("dkg part2: {e}"))?;
    pending.round2_secret = Some(secret2);
    pending.transcript = Some(transcript);
    let associated_data = context(&input.vault_id, &transcript);
    let mut sealed = BTreeMap::new();
    for node in cfg.nodes.iter().filter(|n| n.index != index) {
        let package = packages
            .get(&signer_id(node.index)?)
            .with_context(|| format!("no round-2 package for {}", node.index))?;
        let payload = serde_json::to_vec(&Round2Payload {
            package: serde_json::to_value(package)?,
            contribution: B256::from(pending.contribution),
        })?;
        sealed.insert(node.index, envelope::seal(me, &node.x25519_public()?, &associated_data, &payload)?);
    }
    Ok(Round2Out { index, sealed })
}

/// `eth` is this node's attestation key: it signs `VaultOpened` for the vault derived here.
pub fn round3(
    cfg: &NetworkConfig,
    me: &x25519_dalek::StaticSecret,
    eth: &PrivateKeySigner,
    index: u16,
    pending: Pending,
    input: &Round3In,
    store: &mut Store,
) -> Result<Round3Out> {
    let secret2 = pending.round2_secret.as_ref().context("round 2 not run")?;
    let vault_id = pending.open.vault_id;
    ensure!(input.vault_id == vault_id, "vault id mismatch");
    // L-9: another opening for this account or owner may have completed since round 1.
    ensure!(!store.is_bound(pending.open.account, pending.open.owner), "account or owner already has a vault");
    let transcript = pending.transcript.context("round 2 not run")?;
    ensure!(
        transcript_hash(&vault_id, &input.round1)? == transcript,
        "the round-1 transcript changed since round 2"
    );
    let associated_data = context(&vault_id, &transcript);

    let mut contributions: BTreeMap<u16, [u8; 32]> = BTreeMap::new();
    contributions.insert(index, pending.contribution);
    let mut received = BTreeMap::new();
    for (&sender, sealed) in &input.sealed {
        let node = cfg.node(sender)?;
        let plain = envelope::open(me, &node.x25519_public()?, &associated_data, sealed)?;
        let payload: Round2Payload = serde_json::from_slice(&plain)?;
        let commitment =
            input.round1.iter().find(|r| r.index == sender).context("missing round-1 output")?.sk_commitment;
        if sk_commitment(&vault_id.0, &payload.contribution.0) != commitment.0 {
            bail!("signer {sender} revealed a contribution that does not match its commitment");
        }
        contributions.insert(sender, payload.contribution.0);
        received.insert(signer_id(sender)?, serde_json::from_value::<round2::Package>(payload.package)?);
    }
    ensure!(contributions.len() == usize::from(MAX_SIGNERS), "missing contributions");

    let (key_package, public_key_package) =
        dkg::part3(secret2, &others_round1(index, &input.round1)?, &received).map_err(|e| anyhow!("dkg part3: {e}"))?;
    ensure!(public_key_package.has_even_y(), "group key must have ỹ = 0");
    let ak = ak_bytes(&public_key_package)?;
    let ordered: Vec<[u8; 32]> = contributions.values().copied().collect();
    let sk = vault_sk(&vault_id.0, &ordered);
    let fvk = VaultSecret::from_bytes(sk).full_viewing_key(&ak)?;
    let network = Network::TestNetwork;
    let ufvk = encode_ufvk(&network, &fvk)?;
    let address = encode_ironwood_ua(&network, vault_address(&fvk))?;
    let open = pending.open;
    // C-4: vouch for the address and the viewing key's tag key K derived here, for the request
    // checked in round 1 (owner signature, account, denomination, expiry height against our own
    // tip). `tagKeyHash = keccak256(K)` binds K to the vault: a deposit proof's tag is derived with
    // K, and a proof under any other key carries a `VaultOpened` we never signed.
    let opened = vault_opened(vault_id, open.owner, open.account, open.amount, open.expiry_height, &address, &ufvk);
    let signature = sign_typed(eth, &registry_domain(cfg.chain_id, cfg.registry), &opened)?;
    let out = Round3Out {
        index,
        ak: hex::encode(ak),
        ufvk,
        address,
        public_key_package: serde_json::to_value(&public_key_package)?,
        signature,
    };
    store.save_vault(VaultRecord {
        vault_id,
        owner: open.owner,
        account: open.account,
        amount: open.amount,
        birthday: open.birthday,
        expiry_height: open.expiry_height,
        key_package: serde_json::to_value(&key_package)?,
        public_key_package: serde_json::to_value(&public_key_package)?,
        sk: hex::encode(sk),
        ak: out.ak.clone(),
        ufvk: out.ufvk.clone(),
        address: out.address.clone(),
        decisions: vec![],
        activated: false,
        outstanding: 0,
        claims: vec![],
        used_nonces: vec![],
        inflight: None,
        confirmed: vec![],
        last_event: None,
        activated_tag: None,
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloy::signers::local::PrivateKeySigner;
    use zecured_base::sign_typed;

    use super::*;

    fn cfg() -> NetworkConfig {
        NetworkConfig {
            lightwalletd_url: String::new(),
            base_rpc_url: String::new(),
            chain_id: 84_532,
            registry: Address::repeat_byte(0xaa),
            adapter: Address::repeat_byte(0xbb),
            registry_from_block: 0,
            confirmations: 3,
            vault_expiry_blocks: 288,
            denominations: vec![1_000_000],
            nodes: vec![],
            coordinator: None,
            attest_delay_max_blocks: 0,
            payout_delay_max_blocks: 0,
            commit_depth_blocks: 3,
            commit_depth_secs: 120,
            local_fork: true,
            open_vault: Default::default(),
        }
    }

    fn open(owner: &PrivateKeySigner, birthday: u32) -> VaultOpen {
        let cfg = cfg();
        let msg = OpenVault {
            owner: owner.address(),
            account: Address::repeat_byte(2),
            amount: 1_000_000,
            requestId: B256::repeat_byte(3),
            deadline: u64::MAX,
        };
        VaultOpen {
            vault_id: B256::repeat_byte(7),
            owner: owner.address(),
            account: msg.account,
            amount: msg.amount,
            request_id: msg.requestId,
            deadline: msg.deadline,
            signature: sign_typed(owner, &registry_domain(cfg.chain_id, cfg.registry), &msg).unwrap(),
            birthday,
            expiry_height: birthday.wrapping_add(cfg.vault_expiry_blocks),
        }
    }

    /// M-2 (security review 2026-10-11): a birthday ahead of our tip used to be accepted (up to
    /// +20 blocks); a prompt deposit mined below it would never be scanned.
    #[test]
    fn birthday_must_not_be_ahead_of_our_tip() {
        let dir = std::env::temp_dir().join(format!("zecured-dkg-open-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let (cfg, owner, tip) = (cfg(), PrivateKeySigner::random(), 4_486_000);
        let check = |birthday: u32, tip: u32| check_open(&cfg, &store, &open(&owner, birthday), 1_000, tip);
        check(tip, tip).unwrap();
        check(tip - MAX_TIP_SKEW, tip).unwrap();
        assert!(check(tip + 1, tip).is_err());
        assert!(check(tip + MAX_TIP_SKEW, tip).is_err());
        assert!(check(tip - MAX_TIP_SKEW - 1, tip).is_err());
        // I-4: no overflow on extreme heights.
        assert!(check(u32::MAX, u32::MAX).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
