//! The maker's configuration (a JSON file; the Base key comes from the environment, the Zcash
//! spending key from the wallet file) and the rules it must respect before the maker starts.

use std::{net::SocketAddr, path::PathBuf};

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use zecured_swap::Side;
use zecured_zcash_ops::network::ZcashNetwork;

/// The maker's configuration file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MakerConfig {
    /// Base JSON-RPC (Base Sepolia, a local anvil fork, Base mainnet).
    pub rpc: String,
    pub chain_id: u64,
    /// The Zcash network of the payments, `"testnet"` or `"mainnet"`. Unset: the one the Base chain
    /// goes with (Base Sepolia and anvil: Testnet; Base: Mainnet); another pairing only on a local
    /// fork.
    #[serde(default)]
    pub zcash_network: Option<ZcashNetwork>,
    /// The deployed ZecuredSwap.
    pub swap: Address,
    /// The ZEC/USD price in Morpho's scale (`price()`; `priceHigh()` too when it is a
    /// ZecuredPriceFeed): the lending market's oracle.
    pub oracle: Address,
    /// Light wallet server (Zcash).
    pub lwd: String,
    /// The HTTP API (`GET /swap/quote`, `POST /swap/orders/:id/destination`).
    pub bind: SocketAddr,
    /// The order book (holds the takers' receivers and salts until their orders are done; 0600).
    pub state: PathBuf,
    /// The maker's Ironwood wallet: `{ spending_key, birthday }` (0600), as `zecured-wallet init`.
    pub wallet: PathBuf,
    /// JSON Lines headers of the network's epoch from its activation (NU7 on Testnet, NU6.3 on
    /// Mainnet until its NU7), extended over P2P (tier2 witness builders).
    pub headers: PathBuf,
    /// Cache of per-block summaries for the history tree.
    pub summaries: PathBuf,
    /// The zkVM host (`zecured-tier2-host`), or `mock-host` with `mock_seal` (local fork only).
    pub host: PathBuf,
    /// MOCK seals for a ZecuredSwap behind `MockRiscZeroVerifier` (local fork only).
    #[serde(default)]
    pub mock_seal: bool,
    /// A local anvil fork: the only network where the privacy delays may be zero and seals MOCK.
    #[serde(default)]
    pub local_fork: bool,
    /// The sides the maker quotes.
    pub sides: Vec<Side>,
    /// Spread over (Buy) or under (Sell) the oracle price, in basis points.
    pub spread_bps: u16,
    /// The quotes' pay window, seconds (within ZecuredSwap's `minPayWindow`..`maxPayWindow`).
    pub pay_window_secs: u32,
    /// How long a quote may be opened, seconds.
    pub quote_ttl_secs: u64,
    /// Order bounds (within ZecuredSwap's caps).
    pub min_order_zat: u64,
    pub max_order_zat: u64,
    /// USDC units (6 decimals).
    pub max_order_usdc: u64,
    /// Most orders the maker has open at once.
    pub max_open_orders: usize,
    /// H-05 (L23): a random 1 to `pay_delay_max_blocks` Zcash blocks between receiving a
    /// destination and broadcasting the payment, so the payment does not sit in the block right
    /// after the order opened on Base.
    pub pay_delay_max_blocks: u32,
    /// H-05 (L5): a random 0 to `prove_delay_max_blocks` extra blocks above the payment's before
    /// the proof's anchor, so the public anchor does not point at the block right after the
    /// payment's.
    pub prove_delay_max_blocks: u32,
    /// Seconds per Zcash block (75 on Testnet and Mainnet).
    #[serde(default = "default_block_secs")]
    pub block_secs: u64,
    /// Seconds until a payment's block has the order's tier of work on the relay (estimate).
    pub confirm_estimate_secs: u64,
    /// Seconds to prove a payment (STARK and Groth16; a few with MOCK seals).
    pub proving_estimate_secs: u64,
    /// Seconds kept free before the deadline for the Base transactions and retries.
    pub submit_margin_secs: u64,
    /// The quote refuses while the relay lags the Zcash tip by more blocks than this.
    pub max_relay_lag_blocks: u64,
    /// The quote refuses a ZecuredPriceFeed price older than this (seconds).
    pub max_price_age_secs: u64,
    /// Seconds between two passes of the order engine.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// The first Base block to read `OrderOpened` from (the swap's deployment block).
    #[serde(default)]
    pub from_block: u64,
    /// Mainnet beta (work order 6): quotes only for invitees. Off for the judge deployment.
    #[serde(default)]
    pub invite_only: bool,
    /// keccak256 of each invite code's UTF-8 bytes (the code itself is never stored or logged).
    #[serde(default)]
    pub invite_code_hashes: Vec<B256>,
    /// O-7: a loopback address (127.0.0.1 or ::1) for the operators' watchdog, where
    /// `GET /inventory` answers the maker's free ZEC and USDC. Never the public API's address and
    /// never proxied: the maker's balances stay off the public API. Unset: no such endpoint.
    #[serde(default)]
    pub status_bind: Option<SocketAddr>,
}

