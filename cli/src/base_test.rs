//! TEST ONLY: Base actions with the loan's owner EOA acting as the account.
//!
//! The app uses a fresh, gasless Safe smart account instead. These commands exist to run the
//! whole protocol end to end (e.g. on a local fork) before the account-abstraction stack is
//! wired. The USDC top-up for interest uses the mock's owner key and is a test convenience.

use std::path::Path;

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
    sol,
};
use anyhow::{Context, Result, anyhow, ensure};
use zecured_base::gas_with_headroom;

sol! {
    struct MarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    #[sol(rpc)]
    interface IMorpho {
        function isAuthorized(address authorizer, address authorized) external view returns (bool);
        function setAuthorization(address authorized, bool newIsAuthorized) external;
        function borrow(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, address receiver) external returns (uint256, uint256);
        function repay(MarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, bytes data) external returns (uint256, uint256);
        function position(bytes32 id, address user) external view returns (uint256 supplyShares, uint128 borrowShares, uint128 collateral);
        function market(bytes32 id) external view returns (uint128 totalSupplyAssets, uint128 totalSupplyShares, uint128 totalBorrowAssets, uint128 totalBorrowShares, uint128 lastUpdate, uint128 fee);
    }

    #[sol(rpc)]
    interface IMockUSDC {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address owner) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IRegistry {
        function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32);
        function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment) external returns (bytes32);
    }
}

pub struct Deployment {
    pub registry: Address,
    pub adapter: Address,
    pub usdc: Address,
    pub morpho: Address,
    pub market_id: B256,
    pub market: MarketParams,
}

impl Deployment {
    pub fn load() -> Result<Self> {
        let path =
            std::env::var("ZECURED_DEPLOYMENTS").unwrap_or_else(|_| "contracts/deployments/base-sepolia.json".into());
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(Path::new(&path)).with_context(|| format!("read {path}"))?)?;
        let c = |k: &str| -> Result<Address> { Ok(raw["contracts"][k].as_str().context(k.to_owned())?.parse()?) };
        let m = |k: &str| -> Result<String> { Ok(raw["market"][k].as_str().context(k.to_owned())?.to_owned()) };
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
        })
    }
}

fn owner(wallet: &Path, vault: &B256) -> Result<PrivateKeySigner> {
    let path = wallet.with_file_name(format!("loan-{}.json", hex::encode(&vault[..8])));
    let loan: serde_json::Value = crate::read_secret_json(&path)?;
    Ok(loan["owner_key"].as_str().context("owner_key")?.parse()?)
}

pub async fn borrow(wallet: &Path, vault: &B256, usdc: f64, rpc: &str) -> Result<()> {
    let d = Deployment::load()?;
    let key = owner(wallet, vault)?;
    let me = key.address();
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    let morpho = IMorpho::new(d.morpho, &p);
    for target in [d.registry, d.adapter] {
        if !morpho.isAuthorized(me, target).call().await? {
            let call = morpho.setAuthorization(target, true);
            let gas = gas_with_headroom(call.estimate_gas().await?);
            let r = call.gas(gas).send().await?.get_receipt().await?;
            ensure!(r.status(), "setAuthorization reverted");
        }
    }
    let assets = U256::from((usdc * 1_000_000.0).round() as u64);
    let call = morpho.borrow(d.market.clone(), assets, U256::ZERO, me, me);
    let gas = gas_with_headroom(call.estimate_gas().await?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "borrow reverted");
    println!("borrowed {usdc} USDC (tx {})", r.transaction_hash);
    Ok(())
}

pub async fn repay_release(wallet: &Path, vault: &B256, commitment: B256, rpc: &str) -> Result<()> {
    let d = Deployment::load()?;
    let key = owner(wallet, vault)?;
    let me = key.address();
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    let morpho = IMorpho::new(d.morpho, &p);
    let pos = morpho.position(d.market_id, me).call().await?;
    if pos.borrowShares > 0 {
        // TEST ONLY: top up a little MockUSDC for accrued interest from the mock's owner.
        if let Ok(k) = std::env::var("DEPLOYER_PRIVATE_KEY") {
            let owner: PrivateKeySigner = k.parse()?;
            let po = ProviderBuilder::new().wallet(EthereumWallet::from(owner)).connect_http(rpc.parse()?);
            let r = IMockUSDC::new(d.usdc, &po).mint(me, U256::from(100_000u64)).send().await?.get_receipt().await?;
            ensure!(r.status(), "mint reverted");
        }
        let usdc = IMockUSDC::new(d.usdc, &p);
        ensure!(usdc.approve(d.morpho, U256::MAX).send().await?.get_receipt().await?.status(), "approve reverted");
        let call = morpho.repay(d.market.clone(), U256::ZERO, U256::from(pos.borrowShares), me, Bytes::new());
        let gas = gas_with_headroom(call.estimate_gas().await?);
        let r = call.gas(gas).send().await?.get_receipt().await?;
        ensure!(r.status(), "repay reverted");
        println!("repaid (tx {})", r.transaction_hash);
    }
    ensure_registry_authorized(&morpho, me, d.registry).await?;
    let registry = IRegistry::new(d.registry, &p);
    let call = registry.requestRelease(*vault, commitment);
    let gas = gas_with_headroom(call.estimate_gas().await.map_err(|e| anyhow!("requestRelease: {e}"))?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "requestRelease reverted");
    println!("release requested (tx {})", r.transaction_hash);
    Ok(())
}

/// D14: withdraw `amount` zat of collateral while the vault stays active (debt or not); the
/// Registry refuses it if the position's LTV afterwards would exceed 50 %.
pub async fn partial_release(wallet: &Path, vault: &B256, amount: u64, commitment: B256, rpc: &str) -> Result<()> {
    let d = Deployment::load()?;
    let key = owner(wallet, vault)?;
    let me = key.address();
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    let morpho = IMorpho::new(d.morpho, &p);
    ensure_registry_authorized(&morpho, me, d.registry).await?;
    let registry = IRegistry::new(d.registry, &p);
    let call = registry.requestPartialRelease(*vault, amount, commitment);
    let gas = gas_with_headroom(call.estimate_gas().await.map_err(|e| anyhow!("requestPartialRelease: {e}"))?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "requestPartialRelease reverted");
    println!("partial release of {amount} zat requested (tx {})", r.transaction_hash);
    Ok(())
}

/// Releases withdraw collateral through the account's Morpho authorization of the Registry.
async fn ensure_registry_authorized<P: alloy::providers::Provider>(
    morpho: &IMorpho::IMorphoInstance<P>,
    me: Address,
    registry: Address,
) -> Result<()> {
    if !morpho.isAuthorized(me, registry).call().await? {
        let call = morpho.setAuthorization(registry, true);
        let gas = gas_with_headroom(call.estimate_gas().await?);
        ensure!(call.gas(gas).send().await?.get_receipt().await?.status(), "setAuthorization reverted");
    }
    Ok(())
}
