//! The bots against real contracts on local chains (anvil).
//!
//! - R-24 end to end on a local fork: anvil forking Base Sepolia (the real Morpho Blue and
//!   AdaptiveCurveIRM), a fresh market with MOCK tokens and a MOCK price, a loan just inside the
//!   LLTV, then a year with no activity on the market. The bot's own reads (`market_state`,
//!   `position_in`) must see the interest and plan the liquidation, which then goes through on
//!   Morpho, which accrues exactly what the bot's accrual computes for the same rate and time.
//! - R-5 on a plain local chain: the real SignerBondV2 over the MOCKs of its own unit tests (Registry,
//!   slasher, Intake, compensator, oracle) and a MOCK backend serving receipts a test signer signs;
//!   the bot's armer arms a claim once the receipt arrives, sends nothing for an armed claim, arms it
//!   again after its commitment changes, and drops it once paid.
//!
//! Ignored by default: they need `anvil` on PATH and the contracts built (`forge build` in
//! `contracts/`, for the bytecode), and the fork a Base Sepolia RPC (`BASE_SEPOLIA_RPC_URL`, default
//! the public one). Run with `cargo test -p zecured-bots -- --ignored`.

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

/// The anvil tests run one at a time: two picking a free port at once can pick the same one, and
/// then talk to each other's chain.
static ANVIL_TESTS: std::sync::LazyLock<tokio::sync::Mutex<()>> = std::sync::LazyLock::new(Default::default);

/// An anvil process (forking Base Sepolia, or a plain local chain), killed on drop.
struct Anvil {
    child: Child,
    endpoint: String,
}

impl Anvil {
    fn spawn() -> Result<Self> {
        let fork_url = std::env::var("BASE_SEPOLIA_RPC_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or("https://sepolia.base.org".into());
        Self::start(&["--fork-url", &fork_url, "--fork-block-number", &FORK_BLOCK.to_string()])
    }

    fn local() -> Result<Self> {
        Self::start(&[])
    }

    fn start(args: &[&str]) -> Result<Self> {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let child = Command::new("anvil")
            .args(args)
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

    /// Waits until the chain answers (and fails at once if anvil exited, e.g. on a port taken).
    async fn ready<P: Provider>(&mut self, p: &P) -> Result<()> {
        let started = Instant::now();
        while p.get_block_number().await.is_err() {
            ensure!(self.child.try_wait()?.is_none(), "anvil exited");
            ensure!(started.elapsed() < Duration::from_secs(60), "anvil did not come up");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Ok(())
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
    let _serial = ANVIL_TESTS.lock().await;
    let mut anvil = Anvil::spawn()?;
    let lender: PrivateKeySigner = LENDER_KEY.parse()?;
    let borrower: PrivateKeySigner = BORROWER_KEY.parse()?;
    let (lender_addr, borrower_addr) = (lender.address(), borrower.address());
    let url = anvil.endpoint.parse()?;
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(lender)).connect_http(url);
    anvil.ready(&p).await?;
    ensure!(p.get_chain_id().await? == 84_532, "not the Base Sepolia fork");
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
        zcash: zecured_zcash_ops::network::ZcashNetwork::Testnet,
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

// ----------------------------------------------------------------------------- R-5 on a local chain

sol! {
    /// SignerBondV2's constructor argument (contracts/src/interfaces/v3/ISignerBond.sol).
    struct BondConfigArgs {
        address registry;
        address slasher;
        address intake;
        address compensator;
        address usdc;
        address oracle;
        uint256 minBond;
        uint64 unbondingPeriod;
        uint64 challengeWindow;
        uint256 bountyBps;
        uint256 bountyCap;
        uint64 livenessPeriod;
        uint64 commitGrace;
        uint16 livenessPremiumBps;
    }

    struct MockClaimDetails {
        bytes32 vaultId;
        uint8 kind;
        uint8 status;
        uint64 amount;
        bytes32 commitment;
        uint40 createdAt;
        bool bondPaid;
        address claimant;
    }

    /// The MOCK Registry of the bond's unit tests (contracts/test/mocks/BondV2Mocks.sol).
    #[sol(rpc)]
    interface IMockBondRegistry {
        function setSignerBond(address bond, address token) external;
        function setClaim(bytes32 claimId, MockClaimDetails c) external;
    }
}

/// A MOCK backend serving the receipt the test signer signs for the claim's `commitment` (after
/// `misses` answers of `404 no_receipt`), as the coordinator would.
#[derive(Clone)]
struct ReceiptBackend {
    signer: PrivateKeySigner,
    bond: Address,
    chain_id: u64,
    received_at: u64,
    commitment: std::sync::Arc<std::sync::Mutex<B256>>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    misses: usize,
}

async fn serve_receipt(
    axum::extract::State(b): axum::extract::State<ReceiptBackend>,
    axum::extract::Path(claim_id): axum::extract::Path<B256>,
) -> axum::response::Response {
    use alloy::{signers::SignerSync, sol_types::SolStruct};
    use axum::response::IntoResponse;
    let n = b.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if n < b.misses {
        return (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "code": "no_receipt" })))
            .into_response();
    }
    let commitment = *b.commitment.lock().unwrap();
    let digest = zecured_base::operator::destination_received(claim_id, commitment, b.received_at)
        .eip712_signing_hash(&zecured_base::operator::bond_domain(b.chain_id, b.bond));
    let signature = Bytes::from(b.signer.sign_hash_sync(&digest).unwrap().as_bytes().to_vec());
    axum::Json(serde_json::json!({
        "claimId": claim_id,
        "commitment": commitment,
        "receivedAt": b.received_at,
        "signer": b.signer.address(),
        "signature": signature,
    }))
    .into_response()
}

#[tokio::test]
#[ignore = "needs anvil and the contracts built"]
async fn liveness_arming_on_a_local_chain() -> Result<()> {
    use crate::liveness::{Armer, BackendReceipts, ProviderChain};

    // The armer's warnings, in the test's output on a failure.
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let _serial = ANVIL_TESTS.lock().await;
    let mut anvil = Anvil::local()?;
    let bot: PrivateKeySigner = LENDER_KEY.parse()?;
    let bot_addr = bot.address();
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(bot)).connect_http(anvil.endpoint.parse()?);
    anvil.ready(&p).await?;
    let chain_id = p.get_chain_id().await?;
    // The MOCKs are set up from another key: as in the market maker, every transaction of the bot's
    // key goes through its nonce lock.
    let admin: PrivateKeySigner = BORROWER_KEY.parse()?;
    let admin_addr = admin.address();
    let pa = ProviderBuilder::new().wallet(EthereumWallet::from(admin)).connect_http(anvil.endpoint.parse()?);