fn default_block_secs() -> u64 {
    75
}

fn default_poll_secs() -> u64 {
    15
}

/// The most a spread may be (20 %).
pub const MAX_SPREAD_BPS: u16 = 2_000;
/// H-05 (L5): the least `prove_delay_max_blocks` outside a local fork.
pub const MIN_PROVE_DELAY_MAX_BLOCKS: u32 = 2;

impl MakerConfig {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&raw).context("parse the maker config")?;
        cfg.check()?;
        Ok(cfg)
    }

    /// Seconds an order needs from its destination to its proof on Base, at most: the payment's
    /// privacy delay, its confirmations, the proof's extra blocks, proving, and the margin.
    pub fn required_secs(&self) -> u64 {
        u64::from(self.pay_delay_max_blocks) * self.block_secs
            + self.confirm_estimate_secs
            + u64::from(self.prove_delay_max_blocks) * self.block_secs
            + self.proving_estimate_secs
            + self.submit_margin_secs
    }

    /// What every config must respect; the chain's own bounds are checked at start.
    pub fn check(&self) -> Result<()> {
        ensure!(self.swap != Address::ZERO && self.oracle != Address::ZERO, "swap and oracle addresses");
        ensure!(!self.sides.is_empty(), "the maker quotes at least one side");
        ensure!(self.spread_bps <= MAX_SPREAD_BPS, "spread_bps is at most {MAX_SPREAD_BPS}");
        ensure!(self.min_order_zat > 0 && self.min_order_zat <= self.max_order_zat, "order bounds");
        ensure!(self.max_order_usdc > 0 && self.max_open_orders > 0, "order caps");
        ensure!(self.quote_ttl_secs >= 10, "quote_ttl_secs is at least 10");
        ensure!(self.block_secs > 0 && self.poll_secs > 0, "block_secs and poll_secs");
        // The payments' Zcash network goes with the Base chain (or is named on a local fork).
        self.zcash()?;
        if !self.local_fork {
            ensure!(!self.mock_seal, "MOCK seals only on a local fork");
            ensure!(
                self.pay_delay_max_blocks >= 1,
                "pay_delay_max_blocks is at least 1 outside a local fork (H-05, L23)"
            );
            ensure!(
                self.prove_delay_max_blocks >= MIN_PROVE_DELAY_MAX_BLOCKS,
                "prove_delay_max_blocks is at least {MIN_PROVE_DELAY_MAX_BLOCKS} outside a local fork (H-05, L5)"
            );
        }
        // The maker never quotes a window it cannot meet (work order 5.6): a destination given
        // within the first half of the window must still leave the whole pipeline its time.
        ensure!(
            u64::from(self.pay_window_secs) >= 2 * self.required_secs(),
            "pay_window_secs ({}) must be at least twice the pipeline's {} s (privacy delays, confirmations, proving, margin)",
            self.pay_window_secs,
            self.required_secs()
        );
        if self.invite_only {
            ensure!(!self.invite_code_hashes.is_empty(), "invite_only needs invite_code_hashes");
        }
        if let Some(status) = self.status_bind {
            ensure!(
                status.ip().is_loopback(),
                "status_bind must be a loopback address: it reports the maker's balances"
            );
            ensure!(status != self.bind, "status_bind must not be the public API's address");
        }
        Ok(())
    }

    /// The payments' Zcash network: `zcash_network`, or the one the Base chain goes with.
    pub fn zcash(&self) -> Result<ZcashNetwork> {
        ZcashNetwork::resolve(self.zcash_network, self.chain_id, self.local_fork)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn judge_like() -> MakerConfig {
        MakerConfig {
            rpc: "http://127.0.0.1:8545".into(),
            chain_id: 84_532,
            zcash_network: None,
            swap: Address::repeat_byte(0x11),
            oracle: Address::repeat_byte(0x22),
            lwd: "https://testnet.zec.rocks:443".into(),
            bind: "127.0.0.1:8095".parse().unwrap(),
            state: "data/maker/orders.json".into(),
            wallet: "data/maker/wallet.json".into(),
            headers: "data/tier2/headers_nu7.json".into(),
            summaries: "data/tier2/summaries.json".into(),
            host: "tier2/target/release/zecured-tier2-host".into(),
            mock_seal: false,
            local_fork: false,
            sides: vec![Side::Buy, Side::Sell],
            spread_bps: 100,
            pay_window_secs: 21_600,
            quote_ttl_secs: 120,
            min_order_zat: 100_000,
            max_order_zat: 1_000_000_000,
            max_order_usdc: 200_000_000,
            max_open_orders: 10,
            pay_delay_max_blocks: 8,
            prove_delay_max_blocks: 4,
            block_secs: 75,
            confirm_estimate_secs: 1_200,
            proving_estimate_secs: 3_000,
            submit_margin_secs: 900,
            max_relay_lag_blocks: 5,
            max_price_age_secs: 3_600,
            poll_secs: 15,
            from_block: 0,
            invite_only: false,
            invite_code_hashes: vec![],
            status_bind: None,
        }
    }

    /// The example config parses and, once given the deployment's addresses, passes.
    #[test]
    fn the_example_config() {
        let raw = include_str!("../maker.example.json");
        let mut c: MakerConfig = serde_json::from_str(raw).unwrap();
        assert!(c.check().is_err(), "the example names no deployment");
        c.swap = Address::repeat_byte(0x11);
        c.oracle = Address::repeat_byte(0x22);
        c.check().unwrap();
        assert!(!c.invite_only, "invitations are off for the judge deployment");
    }

    #[test]
    fn the_judge_config_passes() {
        let c = judge_like();
        c.check().unwrap();
        // 8 and 4 blocks of 75 s, 1,200 s of work, 3,000 s of proving, 900 s of margin.
        assert_eq!(c.required_secs(), 600 + 1_200 + 300 + 3_000 + 900);
    }

    #[test]
    fn the_rules() {
        let mut c = judge_like();
        c.mock_seal = true;
        assert!(c.check().is_err(), "MOCK seals off a local fork");
        c.local_fork = true;
        c.pay_delay_max_blocks = 0;
        c.prove_delay_max_blocks = 0;
        c.check().unwrap();
        let mut c = judge_like();
        c.pay_delay_max_blocks = 0;
        assert!(c.check().is_err(), "H-05: a payment right after the order");
        let mut c = judge_like();
        c.prove_delay_max_blocks = 1;
        assert!(c.check().is_err(), "H-05: an anchor right after the payment");
        let mut c = judge_like();
        c.proving_estimate_secs = 10_000;
        assert!(c.check().is_err(), "a window the maker cannot meet");
        let mut c = judge_like();
        c.invite_only = true;
        assert!(c.check().is_err(), "invite-only without codes");
        c.invite_code_hashes = vec![B256::repeat_byte(1)];
        c.check().unwrap();
        let mut c = judge_like();
        c.spread_bps = MAX_SPREAD_BPS + 1;
        assert!(c.check().is_err());
        let mut c = judge_like();
        c.sides.clear();
        assert!(c.check().is_err());
    }

    /// The payments' Zcash network goes with the Base chain unless named, and another pairing is
    /// refused outside a local fork.
    #[test]
    fn the_zcash_network_goes_with_the_chain() {
        let c = judge_like();
        assert_eq!(c.zcash().unwrap(), ZcashNetwork::Testnet);
        let mainnet = MakerConfig {
            chain_id: 8_453,
            ..judge_like()
        };
        mainnet.check().unwrap();
        assert_eq!(mainnet.zcash().unwrap(), ZcashNetwork::Mainnet);
        let raw = serde_json::to_value(MakerConfig {
            zcash_network: Some(ZcashNetwork::Mainnet),
            ..judge_like()
        })
        .unwrap();
        assert_eq!(raw["zcash_network"], "mainnet");
        let named: MakerConfig = serde_json::from_value(raw).unwrap();
        assert!(named.check().is_err(), "Zcash Mainnet on Base Sepolia");
        let rehearsal = MakerConfig {
            local_fork: true,
            ..named
        };
        rehearsal.check().unwrap();
        assert_eq!(rehearsal.zcash().unwrap(), ZcashNetwork::Mainnet);
    }

    /// O-7: the inventory endpoint listens on a loopback address only, apart from the public API.
    #[test]
    fn the_status_endpoint_is_local_only() {
        let mut c = judge_like();
        c.status_bind = Some("127.0.0.1:8096".parse().unwrap());
        c.check().unwrap();
        c.status_bind = Some("[::1]:8096".parse().unwrap());
        c.check().unwrap();
        c.status_bind = Some("0.0.0.0:8096".parse().unwrap());
        assert!(c.check().is_err(), "every interface");
        c.status_bind = Some("10.0.0.7:8096".parse().unwrap());
        assert!(c.check().is_err(), "a private network address");
        c.status_bind = Some(c.bind);
        assert!(c.check().is_err(), "the public API's own address");
        let parsed: MakerConfig = serde_json::from_value({
            let mut v = serde_json::to_value(judge_like()).unwrap();
            v["status_bind"] = serde_json::json!("127.0.0.1:8096");
            v
        })
        .unwrap();
        assert_eq!(parsed.status_bind, Some("127.0.0.1:8096".parse().unwrap()));
    }
}
