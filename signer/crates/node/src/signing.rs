//! Signing sessions: the policy decision and the two FROST rounds for one payout.
//!
//! Round 1 (`commit`): the node recomputes the plan from its own view of both chains,
//! verifies the coordinator's PCZT against it (`zecured_policy::verify`) and returns one
//! FROST commitment per vault spend. Round 2 (`share`): the node signs the sighash and
//! randomizers **it computed itself** from the PCZT it verified; the coordinator only
//! supplies the other signers' commitments.
//!
//! A node has at most one payout in flight per vault (H-1): it does not start a second one
//! while the first can still be mined, and it later confirms only the very transaction it
//! signed. For these v6 transactions without transparent parts, the sighash it signed is the
//! transaction id (ZIP 244; pinned by zcash-ops' `frost_spend` test).

use std::{collections::BTreeMap, time::Instant};

use alloy::primitives::{B256, Bytes};
use anyhow::{Context, Result, anyhow, bail, ensure};
use pczt::Pczt;
use reddsa::frost::redpallas::{
    keys::KeyPackage,
    round1::{SigningCommitments, SigningNonces},
};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zecured_base::SpendCommit;
use zecured_policy::{
    derive::{deposit_tag, payout_ref},
    ledger::{self, Commitment, NextPayout, PayoutPlan, claim_ref},
    verify::{ExpectedInput, TxExpectations, Verified, verify_payout},
};
use zecured_threshold::{self as threshold, signer_id};
use zecured_zcash_ops::{scan::ReceivedNote, spend::PAYOUT_EXPIRY_DELTA};

use crate::{
    Inflight,
    chain::{VaultNotes, owns_receiver, vault_fvk},
    store::{Store, VaultRecord},
};

/// Blocks the coordinator's view of the Zcash tip may be ahead of ours.
const TIP_SKEW: u32 = 5;

/// What a signing session is for.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignKind {
    /// Pay these claims (P2, P3, P6).
    Payout { claim_ids: Vec<B256> },
    /// Refund this non-conforming note (P5).
    Refund { note_id: B256 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitRequest {
    pub session_id: String,
    pub vault_id: B256,
    #[serde(flatten)]
    pub kind: SignKind,
    /// The proved, unsigned PCZT (base64).
    #[serde(with = "crate::b64")]
    pub pczt: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitResponse {
    pub index: u16,
    /// One FROST commitment per vault spend, in action order.
    pub commitments: Vec<serde_json::Value>,
    /// Tier 3, payouts only: the commitment this node signs for the payout it verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_commit: Option<SignedSpendCommit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareRequest {
    pub session_id: String,
    /// Per vault spend (same order as the commitments): the chosen signers' commitments.
    pub commitments: Vec<BTreeMap<u16, serde_json::Value>>,
    /// Tier 3, payouts only (T3-4): every signature the Registry's `commitSpend` used, in the
    /// order it used them. A node releases its shares only if it holds all of them.
    #[serde(default)]
    pub spend_commit_sigs: Vec<Bytes>,
}

/// Tier 3 (docs/TIER3.md § 3): what the signers commit on Base before any of them signs a payout.
/// Every field is a keyed or salted hash, a Base id or a value Base can compute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendCommitRecord {
    pub vault_id: B256,
    pub input_tag: B256,
    pub claim_ids: Vec<B256>,
    pub claim_refs: Vec<B256>,
    pub payout_ref: B256,
    pub change_tag: B256,
    pub change_value: u64,
}

impl SpendCommitRecord {
    /// The EIP-712 message signed for this record.
    pub fn message(&self) -> SpendCommit {
        SpendCommit {
            vaultId: self.vault_id,
            inputTag: self.input_tag,
            claimIds: self.claim_ids.clone(),
            claimRefs: self.claim_refs.clone(),
            payoutRef: self.payout_ref,
            changeTag: self.change_tag,
            changeValue: self.change_value,
        }
    }
}

/// A record and the answering node's signature of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedSpendCommit {
    pub record: SpendCommitRecord,
    pub signature: Bytes,
}

