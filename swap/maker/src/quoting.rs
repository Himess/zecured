//! `GET /swap/quote`: what the maker offers, decided from a snapshot of the market (price, relay,
//! inventory) so that the decision itself is a pure function of its inputs.

use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use zecured_swap::{Side, abi::SwapQuote};

use crate::{api::ApiError, config::MakerConfig, pricing};

/// ZecuredSwap's own bounds, read at start.
#[derive(Clone, Copy, Debug)]
pub struct ChainLimits {
    pub max_order_usdc: U256,
    pub max_order_zat: u64,
    pub min_pay_window: u32,
    pub max_pay_window: u32,
}

/// What a quote depends on, read just before it is built.
#[derive(Clone, Debug)]
pub struct Market {
    /// Morpho-scale ZEC/USD for this side (the high source on Buy when there is one).
    pub price: U256,
    /// Seconds since the price was last fresh, when the feed says.
    pub price_age: Option<u64>,
    /// Base's clock (the latest block's timestamp).
    pub now: u64,
    /// Zcash blocks the relay lags the light wallet server's tip.
    pub relay_lag: u64,
    /// Zatoshi the maker can pay now, less what its unpaid Buy orders already promise.
    pub zcash_free: u64,
    /// USDC the maker can lock now (balance and allowance).
    pub usdc_free: U256,
    /// The maker's orders not done.
    pub open_orders: usize,
}

/// The query of `GET /swap/quote`: a side, one amount, and the taker (required on Sell and in
/// invite-only mode: the quote is then bound to it).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRequest {
    pub side: String,
    #[serde(default)]
    pub zatoshi: Option<u64>,
    /// USDC units (6 decimals).
    #[serde(default)]
    pub usdc: Option<u64>,
    #[serde(default)]
    pub taker: Option<Address>,
}

/// The quote fields, as JSON (integers above 2^53 as decimal strings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteJson {
    pub maker: Address,
    pub taker: Address,
    pub side: u8,
    pub zatoshi: String,
    pub usdc: String,
    pub maker_receiver_commitment: B256,
    pub pay_window: u32,
    pub quote_deadline: String,
    pub nonce: String,
}

impl From<&SwapQuote> for QuoteJson {
    fn from(q: &SwapQuote) -> Self {
        Self {
            maker: q.maker,
            taker: q.taker,
            side: q.side,
            zatoshi: q.zatoshi.to_string(),
            usdc: q.usdc.to_string(),
            maker_receiver_commitment: q.makerReceiverCommitment,
            pay_window: q.payWindow,
            quote_deadline: q.quoteDeadline.to_string(),
            nonce: q.nonce.to_string(),
        }
    }
}

/// The amounts of a quote, before it is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub side: Side,
    pub zatoshi: u64,
    pub usdc: U256,
    pub taker: Address,
}

/// ZIP 317 headroom kept on top of a Buy payment: two spends, two outputs.
pub const PAYMENT_FEE_HEADROOM: u64 = 10_000;

