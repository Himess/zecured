//! Where a payment proof anchors, for both payers (the maker on Buy, the CLI on Sell; docs/SWAP.md
//! "Privacy").
//!
//! The anchor header is public on Base (the journal's `anchor`), the payment's block is not: the
//! statement only shows that the block is one of the anchor's epoch below it. An anchor chosen as
//! "the first header with the order's tier of work above the payment" sits right above the
//! payment's block and gives it away (W-10). So the payer fixes the anchor's height before the
//! payment exists: when the order (on Buy, its destination) reaches it while Zcash block `c` is
//! being mined, `a = c + pay_delay_max_blocks + 1 + r`, with `r` drawn once per order uniformly in
//! `0..=pay_delay_max_blocks` and kept across restarts. The payment follows after its own random
//! delay of 1 to `pay_delay_max_blocks` blocks, so it lands in one of the blocks `c + 1` to
//! `c + pay_delay_max_blocks` unless it waits in the mempool, below `a` whichever it is. A payment
//! mined at or after `a` (late) anchors at `h + 1 + r` instead. The payer proves under the header
//! at exactly `a` once the order's tier of work is on top of it ([`crate::chain::anchor_at`]).
//!
//! The statement needs the payment and its anchor in one epoch (W-20): the anchor never passes the
//! last block of the payment's epoch, a payment mined in that very block cannot be proven, and a
//! payer does not pay while the whole pipeline would not fit before a known activation.

use rand_core_06::{OsRng, RngCore};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zecured_deposit_witness::history;
use zecured_zcash_ops::network::ZcashNetwork;

/// The least `pay_delay_max_blocks` outside a local fork: with 1, the payment would land in the
/// one block after `c`, the only one between the order and its anchor.
pub const MIN_PAY_DELAY_MAX_BLOCKS: u32 = 2;

/// A uniform draw in `0..n` from the operating system's CSPRNG (0 when `n` is 0), without modulo
/// bias.
fn below(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let zone = u32::MAX - u32::MAX % n;
    loop {
        let v = OsRng.next_u32();
        if v < zone {
            return v % n;
        }
    }
}

/// A payer's two random draws for one order, for a `max` of `pay_delay_max_blocks`: the payment's
/// delay, 1 to `max` blocks (1 when `max` is 0, on a local fork), and the anchor's `r`, 0 to `max`.
pub fn draw(max: u32) -> (u32, u32) {
    (1 + below(max), below(max.saturating_add(1)))
}

/// `c`: the height of the Zcash block being mined while the light wallet server's tip is `tip`.
pub fn current_height(tip: u32) -> u32 {
    tip.saturating_add(1)
}

/// The payment's privacy delay as a height: it is broadcast once the tip reaches `c - 1 + delay`,
/// so that its earliest block is `c + delay`.
pub fn pay_after(c: u32, delay: u32) -> u32 {
    c.saturating_sub(1).saturating_add(delay.max(1))
}

/// W-10: the anchor height planned at `c` for a maximum delay of `max` blocks and the draw `r`:
/// `c + max + 1 + r`.
pub fn planned_anchor(c: u32, max: u32, r: u32) -> u32 {
    c.saturating_add(max).saturating_add(1).saturating_add(r)
}

/// Blocks from the tip at which a payment is scheduled to its latest planned anchor:
/// `planned_anchor(tip + 1, max, max) - tip`.
pub fn anchor_window(max: u32) -> u32 {
    max.saturating_mul(2).saturating_add(2)
}

/// W-10, W-20: the anchor height of a payment mined at `h`: the `planned` one when the payment
/// came below it, whatever `h` is; otherwise (a late payment, or none planned: one paid before
/// anchors were planned) `h + 1 + r`. Never past `epoch_last`, the last block of the payment's
/// epoch. `None` when no anchor can prove the payment: it is in that last block.
pub fn anchor_height(planned: Option<u32>, r: u32, h: u32, epoch_last: Option<u32>) -> Option<u32> {
    let a = match planned {
        Some(p) if p > h => p,
        _ => h.saturating_add(1).saturating_add(r),
    };
    let a = epoch_last.map_or(a, |last| a.min(last));
    (a > h).then_some(a)
}

/// The last block of the epoch of block `height` of Zcash `network`: the next network upgrade's
/// activation height less one, or `None` while zcash_protocol knows no later activation.
pub fn epoch_last_height(network: ZcashNetwork, height: u32) -> Option<u32> {
    let params = network.params();
    let branch = BranchId::for_height(&params, BlockHeight::from_u32(height));
    let (_, end) = branch.height_bounds(&params)?;
    end.map(|e| u32::from(e).saturating_sub(1))
}

/// Whether block `height` of Zcash `network` is in an epoch the payment statement proves on.
pub fn provable_epoch(network: ZcashNetwork, height: u32) -> bool {
    history::epoch(network, height).is_ok()
}

