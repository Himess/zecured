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
//!
//! W-1: an `Included` proof says only that X is in a block; it makes final every claim it is given.
//! Before a claim goes into a proof, its committed reference must be the one its claimant computes,
//! `keccak256(salt || txid_X)` (the coordinator holds every claim's salt). A claim X's commitment
//! hid under another reference never goes into a delivery proof: its claimant could never find X, so
//! the coordinator proves the violation instead (F4, at a dispute's priority), and the compensator
//! pays the claim.
//!
//! W-11: `Included` proves only that a named X is in a block, not that X spent the committed input
//! nor that it came after the commitment. A commitment of the vault whose payout the scan does not
//! show (its input unspent, or spent by another transaction) is resolved by its payout reference
//! over the whole Ironwood history ([`Coordinator::resolve_commitments`]). An X found there that did
//! not spend the input is no payout of this coordinator: each of its claims named under a reference
//! its claimant does not compute is proven an F4, as in W-1; its other claims are their claimants'
//! (F3), and its change, if committed, F2.
//!
//! W-10: a proof's anchor is public on Base (the slasher's events and the journal), the payout's
//! block is not. An anchor taken as the first header with the slasher's work above the payout sits
//! right above its block and gives it away. So the anchor of a payout's proofs is fixed when its
//! commitment lands, while Zcash block `c` is being mined: `a = c + payout_delay_max_blocks + 1 + r`,
//! `r` drawn once per payout in `0..=payout_delay_max_blocks` and kept ([`plan_anchor`]). The payout
//! is broadcast after its L23 delay of 1 to `payout_delay_max_blocks` blocks, so it lands in one of
//! the blocks `c + 1` to `c + payout_delay_max_blocks`, below `a` whichever it is. A payout mined at
//! or after `a` (late), or one this coordinator did not plan, anchors at `h + 1 + r`. The anchor
//! never passes the last block of the payout's epoch (W-20). The coordinator waits for the
//! slasher's work on top of exactly that header. Two exceptions, both for a claim that would be
//! paid twice otherwise: when the delayed proof could not land `DEADLINE_MARGIN_SECS` before the
//! dispute's end (`payoutDispute`), or before the liveness deadline of a claim still pending
//! (`livenessDeadline`), the payout is proven under the first header with the work instead.

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
    events::{
        ISignerBond, IZcashHeaderRelay, IZecuredRegistry, IZecuredSlasher, VERSION_VIOLATION, VIOLATION_WRONG_CLAIM_REF,
    },
    relay::{anchor_above, anchor_at, blocks_for_work},
};
use zecured_deposit_witness::{p2p, violation};
use zecured_policy::{
    derive::{deposit_tag, payout_ref, tag_key},
    ledger::{ClaimState, Recipient, claim_ref},
};
use zecured_zcash_ops::{lwd, network::ZcashNetwork, scan::ReceivedNote};

use crate::{
    Coordinator, LIVENESS_WARNING_INTERVAL, RECEIPT_TIMEOUT,
    error::scrubbed,
    log_failure, now,
    proofs::{DepositProofs, Prover, SealFile, guest_active, prove_into, seal_selector, version_active},
    queue::{Enqueued, Priority},
    short,
    store::{CommittedPayout, CoordVault, PlannedAnchor},
};

/// W-10: the Zcash block target spacing, to estimate when an anchor gets the slasher's work.
const BLOCK_SECS: u64 = 75;
/// W-10: what a delivery proof takes at most from its job to Base: the queue, the STARK and its
/// Groth16 wrap (minutes on the deployment's prover, docs/TESTING.md), and the submission.
const PROOF_SECS: u64 = 20 * 60;
/// W-10: how long before a claim's deadline its payout's proof must be on Base.
const DEADLINE_MARGIN_SECS: u64 = 10 * 60;

/// A uniform draw in `0..=max` from the operating system's CSPRNG, without modulo bias.
fn draw_r(max: u32) -> u32 {
    let n = u64::from(max) + 1;
    let zone = (1u64 << 32) - (1u64 << 32) % n;
    loop {
        let v = u64::from(rand_core_06::RngCore::next_u32(&mut rand_core_06::OsRng));
        if v < zone {
            return u32::try_from(v % n).unwrap_or(0);
        }
    }
}

/// W-10: the anchor of a payout committed while the Zcash tip was `tip` (block `c = tip + 1` being
/// mined): `c + max + 1 + r` for `payout_delay_max_blocks = max`, with a fresh draw `r`.
pub(crate) fn plan_anchor(payout_ref: B256, tip: u32, max: u32) -> PlannedAnchor {
    let r = draw_r(max);
    PlannedAnchor {
        payout_ref,
        planned: Some(tip.saturating_add(1).saturating_add(max).saturating_add(1).saturating_add(r)),
        r,
    }
}

/// W-10, W-20: the anchor of a payout mined at `h`: the `planned` one when the payout came below it,
/// whatever `h` is; otherwise (late, or never planned here) `h + 1 + r`. Never past `epoch_last`,
/// the last block of the payout's epoch; `None` when no anchor can prove it (it is in that block).
pub(crate) fn anchor_height(planned: Option<u32>, r: u32, h: u32, epoch_last: Option<u32>) -> Option<u32> {
    let a = match planned {
        Some(p) if p > h => p,
        _ => h.saturating_add(1).saturating_add(r),
    };
    let a = epoch_last.map_or(a, |last| a.min(last));
    (a > h).then_some(a)
}

