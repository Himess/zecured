//! Policy P1: deterministic classification of the notes a vault receives.
//!
//! Every signer node runs this on its own scan of the chain and must reach the same answer:
//! the order is the chain order (height, transaction index in the block, action index in
//! the transaction's Ironwood bundle), the deadline is a block height, and nothing depends
//! on wall-clock time.
//!
//! The **first** note in that order that meets every rule is *conforming*: the deposit a proof
//! activates the vault with. Every other externally received note is *non-conforming*: it leaves
//! the vault as a Refund claim once a proof lands it on Base (T-02), or, only if no guest of the
//! deployment can land it, through an off-chain P5 refund ([`Landing`], I30). Classification
//! stops at the first note that does not have enough confirmations yet, so a decision, once made,
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
    /// The full transaction is a deposit transaction: v6 of the NU7 epoch, value moving only
    /// inside the Ironwood pool (no transparent, Sapling or Orchard-pool parts), outputs
    /// enabled. Checked on the full transaction with the deposit statement's own function
    /// (K2-c), so that P1 and a Tier 2 proof agree.
    pub ironwood_only: bool,
    /// The note pays exactly the vault's published receiver, not another diversified address
    /// of the vault (K2-c: the deposit statement accepts nothing else).
    pub to_vault_receiver: bool,
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
    /// The note pays another diversified address of the vault, not its published one.
    WrongReceiver,
}

/// The classification of one deposit note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Classification {
    Conforming,
    NonConforming(NonConformingReason),
    /// Not enough confirmations yet (this note and every later one).
    Pending,
}

/// I30: what the deployment's deposit guests can land on Base, as the Intake says (its journal
/// version and `minRefundZat`). A note a guest can land becomes a lineage note once someone proves
/// it, and an off-chain refund of it would then look exactly like a theft (F1): such a note leaves
/// the vault only as a Refund claim. Only a note no guest can land may be refunded off-chain (P5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Landing {
    /// Journal v1: Activate mode only. The deposit rule D5, the vault's amount, mined at or
    /// below its expiry height, to its published receiver; it lands as collateral, or as a
    /// refund once the vault cannot take it (a second payment, T-02).
    ActivateOnly,
    /// Journal v2: Refund mode too, any note to the vault's published receiver of at least
    /// `min_refund` zatoshi, at any height, in any transaction with an Ironwood bundle (T-09).
    WithRefunds { min_refund: u64 },
}

impl Landing {
    /// Whether a guest of the deployment can land `note` on Base, as collateral or as a refund.
    /// In doubt it can: the notes this says no to are those no mode proves at all (another
    /// address of the vault, K2-c), or that Activate mode refuses under v1 and that v2 would take
    /// only above its floor.
    pub fn can_land(&self, note: &DepositNote, rules: &DepositRules) -> bool {
        if !note.to_vault_receiver {
            return false;
        }
        let activates = note.ironwood_only && note.value == rules.amount && note.position.height <= rules.expiry_height;
        match *self {
            Landing::ActivateOnly => activates,
            Landing::WithRefunds { min_refund } => activates || note.value >= min_refund,
        }
    }
}

/// Number of confirmations of a note mined at `height` when the chain tip is `tip`.
///
/// Both come from the light wallet server, so the arithmetic never panics (I-4).
pub fn confirmations(height: u32, tip: u32) -> u32 {
    tip.checked_sub(height).map_or(0, |d| d.saturating_add(1))
}

/// Classifies `notes` (any order) and returns them in P1 order with their classification.
pub fn classify(rules: &DepositRules, tip: u32, notes: &[DepositNote]) -> Vec<(DepositNote, Classification)> {
    let mut ordered: Vec<DepositNote> = notes.to_vec();
    // The whole note breaks ties: positions are unique on a real chain, but a lying data source
    // must not make the outcome depend on the order it listed the notes in.
    ordered.sort_by_key(|n| (n.position, n.value, n.ironwood_only, n.to_vault_receiver));

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
        } else if !note.to_vault_receiver {
            Classification::NonConforming(NonConformingReason::WrongReceiver)
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
            to_vault_receiver: true,
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
        assert_eq!(classes(2_000, &n), vec![Classification::NonConforming(NonConformingReason::Late)]);
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
    fn server_supplied_extremes_do_not_overflow() {
        assert_eq!(confirmations(0, u32::MAX), u32::MAX);
        assert_eq!(confirmations(u32::MAX, 0), 0);
        let n = [note(u32::MAX - 10, u64::MAX, u32::MAX, AMOUNT, true)];
        assert_eq!(classes(u32::MAX, &n), vec![Classification::NonConforming(NonConformingReason::Late)]);
        assert_eq!(classes(u32::MAX - 10, &n), vec![Classification::Pending]);
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

    /// K2-c: a note to another diversified address of the vault is never conforming (the
    /// deposit statement could not prove it), and the next note can be.
    #[test]
    fn a_note_to_another_vault_address_is_non_conforming() {
        let elsewhere = DepositNote {
            to_vault_receiver: false,
            ..note(900, 0, 0, AMOUNT, true)
        };
        let n = [elsewhere, note(905, 0, 0, AMOUNT, true)];
        assert_eq!(
            classes(1_000, &n),
            vec![
                Classification::NonConforming(NonConformingReason::WrongReceiver),
                Classification::Conforming
            ]
        );
    }

    /// RQ-10: the outcome does not depend on the order a data source lists the notes in, even
    /// with a (forged) duplicate position.
    /// I30: under v1 only a deposit Activate mode proves can land (a second payment of the
    /// vault's amount lands as a refund); under v2 every note to the published receiver from the
    /// Intake's floor on. A note to another address of the vault never lands.
    #[test]
    fn what_a_deployment_can_land() {
        let v1 = Landing::ActivateOnly;
        let v2 = Landing::WithRefunds { min_refund: 20_000 };
        let good = note(900, 0, 0, AMOUNT, true);
        for landing in [v1, v2] {
            assert!(landing.can_land(&good, &RULES), "the deposit itself, or a second payment of it");
            let elsewhere = DepositNote {
                to_vault_receiver: false,
                ..good.clone()
            };
            assert!(!landing.can_land(&elsewhere, &RULES), "another address of the vault");
        }
        let late = note(1_001, 0, 0, AMOUNT, true);
        let other_amount = note(900, 0, 0, AMOUNT + 1, true);
        let crossing = note(900, 0, 0, AMOUNT, false);
        let tiny = note(900, 0, 0, 19_999, true);
        for n in [&late, &other_amount, &crossing, &tiny] {
            assert!(!v1.can_land(n, &RULES), "{n:?} under v1");
        }
        for n in [&late, &other_amount, &crossing] {
            assert!(v2.can_land(n, &RULES), "{n:?} under v2");
        }
        assert!(!v2.can_land(&tiny, &RULES), "below the Intake's refund floor");
    }

    #[test]
    fn duplicate_positions_are_classified_the_same_in_any_input_order() {
        let a = note(900, 0, 0, AMOUNT, true);
        let b = note(900, 0, 0, AMOUNT - 1, true);
        let one = classify(&RULES, 1_000, &[a.clone(), b.clone()]);
        let other = classify(&RULES, 1_000, &[b, a]);
        assert_eq!(one, other);
    }
}
