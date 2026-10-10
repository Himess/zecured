//! Policy P1: deterministic classification of the notes a vault receives.
//!
//! Every signer node runs this on its own scan of the chain and must reach the same answer:
//! the order is the chain order (height, transaction index in the block, action index in
//! the transaction's Ironwood bundle), the deadline is a block height, and nothing depends
//! on wall-clock time.
//!
//! The **first** note in that order that meets every rule is *conforming*; it is the only
//! note that can ever be attested (minted against). Every other externally received note is
//! *non-conforming* and can only leave the vault through a P5 refund. Classification stops
//! at the first note that does not have enough confirmations yet, so a decision, once made,
//! never changes (short of a reorg deeper than the confirmation depth).

use serde::{Deserialize, Serialize};

/// Chain position of a note; ordering is the P1 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NotePosition {
    pub height: u32,
    pub tx_index: u64,
    pub action_index: u32,
}

/// A note received at the vault's **external** (published) address.
///
/// Notes at the vault's internal address are change from the vault's own payouts and are
/// never classified (only the vault can address its internal receiver).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositNote {
    pub position: NotePosition,
    pub value: u64,
    /// The full transaction moves value only inside the Ironwood pool (no transparent,
    /// Sapling or Orchard-pool parts), as checked on the full transaction.
    pub ironwood_only: bool,
}

/// The vault parameters fixed when it was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositRules {
    /// The exact denomination a conforming deposit must have, in zatoshis.
    pub amount: u64,
    /// The last block height at which a conforming deposit may be mined.
    pub expiry_height: u32,
    /// Confirmations required before a note is classified (1 = mined in the tip block).
    pub confirmations: u32,
}

/// Why a note is non-conforming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonConformingReason {
    /// The vault already has its conforming note.
    SecondPayment,
    /// The value is not exactly the vault's denomination.
    WrongAmount,
    /// Mined after the vault's expiry height.
    Late,
    /// The deposit transaction touches the transparent, Sapling or Orchard pool.
    PoolCrossing,
}

/// The classification of one deposit note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Classification {
    Conforming,
    NonConforming(NonConformingReason),
    /// Not enough confirmations yet (this note and every later one).
    Pending,
}

/// Number of confirmations of a note mined at `height` when the chain tip is `tip`.
pub fn confirmations(height: u32, tip: u32) -> u32 {
    tip.checked_sub(height).map_or(0, |d| d + 1)
}

