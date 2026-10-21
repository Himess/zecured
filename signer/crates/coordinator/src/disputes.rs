//! T-05 (a), D53: upholding a disputed payout confirmation.
//!
//! `confirmPayout` is three signers' word, so the claimant of a confirmed claim may dispute the
//! confirmation (`disputePayout`): the claim counts unpaid again. Anyone upholds the confirmation
//! within the Registry's `inclusionWindow` by proving, with the violation guest's `Included` kind,
//! that a committed payout of the claim's input is in a Zcash block under a relay header with the
//! slasher's confirmation work on top (`ZecuredSlasher.proveInclusion`). Otherwise anyone reopens
//! the claim, and the signers pay it again, or the bonds do. A later proof still upholds a
//! reopened claim (D57), so that a payout which did land never keeps its vault open for good.
//!
//! The coordinator knows its own payouts. For each disputed claim, and each reopened claim whose
//! payout it saw mined, it finds, in the vault's scan, the
//! transaction that spent the claim's input, checks that Base recorded that transaction as the
//! input's payout (`spendRecord(payoutRef)`), proves its inclusion ahead of the deposit proofs in
//! the queue, and submits the proof for every disputed claim of that payout. Nothing here is
//! trusted: the slasher checks the seal, the anchor and the payout's record. The witness holds only
//! the vault's tag key K and reaches the prover through its standard input (H-12).

use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256},
    providers::ProviderBuilder,
};
use anyhow::{Context, Result, bail, ensure};
use tracing::{debug, error, info, warn};
use zcash_protocol::TxId;
use zecured_base::{
    events::{IZcashHeaderRelay, IZecuredRegistry, IZecuredSlasher, VERSION_VIOLATION},
    relay::slash_anchor,
    verify::{CLAIM_DISPUTED, CLAIM_PENDING},
};
use zecured_deposit_witness::{p2p, violation};
use zecured_policy::derive::{deposit_tag, payout_ref, tag_key};
use zecured_zcash_ops::{lwd, scan::ReceivedNote};

use crate::{
    Coordinator, LIVENESS_WARNING_INTERVAL, RECEIPT_TIMEOUT,
    error::scrubbed,
    log_failure,
    proofs::{DepositProofs, Prover, SealFile, guest_active, prove_into, seal_selector, version_active},
    queue::Enqueued,
    short,
    store::CoordVault,
};

/// Kind 5 of the violation guest's journal: a payout's inclusion, not a violation.
const INCLUDED: u8 = 5;
/// `abi.encode` of the violation journal: eight words.
const JOURNAL_LENGTH: usize = 8 * 32;

/// The anchor and the payout reference of an `Included` journal, checked as the slasher reads it:
/// kind 5, the anchor, a zero beneficiary, a non-zero payout reference, then four zero words.
pub(crate) fn inclusion_journal(journal: &[u8]) -> Result<(B256, B256)> {
    ensure!(journal.len() == JOURNAL_LENGTH, "an inclusion journal is {JOURNAL_LENGTH} bytes");
    let word = |i: usize| B256::from_slice(&journal[i * 32..(i + 1) * 32]);
    let mut kind = [0u8; 32];
    kind[31] = INCLUDED;
    ensure!(word(0) == B256::from(kind), "not an inclusion journal (kind)");
    ensure!(word(2) == B256::ZERO, "an inclusion journal names no beneficiary");
    let payout = word(3);
    ensure!(payout != B256::ZERO, "an inclusion journal names a payout");
    ensure!((4..8).all(|i| word(i) == B256::ZERO), "an inclusion journal has nothing after the payout");
    Ok((word(1), payout))
}

/// The tag of every spent note of a vault, with the transaction that spent it and its height.
fn spent_notes(ufvk: &str, notes: &[ReceivedNote]) -> Vec<([u8; 32], TxId, u32)> {
    notes
        .iter()
        .filter_map(|n| {
            let cmx = orchard::note::ExtractedNoteCommitment::from(n.note.commitment()).to_bytes();
            Some((deposit_tag(ufvk, &cmx), n.spent_in?, n.spent_at?))
        })
        .collect()
}

/// Whether Base still takes an inclusion proof for one of [`CoordVault::to_uphold`]'s claims:
/// disputed, or pending again after a reopening (D57).
fn upholdable(status: u8) -> bool {
    status == CLAIM_DISPUTED || status == CLAIM_PENDING
}

/// A disputed payout to prove: its transaction, its block and the disputed claims it pays.
struct Disputed {
    txid: TxId,
    height: u32,
    claims: Vec<B256>,
}

/// One background inclusion proof.
struct InclusionJob {
    tag_key: [u8; 32],
    txid: TxId,
    anchor: u32,
    payout_ref: B256,
    lwd: String,
    settings: DepositProofs,
    seal_path: PathBuf,
}