/// W-20: whether a payment scheduled at the tip `tip` can be paid, anchored and covered by the
/// order's tier of work within one epoch the statement proves on: `blocks` is that pipeline in
/// blocks (the anchor window and the tier's work), `epoch_last` the last block of the tip's epoch.
pub fn fits_in_epoch(tip: u32, blocks: u32, provable: bool, epoch_last: Option<u32>) -> bool {
    provable && epoch_last.is_none_or(|last| tip.saturating_add(blocks) <= last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W-10: within the window, the anchor is the planned height whatever the payment's block is;
    /// it is `h + 1` only when the payment came in the window's last block. Every pay delay, draw
    /// and mempool wait of a payer with `pay_delay_max_blocks` = 8.
    #[test]
    fn w10_the_anchor_height_does_not_depend_on_the_payment_height() {
        let max = 8;
        let tip = 4_500_000;
        let c = current_height(tip);
        for delay in 1..=max {
            for r in 0..=max {
                let planned = planned_anchor(c, max, r);
                // Broadcast once the tip reaches `pay_after`: mined in the next block at the earliest.
                let earliest = pay_after(c, delay) + 1;
                assert_eq!(earliest, c + delay, "the payment lands in one of c + 1 .. c + max");
                for wait in 0..=2 * max + 2 {
                    let h = earliest + wait;
                    let a = anchor_height(Some(planned), r, h, None).unwrap();
                    assert!(a > h);
                    if h < planned {
                        assert_eq!(a, planned, "delay {delay}, r {r}, wait {wait}: the planned anchor");
                        assert_eq!(a == h + 1, h == planned - 1, "h + 1 only at the window's end");
                    } else {
                        assert_eq!(a, h + 1 + r, "a late payment");
                    }
                }
            }
        }
        // The window: the latest planned anchor is anchor_window blocks above the scheduling tip.
        assert_eq!(planned_anchor(c, max, max) - tip, anchor_window(max));
        assert_eq!(anchor_window(8), 18);
    }

    /// W-10: a payment from before anchors were planned anchors as a late one.
    #[test]
    fn w10_an_unplanned_payment_anchors_as_a_late_one() {
        assert_eq!(anchor_height(None, 3, 100, None), Some(104));
        assert_eq!(anchor_height(Some(100), 3, 100, None), Some(104), "mined at the planned height: late");
    }

    /// W-20: the anchor never leaves the payment's epoch; a payment in its epoch's last block has
    /// no anchor at all.
    #[test]
    fn w20_the_anchor_never_leaves_the_payments_epoch() {
        assert_eq!(anchor_height(Some(120), 4, 100, Some(110)), Some(110), "cut at the epoch's last block");
        assert_eq!(anchor_height(Some(120), 4, 100, Some(130)), Some(120), "inside the epoch: as planned");
        assert_eq!(anchor_height(Some(120), 4, 125, Some(127)), Some(127), "late and cut");
        assert_eq!(anchor_height(Some(120), 4, 109, Some(110)), Some(110));
        assert_eq!(anchor_height(Some(120), 4, 110, Some(110)), None, "the epoch's last block: unprovable");
    }

    /// W-20: the whole pipeline fits in a provable epoch, or the payer does not pay.
    #[test]
    fn w20_no_payment_near_a_known_activation() {
        assert!(fits_in_epoch(1_000, 40, true, None), "no known activation");
        assert!(fits_in_epoch(1_000, 40, true, Some(1_040)));
        assert!(!fits_in_epoch(1_001, 40, true, Some(1_040)), "the work would cross the activation");
        assert!(!fits_in_epoch(1_000, 40, false, None), "an epoch the statement does not prove on");
    }

    /// The epochs as zcash_protocol knows them: Testnet's NU7 has no known end; Mainnet's NU6.3
    /// lasts until a release gives NU7 a Mainnet height; NU6.2 blocks are not provable.
    #[test]
    fn the_epochs() {
        assert_eq!(epoch_last_height(ZcashNetwork::Testnet, 4_500_000), None);
        assert!(provable_epoch(ZcashNetwork::Testnet, 4_500_000));
        assert!(!provable_epoch(ZcashNetwork::Testnet, 4_000_000), "before Testnet's NU7");
        assert!(provable_epoch(ZcashNetwork::Mainnet, 3_500_000), "Mainnet's NU6.3");
        assert!(!provable_epoch(ZcashNetwork::Mainnet, 3_000_000));
        // Testnet's NU6.3 epoch ended where NU7 activated (4,465,026).
        assert_eq!(epoch_last_height(ZcashNetwork::Testnet, 4_200_000), Some(4_465_025));
    }

    #[test]
    fn the_draws_are_in_range() {
        for _ in 0..200 {
            let (delay, r) = draw(8);
            assert!((1..=8).contains(&delay) && r <= 8);
        }
        assert_eq!(draw(0), (1, 0), "a local fork without delays");
        assert_eq!(pay_after(101, 1), 101, "the earliest block of a one-block delay is c + 1");
    }
}
