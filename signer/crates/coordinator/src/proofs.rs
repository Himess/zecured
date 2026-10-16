//! Tier 2 (D22): deposit proofs, the only way a deposit reaches Base (Registry v3: no signer
//! attests a deposit, D41).
//!
//! The coordinator proves as a convenience: it already knows every vault's viewing key and
//! deposits, but the Intake trusts only the proof, so a dishonest coordinator can delay a mint,
//! never forge one, and anyone else (the borrower's own machine) can prove instead. It proves
//! - a vault's conforming deposit in Activate mode: the Intake reserves the vault and, once the
//!   relay has moved on, activates it (K3), or lands the deposit as a refund when the vault cannot
//!   take it (T-02);
//! - every other deposit, once the vault cannot take it as its deposit any more, in Refund mode
//!   (journal v2, T-09): it lands as a Refund claim, which the signers pay from the deposit's own
//!   note to the destination the vault's account gives. Nothing is ever refunded off-chain (I30).
//!
//! Proving runs in the background (seconds with a MOCK seal, minutes to hours for a real one,
//! depending on the machine); each vault pass only queues a job or submits a finished seal. Jobs
//! run from one first-in-first-out queue, one or two at a time, one per vault (review H-12,
//! [`crate::queue`]), and the witness, which holds the vault's tag key, reaches the prover through
//! its standard input: it is never written to disk. A seal is made only by a guest whose image id
//! is an active version of Base's verifier version registry (T-07a), and submitted with it.

use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use alloy::{
    eips::BlockNumberOrTag,
    network::EthereumWallet,
    primitives::{Address, B256, Bytes},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};
use zcash_protocol::TxId;
use zecured_base::{
    events::{
        ISignerBond, IZcashHeaderRelay, IZecuredIntake, IZecuredRegistry, IZecuredVersions, VERSION_DEPOSIT, version_id,
    },
    journal::{DepositJournal, MODE_ACTIVATE, decode_deposit_journal_for},
    verify::{STATUS_NONE, VAULT_CANCELLED},
};
use zecured_deposit_witness::{
    p2p,
    witness::{self, Mode},
};
use zecured_node::chain::VaultNotes;
use zecured_policy::derive::deposit_tag;
use zecured_zcash_ops::{keys, lwd, scan::ReceivedNote};

use crate::{
    Coordinator, NETWORK, RECEIPT_TIMEOUT,
    error::scrubbed,
    queue::{Enqueued, ProvingQueue, QueueCounts},
    short,
    store::{CoordVault, VaultStatus},
};

/// Where and how deposit proofs are made.
#[derive(Clone, Debug)]
pub struct DepositProofs {
    /// The zkVM host, `zecured-tier2-host` (tier2/host).
    pub host: PathBuf,
    /// MOCK seals (executor only, they prove nothing) for a deployment with
    /// `MockRiscZeroVerifier` on a local fork; otherwise a succinct proof and its Groth16 wrap.
    pub mock: bool,
    /// NU7-epoch header file (JSON Lines, extended over P2P) and per-block summary cache.
    pub headers: PathBuf,
    pub summaries: PathBuf,
    /// Proofs run at once, 1 (the default) or 2 (H-12: each one takes about 10 GB of RAM on a CPU,
    /// 13 GB of GPU memory).
    pub concurrency: usize,
}

pub(crate) struct Prover {
    pub(crate) settings: DepositProofs,
    /// Finished seals and succinct receipts (no witness ever: it goes through the host's standard
    /// input, with the vault's tag key); owner-only.
    pub(crate) dir: PathBuf,
    pub(crate) queue: Arc<ProvingQueue>,
    /// The image id of the host's deposit guest, read once (`zecured-tier2-host image-id`).
    deposit_image: OnceCell<B256>,
    /// The image id of the host's violation guest (`zecured-tier2-host violation image-id`).
    pub(crate) violation_image: OnceCell<B256>,
}

