//! Vault accounting and payout planning (policies P2, P3, P5, P6).
//!
//! A vault's ZEC is split into two lineages:
//! - the **conforming lineage**: the conforming deposit note and the change notes of the
//!   vault's own payouts. Its value always equals `outstanding + unpaid claims` (I10).
//! - **non-conforming notes**: everything else received at the external address; each can
//!   only leave through a P5 refund to its depositor.
//!
//! Every Base event that burns zZEC creates claims (one per recipient). Claims are paid in
//! Base order, one payout transaction at a time, each paying its own ZIP-317 fee out of
//! the recipient's output. A payout spends the whole conforming lineage and returns the
//! rest (the still-unpaid claims plus `outstanding`) to the vault's internal address, so the
//! zZEC on Base and the ZEC in the vault never diverge.

use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use zecured_zcash_ops::spend::{OutputPrivacy, zip317_fee};

/// Identifier of a claim on Base (`keccak256(abi.encode(vaultId, sequence))`).
pub type ClaimId = [u8; 32];
/// `keccak256(salt32 || receiver43)`.
pub type Commitment = [u8; 32];

/// Seconds a close-from-collateral payout waits for the market maker's recipient so both
/// outputs can share one transaction (docs/ARCHITECTURE.md, payout queue).
pub const MARKET_MAKER_GRACE_SECS: u64 = 120;

/// What created a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClaimKind {
    /// `ReleaseRequested`: the borrower takes the remaining collateral (P2).
    Release,
    /// `LiquidationRelease`: the liquidator receives the seized ZEC (P3).
    Liquidation,
    /// `CloseRelease`, borrower's part (P6).
    CloseUser,
    /// `CloseRelease`, market maker's part (P6).
    CloseMarketMaker,
}

impl ClaimKind {
    /// Third parties' outputs are encrypted without an outgoing viewing key, so the vault
    /// UFVK given to the borrower reveals nothing about them.
    pub fn privacy(self) -> OutputPrivacy {
        match self {
            ClaimKind::Release | ClaimKind::CloseUser => OutputPrivacy::VaultOvk,
            ClaimKind::Liquidation | ClaimKind::CloseMarketMaker => OutputPrivacy::NoOvk,
        }
    }
}

/// Payment state of a claim, as seen by a signer node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimState {
    /// Not paid on Zcash yet.
    Unpaid,
    /// Paid by a mined Zcash transaction (it may still await `confirmPayout` on Base).
    Paid,
}

/// One claim, in the order its Base event was emitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub kind: ClaimKind,
    pub amount: u64,
    pub commitment: Commitment,
    /// Base `(block number, log index)` of the emitting event; the two claims of one
    /// `CloseRelease` share it.
    pub order: (u64, u64),
    /// Base block timestamp of the emitting event.
    pub created_at: u64,
    /// For close claims: the other claim of the same `CloseRelease`.
    pub partner: Option<ClaimId>,
    pub state: ClaimState,
}

/// An off-chain recipient revealed to the signers: raw Orchard-protocol receiver + salt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    #[serde(with = "hex43")]
    pub receiver: [u8; 43],
    pub salt: [u8; 32],
}

impl Recipient {
    /// The commitment that appears on Base for this recipient.
    pub fn commitment(&self) -> Commitment {
        commitment(&self.salt, &self.receiver)
    }
}

/// `keccak256(salt32 || receiver43)`.
pub fn commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> Commitment {
    let mut h = Keccak256::new();
    h.update(salt);
    h.update(receiver);
    h.finalize().into()
}

/// One output of a planned payout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedPayment {
    pub claim: ClaimId,
    pub receiver: [u8; 43],
    /// Value received, after the fee this payment bears.
    pub value: u64,
    pub privacy: OutputPrivacy,
}

/// A payout transaction every signer can reproduce and check a PCZT against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutPlan {
    /// Claims this transaction settles.
    pub claims: Vec<ClaimId>,
    pub payments: Vec<PlannedPayment>,
    /// Number of conforming-lineage notes spent (all of them).
    pub inputs: usize,
    /// Value returned to the vault's internal address.
    pub change: u64,
    pub fee: u64,
}

/// What to do next for a vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextPayout {
    Pay(PayoutPlan),
    /// A close payout is waiting for the market maker's recipient until this Unix time.
    WaitUntil(u64),
    Nothing,
}