/// Classifies `notes` (any order) and returns them in P1 order with their classification.
pub fn classify(rules: &DepositRules, tip: u32, notes: &[DepositNote]) -> Vec<(DepositNote, Classification)> {
    let mut ordered: Vec<DepositNote> = notes.to_vec();
    ordered.sort_by_key(|n| n.position);

    let mut have_conforming = false;
    let mut pending = false;
    let mut out = Vec::with_capacity(ordered.len());
    for note in ordered {
        if pending || confirmations(note.position.height, tip) < rules.confirmations {
            // Later notes can never be decided before this one.
            pending = true;
            out.push((note, Classification::Pending));
            continue;
        }
        let class = if have_conforming {
            Classification::NonConforming(NonConformingReason::SecondPayment)
        } else if !note.ironwood_only {
            Classification::NonConforming(NonConformingReason::PoolCrossing)
        } else if note.value != rules.amount {
            Classification::NonConforming(NonConformingReason::WrongAmount)
        } else if note.position.height > rules.expiry_height {
            Classification::NonConforming(NonConformingReason::Late)
        } else {
            have_conforming = true;
            Classification::Conforming
        };
        out.push((note, class));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMOUNT: u64 = 1_000_000;
    const RULES: DepositRules = DepositRules {
        amount: AMOUNT,
        expiry_height: 1_000,
        confirmations: 3,
    };

    fn note(height: u32, tx_index: u64, action_index: u32, value: u64, ironwood_only: bool) -> DepositNote {
        DepositNote {
            position: NotePosition {
                height,
                tx_index,
                action_index,
            },
            value,
            ironwood_only,
        }
    }

    fn classes(tip: u32, notes: &[DepositNote]) -> Vec<Classification> {
        classify(&RULES, tip, notes).into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn single_good_deposit_needs_confirmations() {
        let n = [note(900, 2, 0, AMOUNT, true)];
        assert_eq!(classes(901, &n), vec![Classification::Pending]);
        assert_eq!(classes(902, &n), vec![Classification::Conforming]);
    }

    #[test]
    fn first_conforming_wins_regardless_of_input_order() {
        let a = note(900, 1, 1, AMOUNT, true);
        let b = note(900, 1, 0, AMOUNT, true); // same tx, earlier action
        let c = note(899, 5, 0, AMOUNT, true); // earlier block
        let got = classify(&RULES, 950, &[a.clone(), b.clone(), c.clone()]);
        assert_eq!(got[0], (c, Classification::Conforming));
        assert_eq!(got[1], (b, Classification::NonConforming(NonConformingReason::SecondPayment)));
        assert_eq!(got[2], (a, Classification::NonConforming(NonConformingReason::SecondPayment)));
    }

    #[test]
    fn bad_notes_do_not_block_a_later_good_one() {
        let n = [
            note(900, 0, 0, AMOUNT - 1, true),
            note(901, 0, 0, AMOUNT, false),
            note(902, 0, 0, AMOUNT, true),
            note(1_001, 0, 0, AMOUNT, true),
        ];
        assert_eq!(
            classes(2_000, &n),
            vec![
                Classification::NonConforming(NonConformingReason::WrongAmount),
                Classification::NonConforming(NonConformingReason::PoolCrossing),
                Classification::Conforming,
                Classification::NonConforming(NonConformingReason::SecondPayment),
            ]
        );
    }

    #[test]
    fn late_deposit_is_non_conforming() {
        let n = [note(1_001, 0, 0, AMOUNT, true)];
        assert_eq!(
            classes(2_000, &n),
            vec![Classification::NonConforming(NonConformingReason::Late)]
        );
    }

    #[test]
    fn deposit_at_expiry_height_conforms() {
        let n = [note(1_000, 0, 0, AMOUNT, true)];
        assert_eq!(classes(2_000, &n), vec![Classification::Conforming]);
    }

    #[test]
    fn pending_note_blocks_later_classification() {
        // The first note is too recent; the second (even later) must wait too.
        let n = [note(998, 0, 0, AMOUNT - 5, true), note(999, 0, 0, AMOUNT, true)];
        assert_eq!(classes(999, &n), vec![Classification::Pending, Classification::Pending]);
    }

    #[test]
    fn decisions_are_stable_as_the_tip_advances() {
        let n = [
            note(900, 0, 0, AMOUNT, true),
            note(905, 0, 0, AMOUNT, true),
            note(910, 0, 0, 7, true),
        ];
        let mut previous: Vec<Classification> = classes(900, &n);
        for tip in 901..930 {
            let now = classes(tip, &n);
            for (before, after) in previous.iter().zip(&now) {
                if *before != Classification::Pending {
                    assert_eq!(before, after, "classification changed at tip {tip}");
                }
            }
            previous = now;
        }
    }

    #[test]
    fn a_note_is_never_both_conforming_and_refundable() {
        // P1/P5 disjointness: exactly one conforming note, every other decided note is
        // non-conforming, so no note can be minted against and refunded.
        let n: Vec<_> = (0..20).map(|i| note(800 + i, 0, 0, AMOUNT, i % 3 != 0)).collect();
        let got = classes(5_000, &n);
        assert_eq!(got.iter().filter(|c| **c == Classification::Conforming).count(), 1);
        assert!(got.iter().all(|c| *c != Classification::Pending));
    }
}
