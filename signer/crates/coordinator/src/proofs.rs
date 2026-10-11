//! Tier 2 (D22): activating vaults on deposit proofs instead of signer attestations.
//!
//! The coordinator proves as a convenience: it already knows every vault's viewing key and
//! deposit, but the Registry trusts only the proof, so a dishonest coordinator can delay a mint,
//! never forge one, and anyone else (the borrower's own machine) can prove instead. Proving runs
//! in the background (seconds with a MOCK seal, minutes to hours for a real one, depending on the
//! machine); each vault pass only starts a job or submits a finished seal.

use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex as StdMutex},
};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes},
    providers::ProviderBuilder,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tracing::{debug, info, warn};
use zcash_protocol::TxId;
use zecured_base::events::IZecuredRegistry;
use zecured_deposit_witness::{p2p, witness};
use zecured_zcash_ops::lwd;

use crate::{Coordinator, RECEIPT_TIMEOUT, error::scrubbed, short, store::VaultStatus};

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
}

pub(crate) struct Prover {
    settings: DepositProofs,
    /// Witnesses (with viewing keys) while a job runs, and finished seals; owner-only.
    dir: PathBuf,
    running: Arc<StdMutex<HashSet<B256>>>,
}

impl Prover {
    pub(crate) fn new(settings: DepositProofs, data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("proofs");
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            settings,
            dir,
            running: Arc::default(),
        })
    }

    fn seal_path(&self, id: &B256) -> PathBuf {
        self.dir.join(format!("{}.seal.json", hex::encode(id)))
    }
}

/// `zecured-tier2-host`'s output.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SealFile {
    image_id: B256,
    journal: Bytes,
    seal: Bytes,
}

/// One background proving job.
struct Job {
    vault: witness::Vault,
    txid: TxId,
    anchor: u32,
    lwd: String,
    settings: DepositProofs,
    witness_path: PathBuf,
    seal_path: PathBuf,
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

fn run_host(host: &Path, args: &[&Path], command: &str) -> Result<()> {
    let out = Command::new(host).arg(command).args(args).output().context("run the zkVM host")?;
    ensure!(out.status.success(), "zkVM host {command} failed");
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
            self.txid,
            &self.vault,
            self.anchor,
        )
        .await?;
        write_private(&self.witness_path, &serde_json::to_vec(&built.witness)?)?;
        let tmp = self.seal_path.with_extension("tmp");
        let (host, mock, witness_path, tmp2) =
            (self.settings.host.clone(), self.settings.mock, self.witness_path.clone(), tmp.clone());
        let sealed = tokio::task::spawn_blocking(move || -> Result<()> {
            if mock {
                run_host(&host, &[&witness_path, &tmp2], "mock-seal")
            } else {
                let receipt = tmp2.with_extension("receipt");
                run_host(&host, &[&witness_path, &receipt], "prove")?;
                let wrapped = run_host(&host, &[&receipt, &tmp2], "groth16");
                let _ = fs::remove_file(&receipt);
                wrapped
            }
        })
        .await?;
        // The witness holds the viewing key: gone as soon as the host is done with it.
        let _ = fs::remove_file(&self.witness_path);
        sealed?;
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

    /// Starts proving the vault's conforming deposit, or submits the finished proof.
    pub(crate) async fn prove_deposit(&self, id: &B256, txid: TxId, deposit_height: u32) -> Result<()> {
        let prover = self.proofs.as_ref().context("deposit proofs are not configured")?;
        let seal_path = prover.seal_path(id);
        if seal_path.exists() {
            return self.submit_proof(id, &seal_path).await;
        }
        if prover.running.lock().unwrap_or_else(|p| p.into_inner()).contains(id) {
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
            witness_path: prover.dir.join(format!("{}.witness.json", hex::encode(id))),
            seal_path,
        };
        let running = prover.running.clone();
        running.lock().unwrap_or_else(|p| p.into_inner()).insert(*id);
        let vault_id = *id;
        tokio::spawn(async move {
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "deposit proof ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "deposit proof failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "deposit proof failed");
                }
            }
            running.lock().unwrap_or_else(|p| p.into_inner()).remove(&vault_id);
        });
        info!(vault = %short(id), "proving the deposit");
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
        ensure!(s.journal.len() == 192 && s.journal[32..64] == id.0, "the proof is for another vault");
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        if registry.vault(*id).call().await.context("Registry.vault")?.status != 0 {
            // Activated meanwhile (a receipt we did not see, or someone else's proof), or cancelled.
            let _ = fs::remove_file(seal_path);
            return Ok(());
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
        let _ = fs::remove_file(seal_path);
        info!(vault = %short(id), "deposit proof accepted on Base");
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

    #[test]
    fn private_files_are_owner_only() {
        let dir = std::env::temp_dir().join(format!("zecured-proofs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("w.json");
        write_private(&p, b"secret").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