/// Errors that mean the vault's books do not add up; signers refuse to sign.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("claims to pay ({claims}) exceed the conforming lineage ({lineage})")]
    Overdrawn { claims: u64, lineage: u64 },
    #[error("no conforming-lineage notes to spend")]
    NoInputs,
    #[error("I10 violated: lineage {lineage} != outstanding {outstanding} + unpaid claims {unpaid}")]
    InvariantI10 {
        lineage: u64,
        outstanding: u64,
        unpaid: u64,
    },
    #[error("refund of {value} zat does not cover the fee {fee}")]
    RefundIsDust { value: u64, fee: u64 },
}

/// I10: the conforming lineage equals `outstanding` plus every claim not yet paid on
/// Zcash (dust and set-aside claims included).
pub fn check_i10(lineage_value: u64, outstanding: u64, claims: &[Claim]) -> Result<(), LedgerError> {
    let unpaid: u64 = claims
        .iter()
        .filter(|c| c.state == ClaimState::Unpaid)
        .map(|c| c.amount)
        .sum();
    if lineage_value == outstanding + unpaid {
        Ok(())
    } else {
        Err(LedgerError::InvariantI10 {
            lineage: lineage_value,
            outstanding,
            unpaid,
        })
    }
}

/// Decides the next payout for a vault.
///
/// `lineage_value` / `lineage_notes`: the unspent conforming-lineage notes (all are spent).
/// `recipient(commitment)`: the recipient revealed off-chain for a commitment, if any.
/// `now`: Unix time, used only for the market maker grace period.
pub fn next_payout(
    claims: &[Claim],
    recipient: impl Fn(&Commitment) -> Option<Recipient>,
    lineage_value: u64,
    lineage_notes: usize,
    now: u64,
) -> Result<NextPayout, LedgerError> {
    let mut unpaid: Vec<&Claim> = claims.iter().filter(|c| c.state == ClaimState::Unpaid).collect();
    unpaid.sort_by_key(|c| (c.order, c.kind == ClaimKind::CloseMarketMaker));
    let by_id = |id: &ClaimId| unpaid.iter().copied().find(|c| &c.id == id);

    for claim in &unpaid {
        // A market maker claim whose borrower claim is still unpaid is handled together
        // with it; once the borrower is paid it is an ordinary claim.
        if claim.kind == ClaimKind::CloseMarketMaker
            && claim.partner.as_ref().and_then(|p| by_id(p)).is_some()
        {
            continue;
        }
        // Without a recipient, a claim is set aside: its value stays in the vault and the
        // queue moves on (docs/ARCHITECTURE.md, payout queue).
        let Some(rcpt) = recipient(&claim.commitment) else {
            continue;
        };

        let mut group: Vec<(&Claim, Recipient)> = vec![(claim, rcpt)];
        if claim.kind == ClaimKind::CloseUser {
            if let Some(mm) = claim.partner.as_ref().and_then(|p| by_id(p)) {
                match recipient(&mm.commitment) {
                    Some(mm_rcpt) => group.push((mm, mm_rcpt)),
                    None if now < claim.created_at + MARKET_MAKER_GRACE_SECS => {
                        return Ok(NextPayout::WaitUntil(claim.created_at + MARKET_MAKER_GRACE_SECS));
                    }
                    None => {} // grace over: pay the borrower alone
                }
            }
        }

        let plan = plan(&group, lineage_value, lineage_notes)?;
        match plan {
            Some(plan) => return Ok(NextPayout::Pay(plan)),
            // Dust: the claim cannot pay its own fee; it stays in the vault.
            None => continue,
        }
    }
    Ok(NextPayout::Nothing)
}

/// Builds the plan paying `group` (one claim, or a close pair with the borrower first).
/// Returns `None` if the fee-bearing claim is dust.
fn plan(
    group: &[(&Claim, Recipient)],
    lineage_value: u64,
    lineage_notes: usize,
) -> Result<Option<PayoutPlan>, LedgerError> {
    if lineage_notes == 0 {
        return Err(LedgerError::NoInputs);
    }
    let claimed: u64 = group.iter().map(|(c, _)| c.amount).sum();
    if claimed > lineage_value {
        return Err(LedgerError::Overdrawn {
            claims: claimed,
            lineage: lineage_value,
        });
    }
    let change = lineage_value - claimed;
    let outputs = group.len() + usize::from(change > 0);
    let fee = zip317_fee(lineage_notes, outputs);

    // The first claim of the group bears the fee (the borrower in a close pair).
    let mut payments = Vec::with_capacity(group.len());
    for (i, (claim, rcpt)) in group.iter().enumerate() {
        let value = if i == 0 {
            match claim.amount.checked_sub(fee) {
                Some(v) if v > 0 => v,
                _ => return Ok(None),
            }
        } else {
            claim.amount
        };
        payments.push(PlannedPayment {
            claim: claim.id,
            receiver: rcpt.receiver,
            value,
            privacy: claim.kind.privacy(),
        });
    }
    Ok(Some(PayoutPlan {
        claims: group.iter().map(|(c, _)| c.id).collect(),
        payments,
        inputs: lineage_notes,
        change,
        fee,
    }))
}