impl Prover {
    pub(crate) fn new(settings: DepositProofs, data_dir: &Path) -> Result<Self> {
        let queue = ProvingQueue::new(settings.concurrency)?;
        let dir = data_dir.join("proofs");
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        // Witness files of jobs from before H-12 hold viewing keys: remove any left by a crash.
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.file_name().and_then(OsStr::to_str).is_some_and(|n| n.ends_with(".witness.json")) {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(Self {
            settings,
            dir,
            queue,
            deposit_image: OnceCell::new(),
            violation_image: OnceCell::new(),
        })
    }

    fn seal_path(&self, id: &B256) -> PathBuf {
        self.dir.join(format!("{}.seal.json", hex::encode(id)))
    }

    /// The image id of the host's guest for `statement` (`None`: the deposit guest; `Some("violation")`).
    pub(crate) async fn image_id(&self, violation: bool) -> Result<B256> {
        let cell = if violation {
            &self.violation_image
        } else {
            &self.deposit_image
        };
        let host = self.settings.host.clone();
        let id = cell
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || {
                    let mut command = Command::new(&host);
                    if violation {
                        command.arg("violation");
                    }
                    let out = command.arg("image-id").stderr(Stdio::null()).output().context("run the zkVM host")?;
                    ensure!(out.status.success(), "zkVM host image-id failed");
                    parse_image_id(&String::from_utf8_lossy(&out.stdout))
                })
                .await?
            })
            .await?;
        Ok(*id)
    }
}

/// The image id `zecured-tier2-host [violation] image-id` prints (`0x` and 64 hex digits).
pub fn parse_image_id(stdout: &str) -> Result<B256> {
    stdout
        .split_whitespace()
        .find(|w| w.len() == 66 && w.starts_with("0x"))
        .context("the zkVM host printed no image id")?
        .parse()
        .map_err(|e| anyhow!("image id: {e}"))
}

/// `zecured-tier2-host`'s output, also what a borrower who proved on their own machine submits
/// (`POST /vaults/{id}/deposit-proof`, review Y10).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealFile {
    pub image_id: B256,
    pub journal: Bytes,
    pub seal: Bytes,
}

/// What one deposit proof proves: a note of a deposit transaction, in one mode.
#[derive(Clone, Debug)]
pub struct Target {
    pub txid: TxId,
    /// The deposit's block height.
    pub height: u32,
    pub mode: Mode,
    /// The action that creates the note (Refund mode names it: a transaction can pay the vault
    /// several notes); Activate mode takes the first note of the vault's amount.
    pub action: Option<u32>,
    /// The note's value: the work its anchor needs depends on it (T-08).
    pub amount: u64,
}

impl Target {
    /// The vault's conforming deposit, in Activate mode.
    pub fn activation(note: &ReceivedNote, vault_amount: u64) -> Self {
        Self {
            txid: note.txid,
            height: note.position.height,
            mode: Mode::Activate,
            action: None,
            amount: vault_amount,
        }
    }

    /// A deposit that cannot back the vault, in Refund mode (T-02).
    pub fn refund(note: &ReceivedNote) -> Self {
        Self {
            txid: note.txid,
            height: note.position.height,
            mode: Mode::Refund,
            action: Some(note.position.action_index),
            amount: note.value(),
        }
    }
}

/// One background proving job.
struct Job {
    vault: witness::Vault,
    target: Target,
    anchor: u32,
    lwd: String,
    settings: DepositProofs,
    seal_path: PathBuf,
}

/// The witness argument that makes the host read its standard input: the host reads the witness
/// from a path, and on Unix `/dev/stdin` is the process's own standard input (a pipe here, never a
/// file). The zkVM provers run on Linux and macOS only.
#[cfg(unix)]
pub(crate) const WITNESS_FROM_STDIN: Option<&str> = Some("/dev/stdin");
#[cfg(not(unix))]
pub(crate) const WITNESS_FROM_STDIN: Option<&str> = None;