    // The bond over its unit tests' MOCKs, and five signers (test keys).
    let signers: Vec<PrivateKeySigner> = (0..5u8)
        .map(|i| PrivateKeySigner::from_bytes(&keccak256([b"zecured.bots.test.signer".as_slice(), &[i]].concat())))
        .collect::<Result<_, _>>()?;
    let signer_addresses: [Address; 5] = std::array::from_fn(|i| signers[i].address());
    let usdc = deploy(&pa, creation_code("MockUSDC.sol/MockUSDC.json")?, (admin_addr,).abi_encode_params()).await?;
    let registry = deploy(
        &pa,
        creation_code("BondV2Mocks.sol/MockBondRegistry.json")?,
        (signer_addresses,).abi_encode_params(),
    )
    .await?;
    let slasher = deploy(&pa, creation_code("BondV2Mocks.sol/MockBondSlasher.json")?, vec![]).await?;
    let intake = deploy(&pa, creation_code("BondV2Mocks.sol/MockBondIntake.json")?, vec![]).await?;
    let compensator =
        deploy(&pa, creation_code("BondV2Mocks.sol/MockBondCompensator.json")?, (usdc,).abi_encode_params()).await?;
    let price = U256::from(40u64) * U256::from(10u64).pow(U256::from(34u64));
    let oracle =
        deploy(&pa, creation_code("BondV2Mocks.sol/MockBondOracle.json")?, (price,).abi_encode_params()).await?;
    let config = BondConfigArgs {
        registry,
        slasher,
        intake,
        compensator,
        usdc,
        oracle,
        minBond: U256::from(100_000_000u64),
        unbondingPeriod: 54 * 3_600,
        challengeWindow: 86_400,
        bountyBps: U256::from(1_000u64),
        bountyCap: U256::from(50_000_000u64),
        livenessPeriod: 86_400,
        commitGrace: 7_200,
        livenessPremiumBps: 500,
    };
    let bond = deploy(&pa, creation_code("SignerBondV2.sol/SignerBondV2.json")?, config.abi_encode()).await?;
    let mock_registry = IMockBondRegistry::new(registry, &pa);
    ensure!(mock_registry.setSignerBond(bond, usdc).send().await?.get_receipt().await?.status());