/// W-20: the last block of the epoch of block `height`: the next network upgrade's activation less
/// one, or `None` while zcash_protocol knows no later activation.
pub(crate) fn epoch_last_height(network: ZcashNetwork, height: u32) -> Option<u32> {
    use zcash_protocol::consensus::{BlockHeight, BranchId};
    let params = network.params();
    let (_, end) = BranchId::for_height(&params, BlockHeight::from_u32(height)).height_bounds(&params)?;
    end.map(|e| u32::from(e).saturating_sub(1))
}

/// W-20: whether a payout broadcast at the tip `tip` could not be proven before the activation of a
/// known network upgrade: its anchor (at most `window` blocks on) and the slasher's work on top of it
/// (`work` blocks) would cross `last`, the last block of the tip's epoch (`None`: none known).
pub(crate) fn too_close_to_activation(tip: u32, window: u32, work: u32, last: Option<u32>) -> bool {
    last.is_some_and(|last| tip.saturating_add(window).saturating_add(work) > last)
}

/// W-10: whether a payout's proof drops its delay: its delayed anchor `a` is not under the work yet,
/// `q` (below it) is, and the delayed proof, `a - q` blocks and a proof away, would not land
/// `DEADLINE_MARGIN_SECS` before `deadline`.
pub(crate) fn drop_delay(now: u64, a: u32, q: u32, deadline: u64) -> bool {
    let eta = now.saturating_add(u64::from(a.saturating_sub(q)) * BLOCK_SECS).saturating_add(PROOF_SECS);
    eta.saturating_add(DEADLINE_MARGIN_SECS) > deadline
}

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
pub(crate) fn spent_notes(ufvk: &str, notes: &[ReceivedNote]) -> Vec<([u8; 32], TxId, u32)> {
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

/// W-1: a claim of a landed payout that its commitment names under a reference other than the one
/// its claimant computes (`keccak256(salt || txid)`): never proven paid by that payout; proven an F4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HiddenClaim {
    pub payout_ref: B256,
    pub claim: B256,
    pub txid: TxId,
    pub height: u32,
}

/// What the payouts of a vault that landed hold: the claims to prove paid, by payout, the claims
/// their commitments hid (W-1), and how many claims could not be checked (no salt, or a record
/// without references): neither proven paid nor proven hidden.
#[derive(Debug, Default)]
pub(crate) struct Landed {
    pub payouts: BTreeMap<B256, ToProve>,
    pub hidden: Vec<HiddenClaim>,
    pub unchecked: usize,
}

/// Section 3: the payouts of the vault's recorded commitments that landed (the transaction that
/// spent the commitment's input is the commitment's own), with their claims among `wanted`, keyed
/// by payout reference. A claim of `urgent` (disputed or reopened) makes its payout's proof a
/// dispute's. W-1: a claim goes in only if its committed reference is `keccak256(salt || txid)` for
/// its recipient's salt (`salt_of`, by claim id).
pub(crate) fn landed_payouts(
    v: &CoordVault,
    spent: &[([u8; 32], TxId, u32)],
    wanted: &[[u8; 32]],
    urgent: &[[u8; 32]],
    salt_of: impl Fn(&[u8; 32]) -> Option<[u8; 32]>,
) -> Landed {
    let mut landed = Landed::default();
    for c in &v.commitments {
        if !c.claims.iter().any(|id| wanted.contains(id)) {
            continue;
        }
        let Some(&(_, txid, height)) = spent.iter().find(|(tag, ..)| B256::from(*tag) == c.input_tag) else {
            continue; // its input is unspent: nothing landed yet
        };
        if B256::from(payout_ref(&v.ufvk, txid.as_ref())) != c.payout_ref {
            continue; // another commitment of the same input landed (a rebuilt payout)
        }
        let mut claims = vec![];
        for (i, id) in c.claims.iter().enumerate().filter(|(_, id)| wanted.contains(id)) {
            match (c.claim_refs.get(i), salt_of(id)) {
                (Some(committed), Some(salt)) if *committed == B256::from(claim_ref(&salt, txid.as_ref())) => {
                    claims.push(B256::from(*id));
                }
                (Some(_), Some(_)) => landed.hidden.push(HiddenClaim {
                    payout_ref: c.payout_ref,
                    claim: B256::from(*id),
                    txid,
                    height,
                }),
                _ => landed.unchecked += 1,
            }
        }
        if claims.is_empty() {
            continue;
        }
        let payouts = &mut landed.payouts;
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
    landed
}

/// W-11: the vault's commitments whose payout the scan does not show: the input is unspent (the
/// payout `inflight` is skipped: it is not mined yet), or spent by a transaction of another payout
/// reference.
pub(crate) fn unresolved<'a>(
    v: &'a CoordVault,
    spent: &[([u8; 32], TxId, u32)],
    inflight: Option<B256>,
) -> Vec<&'a CommittedPayout> {
    v.commitments
        .iter()
        .filter(|c| Some(c.payout_ref) != inflight)
        .filter(|c| match spent.iter().find(|(tag, ..)| B256::from(*tag) == c.input_tag) {
            Some((_, txid, _)) => B256::from(payout_ref(&v.ufvk, txid.as_ref())) != c.payout_ref,
            None => true,
        })
        .collect()
}