/// Decides a quote's terms, or why there is none.
pub fn terms(cfg: &MakerConfig, limits: &ChainLimits, m: &Market, req: &QuoteRequest) -> Result<Terms, ApiError> {
    let side = Side::parse(&req.side).map_err(|_| ApiError::BAD_REQUEST)?;
    if !cfg.sides.contains(&side) {
        return Err(ApiError::SIDE_DISABLED);
    }
    let taker = match (side, req.taker) {
        (_, Some(t)) if t != Address::ZERO => t,
        // An open Sell quote would let anyone lock the maker's USDC for the whole window.
        (Side::Sell, _) => return Err(ApiError::TAKER_REQUIRED),
        (Side::Buy, _) if cfg.invite_only => return Err(ApiError::TAKER_REQUIRED),
        (Side::Buy, _) => Address::ZERO,
    };
    if m.relay_lag > cfg.max_relay_lag_blocks {
        return Err(ApiError::RELAY_BEHIND);
    }
    if m.price_age.is_some_and(|age| age > cfg.max_price_age_secs) || m.price.is_zero() {
        return Err(ApiError::PRICE_STALE);
    }
    if m.open_orders >= cfg.max_open_orders {
        return Err(ApiError::BUSY);
    }
    let (zatoshi, usdc) = match (req.zatoshi, req.usdc) {
        (Some(z), None) => (z, pricing::usdc_for(z, m.price, side, cfg.spread_bps).map_err(|_| ApiError::PRICE_STALE)?),
        (None, Some(u)) => {
            let u = U256::from(u);
            (pricing::zatoshi_for(u, m.price, side, cfg.spread_bps).map_err(|_| ApiError::BAD_REQUEST)?, u)
        }
        _ => return Err(ApiError::BAD_REQUEST),
    };
    let max_zat = cfg.max_order_zat.min(limits.max_order_zat);
    let max_usdc = U256::from(cfg.max_order_usdc).min(limits.max_order_usdc);
    if zatoshi < cfg.min_order_zat || usdc.is_zero() {
        return Err(ApiError::TOO_SMALL);
    }
    if zatoshi > max_zat || usdc > max_usdc {
        return Err(ApiError::TOO_LARGE);
    }
    let enough = match side {
        Side::Buy => m.zcash_free >= zatoshi.saturating_add(PAYMENT_FEE_HEADROOM),
        Side::Sell => m.usdc_free >= usdc,
    };
    if !enough {
        return Err(ApiError::NO_INVENTORY);
    }
    Ok(Terms {
        side,
        zatoshi,
        usdc,
        taker,
    })
}