impl InclusionJob {
    async fn run(self) -> Result<()> {
        let headers = self.settings.headers.clone();
        let last = tokio::task::spawn_blocking(move || p2p::extend_file(&headers, &p2p::seed_peers())).await??;
        ensure!(last >= self.anchor, "the P2P peers are behind the relay");
        let mut client = lwd::connect(&self.lwd).await?;
        // Checked with the statement natively before anything is proven.
        let (witness, journal) = violation::included(
            &mut client,
            &self.settings.headers,
            &self.settings.summaries,
            self.tag_key,
            self.txid,
            self.anchor,
        )
        .await?;
        ensure!(journal.reference == self.payout_ref.0, "the witness names another payout");
        // In memory only: the witness holds the vault's tag key (H-12).
        let witness = serde_json::to_vec(&witness)?;
        prove_into(&self.settings, true, witness, &self.seal_path).await
    }
}

impl Prover {
    /// Where the inclusion proof of the payout `payout_ref` is kept until it is submitted.
    fn inclusion_seal_path(&self, payout_ref: &B256) -> PathBuf {
        self.dir.join(format!("{}.inclusion.json", hex::encode(payout_ref)))
    }
}

impl Coordinator {
    /// Whether a warning about the vault's disputes is due: at most one every half hour per vault.
    fn dispute_warning_due(&self, vault: &B256) -> bool {
        let mut warned = self.dispute_warned.lock().unwrap_or_else(|p| p.into_inner());
        if warned.get(vault).is_some_and(|at| at.elapsed() < LIVENESS_WARNING_INTERVAL) {
            return false;
        }
        warned.insert(*vault, Instant::now());
        true
    }

    /// T-05 (a): upholds the vault's disputed payout confirmations that it can prove, and the
    /// reopened ones whose payout it saw mined (D57), one proof per payout. Without proofs
    /// configured it can only warn: someone else must prove the payout's inclusion.
    pub(crate) async fn uphold_disputes(&self, v: &CoordVault) -> Result<()> {
        let to_uphold = v.to_uphold();
        if to_uphold.is_empty() {
            return Ok(());
        }
        let Some(prover) = self.proofs.as_ref() else {
            if self.dispute_warning_due(&v.vault_id) {
                warn!(
                    vault = %short(&v.vault_id),
                    "a payout confirmation is disputed or reopened and this coordinator makes no proofs: prove the payout's inclusion (T-05)"
                );
            }
            return Ok(());
        };
        let slasher = self.v3().await?.slasher;
        ensure!(slasher != Address::ZERO, "a disputed confirmation on a deployment without a slasher");
        let spent = {
            let zcash = self.zcash.lock().await;
            spent_notes(&v.ufvk, &zcash.scan(&v.vault_id).context("vault not scanned")?.notes)
        };
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let mut payouts: BTreeMap<B256, Disputed> = BTreeMap::new();
        for claim in to_uphold {
            let id = B256::from(claim);
            // Disputed, or reopened (pending again, still upholdable, D57); anything else: upheld,
            // confirmed again or compensated meanwhile, and its event follows.
            if !upholdable(registry.claimDetails(id).call().await.context("Registry.claimDetails")?.status) {
                continue;
            }
            let input = registry.claimInput(id).call().await.context("Registry.claimInput")?;
            let Some(&(_, txid, height)) = spent.iter().find(|(tag, ..)| B256::from(*tag) == input) else {
                if self.dispute_warning_due(&v.vault_id) {
                    warn!(
                        vault = %short(&v.vault_id),
                        "a disputed claim's input is unspent as far as this coordinator sees: no payout to prove"
                    );
                }
                continue;
            };
            let reference = B256::from(payout_ref(&v.ufvk, txid.as_ref()));
            let record = registry.spendRecord(reference).call().await.context("Registry.spendRecord")?;
            if record.inputTag != input {
                if self.dispute_warning_due(&v.vault_id) {
                    error!(
                        vault = %short(&v.vault_id),
                        "a disputed claim's input was spent by a transaction Base has no commitment of: no inclusion to prove"
                    );
                }
                continue;
            }
            payouts
                .entry(reference)
                .or_insert(Disputed {
                    txid,
                    height,
                    claims: vec![],
                })
                .claims
                .push(id);
        }
        for (reference, payout) in payouts {
            if let Err(e) = self.uphold(v, prover, slasher, reference, &payout).await {
                log_failure(&v.vault_id, "inclusion proof", &e);
            }
        }
        Ok(())
    }