/// W-11: the claims commitment `c` names under a reference other than `keccak256(salt || txid_X)`
/// for its payout `x` mined at `height` (`salt_of` by claim id; a claim without a salt here is
/// skipped).
pub(crate) fn hidden_claims(
    c: &CommittedPayout,
    x: TxId,
    height: u32,
    salt_of: impl Fn(&[u8; 32]) -> Option<[u8; 32]>,
) -> Vec<HiddenClaim> {
    c.claims
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let (committed, salt) = (c.claim_refs.get(i)?, salt_of(id)?);
            (*committed != B256::from(claim_ref(&salt, x.as_ref()))).then(|| HiddenClaim {
                payout_ref: c.payout_ref,
                claim: B256::from(*id),
                txid: x,
                height,
            })
        })
        .collect()
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

/// W-1: one background F4 proof: payout `txid` of the vault (viewing key `ufvk`) names the claim of
/// recipient `recipient` under a wrong reference. The witness carries the vault's tag key K and the
/// recipient (its salt and receiver), never the viewing key, and reaches the prover through stdin.
struct HiddenJob {
    ufvk: String,
    txid: TxId,
    anchor: u32,
    recipient: Recipient,
    beneficiary: Address,
    network: ZcashNetwork,
    lwd: String,
    settings: DepositProofs,
    seal_path: PathBuf,
}

impl HiddenJob {
    async fn run(self) -> Result<()> {
        let (headers, network) = (self.settings.headers.clone(), self.network);
        let last = tokio::task::spawn_blocking(move || p2p::extend_file(network, &headers, &p2p::seed_peers(network)))
            .await??;
        ensure!(last >= self.anchor, "the P2P peers are behind the relay");
        let mut client = lwd::connect(&self.lwd).await?;
        lwd::check_network(&mut client, network).await?;
        let (witness, journal) = violation::wrong_claim_ref(
            &mut client,
            network,
            &self.settings.headers,
            &self.settings.summaries,
            &self.ufvk,
            self.txid,
            self.recipient.salt,
            self.recipient.receiver,
            self.anchor,
            self.beneficiary.0.0,
        )
        .await?;
        let _ = journal; // checked against the statement natively by the builder
        let witness = serde_json::to_vec(&witness)?;
        prove_into(&self.settings, true, witness, &self.seal_path).await
    }
}

impl Prover {
    /// Where the inclusion proof of the payout `payout_ref` is kept until it is submitted.
    fn inclusion_seal_path(&self, payout_ref: &B256) -> PathBuf {
        self.dir.join(format!("{}.inclusion.json", hex::encode(payout_ref)))
    }