    // A pending Liquidation claim of the bot.
    let claim_id = keccak256(b"zecured.bots.test.claim");
    let first = keccak256(b"zecured.bots.test.commitment.1");
    let now = p
        .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest)
        .await?
        .context("no block")?
        .header
        .timestamp;
    let created_at = now - 100;
    let set_claim = |commitment: B256, status: u8| MockClaimDetails {
        vaultId: keccak256(b"zecured.bots.test.vault"),
        kind: 1, // Liquidation
        status,
        amount: 1_000_000,
        commitment,
        createdAt: alloy::primitives::Uint::from(created_at),
        bondPaid: false,
        claimant: bot_addr,
    };
    ensure!(mock_registry.setClaim(claim_id, set_claim(first, 1)).send().await?.get_receipt().await?.status());

    // The backend has no receipt at first (the nodes have not seen the claim yet), then signer 2's.
    let backend = ReceiptBackend {
        signer: signers[2].clone(),
        bond,
        chain_id,
        received_at: created_at + 50,
        commitment: std::sync::Arc::new(std::sync::Mutex::new(first)),
        calls: Default::default(),
        misses: 1,
    };
    let app = axum::Router::new()
        .route("/claims/{claim}/receipt", axum::routing::post(serve_receipt))
        .with_state(backend.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let nonces = crate::chain::NonceLock::new(bot_addr);
    let chain = ProviderChain {
        p: &p,
        registry,
        bond,
        nonces: Some(&nonces),
    };
    let mut armer = Armer::new(chain, BackendReceipts::new(&url)?, None)?;
    armer.track(claim_id)?;
    let bond_view = zecured_base::events::ISignerBond::new(bond, &p);
    let nonce = || p.get_transaction_count(bot_addr);

    let r = armer.pass(1_000).await;
    assert_eq!((r.waiting, r.armed), (1, 0), "404: no receipt yet");
    let before = nonce().await?;
    // The arming and another transaction of the bot's key at once (the market maker's close; here
    // a MOCK token approval), both through the key's nonce lock: both go through, at two nonces.
    let token = IMockToken::new(usdc, &p);
    let close = token.approve(bond, U256::from(1u8)).gas(100_000);
    let (r, other) = tokio::join!(
        armer.pass(1_030),
        nonces.send_with(&p, |nonce| async move { Ok(close.nonce(nonce).send().await?) })
    );
    assert_eq!(r.armed, 1, "armed once the receipt came: {r:?}");
    ensure!(other?.get_receipt().await?.status(), "the concurrent transaction failed");
    assert_eq!(nonce().await?, before + 2, "an armLiveness and a close from the bot's key, two nonces");
    let armed = bond_view.livenessArming(claim_id).call().await?;
    assert_eq!(
        (armed.commitment, armed.payableAt),
        (first, created_at + 50),
        "payableAt = max(createdAt, receivedAt)"
    );

    // Armed for its commitment: nothing is sent.
    let r = armer.pass(1_030 + crate::liveness::ARMED_RECHECK).await;
    assert_eq!(r.already_armed, 1);
    assert_eq!(nonce().await?, before + 2, "no transaction for an armed claim");

    // A new commitment voids the arming; the bot arms the claim again with a receipt for it.
    let second = keccak256(b"zecured.bots.test.commitment.2");
    ensure!(mock_registry.setClaim(claim_id, set_claim(second, 1)).send().await?.get_receipt().await?.status());
    *backend.commitment.lock().unwrap() = second;
    let r = armer.pass(1_030 + 2 * crate::liveness::ARMED_RECHECK).await;
    assert_eq!(r.armed, 1, "armed again: {r:?}");
    let armed = bond_view.livenessArming(claim_id).call().await?;
    assert_eq!(armed.commitment, second);

    // Paid: dropped, nothing sent.
    ensure!(mock_registry.setClaim(claim_id, set_claim(second, 2)).send().await?.get_receipt().await?.status());
    let sent = nonce().await?;
    let r = armer.pass(1_030 + 3 * crate::liveness::ARMED_RECHECK).await;
    assert_eq!(r.done, 1);
    assert!(armer.tracked().is_empty());
    assert_eq!(nonce().await?, sent);
    Ok(())
}
