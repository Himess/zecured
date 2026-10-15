//! Tier 2 (D22): activating vaults on deposit proofs instead of signer attestations.
//!
//! The coordinator proves as a convenience: it already knows every vault's viewing key and
//! deposit, but the Registry trusts only the proof, so a dishonest coordinator can delay a mint,
//! never forge one, and anyone else (the borrower's own machine) can prove instead. Proving runs
//! in the background (seconds with a MOCK seal, minutes to hours for a real one, depending on the
//! machine); each vault pass only queues a job or submits a finished seal. Jobs run from one
//! first-in-first-out queue, one or two at a time, one per vault (review H-12, [`crate::queue`]),
//! and the witness, which holds the vault's viewing key, reaches the prover through its standard
//! input: it is never written to disk.

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
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tracing::{debug, info, warn};
use zcash_protocol::TxId;
use zecured_base::events::{IZcashHeaderRelay, IZecuredRegistry};
use zecured_deposit_witness::{p2p, witness};
use zecured_zcash_ops::lwd;

use crate::{
    Coordinator, RECEIPT_TIMEOUT,
    error::scrubbed,
    queue::{Enqueued, ProvingQueue, QueueCounts},
    short,
    store::VaultStatus,
};

/// Where and how deposit proofs are made.
#[derive(Clone, Debug)]
pub struct DepositProofs {
    /// The zkVM host, `zecured-tier2-host` (tier2/host).
    pub host: PathBuf,
    /// MOCK seals (executor only, they prove nothing) for a Registry deployed with
    /// `MockRiscZeroVerifier` on a local fork; otherwise a succinct proof and its Groth16 wrap
    /// (Docker on x86).
    pub mock: bool,
    /// NU7-epoch header file (JSON Lines, extended over P2P) and per-block summary cache.
    pub headers: PathBuf,
    pub summaries: PathBuf,
    /// Proofs run at once, 1 (the default) or 2 (H-12: each one takes about 10 GB of RAM on a CPU,
    /// 13 GB of GPU memory).
    pub concurrency: usize,
}

pub(crate) struct Prover {
    settings: DepositProofs,
    /// Finished seals and succinct receipts (no witness ever: it goes through the host's standard
    /// input); owner-only.
    dir: PathBuf,
    queue: Arc<ProvingQueue>,
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
        Ok(Self { settings, dir, queue })
    }

    fn seal_path(&self, id: &B256) -> PathBuf {
        self.dir.join(format!("{}.seal.json", hex::encode(id)))
    }
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

/// One background proving job.
struct Job {
    vault: witness::Vault,
    txid: TxId,
    anchor: u32,
    lwd: String,
    settings: DepositProofs,
    seal_path: PathBuf,
}

/// The witness argument that makes the host read its standard input: the host reads the witness
/// from a path, and on Unix `/dev/stdin` is the process's own standard input (a pipe here, never a
/// file). The zkVM provers run on Linux and macOS only.
#[cfg(unix)]
const WITNESS_FROM_STDIN: Option<&str> = Some("/dev/stdin");
#[cfg(not(unix))]
const WITNESS_FROM_STDIN: Option<&str> = None;