    /// W-1: where the F4 proof of the claim `claim` that payout `payout_ref` hid is kept until submitted.
    fn hidden_seal_path(&self, payout_ref: &B256, claim: &B256) -> PathBuf {
        self.dir.join(format!("{}.{}.f4.json", hex::encode(payout_ref), hex::encode(claim)))
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
        // W-1: the claims' salts, by id (the recipients' file; never logged).
        let salts: Vec<([u8; 32], [u8; 32])> = {
            let store = self.store.lock().await;
            v.claims.iter().filter_map(|c| Some((c.id, store.recipient(&c.commitment)?.salt))).collect()
        };
        let Landed {
            mut payouts,
            hidden,
            unchecked,
        } = landed_payouts(v, &spent, &wanted, &urgent, |id| salts.iter().find(|(c, _)| c == id).map(|(_, s)| *s));
        if unchecked != 0 && self.dispute_warning_due(&v.vault_id) {
            warn!(
                vault = %short(&v.vault_id),
                claims = unchecked,
                "claims of a landed payout whose reference this coordinator cannot check (no salt or no record): not proven (W-1)"
            );
        }
        for h in &hidden {
            if let Err(e) = self.prove_hidden(v, prover, slasher, h).await {
                log_failure(&v.vault_id, "proving a hidden claim (F4)", &e);
            }
        }
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
        let deadline = self.proof_deadline(&provider, v, &payout.claims).await?;
        let Some(anchor) = self.proof_anchor(&provider, v, slasher, reference, payout.height, deadline).await? else {
            // W-10: the relay has not got the payout's anchor under the slasher's work yet.
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
        // The claims Base now holds paid are final (the slasher upheld or confirmed each; one it
        // skipped as compensated or written off stays as its event says). The books follow at
        // once: a pass before the slasher's event is ingested must not prove this payout again.
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let mut final_now = Vec::with_capacity(claims.len());
        for id in claims {
            let status = registry.claimDetails(*id).call().await.context("Registry.claimDetails")?.status;
            if status == zecured_base::verify::CLAIM_PAID {
                final_now.push(id.0);
            }
        }
        self.update_vault(vault_id, |v| v.proven_on_base(&final_now)).await?;
        Ok(())
    }
}

impl Coordinator {
    /// W-11: resolves the vault's commitments the scan does not show over the whole Ironwood
    /// history, at most once per keeper period. A payout found there that did not spend its input
    /// was committed by others: its hidden claims are proven F4 (with proofs configured), and the
    /// operators are told.
    pub(crate) async fn resolve_commitments(&self, v: &CoordVault) -> Result<()> {
        let spent = {
            let zcash = self.zcash.lock().await;
            match zcash.scan(&v.vault_id) {
                Some(scan) => spent_notes(&v.ufvk, &scan.notes),
                None => return Ok(()),
            }
        };
        let inflight = v.inflight.as_ref().and_then(|f| f.spend_commit.as_ref()).map(|k| k.record.payout_ref);
        let todo: Vec<CommittedPayout> = unresolved(v, &spent, inflight).into_iter().cloned().collect();
        if todo.is_empty() || !self.keeper_due(&v.vault_id, crate::keepers::Keeper::Resolve) {
            return Ok(());
        }
        let tip = self.fetch_zcash_tip().await?;
        let mut guard = self.ironwood.lock().await;
        if guard.is_none() {
            *guard = Some(zecured_zcash_ops::txindex::IronwoodIndex::open(&self.ironwood_path, self.cfg.zcash())?);
        }
        let index = guard.as_mut().context("the Ironwood index")?;
        if index.range().1 < tip {
            let mut client = self.lwd_client().await?;
            index.update(&mut client, tip).await?;
        }
        let k = tag_key(&v.ufvk);
        let mut found = vec![];
        for c in &todo {
            let from = self.searched.lock().unwrap_or_else(|p| p.into_inner()).get(&c.payout_ref).copied().unwrap_or(0);
            let hit = index
                .txs()
                .filter(|(h, _)| *h > from)
                .find(|(_, txid)| B256::from(zecured_policy::derive::payout_ref_with_key(&k, txid)) == c.payout_ref)
                .map(|(h, txid)| (TxId::from_bytes(*txid), *h));
            match hit {
                Some(x) => found.push((c.clone(), x)),
                None => {
                    let read = index.range().1.saturating_sub(zecured_zcash_ops::txindex::REORG_MARGIN);
                    self.searched.lock().unwrap_or_else(|p| p.into_inner()).insert(c.payout_ref, read);
                }
            }
        }
        drop(guard);
        if found.is_empty() {
            return Ok(());
        }
        error!(
            vault = %short(&v.vault_id),
            payouts = found.len(),
            "a commitment of the vault names a mined transaction that did not spend its input (W-11): its hidden claims are proven F4; its other claims are their claimants' (F3), its change F2"
        );
        let Some(prover) = self.proofs.as_ref() else {
            return Ok(());
        };
        let slasher = self.v3().await?.slasher;
        if slasher == Address::ZERO {
            return Ok(());
        }
        let salts: Vec<([u8; 32], [u8; 32])> = {
            let store = self.store.lock().await;
            v.claims.iter().filter_map(|c| Some((c.id, store.recipient(&c.commitment)?.salt))).collect()
        };
        let salt_of = |id: &[u8; 32]| salts.iter().find(|(c, _)| c == id).map(|(_, s)| *s);
        for (c, (x, height)) in found {
            for h in hidden_claims(&c, x, height, salt_of) {
                if let Err(e) = self.prove_hidden(v, prover, slasher, &h).await {
                    log_failure(&v.vault_id, "proving a hidden claim of a foreign commitment (F4)", &e);
                }
            }
        }
        Ok(())
    }

    /// W-20: whether a known network upgrade is too close for the proof of a payout broadcast now
    /// (its anchor window of `2 * payout_delay_max_blocks + 2` blocks and the slasher's work); `false`
    /// while zcash_protocol knows no later activation.
    pub(crate) async fn near_activation(&self, tip: u32) -> Result<bool> {
        let Some(last) = epoch_last_height(self.cfg.zcash(), tip.saturating_add(1)) else {
            return Ok(false);
        };
        let slasher = self.v3().await?.slasher;
        let work = if slasher == Address::ZERO {
            0
        } else {
            let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
            let s = IZecuredSlasher::new(slasher, &provider);
            let relay = s.relay().call().await.context("slasher.relay")?;
            let min = s.slashConfirmationWork().call().await.context("slasher.slashConfirmationWork")?;
            blocks_for_work(&provider, relay, min).await?
        };
        let window = self.cfg.payout_delay_max_blocks.saturating_mul(2).saturating_add(2);
        Ok(too_close_to_activation(tip, window, work, Some(last)))
    }

    /// W-10: the payout's anchor plan, as kept; a payout this coordinator did not plan gets its `r`
    /// now, kept at once so that every proof of the payout takes the same anchor.
    pub(crate) async fn anchor_plan(&self, vault_id: &B256, reference: B256) -> Result<(Option<u32>, u32)> {
        let fresh = draw_r(self.cfg.payout_delay_max_blocks);
        self.update_vault(vault_id, |cur| match cur.anchors.iter().find(|a| a.payout_ref == reference) {
            Some(a) => (a.planned, a.r),
            None => {
                cur.anchors.push(PlannedAnchor {
                    payout_ref: reference,
                    planned: None,
                    r: fresh,
                });
                (None, fresh)
            }
        })
        .await
    }

