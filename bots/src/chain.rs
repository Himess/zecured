//! Shared Base access for the bots: deployment file, ABIs, position math, recipients.

use std::path::Path;

use alloy::{
    primitives::{Address, B256, U256},
    providers::Provider,
    sol,
};
use anyhow::{Context, Result, anyhow, bail};
use rand_core_06::{OsRng, RngCore};
use serde_json::{Value, json};

sol! {
    #[derive(Debug)]
    struct MarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    struct CloseRequest {
        bytes32 vaultId;
        address account;
        address executor;
        uint16 marginBps;
        uint64 minUserAmount;
        bytes32 userCommitment;
        uint256 nonce;
        uint64 deadline;
    }

    #[sol(rpc)]
    interface IMorpho {
        function position(bytes32 id, address user) external view returns (uint256 supplyShares, uint128 borrowShares, uint128 collateral);
        function market(bytes32 id) external view returns (uint128 totalSupplyAssets, uint128 totalSupplyShares, uint128 totalBorrowAssets, uint128 totalBorrowShares, uint128 lastUpdate, uint128 fee);
        function accrueInterest(MarketParams marketParams) external;
    }

    #[sol(rpc)]
    interface IOracle {
        function price() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function balanceOf(address owner) external view returns (uint256);
    }

    #[sol(rpc)]
    interface ISettlementAdapter {
        function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment) external returns (uint256 seized, uint256 repaid);
        function closeWithCollateral(CloseRequest req, bytes userSig, bytes32 mmCommitment) external;
    }

    #[sol(rpc)]
    interface IRegistryView {
        function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
    }
}

/// Morpho's virtual shares/assets (SharesMathLib).
const VIRTUAL_SHARES: u128 = 1_000_000;
const VIRTUAL_ASSETS: u128 = 1;
/// Morpho oracle price scale.
pub const ORACLE_PRICE_SCALE: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;
/// 1e18 (WAD).
const WAD: u128 = 1_000_000_000_000_000_000;

/// Addresses and market from `contracts/deployments/base-sepolia.json`.
#[derive(Clone, Debug)]
pub struct Deployment {
    pub registry: Address,
    pub adapter: Address,
    pub usdc: Address,
    pub morpho: Address,
    pub market_id: B256,
    pub market: MarketParams,
    pub block: u64,
}

impl Deployment {
    pub fn load(path: &Path) -> Result<Self> {
        let raw: Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
        let c = |k: &str| -> Result<Address> {
            raw["contracts"][k].as_str().context(format!("missing contracts.{k}"))?.parse().map_err(|e| anyhow!("{k}: {e}"))
        };
        let m = |k: &str| -> Result<String> { Ok(raw["market"][k].as_str().context(format!("missing market.{k}"))?.to_owned()) };
        Ok(Self {
            registry: c("ZecuredRegistry")?,
            adapter: c("SettlementAdapter")?,
            usdc: c("MockUSDC")?,
            morpho: m("morphoBlue")?.parse()?,
            market_id: m("marketId")?.parse()?,
            market: MarketParams {
                loanToken: m("loanToken")?.parse()?,
                collateralToken: m("collateralToken")?.parse()?,
                oracle: m("oracle")?.parse()?,
                irm: m("irm")?.parse()?,
                lltv: m("lltv")?.parse()?,
            },
            block: raw["block"].as_u64().context("missing block")?,
        })
    }
}

/// A borrower's position, in asset units.
#[derive(Clone, Copy, Debug)]
pub struct Position {
    pub borrow_shares: u128,
    pub debt: u128,
    pub collateral: u128,
    pub price: u128,
    /// Maximum debt at the liquidation threshold.
    pub max_debt: u128,
}

impl Position {
    pub fn healthy(&self) -> bool {
        self.debt <= self.max_debt
    }
}

fn mul_div_up(a: u128, b: u128, d: u128) -> u128 {
    let p = U256::from(a) * U256::from(b);
    let d = U256::from(d);
    let q = (p + d - U256::from(1u8)) / d;
    u128::try_from(q).unwrap_or(u128::MAX)
}

fn mul_div_down(a: u128, b: u128, d: u128) -> u128 {
    let q = U256::from(a) * U256::from(b) / U256::from(d);
    u128::try_from(q).unwrap_or(u128::MAX)
}

pub async fn position<P: Provider>(p: &P, d: &Deployment, user: Address) -> Result<Position> {
    let morpho = IMorpho::new(d.morpho, p);
    let pos = morpho.position(d.market_id, user).call().await?;
    let mkt = morpho.market(d.market_id).call().await?;
    let price = IOracle::new(d.market.oracle, p).price().call().await?;
    let price = u128::try_from(price).map_err(|_| anyhow!("price overflow"))?;
    let lltv = u128::try_from(d.market.lltv).map_err(|_| anyhow!("lltv overflow"))?;
    let borrow_shares = pos.borrowShares;
    let debt = mul_div_up(
        borrow_shares,
        mkt.totalBorrowAssets + VIRTUAL_ASSETS,
        mkt.totalBorrowShares + VIRTUAL_SHARES,
    );
    let collateral_value = mul_div_down(pos.collateral, price, ORACLE_PRICE_SCALE);
    Ok(Position {
        borrow_shares,
        debt,
        collateral: pos.collateral,
        price,
        max_debt: mul_div_down(collateral_value, lltv, WAD),
    })
}

/// Reveals a fresh (receiver, salt) for the bot's Ironwood address to the signers through
/// the backend and returns the salted commitment to put on Base.
pub async fn fresh_commitment(backend: &str, zcash_address: &str) -> Result<B256> {
    let mut salt = [0u8; 32];
    OsRng.fill_bytes(&mut salt);
    let resp = reqwest::Client::new()
        .post(format!("{}/recipients", backend.trim_end_matches('/')))
        .json(&json!({ "receiver": zcash_address, "salt": hex::encode(salt) }))
        .send()
        .await
        .context("backend unreachable")?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if !status.is_success() {
        bail!("backend refused the recipient: {}", v["error"]);
    }
    v["commitment"].as_str().context("no commitment")?.parse().map_err(|e| anyhow!("{e}"))
}

/// Approves `spender` for unlimited USDC once.
pub async fn ensure_allowance<P: Provider>(p: &P, usdc: Address, owner: Address, spender: Address) -> Result<()> {
    let token = IERC20::new(usdc, p);
    if token.allowance(owner, spender).call().await? < U256::from(u128::MAX) {
        let receipt = token.approve(spender, U256::MAX).send().await?.get_receipt().await?;
        anyhow::ensure!(receipt.status(), "approve reverted");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_matches_morpho() {
        assert_eq!(mul_div_up(10, 3, 4), 8);
        assert_eq!(mul_div_down(10, 3, 4), 7);
        // 0.01 ZEC (1e6 zat) at $40 (price = 40e34): value 40e34 * 1e6 / 1e36 = 0.4 USDC = 400_000 units.
        assert_eq!(mul_div_down(1_000_000, 40 * 10u128.pow(34), ORACLE_PRICE_SCALE), 400_000);
    }
}
