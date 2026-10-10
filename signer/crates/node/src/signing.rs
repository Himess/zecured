//! Signing sessions: the policy decision and the two FROST rounds for one payout.
//!
//! Round 1 (`commit`): the node recomputes the plan from its own view of both chains,
//! verifies the coordinator's PCZT against it (`zecured_policy::verify`) and returns one
//! FROST commitment per vault spend. Round 2 (`share`): the node signs the sighash and
//! randomizers **it computed itself** from the PCZT it verified; the coordinator only
//! supplies the other signers' commitments.

use std::collections::BTreeMap;

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow, bail, ensure};
use pczt::Pczt;
use reddsa::frost::redpallas::{
    keys::KeyPackage,
    round1::{SigningCommitments, SigningNonces},
};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zecured_policy::{
    ledger::{self, NextPayout, PayoutPlan},
    verify::{ExpectedInput, TxExpectations, Verified, verify_payout},
};
use zecured_threshold::{self as threshold, signer_id};
use zecured_zcash_ops::{scan::ReceivedNote, spend};

use crate::{
    chain::{VaultNotes, vault_fvk},
    store::{Store, VaultRecord},
};

/// Blocks after the current tip a payout transaction may expire (builder default is 20).
const MAX_EXPIRY_AHEAD: u32 = 40;

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareRequest {
    pub session_id: String,
    /// Per vault spend (same order as the commitments): the chosen signers' commitments.
    pub commitments: Vec<BTreeMap<u16, serde_json::Value>>,
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
    nonces: Vec<SigningNonces>,
    commitments: Vec<SigningCommitments>,
    /// Nullifiers of the vault notes this payout spends (to confirm it later).
    pub inputs: Vec<[u8; 32]>,
}

/// The plan and inputs this node expects for `kind`, from its own view.
pub fn expected_plan(
    store: &Store,
    vault: &VaultRecord,
    notes: &VaultNotes,
    kind: &SignKind,
    now: u64,
) -> Result<(PayoutPlan, Vec<ReceivedNote>)> {
    match kind {
        SignKind::Payout { claim_ids } => {
            ensure!(vault.activated, "vault is not active on Base");
            ledger::check_i10(notes.lineage_value(), vault.outstanding, &vault.claims)
                .map_err(|e| anyhow!("refusing to sign: {e}"))?;
            let next = ledger::next_payout(
                &vault.claims,
                |c| store.recipient(c),
                notes.lineage_value(),
                notes.lineage.len(),
                now,
            )?;
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
            let plan = ledger::refund_plan(note.value(), request.recipient.receiver)?;
            Ok((plan, vec![note.clone()]))
        }
    }
}

/// Round 1: verify and commit.
pub fn commit(
    vault: &VaultRecord,
    plan: &PayoutPlan,
    inputs: &[ReceivedNote],
    tip: u32,
    req: &CommitRequest,
) -> Result<(Session, Vec<SigningCommitments>)> {
    let pczt = Pczt::parse(&req.pczt).map_err(|e| anyhow!("parse PCZT: {e:?}"))?;
    let fvk = vault_fvk(vault)?;
    let target = tip + 1;
    let expectations = TxExpectations {
        consensus_branch_id: u32::from(BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(target))),
        min_expiry: target,
        max_expiry: tip + MAX_EXPIRY_AHEAD,
    };
    let expected: Vec<ExpectedInput> = inputs
        .iter()
        .map(|n| ExpectedInput {
            nullifier: n.nullifier.to_bytes(),
            value: n.value(),
        })
        .collect();
    let verified = verify_payout(&pczt, &fvk, &expected, plan, &expectations).map_err(|e| anyhow!("PCZT refused: {e}"))?;
    start_session(vault, req, verified, expected.iter().map(|e| e.nullifier).collect())
}

/// Demo-only "compromised" behaviour: commit to every vault spend in any PCZT, no policy.
pub fn commit_unchecked(vault: &VaultRecord, req: &CommitRequest) -> Result<(Session, Vec<SigningCommitments>)> {
    let pczt = Pczt::parse(&req.pczt).map_err(|e| anyhow!("parse PCZT: {e:?}"))?;
    let (sighash, spends) = spend::signing_requests_all(&pczt)?;
    start_session(vault, req, Verified { sighash, spends }, vec![])
}

fn start_session(
    vault: &VaultRecord,
    req: &CommitRequest,
    verified: Verified,
    inputs: Vec<[u8; 32]>,
) -> Result<(Session, Vec<SigningCommitments>)> {
    let key_package: KeyPackage = serde_json::from_value(vault.key_package.clone())?;
    let mut rng = rand_core_06::OsRng;
    let (nonces, commitments): (Vec<_>, Vec<_>) = verified
        .spends
        .iter()
        .map(|_| threshold::commit(&key_package, &mut rng))
        .unzip();
    Ok((
        Session {
            vault_id: vault.vault_id,
            kind: req.kind.clone(),
            verified,
            nonces,
            commitments: commitments.clone(),
            inputs,
        },
        commitments,
    ))
}

/// Round 2: signature shares over this node's own sighash and randomizers.
pub fn share(index: u16, vault: &VaultRecord, session: Session, req: &ShareRequest) -> Result<Vec<serde_json::Value>> {
    ensure!(
        req.commitments.len() == session.verified.spends.len(),
        "one commitment set per spend expected"
    );
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
        let share = threshold::sign_share(&package, &session.nonces[k], &key_package, &session.verified.spends[k].alpha)?;
        shares.push(serde_json::to_value(share)?);
    }
    Ok(shares)
}