/// Runs `host command args..`, writing `input` (if any) to its standard input and closing it.
fn run_host(host: &Path, command: &str, args: &[&OsStr], input: Option<&[u8]>) -> Result<()> {
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

impl Job {
    async fn run(self) -> Result<()> {
        let stdin =
            WITNESS_FROM_STDIN.context("deposit proofs need a Unix host (the witness goes through /dev/stdin)")?;
        let headers = self.settings.headers.clone();
        let last = tokio::task::spawn_blocking(move || p2p::extend_file(&headers, &p2p::seed_peers())).await??;
        ensure!(last >= self.anchor, "the P2P peers are behind the relay");
        let mut client = lwd::connect(&self.lwd).await?;
        let built = witness::build(
            &mut client,
            &self.settings.headers,
            &self.settings.summaries,
            self.txid,
            &self.vault,
            self.anchor,
        )
        .await?;
        // In memory only: the witness holds the vault's viewing key (H-12).
        let witness = serde_json::to_vec(&built.witness)?;
        drop(built);
        let tmp = self.seal_path.with_extension("tmp");
        let (host, mock, tmp2) = (self.settings.host.clone(), self.settings.mock, tmp.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            if mock {
                run_host(&host, "mock-seal", &[stdin.as_ref(), tmp2.as_os_str()], Some(&witness))
            } else {
                let receipt = tmp2.with_extension("receipt");
                run_host(&host, "prove", &[stdin.as_ref(), receipt.as_os_str()], Some(&witness))?;
                drop(witness);
                let wrapped = run_host(&host, "groth16", &[receipt.as_os_str(), tmp2.as_os_str()], None);
                let _ = fs::remove_file(&receipt);
                wrapped
            }
        })
        .await??;
        fs::rename(&tmp, &self.seal_path)?;
        Ok(())
    }
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

    /// Queues a proof of the vault's conforming deposit, or submits the finished proof.
    pub(crate) async fn prove_deposit(&self, id: &B256, txid: TxId, deposit_height: u32) -> Result<()> {
        let prover = self.proofs.as_ref().context("deposit proofs are not configured")?;
        let seal_path = prover.seal_path(id);
        if seal_path.exists() {
            return self.submit_proof(id, &seal_path).await;
        }
        // H-12: one job per vault, queued or running.
        if prover.queue.contains(id) {
            return Ok(());
        }
        let Some(anchor) = self.anchor_above(deposit_height).await? else {
            // The relay has not reached the deposit's block with enough work on top yet.
            return Ok(());
        };
        let v = self.vault(id).await?;
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
            txid,
            anchor,
            lwd: self.cfg.lightwalletd_url.clone(),
            settings: prover.settings.clone(),
            seal_path,
        };
        let vault_id = *id;
        let queued = prover.queue.enqueue(vault_id, async move {
            info!(vault = %short(&vault_id), "proving the deposit");
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "deposit proof ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "deposit proof failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "deposit proof failed");
                }
            }
        });
        if queued == Enqueued::Queued {
            info!(vault = %short(id), "deposit proof queued");
        }
        Ok(())
    }

    async fn anchor_above(&self, deposit_height: u32) -> Result<Option<u32>> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        zecured_base::relay::anchor_above(&provider, self.cfg.registry, deposit_height).await
    }

    /// Submits a finished proof with the five signers' `VaultOpened` signatures, sorted by
    /// signer address as the Registry requires.
    async fn submit_proof(&self, id: &B256, seal_path: &Path) -> Result<()> {
        let s: SealFile = serde_json::from_slice(&fs::read(seal_path)?)?;
        self.submit_seal(id, s).await?;
        let _ = fs::remove_file(seal_path);
        Ok(())
    }

    /// Y10: a deposit proof made elsewhere (the borrower's own machine, `zecured-wallet loan
    /// prove`), submitted with the vault's `VaultOpened` signatures and this coordinator's gas,
    /// so that proving one's own deposit needs no ETH. The Registry checks everything; this only
    /// relays. Refused when the vault is not waiting for its deposit.
    pub async fn submit_external_proof(&self, id: B256, seal: SealFile) -> Result<()> {
        let v = self.vault(&id).await?;
        ensure!(v.status == VaultStatus::AwaitingDeposit, "the vault is not waiting for a deposit proof");
        self.submit_seal(&id, seal).await
    }

    async fn submit_seal(&self, id: &B256, s: SealFile) -> Result<()> {
        ensure!(s.journal.len() == 192 && s.journal[32..64] == id.0, "the proof is for another vault");
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        if registry.vault(*id).call().await.context("Registry.vault")?.status != 0 {
            // Activated meanwhile (a receipt we did not see, or someone else's proof), or cancelled.
            return Ok(());
        }
        if registry.pendingActivation(*id).call().await.context("Registry.pendingActivation")?.account != Address::ZERO
        {
            // K3: a proof (ours, or the borrower's own) already waits for finalization.
            return self.mark_proof_pending(id).await;
        }
        ensure!(registry.depositImageId().call().await? == s.image_id, "the Registry expects another deposit guest");
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
        let call = registry.attestDepositWithProof(s.journal, s.seal, sigs.into_iter().map(|(_, s)| s).collect());
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
        self.update_vault(id, |cur| {
            if cur.status == VaultStatus::AwaitingDeposit {
                cur.status = VaultStatus::DepositSeen;
            }
            cur.attest_tx = Some(receipt.transaction_hash);
        })
        .await?;
        info!(vault = %short(id), "deposit proof accepted on Base; activation pending (K3)");
        Ok(())
    }

    async fn mark_proof_pending(&self, id: &B256) -> Result<()> {
        self.update_vault(id, |cur| {
            if cur.status == VaultStatus::AwaitingDeposit {
                cur.status = VaultStatus::DepositSeen;
            }
        })
        .await
    }

    /// K3: once the Registry allows it, activates a vault whose deposit proof is pending; drops
    /// the pending activation instead if its anchor left the relay's best chain, and proves the
    /// deposit again against the chain that won. Anyone may do either: the Registry checks
    /// everything, so this is only a convenience.
    pub(crate) async fn finalize_pending(&self, id: &B256) -> Result<()> {
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let pending = registry.pendingActivation(*id).call().await.context("Registry.pendingActivation")?;
        if pending.account == Address::ZERO {
            // Finalized (its VaultActivated will move the vault on) or dropped by someone else.
            if self.proves_deposits() && registry.vault(*id).call().await.context("Registry.vault")?.status == 0 {
                self.update_vault(id, |cur| {
                    if cur.status == VaultStatus::DepositSeen {
                        cur.status = VaultStatus::AwaitingDeposit;
                    }
                })
                .await?;
            }
            return Ok(());
        }
        let relay = IZcashHeaderRelay::new(registry.relay().call().await.context("Registry.relay")?, &provider);
        if !relay.isInBestChain(pending.anchor).call().await.context("relay.isInBestChain")? {
            let call = registry.dropPendingActivation(*id);
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
        let call = registry.finalizeActivation(*id);
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
