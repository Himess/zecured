//! Proven delivery (section 3), and disputed payouts (T-05 (a), D53, D57).
//!
//! A payout confirmation is three signers' word; the claimant may dispute it. Since v4 the
//! coordinator does not wait for a dispute: it proves the delivery of every payout it committed.
//! Once a payout's block has the slasher's `slashConfirmationWork` on top of it on the header
//! relay, it builds the violation guest's `Included` witness for the transaction (which names the
//! payout by `payoutRef = BLAKE2b(K, "Zecured_PayRef__", txid)`, committed on Base before it was
//! signed), proves it, and submits `ZecuredSlasher.proveInclusionBatch(journal, seal, imageId,
//! claimIds)` for the payout's claims: each becomes final on Base (`Paid`, `paidAt == 0`, never
//! disputable again), whether it was pending, confirmed, disputed or reopened, and also on a
//! frozen or compromised vault before the compensator tallies it (R-2). A claim already final is
//! skipped by the slasher, never a revert, so a proof someone else sent first does no harm.
//!
//! The payouts come from the vault's commitments on Base (`SpendCommitted`, applied to the
//! coordinator's records): the commitment whose own transaction spent its input (a rebuilt payout
//! has its own commitment of the same input, and every commitment of an input names the same
//! claims, RT-4). A disputed claim no recorded commitment covers is traced as before: its input
//! (`claimInput`), the transaction that spent it, and that transaction's record on Base.
//!
//! Proofs wait in the proving queue behind the deposits: a disputed payout's ahead of swap
//! settlements and routine proofs, since its inclusion window runs (crate::queue). Nothing here is
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
};
use zecured_deposit_witness::{p2p, violation};
use zecured_policy::derive::{deposit_tag, payout_ref, tag_key};
use zecured_zcash_ops::{lwd, network::ZcashNetwork, scan::ReceivedNote};

use crate::{
    Coordinator, LIVENESS_WARNING_INTERVAL, RECEIPT_TIMEOUT,
    error::scrubbed,
    log_failure,
    proofs::{DepositProofs, Prover, SealFile, guest_active, prove_into, seal_selector, version_active},
    queue::{Enqueued, Priority},
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

/// A payout to prove: its transaction, its block, the claims it settles that are not final yet,
/// and how urgent its proof is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToProve {
    pub txid: TxId,
    pub height: u32,
    pub claims: Vec<B256>,
    pub priority: Priority,
}

/// Section 3: the payouts of the vault's recorded commitments that landed (the transaction that
/// spent the commitment's input is the commitment's own), with their claims among `wanted`, keyed
/// by payout reference. A claim of `urgent` (disputed or reopened) makes its payout's proof a
/// dispute's.
pub(crate) fn landed_payouts(
    v: &CoordVault,
    spent: &[([u8; 32], TxId, u32)],
    wanted: &[[u8; 32]],
    urgent: &[[u8; 32]],
) -> BTreeMap<B256, ToProve> {
    let mut payouts: BTreeMap<B256, ToProve> = BTreeMap::new();
    for c in &v.commitments {
        let claims: Vec<B256> = c.claims.iter().filter(|id| wanted.contains(id)).map(|id| B256::from(*id)).collect();
        if claims.is_empty() {
            continue;
        }
        let Some(&(_, txid, height)) = spent.iter().find(|(tag, ..)| B256::from(*tag) == c.input_tag) else {
            continue; // its input is unspent: nothing landed yet
        };
        if B256::from(payout_ref(&v.ufvk, txid.as_ref())) != c.payout_ref {
            continue; // another commitment of the same input landed (a rebuilt payout)
        }
        let priority = if claims.iter().any(|id| urgent.contains(&id.0)) {
            Priority::Dispute
        } else {
            Priority::RoutineInclusion
        };
        let entry = payouts.entry(c.payout_ref).or_insert(ToProve {
            txid,
            height,
            claims: vec![],
            priority,
        });
        entry.priority = entry.priority.min(priority);
        for id in claims {
            if !entry.claims.contains(&id) {
                entry.claims.push(id);
            }
        }
    }
    payouts
}

/// One background inclusion proof.
struct InclusionJob {
    tag_key: [u8; 32],
    txid: TxId,
    anchor: u32,
    payout_ref: B256,
    /// The deployment's Zcash network: its peers, light wallet server and statement.
    network: ZcashNetwork,
    lwd: String,
    settings: DepositProofs,
    seal_path: PathBuf,
}

