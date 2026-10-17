//! R-24 end to end on a local fork: anvil forking Base Sepolia (the real Morpho Blue and
//! AdaptiveCurveIRM), a fresh market with MOCK tokens and a MOCK price, a loan just inside the LLTV,
//! then a year with no activity on the market. The bot's own reads (`market_state`, `position_in`)
//! must see the interest and plan the liquidation, which then goes through on Morpho, which accrues
//! exactly what the bot's accrual computes for the same rate and time.
//!
//! Ignored by default: it needs `anvil` on PATH, the contracts built (`forge build` in `contracts/`,
//! for the MOCK tokens' bytecode) and a Base Sepolia RPC (`BASE_SEPOLIA_RPC_URL`, default the
//! public one). Run with `cargo test -p zecured-bots -- --ignored r24_on_a_fork`.

use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, Bytes, U256, address, keccak256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{SolEvent, SolValue},
};
use anyhow::{Context, Result, ensure};

use crate::chain::{self, Deployment, Liquidation, MarketTotals, Position};

/// Morpho Blue and the AdaptiveCurveIRM on Base Sepolia (docs/ref/FACTS.md), the fork block the
/// contracts' fork tests pin, and the market's 62.5 % LLTV.
const MORPHO: Address = address!("BBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
const ADAPTIVE_CURVE_IRM: Address = address!("46415998764C29aB2a25CbeA6254146D50D22687");
const FORK_BLOCK: u64 = 47_948_000;
const LLTV: u128 = 625_000_000_000_000_000;
/// Anvil's first two test accounts: publicly known keys, for local test chains only.
const LENDER_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const BORROWER_KEY: &str = "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

sol! {
    struct ForkMarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    #[sol(rpc)]
    interface IMorphoFork {
        function createMarket(ForkMarketParams marketParams) external;
        function supply(ForkMarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, bytes data) external returns (uint256, uint256);
        function supplyCollateral(ForkMarketParams marketParams, uint256 assets, address onBehalf, bytes data) external;
        function borrow(ForkMarketParams marketParams, uint256 assets, uint256 shares, address onBehalf, address receiver) external returns (uint256, uint256);
        function liquidate(ForkMarketParams marketParams, address borrower, uint256 seizedAssets, uint256 repaidShares, bytes data) external returns (uint256, uint256);
        function position(bytes32 id, address user) external view returns (uint256 supplyShares, uint128 borrowShares, uint128 collateral);
        function market(bytes32 id) external view returns (uint128 totalSupplyAssets, uint128 totalSupplyShares, uint128 totalBorrowAssets, uint128 totalBorrowShares, uint128 lastUpdate, uint128 fee);

        event AccrueInterest(bytes32 indexed id, uint256 prevBorrowRate, uint256 interest, uint256 feeShares);
        event Liquidate(bytes32 indexed id, address indexed caller, address indexed borrower, uint256 repaidAssets, uint256 repaidShares, uint256 seizedAssets, uint256 badDebtAssets, uint256 badDebtShares);
    }

    /// The MOCK tokens (MockUSDC) of this test's market.
    #[sol(rpc)]
    interface IMockToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

/// An anvil process forking Base Sepolia, killed on drop.
struct Anvil {
    child: Child,
    endpoint: String,
}

impl Anvil {
    fn spawn() -> Result<Self> {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let fork_url = std::env::var("BASE_SEPOLIA_RPC_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or("https://sepolia.base.org".into());
        let child = Command::new("anvil")
            .args(["--fork-url", &fork_url, "--fork-block-number", &FORK_BLOCK.to_string()])
            .args(["--port", &port.to_string(), "--silent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start anvil (Foundry's bin directory on PATH)")?;
        Ok(Self {
            child,
            endpoint: format!("http://127.0.0.1:{port}"),
        })
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The creation bytecode of a contract under `contracts/out` (forge's artifact).
fn creation_code(artifact: &str) -> Result<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts/out").join(artifact);
    let raw: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("{} (run forge build in contracts/)", path.display()))?,
    )?;
    let hex_code = raw["bytecode"]["object"].as_str().context("artifact without bytecode")?;
    Ok(hex::decode(hex_code.trim_start_matches("0x"))?)
}

async fn deploy<P: Provider>(p: &P, code: Vec<u8>, args: Vec<u8>) -> Result<Address> {
    let mut init = code;
    init.extend_from_slice(&args);
    let tx = TransactionRequest::default().with_deploy_code(Bytes::from(init));
    let receipt = p.send_transaction(tx).await?.get_receipt().await?;
    ensure!(receipt.status(), "deployment reverted");
    receipt.contract_address.context("no contract address")
}

#[tokio::test]
#[ignore = "needs anvil, the contracts built and a Base Sepolia RPC"]
async fn r24_on_a_fork() -> Result<()> {
    let anvil = Anvil::spawn()?;
    let lender: PrivateKeySigner = LENDER_KEY.parse()?;
    let borrower: PrivateKeySigner = BORROWER_KEY.parse()?;
    let (lender_addr, borrower_addr) = (lender.address(), borrower.address());
    let url = anvil.endpoint.parse()?;
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(lender)).connect_http(url);
    let started = Instant::now();
    while p.get_block_number().await.is_err() {
        ensure!(started.elapsed() < Duration::from_secs(60), "anvil did not come up");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let pb = ProviderBuilder::new().wallet(EthereumWallet::from(borrower)).connect_http(anvil.endpoint.parse()?);

    // A fresh zZEC-like market on the real Morpho Blue and AdaptiveCurveIRM: MOCK tokens and price.
    let usdc_code = creation_code("MockUSDC.sol/MockUSDC.json")?;
    let loan = deploy(&p, usdc_code.clone(), (lender_addr,).abi_encode_params()).await?;
    let collateral = deploy(&p, usdc_code, (lender_addr,).abi_encode_params()).await?;
    let price = U256::from(40u64) * U256::from(10u64).pow(U256::from(34u64)); // 0.4 loan units per collateral unit
    let oracle =
        deploy(&p, creation_code("MockZecOracle.sol/MockZecOracle.json")?, (lender_addr, price).abi_encode_params())
            .await?;
    let params = ForkMarketParams {
        loanToken: loan,
        collateralToken: collateral,
        oracle,
        irm: ADAPTIVE_CURVE_IRM,
        lltv: U256::from(LLTV),
    };
    // MarketParamsLib.id: keccak256 of the params' five words.
    let market_id: B256 = keccak256(params.abi_encode());
    let morpho = IMorphoFork::new(MORPHO, &p);
    let send =
        |r: alloy::rpc::types::TransactionReceipt, what: &str| -> Result<alloy::rpc::types::TransactionReceipt> {
            ensure!(r.status(), "{what} reverted");
            Ok(r)
        };
    send(morpho.createMarket(params.clone()).send().await?.get_receipt().await?, "createMarket")?;
    ensure!(morpho.market(market_id).call().await?.lastUpdate != 0, "the market id is Morpho's");
    // The lender supplies 1M units; the borrower puts up 5,000,000 units of collateral (2,000,000
    // units of value) and borrows 1,249,000: 62.45 %, healthy against the 62.5 % LLTV.
    let (usdc, coll) = (IMockToken::new(loan, &p), IMockToken::new(collateral, &p));
    send(usdc.mint(lender_addr, U256::from(10u64.pow(13))).send().await?.get_receipt().await?, "mint")?;
    send(usdc.approve(MORPHO, U256::MAX).send().await?.get_receipt().await?, "approve")?;
    let supply = morpho.supply(params.clone(), U256::from(10u64.pow(12)), U256::ZERO, lender_addr, Bytes::new());
    send(supply.send().await?.get_receipt().await?, "supply")?;
    send(coll.mint(borrower_addr, U256::from(5_000_000u64)).send().await?.get_receipt().await?, "mint")?;
    let as_borrower = IMorphoFork::new(MORPHO, &pb);
    let coll_b = IMockToken::new(collateral, &pb);
    send(coll_b.approve(MORPHO, U256::MAX).send().await?.get_receipt().await?, "approve")?;
    let put = as_borrower.supplyCollateral(params.clone(), U256::from(5_000_000u64), borrower_addr, Bytes::new());
    send(put.send().await?.get_receipt().await?, "supplyCollateral")?;
    let borrow = as_borrower.borrow(params.clone(), U256::from(1_249_000u64), U256::ZERO, borrower_addr, borrower_addr);
    send(borrow.send().await?.get_receipt().await?, "borrow")?;

    // A year with no activity on the market.
    let _: serde_json::Value = p.raw_request("evm_increaseTime".into(), (365u64 * 86_400,)).await?;
    let _: serde_json::Value = p.raw_request("evm_mine".into(), ()).await?;

    // What the bot reads.
    let d = Deployment {
        registry: Address::ZERO,
        adapter: Address::ZERO,
        usdc: loan,
        morpho: MORPHO,
        market_id,
        market: chain::MarketParams {
            loanToken: loan,
            collateralToken: collateral,
            oracle,
            irm: ADAPTIVE_CURVE_IRM,
            lltv: U256::from(LLTV),
        },
        block: 0,
    };
    let state = chain::market_state(&p, &d).await?;
    let pos = chain::position_in(&p, &d, &state, borrower_addr).await?;
    let stored = morpho.market(market_id).call().await?;
    let stored = MarketTotals {
        total_supply_assets: stored.totalSupplyAssets,
        total_supply_shares: stored.totalSupplyShares,
        total_borrow_assets: stored.totalBorrowAssets,
        total_borrow_shares: stored.totalBorrowShares,
        last_update: u64::try_from(stored.lastUpdate)?,
        fee: stored.fee,
    };
    ensure!(state.timestamp >= stored.last_update + 365 * 86_400, "a year passed");
    let stale = Position::new(
        pos.borrow_shares,
        pos.collateral,
        &chain::MarketState {
            total_borrow_assets: stored.total_borrow_assets,
            total_borrow_shares: stored.total_borrow_shares,
            ..state
        },
    );
    assert!(stale.healthy(), "on Morpho's stored totals the position looks healthy: {stale:?}");
    assert!(state.total_borrow_assets > stored.total_borrow_assets, "the bot sees the interest");
    assert!(!pos.healthy(), "with the interest the position is liquidatable: {pos:?}");
    let plan = chain::plan_liquidation(&pos, u64::try_from(pos.collateral)?).context("the bot plans a liquidation")?;
    assert_eq!(plan, Liquidation::RepayShares(pos.borrow_shares));

    // The liquidation goes through with nobody accruing first; Morpho accrues exactly what the
    // bot's accrual gives for the rate and time Morpho used.
    let (_, repaid_shares) = plan.arguments();
    let receipt = morpho.liquidate(params, borrower_addr, U256::ZERO, repaid_shares, Bytes::new()).send().await?;
    let receipt = send(receipt.get_receipt().await?, "liquidate")?;
    let block = p.get_block_by_number(receipt.block_number.context("no block")?.into()).await?.context("no block")?;
    let mut accrued = None;
    let mut repaid = None;
    for log in receipt.logs() {
        if let Ok(e) = IMorphoFork::AccrueInterest::decode_log(&log.inner) {
            accrued = Some((e.prevBorrowRate, e.interest));
        }
        if let Ok(e) = IMorphoFork::Liquidate::decode_log(&log.inner) {
            repaid = Some(e.repaidAssets);
        }
    }
    let (rate, interest) = accrued.context("Morpho accrued interest in the liquidation")?;
    let ours = chain::expected_market_balances(stored, u128::try_from(rate)?, block.header.timestamp, true)
        .context("accrual")?;
    assert_eq!(
        U256::from(ours.total_borrow_assets - stored.total_borrow_assets),
        interest,
        "the bot's accrual is Morpho's"
    );
    let repaid = repaid.context("a Liquidate event")?;
    assert!(
        repaid >= U256::from(pos.debt),
        "Morpho charged at least the debt the bot saw ({repaid} >= {})",
        pos.debt
    );
    let position = morpho.position(market_id, borrower_addr).call().await?;
    assert_eq!(position.borrowShares, 0, "every share repaid");
    Ok(())
}