/// P5: refund one non-conforming note to its depositor, net of the fee; no change.
pub fn refund_plan(note_value: u64, receiver: [u8; 43]) -> Result<PayoutPlan, LedgerError> {
    let fee = zip317_fee(1, 1);
    let value = note_value
        .checked_sub(fee)
        .filter(|v| *v > 0)
        .ok_or(LedgerError::RefundIsDust {
            value: note_value,
            fee,
        })?;
    Ok(PayoutPlan {
        claims: vec![],
        payments: vec![PlannedPayment {
            claim: [0; 32],
            receiver,
            value,
            privacy: OutputPrivacy::VaultOvk,
        }],
        inputs: 1,
        change: 0,
        fee,
    })
}

mod hex43 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 43], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 43], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s)
            .map_err(serde::de::Error::custom)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("receiver must be 43 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DEPOSIT: u64 = 5_000_000;
    const FEE_1_IN_1_OUT: u64 = 10_000;
    const FEE_1_IN_2_OUT: u64 = 10_000;
    const FEE_1_IN_3_OUT: u64 = 15_000;

    fn rcpt(tag: u8) -> Recipient {
        Recipient {
            receiver: [tag; 43],
            salt: [tag.wrapping_add(100); 32],
        }
    }

    fn claim(id: u8, kind: ClaimKind, amount: u64, order: (u64, u64), partner: Option<u8>) -> Claim {
        Claim {
            id: [id; 32],
            kind,
            amount,
            commitment: rcpt(id).commitment(),
            order,
            created_at: 1_000,
            partner: partner.map(|p| [p; 32]),
            state: ClaimState::Unpaid,
        }
    }

    fn known(ids: &[u8]) -> impl Fn(&Commitment) -> Option<Recipient> {
        let map: HashMap<Commitment, Recipient> =
            ids.iter().map(|&i| (rcpt(i).commitment(), rcpt(i))).collect();
        move |c| map.get(c).copied()
    }

    #[test]
    fn fees_match_zip317() {
        assert_eq!(zip317_fee(1, 1), FEE_1_IN_1_OUT);
        assert_eq!(zip317_fee(1, 2), FEE_1_IN_2_OUT);
        assert_eq!(zip317_fee(1, 3), FEE_1_IN_3_OUT);
    }

    #[test]
    fn full_release_pays_borrower_minus_fee_without_change() {
        let c = claim(1, ClaimKind::Release, DEPOSIT, (10, 0), None);
        let got = next_payout(&[c.clone()], known(&[1]), DEPOSIT, 1, 2_000).unwrap();
        let NextPayout::Pay(p) = got else { panic!("{got:?}") };
        assert_eq!(p.change, 0);
        assert_eq!(p.fee, FEE_1_IN_1_OUT);
        assert_eq!(p.payments.len(), 1);
        assert_eq!(p.payments[0].value, DEPOSIT - FEE_1_IN_1_OUT);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::VaultOvk);
    }

    #[test]
    fn partial_liquidation_keeps_the_rest_in_the_vault() {
        // ~70% seized at 62.5% LLTV; the rest stays outstanding.
        let seized = 3_520_000;
        let c = claim(2, ClaimKind::Liquidation, seized, (10, 1), None);
        let NextPayout::Pay(p) = next_payout(&[c.clone()], known(&[2]), DEPOSIT, 1, 2_000).unwrap() else {
            panic!()
        };
        assert_eq!(p.payments[0].value, seized - FEE_1_IN_2_OUT);
        assert_eq!(p.payments[0].privacy, OutputPrivacy::NoOvk);
        assert_eq!(p.change, DEPOSIT - seized);
        // I10 after the payout: lineage = change, outstanding = DEPOSIT - seized, no unpaid.
        let mut paid = c;
        paid.state = ClaimState::Paid;
        check_i10(p.change, DEPOSIT - seized, &[paid]).unwrap();
    }

    #[test]
    fn close_pays_both_in_one_tx_and_borrower_bears_the_fee() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let NextPayout::Pay(p) =
            next_payout(&[mm.clone(), user.clone()], known(&[3, 4]), DEPOSIT, 1, 1_010).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id, mm.id]);
        assert_eq!(p.change, 0);
        assert_eq!(p.fee, FEE_1_IN_2_OUT);
        assert_eq!(p.payments[0].value, 1_200_000 - FEE_1_IN_2_OUT);
        assert_eq!(p.payments[1].value, 3_800_000);
        assert_eq!(p.payments[1].privacy, OutputPrivacy::NoOvk);
    }

    #[test]
    fn close_waits_for_market_maker_then_pays_borrower_alone() {
        let user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let claims = [user.clone(), mm.clone()];
        assert_eq!(
            next_payout(&claims, known(&[3]), DEPOSIT, 1, 1_050).unwrap(),
            NextPayout::WaitUntil(1_000 + MARKET_MAKER_GRACE_SECS)
        );
        let NextPayout::Pay(p) = next_payout(&claims, known(&[3]), DEPOSIT, 1, 1_200).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![user.id]);
        // The market maker's share stays in the vault as change.
        assert_eq!(p.change, 3_800_000);
        assert_eq!(p.fee, FEE_1_IN_2_OUT);
    }

    #[test]
    fn late_market_maker_is_paid_alone_and_bears_its_fee() {
        let mut user = claim(3, ClaimKind::CloseUser, 1_200_000, (11, 0), Some(4));
        user.state = ClaimState::Paid;
        let mm = claim(4, ClaimKind::CloseMarketMaker, 3_800_000, (11, 0), Some(3));
        let NextPayout::Pay(p) = next_payout(&[user, mm.clone()], known(&[4]), 3_800_000, 1, 5_000).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![mm.id]);
        assert_eq!(p.payments[0].value, 3_800_000 - FEE_1_IN_1_OUT);
        assert_eq!(p.change, 0);
    }

    #[test]
    fn claim_without_recipient_does_not_block_the_queue() {
        let a = claim(5, ClaimKind::Liquidation, 1_000_000, (12, 0), None);
        let b = claim(6, ClaimKind::Liquidation, 1_500_000, (13, 0), None);
        let NextPayout::Pay(p) = next_payout(&[a.clone(), b.clone()], known(&[6]), DEPOSIT, 1, 2_000).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![b.id]);
        // a's value stays in the vault: change = deposit - b.
        assert_eq!(p.change, DEPOSIT - 1_500_000);
    }

    #[test]
    fn dust_claim_stays_in_the_vault() {
        let dust = claim(7, ClaimKind::Liquidation, FEE_1_IN_2_OUT, (12, 0), None);
        let ok = claim(8, ClaimKind::Liquidation, 2_000_000, (13, 0), None);
        let NextPayout::Pay(p) = next_payout(&[dust, ok.clone()], known(&[7, 8]), DEPOSIT, 1, 2_000).unwrap() else {
            panic!()
        };
        assert_eq!(p.claims, vec![ok.id]);
        assert_eq!(p.change, DEPOSIT - 2_000_000);
    }

    #[test]
    fn overdrawn_ledger_is_refused() {
        let c = claim(9, ClaimKind::Release, DEPOSIT + 1, (10, 0), None);
        assert_eq!(
            next_payout(&[c], known(&[9]), DEPOSIT, 1, 2_000),
            Err(LedgerError::Overdrawn {
                claims: DEPOSIT + 1,
                lineage: DEPOSIT
            })
        );
    }

    #[test]
    fn i10_detects_mismatch() {
        let c = claim(10, ClaimKind::Liquidation, 1_000_000, (10, 0), None);
        check_i10(DEPOSIT, DEPOSIT - 1_000_000, &[c.clone()]).unwrap();
        assert!(check_i10(DEPOSIT - 1, DEPOSIT - 1_000_000, &[c]).is_err());
    }

    #[test]
    fn refund_is_net_of_fee_and_rejects_dust() {
        let p = refund_plan(1_234_567, [1; 43]).unwrap();
        assert_eq!(p.payments[0].value, 1_234_567 - FEE_1_IN_1_OUT);
        assert_eq!(p.change, 0);
        assert!(refund_plan(FEE_1_IN_1_OUT, [1; 43]).is_err());
    }

    #[test]
    fn commitment_is_keccak_of_salt_then_receiver() {
        let r = Recipient {
            receiver: [0xab; 43],
            salt: [0x01; 32],
        };
        let mut preimage = vec![0x01; 32];
        preimage.extend([0xab; 43]);
        assert_eq!(r.commitment(), <[u8; 32]>::from(Keccak256::digest(&preimage)));
    }
}