impl InclusionJob {
    async fn run(self) -> Result<()> {
        let (headers, network) = (self.settings.headers.clone(), self.network);
        let last = tokio::task::spawn_blocking(move || p2p::extend_file(network, &headers, &p2p::seed_peers(network)))
            .await??;
        ensure!(last >= self.anchor, "the P2P peers are behind the relay");
        let mut client = lwd::connect(&self.lwd).await?;
        lwd::check_network(&mut client, network).await?;
        // Checked with the statement natively before anything is proven.
        let (witness, journal) = violation::included(
            &mut client,
            network,
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

    /// Section 3 and T-05 (a): proves the delivery of the vault's landed payouts whose claims are
    /// not final on Base yet, one proof per payout, a disputed one first. Without proofs configured
    /// it can only warn about disputes: someone else must prove those payouts' inclusion.
    pub(crate) async fn prove_deliveries(&self, v: &CoordVault) -> Result<()> {
        let routine = v.to_prove();
        let urgent = v.to_uphold();
        if routine.is_empty() && urgent.is_empty() {
            return Ok(());
        }
        let Some(prover) = self.proofs.as_ref() else {
            if !urgent.is_empty() && self.dispute_warning_due(&v.vault_id) {
                warn!(
                    vault = %short(&v.vault_id),
                    "a payout confirmation is disputed or reopened and this coordinator makes no proofs: prove the payout's inclusion (T-05)"
                );
            }
            return Ok(());
        };
        let slasher = self.v3().await?.slasher;
        ensure!(slasher != Address::ZERO, "proven delivery on a deployment without a slasher");
        let spent = {
            let zcash = self.zcash.lock().await;
            spent_notes(&v.ufvk, &zcash.scan(&v.vault_id).context("vault not scanned")?.notes)
        };
        let mut wanted = routine.clone();
        wanted.extend(urgent.iter().copied().filter(|c| !routine.contains(c)));
        let mut payouts = landed_payouts(v, &spent, &wanted, &urgent);
        // A disputed claim no recorded commitment covers: traced on Base.
        let covered: Vec<B256> = payouts.values().flat_map(|p| p.claims.iter().copied()).collect();
        let untraced: Vec<[u8; 32]> = urgent.iter().copied().filter(|c| !covered.contains(&B256::from(*c))).collect();
        if !untraced.is_empty() {
            self.trace_disputes(v, &spent, &untraced, &mut payouts).await?;
        }
        for (reference, payout) in payouts {
            if let Err(e) = self.prove_payout(v, prover, slasher, reference, &payout).await {
                log_failure(&v.vault_id, "inclusion proof", &e);
            }
        }
        Ok(())
    }

    /// T-05 (a), D57: the payouts of disputed (or reopened) claims, found through their input on
    /// Base: the transaction that spent it in the vault's scan, if Base recorded it as that input's
    /// payout.
    async fn trace_disputes(
        &self,
        v: &CoordVault,
        spent: &[([u8; 32], TxId, u32)],
        claims: &[[u8; 32]],
        payouts: &mut BTreeMap<B256, ToProve>,
    ) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        for claim in claims {
            let id = B256::from(*claim);
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
            let entry = payouts.entry(reference).or_insert(ToProve {
                txid,
                height,
                claims: vec![],
                priority: Priority::Dispute,
            });
            entry.priority = Priority::Dispute;
            if !entry.claims.contains(&id) {
                entry.claims.push(id);
            }
        }
        Ok(())
    }

    /// Submits the payout's finished inclusion proof, or queues it at its priority once the relay
    /// has the slasher's work on top of the payout's block.
    async fn prove_payout(
        &self,
        v: &CoordVault,
        prover: &Prover,
        slasher: Address,
        reference: B256,
        payout: &ToProve,
    ) -> Result<()> {
        let seal_path = prover.inclusion_seal_path(&reference);
        if seal_path.exists() {
            return self.submit_inclusion(&v.vault_id, slasher, reference, &seal_path, &payout.claims).await;
        }
        if prover.queue.contains(&reference) {
            // A routine proof that became a dispute's moves up.
            prover.queue.enqueue(reference, payout.priority, async {});
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
            network: self.cfg.zcash(),
            lwd: self.cfg.lightwalletd_url.clone(),
            settings: prover.settings.clone(),
            seal_path,
        };
        let vault_id = v.vault_id;
        let queued = prover.queue.enqueue(reference, payout.priority, async move {
            info!(vault = %short(&vault_id), "proving a payout's delivery");
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "delivery proof ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "delivery proof failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "delivery proof failed");
                }
            }
        });
        if queued == Enqueued::Queued {
            info!(vault = %short(&v.vault_id), priority = ?payout.priority, "delivery proof queued");
        }
        Ok(())
    }

    /// `proveInclusionBatch` for the payout's claims not final yet, once Base is known to take the
    /// proof: it names this payout, its anchor is still on the relay's best chain under the
    /// slasher's work (else it is proven again under a new one), and its guest and router selector
    /// are an active violation version (T-07a). The slasher skips a claim that is final or no
    /// longer upholdable, so the batch never reverts for one.
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
        let call = slasher.proveInclusionBatch(s.journal.clone(), s.seal.clone(), s.image_id, claims.to_vec());
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate proveInclusionBatch")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send proveInclusionBatch")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("proveInclusionBatch receipt")?;
        ensure!(receipt.status(), "proveInclusionBatch reverted");
        info!(vault = %short(vault_id), claims = claims.len(), "a payout's delivery is proven on Base: its claims are final");
        let _ = fs::remove_file(seal_path);
        Ok(())
    }
}

