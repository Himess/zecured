//! Quote amounts from the oracle price and the spread. The price is in Morpho's scale: USDC units
//! (6 decimals) per zatoshi, times 1e36, so `usdc = zatoshi * price / 1e36`. Every rounding favours
//! the maker by at most one unit: on Buy the taker pays at least the spread-adjusted value, on Sell
//! receives at most it. ZecuredSwap itself converts nothing: the quote fixes both amounts.

use alloy::primitives::U256;
use anyhow::{Result, ensure};
use zecured_swap::Side;

/// Morpho's oracle price scale.
pub fn price_scale() -> U256 {
    U256::from(10u64).pow(U256::from(36u64))
}

const BPS: u64 = 10_000;

fn factor(side: Side, spread_bps: u16) -> U256 {
    match side {
        Side::Buy => U256::from(BPS + u64::from(spread_bps)),
        Side::Sell => U256::from(BPS - u64::from(spread_bps)),
    }
}

/// `ceil(a / b)` for `b > 0` (no overflow: `a` is far below 2^255 here).
fn div_up(a: U256, b: U256) -> U256 {
    (a + b - U256::from(1u64)) / b
}

/// The USDC of an order of `zatoshi` at `price`: rounded up on Buy (the taker pays), down on Sell
/// (the taker receives).
pub fn usdc_for(zatoshi: u64, price: U256, side: Side, spread_bps: u16) -> Result<U256> {
    ensure!(price > U256::ZERO, "no price");
    let num = U256::from(zatoshi) * price * factor(side, spread_bps);
    let den = price_scale() * U256::from(BPS);
    Ok(match side {
        Side::Buy => div_up(num, den),
        Side::Sell => num / den,
    })
}

/// The zatoshi of an order of `usdc` at `price`: rounded down on Buy (the taker receives), up on
/// Sell (the taker pays).
pub fn zatoshi_for(usdc: U256, price: U256, side: Side, spread_bps: u16) -> Result<u64> {
    ensure!(price > U256::ZERO, "no price");
    let num = usdc * price_scale() * U256::from(BPS);
    let den = price * factor(side, spread_bps);
    let z = match side {
        Side::Buy => num / den,
        Side::Sell => div_up(num, den),
    };
    Ok(u64::try_from(z)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// $40.00 per ZEC: 40e6 USDC units per 1e8 zatoshi, times 1e36.
    fn forty() -> U256 {
        U256::from(40_000_000u64) * price_scale() / U256::from(100_000_000u64)
    }

    #[test]
    fn amounts_at_the_oracle_price() {
        // 0.5 ZEC at $40, no spread: 20 USDC either way.
        assert_eq!(usdc_for(50_000_000, forty(), Side::Buy, 0).unwrap(), U256::from(20_000_000u64));
        assert_eq!(usdc_for(50_000_000, forty(), Side::Sell, 0).unwrap(), U256::from(20_000_000u64));
        assert_eq!(zatoshi_for(U256::from(20_000_000u64), forty(), Side::Buy, 0).unwrap(), 50_000_000);
        // 1 % spread: the taker pays 20.2 USDC to buy 0.5 ZEC, gets 19.8 USDC selling it.
        assert_eq!(usdc_for(50_000_000, forty(), Side::Buy, 100).unwrap(), U256::from(20_200_000u64));
        assert_eq!(usdc_for(50_000_000, forty(), Side::Sell, 100).unwrap(), U256::from(19_800_000u64));
    }

    /// Rounding favours the maker by at most one unit (attack 10: 6 vs 8 decimals).
    #[test]
    fn rounding_favours_the_maker() {
        // 1 zatoshi at $40 is 0.4 USDC units: the buyer pays 1, the seller gets 0.
        assert_eq!(usdc_for(1, forty(), Side::Buy, 0).unwrap(), U256::from(1u64));
        assert_eq!(usdc_for(1, forty(), Side::Sell, 0).unwrap(), U256::ZERO);
        // 1 USDC unit buys 2 zatoshi (2.5 rounded down); selling for 1 unit takes 3 (2.5 up).
        assert_eq!(zatoshi_for(U256::from(1u64), forty(), Side::Buy, 0).unwrap(), 2);
        assert_eq!(zatoshi_for(U256::from(1u64), forty(), Side::Sell, 0).unwrap(), 3);
        for z in [1u64, 7, 123_456, 99_999_999, 1_000_000_000] {
            let buy = usdc_for(z, forty(), Side::Buy, 37).unwrap();
            let sell = usdc_for(z, forty(), Side::Sell, 37).unwrap();
            assert!(buy >= sell);
            // Round trip: what the buyer's USDC buys back is never more than the quote.
            assert!(zatoshi_for(buy, forty(), Side::Buy, 37).unwrap() >= z.saturating_sub(1));
        }
    }

    #[test]
    fn no_price_no_quote() {
        assert!(usdc_for(1, U256::ZERO, Side::Buy, 0).is_err());
        assert!(zatoshi_for(U256::from(1u64), U256::ZERO, Side::Sell, 0).is_err());
    }
}