/// Runs `host command args..`, writing `input` (if any) to its standard input and closing it.
pub(crate) fn run_host(host: &Path, command: &str, args: &[&OsStr], input: Option<&[u8]>) -> Result<()> {
    let mut child = Command::new(host)
        .arg(command)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("run the zkVM host")?;
    if let Some(bytes) = input {
        // The host reads all of it before anything else; dropping the pipe closes it (end of file).
        let mut stdin = child.stdin.take().context("the zkVM host's standard input")?;
        let written = stdin.write_all(bytes);
        drop(stdin);
        if let Err(e) = written {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::Error::from(e).context("pass the witness to the zkVM host"));
        }
    }
    let status = child.wait().context("wait for the zkVM host")?;
    ensure!(status.success(), "zkVM host {command} failed");
    Ok(())
}

/// Proves `witness` with the host into `seal_path` (through a temporary file): a MOCK seal, or a
/// succinct proof wrapped in Groth16. `violation` selects the violation guest.
pub(crate) async fn prove_into(
    settings: &DepositProofs,
    violation: bool,
    witness: Vec<u8>,
    seal_path: &Path,
) -> Result<()> {
    let stdin = WITNESS_FROM_STDIN.context("proofs need a Unix host (the witness goes through /dev/stdin)")?;
    let tmp = seal_path.with_extension("tmp");
    let (host, mock, tmp2) = (settings.host.clone(), settings.mock, tmp.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let run = |command: &str, args: &[&OsStr], input: Option<&[u8]>| {
            if violation {
                let mut all: Vec<&OsStr> = vec![OsStr::new(command)];
                all.extend_from_slice(args);
                run_host(&host, "violation", &all, input)
            } else {
                run_host(&host, command, args, input)
            }
        };
        if mock {
            run("mock-seal", &[stdin.as_ref(), tmp2.as_os_str()], Some(&witness))
        } else {
            let receipt = tmp2.with_extension("receipt");
            run("prove", &[stdin.as_ref(), receipt.as_os_str()], Some(&witness))?;
            drop(witness);
            let wrapped = run("groth16", &[receipt.as_os_str(), tmp2.as_os_str()], None);
            let _ = fs::remove_file(&receipt);
            wrapped
        }
    })
    .await??;
    fs::rename(&tmp, seal_path)?;
    Ok(())
}

impl Job {
    async fn run(self) -> Result<()> {
        let headers = self.settings.headers.clone();
        let last = tokio::task::spawn_blocking(move || p2p::extend_file(&headers, &p2p::seed_peers())).await??;
        ensure!(last >= self.anchor, "the P2P peers are behind the relay");
        let mut client = lwd::connect(&self.lwd).await?;
        let built = witness::build(
            &mut client,
            &self.settings.headers,
            &self.settings.summaries,
            self.target.txid,
            &self.vault,
            self.target.mode,
            self.target.action,
            self.anchor,
        )
        .await?;
        // In memory only: the witness holds the vault's tag key (H-12).
        let witness = serde_json::to_vec(&built.witness)?;
        drop(built);
        prove_into(&self.settings, false, witness, &self.seal_path).await
    }
}

/// T-07a: whether Base takes proofs of the `kind` guest `image_id` with the router `selector`: an
/// active version of the registry at `versions`.
pub(crate) async fn version_active<P: Provider>(
    provider: &P,
    versions: Address,
    kind: u8,
    selector: [u8; 4],
    image_id: B256,
) -> Result<bool> {
    IZecuredVersions::new(versions, provider)
        .isActive(version_id(kind, selector, image_id))
        .call()
        .await
        .context("versions.isActive")
}