    /// W-10: the anchor of a proof of payout `reference`, mined at `h`: its planned anchor once the
    /// relay has the slasher's work on top of it; `None` until then. With a `deadline` the delayed
    /// proof would miss, the first header with the work instead.
    async fn proof_anchor<P: alloy::providers::Provider>(
        &self,
        provider: &P,
        v: &CoordVault,
        slasher: Address,
        reference: B256,
        h: u32,
        deadline: Option<u64>,
    ) -> Result<Option<u32>> {
        let (planned, r) = self.anchor_plan(&v.vault_id, reference).await?;
        let epoch_last = epoch_last_height(self.cfg.zcash(), h);
        let Some(a) = anchor_height(planned, r, h, epoch_last) else {
            if self.dispute_warning_due(&v.vault_id) {
                error!(vault = %short(&v.vault_id), "a payout was mined in its epoch's last block: no anchor can prove it (W-20)");
            }
            return Ok(None);
        };
        let s = IZecuredSlasher::new(slasher, provider);
        let relay = s.relay().call().await.context("slasher.relay")?;
        let min = s.slashConfirmationWork().call().await.context("slasher.slashConfirmationWork")?;
        if anchor_at(provider, relay, a, min).await?.is_some() {
            return Ok(Some(a));
        }
        let Some(deadline) = deadline else {
            return Ok(None);
        };
        let Some(q) = anchor_above(provider, relay, h, min).await? else {
            return Ok(None); // no header above the payout has the work yet
        };
        let q = epoch_last.map_or(q, |last| q.min(last));
        if !drop_delay(now(), a, q, deadline) {
            return Ok(None);
        }
        warn!(vault = %short(&v.vault_id), "a claim's deadline is near: its payout is proven without the anchor's delay (W-10)");
        Ok(Some(q))
    }

    /// W-10: the earliest deadline among the payout's claims that a proof of it must beat: a disputed
    /// claim's end of dispute (`payoutDispute`), a pending one's liveness deadline. `None` when every
    /// claim is confirmed (a dispute would give the proof its whole window).
    async fn proof_deadline<P: alloy::providers::Provider>(
        &self,
        provider: &P,
        v: &CoordVault,
        claims: &[B256],
    ) -> Result<Option<u64>> {
        let state = |id: &B256| v.claims.iter().find(|c| B256::from(c.id) == *id).map(|c| c.state);
        let registry = IZecuredRegistry::new(self.cfg.registry, provider);
        let mut bond = None;
        let mut earliest: Option<u64> = None;
        for id in claims {
            let due = match state(id) {
                Some(ClaimState::Disputed) => {
                    registry.payoutDispute(*id).call().await.context("Registry.payoutDispute")?.dueAt
                }
                Some(ClaimState::Unpaid) => {
                    let b = match bond {
                        Some(b) => b,
                        None => *bond.insert(self.v3().await?.bond),
                    };
                    if b == Address::ZERO {
                        continue;
                    }
                    ISignerBond::new(b, provider).livenessDeadline(*id).call().await.context("bond.livenessDeadline")?
                }
                _ => continue,
            };
            if due != 0 {
                earliest = Some(earliest.map_or(due, |e| e.min(due)));
            }
        }
        Ok(earliest)
    }

    /// W-1: a claim the landed payout's commitment hid: its F4 proof is submitted once made, or queued
    /// at a dispute's priority once the relay has the slasher's work on top of the payout's block.
    /// Nothing about the claim's recipient is logged.
    async fn prove_hidden(&self, v: &CoordVault, prover: &Prover, slasher: Address, h: &HiddenClaim) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let s = IZecuredSlasher::new(slasher, &provider);
        if s.claimCaseOpen(h.claim).call().await.context("slasher.claimCaseOpen")? {
            return Ok(()); // proven already; its execution compensates the claim
        }
        let seal_path = prover.hidden_seal_path(&h.payout_ref, &h.claim);
        if seal_path.exists() {
            return self.submit_hidden(&v.vault_id, slasher, &seal_path).await;
        }
        let key = alloy::primitives::keccak256([h.payout_ref.0, h.claim.0].concat());
        if prover.queue.contains(&key) {
            return Ok(());
        }
        if self.dispute_warning_due(&v.vault_id) {
            error!(
                vault = %short(&v.vault_id),
                "a landed payout's commitment hid a claim from its claimant (W-1): not proven paid; proving the violation (F4)"
            );
        }
        let recipient = {
            let store = self.store.lock().await;
            let claim = v.claims.iter().find(|c| B256::from(c.id) == h.claim).context("a hidden claim is unknown")?;
            store.recipient(&claim.commitment).context("a hidden claim has no recipient")?
        };
        let image = prover.image_id(true).await?;
        let versions = s.versions().call().await.context("slasher.versions")?;
        if !guest_active(&provider, versions, VERSION_VIOLATION, image).await? {
            warn!("the configured violation guest is not an active verifier version on Base: no F4 proof (T-07a)");
            return Ok(());
        }
        let Some(anchor) = self.proof_anchor(&provider, v, slasher, h.payout_ref, h.height, None).await? else {
            return Ok(()); // W-10: the payout's anchor is not under the slasher's work yet
        };
        let job = HiddenJob {
            ufvk: v.ufvk.clone(),
            txid: h.txid,
            anchor,
            recipient,
            beneficiary: self.gas.address(),
            network: self.cfg.zcash(),
            lwd: self.cfg.lightwalletd_url.clone(),
            settings: prover.settings.clone(),
            seal_path,
        };
        let vault_id = v.vault_id;
        let _ = prover.queue.enqueue(key, Priority::Dispute, async move {
            info!(vault = %short(&vault_id), "proving a hidden claim (F4)");
            match job.run().await {
                Ok(()) => info!(vault = %short(&vault_id), "F4 proof ready"),
                Err(e) => {
                    warn!(vault = %short(&vault_id), "F4 proof failed");
                    debug!(vault = %short(&vault_id), error = %scrubbed(&e), "F4 proof failed");
                }
            }
        });
        Ok(())
    }