    /// Submits the payout's finished inclusion proof, or queues it at the front of the queue.
    async fn uphold(
        &self,
        v: &CoordVault,
        prover: &Prover,
        slasher: Address,
        reference: B256,
        payout: &Disputed,
    ) -> Result<()> {
        let seal_path = prover.inclusion_seal_path(&reference);
        if seal_path.exists() {
            return self.submit_inclusion(&v.vault_id, slasher, reference, &seal_path, &payout.claims).await;
        }
        if prover.queue.contains(&reference) {
            return Ok(());
        }
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        // T-07a: only with a violation guest the slasher takes proofs of.
        let image = prover.image_id(true).await?;
        let versions = IZecuredSlasher::new(slasher, &provider).versions().call().await.context("slasher.versions")?;
        if !guest_active(&provider, versions, VERSION_VIOLATION, image).await? {
            warn!(
                "the configured violation guest is not an active verifier version on Base: no inclusion proof (T-07a)"
            );
            return Ok(());
        }
        let Some(anchor) = slash_anchor(&provider, slasher, payout.height).await? else {
            // The relay has not got the payout's block deep enough under the slasher's work yet.
            return Ok(());
        };
        let job = InclusionJob {
            tag_key: tag_key(&v.ufvk),
            txid: payout.txid,
            anchor,
            payout_ref: reference,
            lwd: self.cfg.lightwalletd_url.clone(),
            settings: prover.settings.clone(),
            seal_path,
        };
        let vault_id = v.vault_id;
        let queued = prover.queue.enqueue_urgent(reference, async move {
            info!(vault = %short(&vault_id), "proving a disputed payout's inclusion");
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "inclusion proof ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "inclusion proof failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "inclusion proof failed");
                }
            }
        });
        if queued == Enqueued::Queued {
            info!(vault = %short(&v.vault_id), "inclusion proof queued ahead of the deposit proofs (T-05)");
        }
        Ok(())
    }

    /// `proveInclusion` for each disputed claim of the payout, once Base is known to take the proof:
    /// it names this payout, its anchor is still on the relay's best chain under the slasher's work
    /// (else it is proven again under a new one), and its guest and router selector are an active
    /// violation version (T-07a).
    async fn submit_inclusion(
        &self,
        vault_id: &B256,
        slasher: Address,
        reference: B256,
        seal_path: &std::path::Path,
        claims: &[B256],
    ) -> Result<()> {
        let s: SealFile = serde_json::from_slice(&fs::read(seal_path)?)?;
        let (anchor, proven) = inclusion_journal(&s.journal)?;
        if proven != reference {
            let _ = fs::remove_file(seal_path);
            bail!("the inclusion proof names another payout");
        }
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let slasher = IZecuredSlasher::new(slasher, &provider);
        let relay = IZcashHeaderRelay::new(slasher.relay().call().await.context("slasher.relay")?, &provider);
        let min = slasher.slashConfirmationWork().call().await.context("slasher.slashConfirmationWork")?;
        if !relay.isInBestChain(anchor).call().await.context("relay.isInBestChain")?
            || relay.confirmationWork(anchor).call().await.context("relay.confirmationWork")? < min
        {
            let _ = fs::remove_file(seal_path);
            warn!(vault = %short(vault_id), "an inclusion proof's anchor left the relay's best chain: proving again");
            return Ok(());
        }
        let versions = slasher.versions().call().await.context("slasher.versions")?;
        ensure!(
            version_active(&provider, versions, VERSION_VIOLATION, seal_selector(&s.seal)?, s.image_id).await?,
            "the seal's guest and verifier are not an active violation version on Base (T-07a)"
        );
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        for claim in claims {
            if !upholdable(registry.claimDetails(*claim).call().await.context("Registry.claimDetails")?.status) {
                continue;
            }
            let call = slasher.proveInclusion(s.journal.clone(), s.seal.clone(), s.image_id, *claim);
            let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate proveInclusion")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send proveInclusion")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("proveInclusion receipt")?;
            ensure!(receipt.status(), "proveInclusion reverted");
            info!(vault = %short(vault_id), "a disputed payout confirmation was upheld by its inclusion proof (T-05)");
        }
        let _ = fs::remove_file(seal_path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(words: [[u8; 32]; 8]) -> Vec<u8> {
        words.concat()
    }

    fn included(anchor: u8, payout: u8) -> [[u8; 32]; 8] {
        let mut kind = [0u8; 32];
        kind[31] = INCLUDED;
        [
            kind,
            [anchor; 32],
            [0; 32],
            [payout; 32],
            [0; 32],
            [0; 32],
            [0; 32],
            [0; 32],
        ]
    }

    /// The journal is read as the slasher reads it (`_inclusion`): kind 5, no beneficiary, a payout
    /// reference, nothing after it; anything else is refused before a transaction is sent.
    #[test]
    fn inclusion_journals_are_read_as_the_slasher_reads_them() {
        assert_eq!(inclusion_journal(&journal(included(1, 2))).unwrap(), (B256::repeat_byte(1), B256::repeat_byte(2)));
        let mut f1 = included(1, 2);
        f1[0][31] = 1;
        let mut beneficiary = included(1, 2);
        beneficiary[2][31] = 7;
        let mut tail = included(1, 2);
        tail[7][31] = 1;
        for bad in [f1, beneficiary, tail, included(1, 0)] {
            assert!(inclusion_journal(&journal(bad)).is_err());
        }
        assert!(inclusion_journal(&journal(included(1, 2))[..224]).is_err(), "seven words");
    }
}