/// The quote of `terms`: the maker's nonce, the window, the deadline and, on Sell, the commitment
/// to the maker's receiver.
pub fn quote(
    cfg: &MakerConfig,
    terms: &Terms,
    maker: Address,
    now: u64,
    nonce: U256,
    maker_commitment: Option<B256>,
) -> SwapQuote {
    SwapQuote {
        maker,
        taker: terms.taker,
        side: terms.side.as_u8(),
        zatoshi: terms.zatoshi,
        usdc: terms.usdc,
        makerReceiverCommitment: match terms.side {
            Side::Buy => B256::ZERO,
            Side::Sell => maker_commitment.unwrap_or_default(),
        },
        payWindow: cfg.pay_window_secs,
        quoteDeadline: now + cfg.quote_ttl_secs,
        nonce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::tests::judge_like, pricing::price_scale};

    fn limits() -> ChainLimits {
        ChainLimits {
            max_order_usdc: U256::from(200_000_000u64),
            max_order_zat: 1_000_000_000,
            min_pay_window: 7_200,
            max_pay_window: 21_600,
        }
    }

    fn market() -> Market {
        Market {
            // $40 per ZEC.
            price: U256::from(40_000_000u64) * price_scale() / U256::from(100_000_000u64),
            price_age: Some(60),
            now: 1_760_000_000,
            relay_lag: 1,
            zcash_free: 2_000_000_000,
            usdc_free: U256::from(500_000_000u64),
            open_orders: 0,
        }
    }

    fn req(side: &str, zatoshi: Option<u64>, usdc: Option<u64>, taker: Option<Address>) -> QuoteRequest {
        QuoteRequest {
            side: side.into(),
            zatoshi,
            usdc,
            taker,
        }
    }

    #[test]
    fn a_buy_quote() {
        let cfg = judge_like();
        let t = terms(&cfg, &limits(), &market(), &req("buy", Some(50_000_000), None, None)).unwrap();
        assert_eq!(t.side, Side::Buy);
        assert_eq!(t.usdc, U256::from(20_200_000u64), "0.5 ZEC at $40 plus 1 %");
        assert_eq!(t.taker, Address::ZERO, "an open Buy quote");
        let q = quote(&cfg, &t, Address::repeat_byte(9), 1_760_000_000, U256::from(5), None);
        assert_eq!(q.makerReceiverCommitment, B256::ZERO);
        assert_eq!(q.payWindow, 21_600);
        assert_eq!(q.quoteDeadline, 1_760_000_120);
        // By USDC amount.
        let t = terms(&cfg, &limits(), &market(), &req("buy", None, Some(20_200_000), None)).unwrap();
        assert_eq!(t.zatoshi, 50_000_000);
    }

    #[test]
    fn a_sell_quote_is_bound_to_its_taker() {
        let cfg = judge_like();
        assert_eq!(
            terms(&cfg, &limits(), &market(), &req("sell", Some(50_000_000), None, None)),
            Err(ApiError::TAKER_REQUIRED)
        );
        let taker = Address::repeat_byte(7);
        let t = terms(&cfg, &limits(), &market(), &req("sell", Some(50_000_000), None, Some(taker))).unwrap();
        assert_eq!(t.usdc, U256::from(19_800_000u64), "0.5 ZEC at $40 less 1 %");
        let q = quote(&cfg, &t, Address::repeat_byte(9), 1_760_000_000, U256::from(5), Some(B256::repeat_byte(4)));
        assert_eq!(q.taker, taker);
        assert_eq!(q.makerReceiverCommitment, B256::repeat_byte(4));
    }

    #[test]
    fn refusals() {
        let cfg = judge_like();
        let l = limits();
        let buy = req("buy", Some(50_000_000), None, None);
        let mut m = market();
        m.relay_lag = 6;
        assert_eq!(terms(&cfg, &l, &m, &buy), Err(ApiError::RELAY_BEHIND));
        let mut m = market();
        m.price_age = Some(3_601);
        assert_eq!(terms(&cfg, &l, &m, &buy), Err(ApiError::PRICE_STALE));
        let mut m = market();
        m.open_orders = 10;
        assert_eq!(terms(&cfg, &l, &m, &buy), Err(ApiError::BUSY));
        let mut m = market();
        m.zcash_free = 50_000_000;
        assert_eq!(terms(&cfg, &l, &m, &buy), Err(ApiError::NO_INVENTORY), "the fee headroom too");
        let mut m = market();
        m.usdc_free = U256::from(1u64);
        assert_eq!(
            terms(&cfg, &l, &m, &req("sell", Some(50_000_000), None, Some(Address::repeat_byte(7)))),
            Err(ApiError::NO_INVENTORY)
        );
        let m = market();
        assert_eq!(terms(&cfg, &l, &m, &req("buy", Some(99_999), None, None)), Err(ApiError::TOO_SMALL));
        assert_eq!(terms(&cfg, &l, &m, &req("buy", Some(1_000_000_001), None, None)), Err(ApiError::TOO_LARGE));
        // 6 ZEC at $40 is 240 USDC: above the judge cap of 200.
        assert_eq!(terms(&cfg, &l, &m, &req("buy", Some(600_000_000), None, None)), Err(ApiError::TOO_LARGE));
        assert_eq!(terms(&cfg, &l, &m, &req("buy", Some(1), Some(1), None)), Err(ApiError::BAD_REQUEST));
        assert_eq!(terms(&cfg, &l, &m, &req("buy", None, None, None)), Err(ApiError::BAD_REQUEST));
        assert_eq!(terms(&cfg, &l, &m, &req("swap", Some(1), None, None)), Err(ApiError::BAD_REQUEST));
        let mut only_buy = judge_like();
        only_buy.sides = vec![Side::Buy];
        assert_eq!(
            terms(&only_buy, &l, &m, &req("sell", Some(50_000_000), None, Some(Address::repeat_byte(7)))),
            Err(ApiError::SIDE_DISABLED)
        );
        let mut invite = judge_like();
        invite.invite_only = true;
        invite.invite_code_hashes = vec![B256::repeat_byte(1)];
        assert_eq!(terms(&invite, &l, &m, &buy), Err(ApiError::TAKER_REQUIRED), "invitees get bound quotes");
    }
}