    /// W-1: `proveViolation(F4, ...)` with a finished proof, once its anchor is still on the relay's best
    /// chain under the slasher's work and its guest an active violation version.
    async fn submit_hidden(&self, vault_id: &B256, slasher: Address, seal_path: &std::path::Path) -> Result<()> {
        let s: SealFile = serde_json::from_slice(&fs::read(seal_path)?)?;
        ensure!(s.journal.len() == JOURNAL_LENGTH, "an F4 journal is {JOURNAL_LENGTH} bytes");
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let slasher = IZecuredSlasher::new(slasher, &provider);
        let anchor = B256::from_slice(&s.journal[32..64]);
        let relay = IZcashHeaderRelay::new(slasher.relay().call().await.context("slasher.relay")?, &provider);
        let min = slasher.slashConfirmationWork().call().await.context("slasher.slashConfirmationWork")?;
        if !relay.isInBestChain(anchor).call().await.context("relay.isInBestChain")?
            || relay.confirmationWork(anchor).call().await.context("relay.confirmationWork")? < min
        {
            let _ = fs::remove_file(seal_path);
            warn!(vault = %short(vault_id), "an F4 proof's anchor left the relay's best chain: proving again");
            return Ok(());
        }
        let call = slasher.proveViolation(VIOLATION_WRONG_CLAIM_REF, s.journal.clone(), s.seal.clone(), s.image_id);
        if let Err(e) = call.call().await {
            // Proven by someone else first (the case is open), or no longer a violation.
            let _ = fs::remove_file(seal_path);
            debug!(vault = %short(vault_id), error = %scrubbed(&anyhow::anyhow!(e)), "the F4 proof is not taken");
            return Ok(());
        }
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate proveViolation")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send proveViolation")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("proveViolation receipt")?;
        ensure!(receipt.status(), "proveViolation reverted");
        info!(vault = %short(vault_id), "an F4 case is open on Base: its execution compensates the hidden claim (W-1)");
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

    /// W-10: within the window, the anchor is the planned height whatever the payout's block is; it is
    /// `h + 1` only when the payout came in the window's last block. Every L23 delay, draw and mempool
    /// wait of a coordinator with `payout_delay_max_blocks` = 8.
    #[test]
    fn w10_the_anchor_does_not_depend_on_the_payout_height() {
        let max = 8;
        let tip = 4_500_000;
        for delay in 1..=max {
            for r in 0..=max {
                let planned = tip + 1 + max + 1 + r;
                // Broadcast once the tip reaches `tip + delay` (`broadcast_from`): mined in the next
                // block at the earliest, one of `c + 1 .. c + max` for `c = tip + 1`.
                let earliest = tip + delay + 1;
                for wait in 0..=2 * max + 2 {
                    let h = earliest + wait;
                    let a = anchor_height(Some(planned), r, h, None).unwrap();
                    assert!(a > h);
                    if h < planned {
                        assert_eq!(a, planned, "delay {delay}, r {r}, wait {wait}: the planned anchor");
                        assert_eq!(a == h + 1, h == planned - 1, "h + 1 only at the window's end");
                    } else {
                        assert_eq!(a, h + 1 + r, "a late payout");
                    }
                }
            }
        }
        let p = plan_anchor(B256::repeat_byte(1), tip, max);
        assert!(p.r <= max);
        assert_eq!(p.planned, Some(tip + 1 + max + 1 + p.r));
        assert_eq!(plan_anchor(B256::ZERO, tip, 0).planned, Some(tip + 2), "a local fork without delays");
        for _ in 0..200 {
            assert!(draw_r(8) <= 8);
        }
        assert_eq!(draw_r(0), 0);
    }

    /// W-10: a payout this coordinator did not plan (a disputed one traced on Base, or one from
    /// before v4.1) anchors as a late one.
    #[test]
    fn w10_an_unplanned_payout_anchors_as_a_late_one() {
        assert_eq!(anchor_height(None, 3, 100, None), Some(104));
        assert_eq!(anchor_height(Some(100), 3, 100, None), Some(104), "mined at the planned height: late");
    }