/// T-07a, before proving: whether some active version of `kind` (any router selector) is for the
/// guest `image_id`. Proving for a guest Base does not take would waste the prover's time.
pub(crate) async fn guest_active<P: Provider>(
    provider: &P,
    versions: Address,
    kind: u8,
    image_id: B256,
) -> Result<bool> {
    let registry = IZecuredVersions::new(versions, provider);
    for id in registry.versionIds(kind).call().await.context("versions.versionIds")? {
        if registry.version(id).call().await.context("versions.version")?.imageId == image_id
            && registry.isActive(id).call().await.context("versions.isActive")?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The router selector a seal starts with.
pub(crate) fn seal_selector(seal: &[u8]) -> Result<[u8; 4]> {
    seal.get(..4).and_then(|s| s.try_into().ok()).context("the seal is shorter than its selector")
}

impl Coordinator {
    /// Proof mode: vaults are activated with `attestDepositWithProof`, made by this coordinator.
    pub fn with_deposit_proofs(mut self, settings: DepositProofs, data_dir: &Path) -> Result<Self> {
        self.proofs = Some(Prover::new(settings, data_dir)?);
        Ok(self)
    }

    pub fn proves_deposits(&self) -> bool {
        self.proofs.is_some()
    }

    /// H-12: the deposit-proof queue's counts (`None` without deposit proofs), for `GET /status`.
    pub fn proving_status(&self) -> Option<QueueCounts> {
        self.proofs.as_ref().map(|p| p.queue.counts())
    }

    /// Queues a proof of `target` for the vault, or submits the vault's finished proof. One proof
    /// per vault at a time (H-12).
    pub(crate) async fn prove_deposit(&self, id: &B256, target: &Target) -> Result<()> {
        let prover = self.proofs.as_ref().context("deposit proofs are not configured")?;
        let seal_path = prover.seal_path(id);
        if seal_path.exists() {
            return self.submit_proof(id, &seal_path).await;
        }
        // H-12: one job per vault, queued or running.
        if prover.queue.contains(id) {
            return Ok(());
        }
        let v = self.vault(id).await?;
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        // Landed already (its event has not reached the safe head yet): nothing to prove.
        let note_tag = self.target_tag(&v, target).await?;
        if IZecuredRegistry::new(self.cfg.registry, &provider)
            .depositTagUsed(note_tag)
            .call()
            .await
            .context("Registry.depositTagUsed")?
        {
            return Ok(());
        }
        let intake = IZecuredIntake::new(self.v3().await?.intake, &provider);
        // T-07a: only for a guest Base takes proofs of.
        let image = prover.image_id(false).await?;
        let versions = intake.versions().call().await.context("Intake.versions")?;
        if !guest_active(&provider, versions, VERSION_DEPOSIT, image).await? {
            warn!("the configured deposit guest is not an active verifier version on Base: not proving (T-07a)");
            return Ok(());
        }
        let relay = intake.relay().call().await.context("Intake.relay")?;
        let min_work = intake
            .minConfirmationWorkFor(target.amount)
            .call()
            .await
            .context("Intake.minConfirmationWorkFor (a deposit above the largest work tier cannot be proven)")?;
        let Some(anchor) = zecured_base::relay::anchor_above(&provider, relay, target.height, min_work).await? else {
            // The relay has not reached the deposit's block with enough work on top yet.
            return Ok(());
        };
        let job = Job {
            vault: witness::Vault {
                vault_id: id.0,
                owner: v.owner.0.0,
                account: v.account.0.0,
                address: v.address.clone(),
                ufvk: v.ufvk.clone(),
                expiry_height: v.expiry_height,
                amount: v.amount,
            },
            target: target.clone(),
            anchor,
            lwd: self.cfg.lightwalletd_url.clone(),
            settings: prover.settings.clone(),
            seal_path,
        };
        let vault_id = *id;
        let what = match target.mode {
            Mode::Activate => "the deposit",
            Mode::Refund => "a deposit's refund",
        };
        let queued = prover.queue.enqueue(vault_id, async move {
            info!(vault = %short(&vault_id), "proving {what}");
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "proof of {what} ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "proof of {what} failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "deposit proof failed");
                }
            }
        });
        if queued == Enqueued::Queued {
            info!(vault = %short(id), "proof of {what} queued");
        }
        Ok(())
    }

    /// The tag of the note `target` proves: the vault's first note of its amount in the
    /// transaction (Activate mode), or the note of the action it names.
    async fn target_tag(&self, v: &CoordVault, target: &Target) -> Result<B256> {
        let zcash = self.zcash.lock().await;
        let scan = zcash.scan(&v.vault_id).context("vault not scanned")?;
        let note = scan
            .notes
            .iter()
            .find(|n| {
                n.txid == target.txid
                    && match target.action {
                        Some(a) => n.position.action_index == a,
                        None => n.value() == v.amount && n.scope == orchard::keys::Scope::External,
                    }
            })
            .context("the proven note is not in the vault's scan")?;
        let cmx = orchard::note::ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
        Ok(B256::from(deposit_tag(&v.ufvk, &cmx)))
    }

    /// T-02: proves, in Refund mode, the vault's deposits that will never back it, one at a time:
    /// they land as Refund claims and are paid back from their own notes once the vault's account
    /// gives their destination. Only once the vault cannot take a deposit any more (active,
    /// closed, cancelled or expired): on a vault Base does not know yet, a refund would cancel it.
    /// Needs a deployment that reads v2 journals (Refund mode, T-09); with v1 journals such
    /// deposits stay in the vault, never refunded off-chain (I30), until the v2 guest is deployed.
    /// Notes below the Intake's `minRefundZat`, and notes to another address of the vault, cannot
    /// be proven and stay too.
    pub(crate) async fn prove_refunds(&self, v: &CoordVault, notes: &VaultNotes) -> Result<()> {
        let Some(prover) = self.proofs.as_ref() else {
            return Ok(());
        };
        if !matches!(
            v.status,
            VaultStatus::Active | VaultStatus::Closed | VaultStatus::Cancelled | VaultStatus::Expired
        ) || notes.unlanded.is_empty()
        {
            return Ok(());
        }
        let seal_path = prover.seal_path(&v.vault_id);
        if seal_path.exists() {
            return self.submit_proof(&v.vault_id, &seal_path).await;
        }
        if prover.queue.contains(&v.vault_id) {
            return Ok(());
        }
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let intake = IZecuredIntake::new(self.v3().await?.intake, &provider);
        if !intake.journalV2().call().await.context("Intake.journalV2")? {
            debug!(vault = %short(&v.vault_id), "deposits wait for a refund: this deployment reads v1 journals (no Refund mode)");
            return Ok(());
        }
        let min = intake.minRefundZat().call().await.context("Intake.minRefundZat")?;
        let receiver = keys::ironwood_receiver(&NETWORK, &v.address).ok().map(|r| r.to_raw_address_bytes());
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        for (_, note) in &notes.unlanded {
            if note.value() < min || receiver != Some(note.recipient.to_raw_address_bytes()) {
                continue;
            }
            let cmx = orchard::note::ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
            let tag = B256::from(deposit_tag(&v.ufvk, &cmx));
            if registry.depositTagUsed(tag).call().await.context("Registry.depositTagUsed")? {
                continue; // landed; its event has not reached the safe head yet
            }
            // A deposit above the largest work tier can never be proven (T-08).
            if intake.minConfirmationWorkFor(note.value()).call().await.is_err() {
                continue;
            }
            return self.prove_deposit(&v.vault_id, &Target::refund(note)).await;
        }
        Ok(())
    }

    /// Submits a finished proof, then forgets it.
    async fn submit_proof(&self, id: &B256, seal_path: &Path) -> Result<()> {
        let s: SealFile = serde_json::from_slice(&fs::read(seal_path)?)?;
        self.submit_seal(id, s).await?;
        let _ = fs::remove_file(seal_path);
        Ok(())
    }

    /// Y10: a deposit proof made elsewhere (the borrower's own machine, `zecured-wallet loan
    /// prove`), submitted with the vault's `VaultOpened` signatures and this coordinator's gas,
    /// so that proving one's own deposit needs no ETH. The Intake checks everything; this only
    /// relays. Refused unless it is an activation and the vault is waiting for its deposit.
    pub async fn submit_external_proof(&self, id: B256, seal: SealFile) -> Result<()> {
        let v = self.vault(&id).await?;
        ensure!(v.status == VaultStatus::AwaitingDeposit, "the vault is not waiting for a deposit proof");
        // The journal's mode is its first word in v2; a v1 journal is always an activation.
        ensure!(
            seal.journal.len() != zecured_base::journal::JOURNAL_LENGTH || seal.journal[31] == MODE_ACTIVATE,
            "only an activation is relayed here"
        );
        self.submit_seal(&id, seal).await
    }

    /// Submits a proof to the Intake with the vault's five `VaultOpened` signatures (sorted by
    /// signer address, as the Intake requires) and the image id the seal was made for, once Base
    /// is known to take it: the journal is of the Intake's version and for this vault, the note is
    /// not on Base yet, and the image id with the seal's router selector is an active version
    /// (with, for journal v2, the journal's branch id allowed: T-07a).
    async fn submit_seal(&self, id: &B256, s: SealFile) -> Result<()> {
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let intake = IZecuredIntake::new(self.v3().await?.intake, &provider);
        let v2 = intake.journalV2().call().await.context("Intake.journalV2")?;
        let journal = decode_deposit_journal_for(&s.journal, v2)?;
        ensure!(journal.vaultId == *id, "the proof is for another vault");
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        if registry.depositTagUsed(journal.depositTag).call().await.context("Registry.depositTagUsed")? {
            // On Base already (a lost receipt, or the same note proven by someone else).
            return Ok(());
        }
        let versions = intake.versions().call().await.context("Intake.versions")?;
        self.check_deposit_version(&provider, versions, &s, &journal, v2).await?;
        let v = self.vault(id).await?;
        ensure!(v.opened_signatures.len() == 5, "the vault has no VaultOpened signatures (opened before C-4)");
        let mut sigs: Vec<(Address, Bytes)> = v
            .opened_signatures
            .iter()
            .map(|o| {
                let eth = self.cfg.node(o.index)?.eth;
                Ok((eth, o.signature.clone()))
            })
            .collect::<Result<_>>()?;
        sigs.sort_by_key(|(a, _)| *a);
        let call =
            intake.attestDepositWithProof(s.journal, s.seal, s.image_id, sigs.into_iter().map(|(_, s)| s).collect());
        let gas =
            zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate attestDepositWithProof")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send attestDepositWithProof")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("attestDepositWithProof receipt")?;
        ensure!(receipt.status(), "attestDepositWithProof reverted");
        if journal.mode == MODE_ACTIVATE {
            self.update_vault(id, |cur| {
                if cur.status == VaultStatus::AwaitingDeposit {
                    cur.status = VaultStatus::DepositSeen;
                }
                cur.attest_tx = Some(receipt.transaction_hash);
            })
            .await?;
            // The Intake reserves the vault (K3), or lands the deposit as a refund when the vault
            // cannot take it (T-02); the Registry's events say which.
            info!(vault = %short(id), "deposit proof accepted on Base");
        } else {
            info!(vault = %short(id), "refund proof accepted on Base: the deposit lands as a Refund claim (T-02)");
        }
        Ok(())
    }

    /// T-07a: Base takes this seal: its image id with its router selector is an active Deposit
    /// version and, for a v2 journal, the version allows the journal's branch id.
    async fn check_deposit_version<P: Provider>(
        &self,
        provider: &P,
        versions: Address,
        s: &SealFile,
        journal: &DepositJournal,
        v2: bool,
    ) -> Result<()> {
        let selector = seal_selector(&s.seal)?;
        ensure!(
            version_active(provider, versions, VERSION_DEPOSIT, selector, s.image_id).await?,
            "the seal's guest and verifier are not an active deposit version on Base (T-07a)"
        );
        if v2 {
            let allowed = IZecuredVersions::new(versions, provider)
                .branchAllowed(version_id(VERSION_DEPOSIT, selector, s.image_id), journal.branchId)
                .call()
                .await
                .context("versions.branchAllowed")?;
            ensure!(allowed, "the version does not allow the journal's consensus branch");
        }
        Ok(())
    }

    /// K3, as a keyless keeper (anyone may do each, the Intake checks everything): activates a
    /// vault whose deposit proof is pending once the Intake allows it; drops the pending
    /// activation instead if its anchor left the relay's best chain (the deposit is proven again
    /// against the chain that won); after a halt, when it can never activate, converts it into a
    /// refund (T-02, T-03). While minting is paused it waits.
    pub(crate) async fn finalize_pending(&self, id: &B256) -> Result<()> {
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let contracts = self.v3().await?;
        let intake = IZecuredIntake::new(contracts.intake, &provider);
        let pending = intake.pendingActivation(*id).call().await.context("Intake.pendingActivation")?;
        if pending.account == Address::ZERO {
            // Finalized, dropped or converted (by us or anyone): follow the vault on Base.
            return self.follow_vault_status(id).await;
        }
        let relay = IZcashHeaderRelay::new(intake.relay().call().await.context("Intake.relay")?, &provider);
        if !relay.isInBestChain(pending.anchor).call().await.context("relay.isInBestChain")? {
            let call = intake.dropPendingActivation(*id);
            let gas =
                zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate dropPendingActivation")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send dropPendingActivation")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("dropPendingActivation receipt")?;
            ensure!(receipt.status(), "dropPendingActivation reverted");
            warn!(vault = %short(id), "pending activation dropped: its anchor left the relay's best chain");
            return self
                .update_vault(id, |cur| {
                    if cur.status == VaultStatus::DepositSeen {
                        cur.status = VaultStatus::AwaitingDeposit;
                    }
                })
                .await;
        }
        if contracts.bond != Address::ZERO
            && ISignerBond::new(contracts.bond, &provider).halted().call().await.context("bond.halted")?
        {
            let call = intake.refundPending(*id);
            let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate refundPending")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send refundPending")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("refundPending receipt")?;
            ensure!(receipt.status(), "refundPending reverted");
            warn!(vault = %short(id), "minting is halted: the pending activation became a refund (T-02)");
            return Ok(());
        }
        if intake.mintingPaused().call().await.context("Intake.mintingPaused")? {
            return Ok(()); // T-03: it waits for the pause to end
        }
        let tip = relay.bestChainTip().call().await.context("relay.bestChainTip")?;
        let now = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .context("latest Base block")?
            .context("no latest Base block")?
            .header
            .timestamp;
        if tip.height < pending.readyHeight || alloy::primitives::U256::from(now) < pending.readyTime {
            return Ok(());
        }
        let call = intake.finalizeActivation(*id);
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate finalizeActivation")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send finalizeActivation")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("finalizeActivation receipt")?;
        ensure!(receipt.status(), "finalizeActivation reverted");
        info!(vault = %short(id), "vault activated on its deposit proof");
        Ok(())
    }

    /// A vault whose activation is no longer pending: waiting for its deposit again if Base
    /// forgot it (dropped), cancelled if it became a refund; an activation moves it on through its
    /// own event.
    async fn follow_vault_status(&self, id: &B256) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let status = IZecuredRegistry::new(self.cfg.registry, &provider)
            .vault(*id)
            .call()
            .await
            .context("Registry.vault")?
            .status;
        self.update_vault(id, |cur| {
            if cur.status == VaultStatus::DepositSeen {
                match status {
                    STATUS_NONE => cur.status = VaultStatus::AwaitingDeposit,
                    VAULT_CANCELLED => cur.status = VaultStatus::Cancelled,
                    _ => {}
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_files_parse() {
        let json = r#"{ "imageId": "0x1a2ba0a861d63a6be1a6cac73141244fa7a178419df41a6d4b0b92cc06d02599",
                        "journal": "0x00", "seal": "0xffffffff" }"#;
        let s: SealFile = serde_json::from_str(json).unwrap();
        assert_eq!(s.seal.len(), 4);
        assert_eq!(s.image_id[0], 0x1a);
        assert_eq!(seal_selector(&s.seal).unwrap(), [0xff; 4]);
        assert!(seal_selector(&[1, 2, 3]).is_err());
    }

    /// The host prints the image id on a line of its own (`image-id`), or after a label.
    #[test]
    fn image_ids_are_read_from_the_host() {
        let id = "0x1a2ba0a861d63a6be1a6cac73141244fa7a178419df41a6d4b0b92cc06d02599";
        assert_eq!(parse_image_id(&format!("{id}\n")).unwrap(), id.parse::<B256>().unwrap());
        assert_eq!(parse_image_id(&format!("image id {id} (test network)\n")).unwrap(), id.parse::<B256>().unwrap());
        assert!(parse_image_id("no id here").is_err());
    }

    fn settings(concurrency: usize) -> DepositProofs {
        DepositProofs {
            host: PathBuf::from("zecured-tier2-host"),
            mock: true,
            headers: PathBuf::new(),
            summaries: PathBuf::new(),
            concurrency,
        }
    }

    /// H-12: the prover's directory never keeps a witness (a crash of a job from before the queue
    /// could have left one), and the concurrency is 1 or 2.
    #[test]
    fn the_prover_keeps_no_witness_and_runs_one_or_two_proofs() {
        let dir = crate::store::tests::TempDir::new("prover");
        let proofs = dir.0.join("proofs");
        fs::create_dir_all(&proofs).unwrap();
        let (witness, seal) = (proofs.join(format!("{}.witness.json", "07".repeat(32))), proofs.join("x.seal.json"));
        fs::write(&witness, b"{\"vault_ufvk\":\"uviewtest1...\"}").unwrap();
        fs::write(&seal, b"{}").unwrap();
        let prover = Prover::new(settings(1), &dir.0).unwrap();
        assert!(!witness.exists() && seal.exists(), "the witness is removed, a finished seal kept");
        assert_eq!(prover.queue.counts().concurrency, 1);
        assert!(Prover::new(settings(0), &dir.0).is_err());
        assert!(Prover::new(settings(3), &dir.0).is_err());
    }

    /// A program that copies its standard input to `out`, reading it the way the host reads its
    /// witness argument: on Unix as the path `/dev/stdin`, which is what the host is given.
    fn copy_stdin(out: &Path) -> (PathBuf, String, Vec<std::ffi::OsString>) {
        if cfg!(unix) {
            let script = format!("cat {} > \"$0\"", WITNESS_FROM_STDIN.unwrap_or_default());
            (PathBuf::from("/bin/sh"), "-c".into(), vec![script.into(), out.as_os_str().to_owned()])
        } else {
            let path = out.display().to_string().replace('\'', "''");
            let script = format!("$w = [Console]::In.ReadToEnd(); [IO.File]::WriteAllText('{path}', $w)");
            (
                PathBuf::from("powershell"),
                "-NoProfile".into(),
                vec!["-NonInteractive".into(), "-Command".into(), script.into()],
            )
        }
    }

    /// H-12: the witness reaches the prover through its standard input, all of it (more than a
    /// pipe's buffer), and the input is closed so that the prover sees its end.
    #[test]
    fn the_witness_goes_through_the_standard_input() {
        let dir = crate::store::tests::TempDir::new("stdin");
        let out = dir.0.join("seen.json");
        let witness: Vec<u8> = (0..200_000).map(|i| b"{\"vault_ufvk\":\"uviewtest1\"}"[i % 26]).collect();
        let (program, command, args) = copy_stdin(&out);
        let args: Vec<&OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        run_host(&program, &command, &args, Some(&witness)).unwrap();
        assert_eq!(fs::read(&out).unwrap(), witness);
        // A host that cannot start, or fails, is an error.
        let e = run_host(Path::new("zecured-no-such-host"), "prove", &[], Some(b"{}")).unwrap_err();
        assert!(format!("{e:#}").contains("run the zkVM host"));
    }
}