/// Whether Base still takes an inclusion proof for a claim of [`CoordVault::to_uphold`]: disputed,
/// or pending again after a reopening (D57).
fn upholdable(status: u8) -> bool {
    status == zecured_base::verify::CLAIM_DISPUTED || status == zecured_base::verify::CLAIM_PENDING
}

#[cfg(test)]
mod tests {
    use zecured_policy::ledger::{Claim, ClaimKind, ClaimState};

    use super::*;
    use crate::store::{CommittedPayout, tests::vault};

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

    fn claim(id: u8, state: ClaimState) -> Claim {
        Claim {
            id: [id; 32],
            kind: ClaimKind::Liquidation,
            amount: 100_000,
            commitment: [id; 32],
            order: (1, u64::from(id)),
            created_at: 0,
            partner: None,
            state,
            refund_tag: None,
            bond_paid: false,
            proven: false,
            confirmed_at: 0,
        }
    }

    /// Section 3: the payouts to prove are the recorded commitments whose own transaction spent
    /// their input; their claims not final yet, a dispute's first; a commitment whose input is
    /// unspent, or whose input another commitment's transaction spent (a rebuild), has nothing to
    /// prove.
    #[test]
    fn the_payouts_to_prove_are_the_commitments_that_landed() {
        let mut v = vault(1);
        v.ufvk = "uviewtest1vault".into();
        let tx = |b: u8| TxId::from_bytes([b; 32]);
        let reference = |b: u8| B256::from(payout_ref(&v.ufvk, &[b; 32]));
        let (input_a, input_b, input_c) = (B256::repeat_byte(0xa), B256::repeat_byte(0xb), B256::repeat_byte(0xc));
        v.commitments = vec![
            // Landed (tx 1 spent input a): claims 1 and 2.
            CommittedPayout {
                payout_ref: reference(1),
                input_tag: input_a,
                claims: vec![[1; 32], [2; 32]],
            },
            // Its input b is unspent: claim 3 waits.
            CommittedPayout {
                payout_ref: reference(3),
                input_tag: input_b,
                claims: vec![[3; 32]],
            },
            // A first commitment of input c that expired unmined; the rebuild (tx 5) landed.
            CommittedPayout {
                payout_ref: reference(4),
                input_tag: input_c,
                claims: vec![[4; 32]],
            },
            CommittedPayout {
                payout_ref: reference(5),
                input_tag: input_c,
                claims: vec![[4; 32]],
            },
        ];
        v.claims = vec![
            claim(1, ClaimState::Paid),
            Claim {
                proven: true,
                ..claim(2, ClaimState::Paid)
            },
            claim(3, ClaimState::Unpaid),
            claim(4, ClaimState::Disputed),
        ];
        assert_eq!(v.to_prove(), vec![[1; 32], [3; 32], [4; 32]], "never a final claim");
        let spent = vec![(input_a.0, tx(1), 100), (input_c.0, tx(5), 120)];
        let payouts = landed_payouts(&v, &spent, &v.to_prove(), &v.to_uphold());
        assert_eq!(payouts.len(), 2);
        let first = &payouts[&reference(1)];
        assert_eq!((first.txid, first.height, first.priority), (tx(1), 100, Priority::RoutineInclusion));
        assert_eq!(first.claims, vec![B256::repeat_byte(1)], "the proven claim is left out");
        let rebuilt = &payouts[&reference(5)];
        assert_eq!((rebuilt.claims.clone(), rebuilt.priority), (vec![B256::repeat_byte(4)], Priority::Dispute));
        assert!(!payouts.contains_key(&reference(4)), "the expired first commitment never landed");
        assert!(!payouts.contains_key(&reference(3)), "its input is unspent");
    }
}