    /// W-20: the anchor never leaves the payout's epoch; a payout in its epoch's last block has no
    /// anchor at all. The epochs are zcash_protocol's: Testnet's NU7 has no known end.
    #[test]
    fn w20_the_anchor_never_leaves_the_payouts_epoch() {
        assert_eq!(anchor_height(Some(120), 4, 100, Some(110)), Some(110), "cut at the epoch's last block");
        assert_eq!(anchor_height(Some(120), 4, 100, Some(130)), Some(120));
        assert_eq!(anchor_height(Some(120), 4, 125, Some(127)), Some(127), "late and cut");
        assert_eq!(anchor_height(Some(120), 4, 110, Some(110)), None, "the epoch's last block: unprovable");
        assert_eq!(epoch_last_height(ZcashNetwork::Testnet, 4_500_000), None);
        assert_eq!(epoch_last_height(ZcashNetwork::Testnet, 4_200_000), Some(4_465_025), "until NU7 activated");
    }

    /// W-11: a commitment whose payout the scan does not show is resolved over the history: one whose
    /// input another payout spent (a foreign commitment, or a rebuilt payout's), or whose input is
    /// unspent, never the payout in flight; a claim it names under a wrong reference for the X found
    /// is hidden (F4), one under the right reference is not.
    #[test]
    fn w11_a_foreign_commitment_is_resolved_and_its_hidden_claims_found() {
        let mut v = crate::store::tests::vault(1);
        v.ufvk = "uviewtest1vault".into();
        let (x1, old) = ([0x11u8; 32], [0x0du8; 32]);
        let honest = |id: u8, tx: &[u8; 32]| B256::from(claim_ref(&[id; 32], tx));
        let own = CommittedPayout {
            payout_ref: B256::from(payout_ref(&v.ufvk, &x1)),
            input_tag: B256::repeat_byte(0x7a),
            claims: vec![[1; 32]],
            claim_refs: vec![honest(1, &x1)],
        };
        // Colluders commit the same input again, naming a transaction mined months before.
        let foreign = CommittedPayout {
            payout_ref: B256::from(payout_ref(&v.ufvk, &old)),
            input_tag: B256::repeat_byte(0x7a),
            claims: vec![[1; 32], [2; 32]],
            claim_refs: vec![honest(1, &old), B256::repeat_byte(0xee)],
        };
        let pending = CommittedPayout {
            payout_ref: B256::repeat_byte(0x99),
            input_tag: B256::repeat_byte(0x7b),
            claims: vec![[3; 32]],
            claim_refs: vec![B256::repeat_byte(0x33)],
        };
        v.commitments = vec![own.clone(), foreign.clone(), pending.clone()];
        let spent = vec![([0x7a; 32], TxId::from_bytes(x1), 500)];
        let refs = |cs: Vec<&CommittedPayout>| cs.into_iter().map(|c| c.payout_ref).collect::<Vec<_>>();
        assert_eq!(refs(unresolved(&v, &spent, None)), vec![foreign.payout_ref, pending.payout_ref]);
        assert_eq!(
            refs(unresolved(&v, &spent, Some(pending.payout_ref))),
            vec![foreign.payout_ref],
            "not the payout in flight"
        );
        let salt = |id: &[u8; 32]| Some([id[0]; 32]);
        let hidden = hidden_claims(&foreign, TxId::from_bytes(old), 200, salt);
        assert_eq!(
            hidden.iter().map(|h| h.claim).collect::<Vec<_>>(),
            vec![B256::repeat_byte(2)],
            "claim 2's wrong reference: F4"
        );
        assert_eq!((hidden[0].payout_ref, hidden[0].height), (foreign.payout_ref, 200));
        assert!(hidden_claims(&foreign, TxId::from_bytes(old), 200, |_| None).is_empty(), "no salt here: not judged");
    }

    /// W-20: no payout is broadcast when its anchor window and the slasher's work would cross a known
    /// activation; with none known, payouts go on.
    #[test]
    fn w20_no_payout_near_a_known_activation() {
        assert!(!too_close_to_activation(1_000, 18, 24, None), "no known activation");
        assert!(!too_close_to_activation(1_000, 18, 24, Some(1_042)));
        assert!(too_close_to_activation(1_001, 18, 24, Some(1_042)), "the work would cross the activation");
        assert!(too_close_to_activation(1_000, 18, u32::MAX, Some(5_000)), "a relay without work: wait");
    }

    /// W-10: the delay is dropped only when the delayed proof would miss a claim's deadline by the
    /// margin: never for a dispute's window of hours, always when the deadline is minutes away.
    #[test]
    fn w10_the_delay_is_dropped_only_before_a_near_deadline() {
        let now = 1_000_000;
        // 18 blocks to the delayed anchor's work: 22.5 minutes, plus the proof's 20.
        assert!(!drop_delay(now, 120, 102, now + 6 * 3_600), "a dispute's six hours: keep the delay");
        assert!(drop_delay(now, 120, 102, now + 40 * 60), "a liveness deadline 40 minutes away: drop it");
        let edge = now + 18 * BLOCK_SECS + PROOF_SECS + DEADLINE_MARGIN_SECS;
        assert!(!drop_delay(now, 120, 102, edge));
        assert!(drop_delay(now, 120, 102, edge - 1));
    }