/// The commitment of a verified payout (docs/TIER3.md §§ 2, 3), computed by every node from its own
/// view: the tag of the one lineage note it spends (`input_cmx`), the plan's claims with the salts
/// of their recipients, references to the verified sighash (the txid of these transactions) and
/// the tag of the change note the verified PCZT returns to the vault.
pub fn spend_commit(
    vault_id: B256,
    ufvk: &str,
    input_cmx: &[u8; 32],
    claims: &[([u8; 32], [u8; 32])],
    verified: &Verified,
    change: u64,
) -> Result<SpendCommitRecord> {
    let txid = verified.sighash;
    let change_tag = match (change, verified.change_cmx) {
        (0, None) => [0u8; 32],
        (c, Some(cmx)) if c > 0 => deposit_tag(ufvk, &cmx),
        _ => bail!("the verified change output does not match the plan"),
    };
    Ok(SpendCommitRecord {
        vault_id,
        input_tag: B256::from(deposit_tag(ufvk, input_cmx)),
        claim_ids: claims.iter().map(|(id, _)| B256::from(*id)).collect(),
        claim_refs: claims.iter().map(|(_, salt)| B256::from(claim_ref(salt, &txid))).collect(),
        payout_ref: B256::from(payout_ref(ufvk, &txid)),
        change_tag: B256::from(change_tag),
        change_value: change,
    })
}