    /// W-10: the anchor of a payout is drawn once and kept: the store reopened after a restart gives
    /// the same `r`, and a second proof of the payout takes the same anchor.
    #[tokio::test]
    async fn w10_the_anchor_plan_is_kept_across_restarts() {
        let dir = crate::store::tests::TempDir::new("w10-anchor");
        let urls: Vec<String> = (1..=5).map(|_| "http://127.0.0.1:9".to_owned()).collect();
        let gas = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
        let mut cfg = crate::nodes::tests::config(&urls);
        cfg.payout_delay_max_blocks = 1_000_000; // a collision of two draws is then unlikely
        let v = crate::store::tests::vault(1);
        let reference = B256::repeat_byte(0x9f);
        let first = {
            let c = Coordinator::new(cfg.clone(), &dir.0, &gas).unwrap();
            c.store.lock().await.save(v.clone()).unwrap();
            let first = c.anchor_plan(&v.vault_id, reference).await.unwrap();
            assert_eq!(c.anchor_plan(&v.vault_id, reference).await.unwrap(), first, "drawn once");
            first
        };
        assert_eq!(first.0, None, "not planned here: a late payout's anchor");
        let c = Coordinator::new(cfg, &dir.0, &gas).unwrap();
        assert_eq!(c.anchor_plan(&v.vault_id, reference).await.unwrap(), first, "kept across a restart");
        assert_eq!(c.vault(&v.vault_id).await.unwrap().anchors.len(), 1);
    }

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
        // W-1: each claim's salt is [id; 32]; an honest commitment names it under keccak256(salt || txid).
        let honest =
            |claims: &[u8], tx: u8| claims.iter().map(|c| B256::from(claim_ref(&[*c; 32], &[tx; 32]))).collect();
        v.commitments = vec![
            // Landed (tx 1 spent input a): claims 1 and 2.
            CommittedPayout {
                payout_ref: reference(1),
                input_tag: input_a,
                claims: vec![[1; 32], [2; 32]],
                claim_refs: honest(&[1, 2], 1),
            },
            // Its input b is unspent: claim 3 waits.
            CommittedPayout {
                payout_ref: reference(3),
                input_tag: input_b,
                claims: vec![[3; 32]],
                claim_refs: honest(&[3], 3),
            },
            // A first commitment of input c that expired unmined; the rebuild (tx 5) landed.
            CommittedPayout {
                payout_ref: reference(4),
                input_tag: input_c,
                claims: vec![[4; 32]],
                claim_refs: honest(&[4], 4),
            },
            CommittedPayout {
                payout_ref: reference(5),
                input_tag: input_c,
                claims: vec![[4; 32]],
                claim_refs: honest(&[4], 5),
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
        let salt = |id: &[u8; 32]| Some([id[0]; 32]);
        let landed = landed_payouts(&v, &spent, &v.to_prove(), &v.to_uphold(), salt);
        assert!(landed.hidden.is_empty() && landed.unchecked == 0);
        let payouts = landed.payouts;
        assert_eq!(payouts.len(), 2);
        let first = &payouts[&reference(1)];
        assert_eq!((first.txid, first.height, first.priority), (tx(1), 100, Priority::RoutineInclusion));
        assert_eq!(first.claims, vec![B256::repeat_byte(1)], "the proven claim is left out");
        let rebuilt = &payouts[&reference(5)];
        assert_eq!((rebuilt.claims.clone(), rebuilt.priority), (vec![B256::repeat_byte(4)], Priority::Dispute));
        assert!(!payouts.contains_key(&reference(4)), "the expired first commitment never landed");
        assert!(!payouts.contains_key(&reference(3)), "its input is unspent");
    }

    /// W-1: a claim the landed payout's commitment named under a reference its claimant never
    /// computes is not proven paid: it is reported hidden (to be proven an F4); a claim whose salt
    /// this coordinator lacks is neither proven nor reported.
    #[test]
    fn w1_a_claim_under_a_wrong_reference_is_never_proven_paid() {
        let mut v = vault(1);
        v.ufvk = "uviewtest1vault".into();
        let input = B256::repeat_byte(0xa);
        let x = B256::from(payout_ref(&v.ufvk, &[7; 32]));
        v.commitments = vec![CommittedPayout {
            payout_ref: x,
            input_tag: input,
            claims: vec![[1; 32], [2; 32], [3; 32]],
            claim_refs: vec![
                B256::from(claim_ref(&[1; 32], &[7; 32])),
                B256::repeat_byte(0xee), // not keccak256(salt || txid): its claimant never finds X
                B256::from(claim_ref(&[3; 32], &[7; 32])),
            ],
        }];
        v.claims = vec![
            claim(1, ClaimState::Unpaid),
            claim(2, ClaimState::Unpaid),
            claim(3, ClaimState::Unpaid),
        ];
        let spent = vec![(input.0, TxId::from_bytes([7; 32]), 300)];
        let salt = |id: &[u8; 32]| (id[0] != 3).then_some([id[0]; 32]);
        let landed = landed_payouts(&v, &spent, &v.to_prove(), &v.to_uphold(), salt);
        assert_eq!(landed.payouts[&x].claims, vec![B256::repeat_byte(1)], "only the honest claim");
        assert_eq!(
            landed.hidden,
            vec![HiddenClaim {
                payout_ref: x,
                claim: B256::repeat_byte(2),
                txid: TxId::from_bytes([7; 32]),
                height: 300
            }]
        );
        assert_eq!(landed.unchecked, 1, "claim 3: no salt here");
    }
}