/// T3-5 (liveness): a payout from a lineage note pays every pending claim already committed
/// against that note (`bound`), so that none stays bound to a note that is then spent without it.
pub fn check_bound_claims(plan_claims: &[[u8; 32]], bound: &[[u8; 32]]) -> Result<()> {
    match bound.iter().find(|c| !plan_claims.contains(c)) {
        Some(c) => {
            bail!("claim 0x{} is committed against this lineage note but not in the payout", hex::encode(&c[..4]))
        }
        None => Ok(()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareResponse {
    pub index: u16,
    pub shares: Vec<serde_json::Value>,
}

/// A session between the two rounds.
pub struct Session {
    pub vault_id: B256,
    pub kind: SignKind,
    pub verified: Verified,
    /// Tier 3: the commitment this node signed in round 1 (payouts only).
    pub spend_commit: Option<SpendCommitRecord>,
    nonces: Vec<SigningNonces>,
    commitments: Vec<SigningCommitments>,
    /// Nullifiers of the vault notes this payout spends (to confirm it later).
    pub inputs: Vec<[u8; 32]>,
    /// Change the plan returns to the vault.
    pub change: u64,
    /// H-1: id of the payout that was in flight when this session started (it could no longer
    /// be mined then). Round 2 refuses if another payout was recorded since.
    pub prior_inflight: Option<[u8; 32]>,
    /// DEMO: started without any policy check; never recorded as in flight.
    pub unchecked: bool,
    /// When round 1 ran (sessions expire, L-7).
    pub created: Instant,
}

impl Session {
    /// The in-flight record this session's payout becomes once its shares are released.
    pub fn inflight(&self) -> Option<Inflight> {
        match &self.kind {
            SignKind::Payout { claim_ids } if !self.unchecked => Some(Inflight {
                claim_ids: claim_ids.iter().map(|c| c.0).collect(),
                inputs: self.inputs.clone(),
                txid: Some(self.verified.sighash),
                expiry_height: self.verified.expiry_height,
                change: self.change,
                spend_commit: None,
            }),
            _ => None,
        }
    }
}

/// H-1, round 1: a node never commits to a second payout while one it signed can still be
/// mined (before its expiry height, with all its inputs unspent): the coordinator could
/// otherwise broadcast one and attribute it to the other's claims. Signing the very same
/// transaction (`txid`) again is allowed: a retry after a failed round 2, with fresh nonces,
/// cannot pay anything twice. Returns the id of the payout in flight, if any (round 2 checks
/// it has not changed).
pub fn check_no_live_payout(
    inflight: Option<&Inflight>,
    tip: u32,
    unspent: &[[u8; 32]],
    txid: &[u8; 32],
) -> Result<Option<[u8; 32]>> {
    let Some(inflight) = inflight else {
        return Ok(None);
    };
    // A record from before txids were kept has no expiry: it does not block (legacy).
    let Some(signed) = inflight.txid else {
        return Ok(None);
    };
    let can_be_mined = tip < inflight.expiry_height && inflight.inputs.iter().all(|n| unspent.contains(n));
    ensure!(
        !can_be_mined || signed == *txid,
        "a payout this node signed can still be mined (until height {}): not signing another",
        inflight.expiry_height
    );
    Ok(Some(signed))
}

/// H-1, round 2: shares are released only if the payout in flight is still the one round 1
/// saw (or this very transaction): two sessions committed in parallel cannot both be signed.
pub fn check_release(current: Option<&Inflight>, prior: Option<[u8; 32]>, txid: [u8; 32]) -> Result<()> {
    let current = current.and_then(|i| i.txid);
    ensure!(current == prior || current == Some(txid), "another payout was signed since this session started");
    Ok(())
}

/// H-1, confirmation: the claims and the transaction must be exactly the payout this node
/// signed (`txid` in internal byte order).
pub fn check_confirmation(inflight: &Inflight, claim_ids: &[[u8; 32]], txid: &[u8; 32]) -> Result<()> {
    ensure!(claim_ids == inflight.claim_ids, "claims differ from the payout we signed");
    if let Some(signed) = inflight.txid {
        ensure!(signed == *txid, "not the transaction this node signed");
    }
    Ok(())
}

/// The plan and inputs this node expects for `kind`, from its own view.
pub fn expected_plan(
    store: &Store,
    vault: &VaultRecord,
    notes: &VaultNotes,
    kind: &SignKind,
    now: u64,
) -> Result<(PayoutPlan, Vec<ReceivedNote>)> {
    let fvk = vault_fvk(vault)?;
    match kind {
        SignKind::Payout { claim_ids } => {
            ensure!(vault.activated, "vault is not active on Base");
            ledger::check_i10(notes.lineage_value(), vault.outstanding, &vault.claims)
                .map_err(|e| anyhow!("refusing to sign: {e}"))?;
            // H-2: a recipient that is one of the vault's own addresses is no recipient: the
            // claim is set aside rather than paid back into the vault outside its books.
            let recipient = |c: &Commitment| store.recipient(c).filter(|r| !owns_receiver(&fvk, &r.receiver));
            let next = ledger::next_payout(&vault.claims, recipient, notes.lineage_value(), notes.lineage.len(), now)?;
            let NextPayout::Pay(plan) = next else {
                bail!("nothing is payable for this vault right now ({next:?})");
            };
            let wanted: Vec<[u8; 32]> = claim_ids.iter().map(|c| c.0).collect();
            ensure!(plan.claims == wanted, "requested claims are not the next payable claims");
            Ok((plan, notes.lineage.clone()))
        }
        SignKind::Refund { note_id } => {
            let (_, note) = notes
                .non_conforming
                .iter()
                .find(|(id, _)| *id == note_id.0)
                .context("not an unspent non-conforming note")?;
            let request = vault
                .refunds
                .iter()
                .find(|r| r.note_id == note_id.0)
                .context("no accepted refund request for this note")?;
            ensure!(!owns_receiver(&fvk, &request.recipient.receiver), "a refund cannot go to the vault itself");
            let plan = ledger::refund_plan(note.value(), request.recipient.receiver)?;
            Ok((plan, vec![note.clone()]))
        }
    }
}

/// Transaction-level expectations for a payout built on top of `tip` (I-4: the tip comes from
/// the light wallet server, so no arithmetic on it may overflow).
pub fn expectations(tip: u32) -> Result<TxExpectations> {
    let target = tip.checked_add(1).context("tip height overflow")?;
    Ok(TxExpectations {
        consensus_branch_id: u32::from(BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(target))),
        min_expiry: target,
        max_expiry: target.saturating_add(PAYOUT_EXPIRY_DELTA).saturating_add(TIP_SKEW),
    })
}

/// Round 1: verify and commit.
pub fn commit(
    vault: &VaultRecord,
    plan: &PayoutPlan,
    inputs: &[ReceivedNote],
    tip: u32,
    req: &CommitRequest,
    inflight: Option<&Inflight>,
    unspent: &[[u8; 32]],
) -> Result<(Session, Vec<SigningCommitments>)> {
    let expectations = expectations(tip)?;
    let pczt = Pczt::parse(&req.pczt).map_err(|e| anyhow!("parse PCZT: {e:?}"))?;
    let fvk = vault_fvk(vault)?;
    let expected: Vec<ExpectedInput> = inputs
        .iter()
        .map(|n| ExpectedInput {
            nullifier: n.nullifier.to_bytes(),
            value: n.value(),
        })
        .collect();
    let verified =
        verify_payout(&pczt, &fvk, &expected, plan, &expectations).map_err(|e| anyhow!("PCZT refused: {e}"))?;
    // H-1: checked against the verified transaction, so a retry of the same one is allowed.
    let prior_inflight = match &req.kind {
        SignKind::Payout { .. } => check_no_live_payout(inflight, tip, unspent, &verified.sighash)?,
        SignKind::Refund { .. } => None,
    };
    let inputs = expected.iter().map(|e| e.nullifier).collect();
    start_session(vault, req, verified, inputs, plan.change, prior_inflight, false)
}

/// DEMO ONLY (feature `demo`): the "compromised" behaviour, committing to every vault spend
/// of any PCZT without a policy check.
#[cfg(feature = "demo")]
pub fn commit_unchecked(vault: &VaultRecord, req: &CommitRequest) -> Result<(Session, Vec<SigningCommitments>)> {
    let pczt = Pczt::parse(&req.pczt).map_err(|e| anyhow!("parse PCZT: {e:?}"))?;
    let expiry_height = *pczt.global().expiry_height();
    let (sighash, spends) = zecured_zcash_ops::spend::signing_requests_all(&pczt)?;
    let verified = Verified {
        sighash,
        spends,
        expiry_height,
        change_cmx: None,
    };
    start_session(vault, req, verified, vec![], 0, None, true)
}

fn start_session(
    vault: &VaultRecord,
    req: &CommitRequest,
    verified: Verified,
    inputs: Vec<[u8; 32]>,
    change: u64,
    prior_inflight: Option<[u8; 32]>,
    unchecked: bool,
) -> Result<(Session, Vec<SigningCommitments>)> {
    let key_package: KeyPackage = serde_json::from_value(vault.key_package.clone())?;
    let mut rng = rand_core_06::OsRng;
    let (nonces, commitments): (Vec<_>, Vec<_>) =
        verified.spends.iter().map(|_| threshold::commit(&key_package, &mut rng)).unzip();
    Ok((
        Session {
            vault_id: vault.vault_id,
            kind: req.kind.clone(),
            verified,
            spend_commit: None,
            nonces,
            commitments: commitments.clone(),
            inputs,
            change,
            prior_inflight,
            unchecked,
            created: Instant::now(),
        },
        commitments,
    ))
}

/// Round 2: signature shares over this node's own sighash and randomizers.
pub fn share(index: u16, vault: &VaultRecord, session: &Session, req: &ShareRequest) -> Result<Vec<serde_json::Value>> {
    ensure!(req.commitments.len() == session.verified.spends.len(), "one commitment set per spend expected");
    let key_package: KeyPackage = serde_json::from_value(vault.key_package.clone())?;
    let mut shares = Vec::with_capacity(req.commitments.len());
    for (k, set) in req.commitments.iter().enumerate() {
        let mut parsed = BTreeMap::new();
        for (i, c) in set {
            parsed.insert(signer_id(*i)?, serde_json::from_value::<SigningCommitments>(c.clone())?);
        }
        let mine = parsed.get(&signer_id(index)?).context("our commitment is not in the signing set")?;
        ensure!(*mine == session.commitments[k], "our commitment was altered");
        let package = threshold::signing_package(parsed, &session.verified.sighash)?;
        let share =
            threshold::sign_share(&package, &session.nonces[k], &key_package, &session.verified.spends[k].alpha)?;
        shares.push(serde_json::to_value(share)?);
    }
    Ok(shares)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];
    const INPUT: [u8; 32] = [9; 32];
    const TX1: [u8; 32] = [11; 32];
    const TX2: [u8; 32] = [12; 32];

    fn verified(change_cmx: Option<[u8; 32]>) -> Verified {
        Verified {
            sighash: TX1,
            spends: vec![],
            expiry_height: 1_040,
            change_cmx,
        }
    }

    /// Tier 3: every field of the commitment comes from the node's own derivations (the same
    /// functions as the violation guest's), and the change must be the verified change output.
    #[test]
    fn the_commitment_is_derived_from_the_verified_payout() {
        let (ufvk, input_cmx, change_cmx, salt) = ("uviewtest1a", [3u8; 32], [4u8; 32], [5u8; 32]);
        let r =
            spend_commit(B256::repeat_byte(1), ufvk, &input_cmx, &[(A, salt)], &verified(Some(change_cmx)), 400_000)
                .unwrap();
        assert_eq!(r.input_tag, B256::from(deposit_tag(ufvk, &input_cmx)));
        assert_eq!(r.payout_ref, B256::from(payout_ref(ufvk, &TX1)));
        assert_eq!(r.claim_ids, vec![B256::from(A)]);
        assert_eq!(r.claim_refs, vec![B256::from(claim_ref(&salt, &TX1))]);
        assert_eq!(r.change_tag, B256::from(deposit_tag(ufvk, &change_cmx)));
        assert_eq!(r.change_value, 400_000);
        assert_eq!(r.message().changeValue, 400_000);

        let empty = spend_commit(B256::repeat_byte(1), ufvk, &input_cmx, &[(A, salt)], &verified(None), 0).unwrap();
        assert_eq!(empty.change_tag, B256::ZERO);
        // A plan with change but no verified change output, or the reverse, is no payout to commit.
        assert!(spend_commit(B256::repeat_byte(1), ufvk, &input_cmx, &[(A, salt)], &verified(None), 1).is_err());
        assert!(
            spend_commit(B256::repeat_byte(1), ufvk, &input_cmx, &[(A, salt)], &verified(Some(change_cmx)), 0).is_err()
        );
    }

    /// T3-5: a payout from a lineage note keeps every pending claim committed against that note.
    #[test]
    fn a_payout_keeps_the_claims_bound_to_its_input() {
        check_bound_claims(&[A, B], &[A]).unwrap();
        check_bound_claims(&[A], &[]).unwrap();
        assert!(check_bound_claims(&[B], &[A]).is_err());
    }

    fn signed(claims: &[[u8; 32]], txid: [u8; 32]) -> Inflight {
        Inflight {
            claim_ids: claims.to_vec(),
            inputs: vec![INPUT],
            txid: Some(txid),
            expiry_height: 1_040,
            change: 400_000,
            spend_commit: None,
        }
    }

    /// H-1 (security review 2026-10-11), node half of the reviewer's
    /// `misattributed_confirmation_double_pays_when_amounts_match`: with claim A set aside the
    /// coordinator gets S1 (pays B, TX1) signed, then asks for S2 (pays A) from the same inputs,
    /// broadcasts TX1 and asks to confirm A with it.
    #[test]
    fn a_second_payout_from_the_same_notes_is_refused_while_the_first_can_be_mined() {
        let s1 = signed(&[B], TX1);
        // Round 1 of S2: refused while TX1 can still be mined.
        assert!(check_no_live_payout(Some(&s1), 1_000, &[INPUT], &TX2).is_err());
        assert!(check_no_live_payout(Some(&s1), 1_039, &[INPUT], &TX2).is_err());
        // Allowed once TX1 expired unmined, or once its inputs are spent (it, or nothing, was mined).
        assert_eq!(check_no_live_payout(Some(&s1), 1_040, &[INPUT], &TX2).unwrap(), Some(TX1));
        assert_eq!(check_no_live_payout(Some(&s1), 1_000, &[], &TX2).unwrap(), Some(TX1));
        assert_eq!(check_no_live_payout(None, 1_000, &[INPUT], &TX2).unwrap(), None);
    }

    /// A signing retry (round 2 failed on another node) commits to the same transaction again.
    #[test]
    fn the_same_payout_can_be_signed_again_while_it_is_in_flight() {
        let s1 = signed(&[B], TX1);
        assert_eq!(check_no_live_payout(Some(&s1), 1_000, &[INPUT], &TX1).unwrap(), Some(TX1));
        check_release(Some(&s1), Some(TX1), TX1).unwrap();
    }

    #[test]
    fn two_sessions_committed_in_parallel_cannot_both_be_signed() {
        // Both sessions passed round 1 while nothing was in flight.
        check_release(None, None, TX1).unwrap();
        let s1 = signed(&[B], TX1);
        // S1's shares were released: S2 (another transaction) must not be.
        assert!(check_release(Some(&s1), None, TX2).is_err());
        // The same transaction through another session is fine.
        check_release(Some(&s1), None, TX1).unwrap();
        // After TX1 expired, a session started then (prior = TX1) replaces it.
        check_release(Some(&s1), Some(TX1), TX2).unwrap();
    }

    #[test]
    fn only_the_signed_transaction_confirms_its_own_claims() {
        let s2 = signed(&[A], TX2);
        // TX1 (paying B) was mined: it confirms neither A ...
        assert!(check_confirmation(&s2, &[A], &TX1).is_err());
        // ... nor B, which this node's in-flight payout does not pay.
        assert!(check_confirmation(&s2, &[B], &TX1).is_err());
        check_confirmation(&s2, &[A], &TX2).unwrap();
    }

    #[test]
    fn server_supplied_tips_cannot_overflow() {
        assert!(expectations(u32::MAX).is_err());
        let e = expectations(u32::MAX - 1).unwrap();
        assert_eq!(e.min_expiry, u32::MAX);
        assert_eq!(e.max_expiry, u32::MAX);
        let e = expectations(4_486_000).unwrap();
        assert_eq!(e.max_expiry, 4_486_001 + PAYOUT_EXPIRY_DELTA + TIP_SKEW);
    }
}
