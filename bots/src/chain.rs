//! Shared Base access for the bots: deployment file, ABIs, position math, recipients.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use alloy::{
    primitives::{Address, B256, U256, keccak256},
    providers::Provider,
    sol,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rand_core_06::{OsRng, RngCore};
use serde_json::{Value, json};
use zcash_protocol::consensus::Network;

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

    /// S18 self-deleverage (Adapter domain), signed by the borrower's smart account.
    struct DeleverageRequest {
        bytes32 vaultId;
        address account;
        address executor;
        uint16 marginBps;
        uint256 repayAssets;
        uint64 maxCollateral;
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
        function closeWithCollateral(CloseRequest req, bytes userSig, bytes32 mmCommitment, uint256 maxRepayAssets, uint64 minMmAmount) external;
        function deleverageWithCollateral(DeleverageRequest req, bytes userSig, bytes32 mmCommitment, uint64 minMmAmount) external returns (uint64 mmAmount);
    }

    #[sol(rpc)]
    interface IRegistryView {
        function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
    }
}

/// `VaultStatus.Active` and `VaultStatus.Closed` (the same in the v2 and v3 Registries).
pub const VAULT_ACTIVE: u8 = 1;
pub const VAULT_CLOSED: u8 = 2;

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
        let raw: Value =
            serde_json::from_slice(&std::fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
        let c = |k: &str| -> Result<Address> {
            raw["contracts"][k]
                .as_str()
                .context(format!("missing contracts.{k}"))?
                .parse()
                .map_err(|e| anyhow!("{k}: {e}"))
        };
        let m = |k: &str| -> Result<String> {
            Ok(raw["market"][k].as_str().context(format!("missing market.{k}"))?.to_owned())
        };
        Ok(Self {
            registry: c("ZecuredRegistry")?,
            adapter: c("SettlementAdapter")?,
            usdc: m("loanToken")?.parse()?, // Circle USDC on Base Sepolia, MockUSDC on the local fork
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

/// Morpho's liquidation incentive constants (ConstantsLib).
const LIQUIDATION_CURSOR: u128 = 300_000_000_000_000_000;
const MAX_LIQUIDATION_INCENTIVE_FACTOR: u128 = 1_150_000_000_000_000_000;

/// What Morpho's math needs of the market, read once per pass: the borrow totals as last stored
/// (interest accrued since the market's last update is not in them), the oracle price and the LLTV.
#[derive(Clone, Copy, Debug)]
pub struct MarketState {
    pub total_borrow_assets: u128,
    pub total_borrow_shares: u128,
    pub price: u128,
    pub lltv: u128,
}

/// A borrower's position, in asset units, with Morpho's rounding.
#[derive(Clone, Copy, Debug)]
pub struct Position {
    pub borrow_shares: u128,
    /// The debt, rounded up (`toAssetsUp`, as Morpho's health check counts it).
    pub debt: u128,
    pub collateral: u128,
    pub price: u128,
    /// Maximum debt at the liquidation threshold.
    pub max_debt: u128,
    /// The debt as Morpho's liquidation counts it when every share is repaid (`toAssetsDown`).
    pub debt_down: u128,
    /// Morpho's liquidation incentive factor for this market, in WAD.
    pub incentive: u128,
}

impl Position {
    pub fn new(borrow_shares: u128, collateral: u128, m: &MarketState) -> Self {
        let (assets, shares) = (m.total_borrow_assets + VIRTUAL_ASSETS, m.total_borrow_shares + VIRTUAL_SHARES);
        let collateral_value = mul_div_down(collateral, m.price, ORACLE_PRICE_SCALE);
        Self {
            borrow_shares,
            debt: mul_div_up(borrow_shares, assets, shares),
            collateral,
            price: m.price,
            max_debt: mul_div_down(collateral_value, m.lltv, WAD),
            debt_down: mul_div_down(borrow_shares, assets, shares),
            incentive: liquidation_incentive_factor(m.lltv),
        }
    }

    pub fn healthy(&self) -> bool {
        self.debt <= self.max_debt
    }

    /// The collateral Morpho seizes for repaying every borrow share (`Morpho.liquidate` with
    /// `seizedAssets = 0`): the debt plus the incentive, at the oracle price. `None` at a zero
    /// price, where Morpho's division reverts.
    pub fn seized_for_all_shares(&self) -> Option<u128> {
        (self.price != 0)
            .then(|| mul_div_down(mul_div_down(self.debt_down, self.incentive, WAD), ORACLE_PRICE_SCALE, self.price))
    }
}

/// Morpho's liquidation incentive factor: `min(1.15, 1 / (1 - 0.3 * (1 - lltv)))`, in WAD, rounded
/// as Morpho rounds it (1.126760563380281690 at the market's 62.5 % LLTV).
pub fn liquidation_incentive_factor(lltv: u128) -> u128 {
    let lltv = lltv.min(WAD);
    let denominator = WAD - mul_div_down(LIQUIDATION_CURSOR, WAD - lltv, WAD);
    mul_div_down(WAD, WAD, denominator).min(MAX_LIQUIDATION_INCENTIVE_FACTOR)
}

/// How the liquidator calls `SettlementAdapter.liquidate` (review H-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liquidation {
    /// `liquidate(account, 0, shares)`: repay the whole debt. Morpho seizes what it is worth plus the
    /// incentive; the rest of the collateral stays in the vault, which stays open.
    RepayShares(u128),
    /// `liquidate(account, collateral, 0)`: the position is underwater (past 1/LIF, about 88.75 % LTV
    /// here), so repaying every share would seize more collateral than there is and Morpho would
    /// revert. Seize all of it instead; Morpho realizes the rest of the debt as bad debt and the vault
    /// closes.
    SeizeAll(u64),
}

impl Liquidation {
    /// `(seizedAssets, repaidShares)` for the Adapter.
    pub fn arguments(self) -> (U256, U256) {
        match self {
            Self::RepayShares(shares) => (U256::ZERO, U256::from(shares)),
            Self::SeizeAll(collateral) => (U256::from(collateral), U256::ZERO),
        }
    }
}

/// H-04: the liquidation of an unhealthy position, from the same state Morpho reads (`None` for a
/// healthy position or one without debt). `outstanding` is the vault's outstanding on the Registry,
/// which bounds what the Adapter lets Morpho seize; it equals the position's collateral (I1, I2).
///
/// Repaying every share works exactly when the collateral it seizes is no more than the collateral
/// there is; when it is more, Morpho underflows. With Morpho's rounding, whenever repaying every share
/// would seize too much, seizing all the collateral repays at most every share, so one of the two
/// always goes through at the state they are computed from.
pub fn plan_liquidation(pos: &Position, outstanding: u64) -> Option<Liquidation> {
    if pos.borrow_shares == 0 || pos.healthy() {
        return None;
    }
    let seizable = pos.collateral.min(u128::from(outstanding));
    if seizable == 0 {
        return None;
    }
    match pos.seized_for_all_shares() {
        Some(seized) if seized <= seizable => Some(Liquidation::RepayShares(pos.borrow_shares)),
        // `seizable <= outstanding`, a u64.
        _ => Some(Liquidation::SeizeAll(u64::try_from(seizable).ok()?)),
    }
}

/// The liquidations to try, in order. Morpho accrues interest before it liquidates, so the debt it
/// sees can exceed the stored debt the plan was made from: a position just short of underwater by
/// the plan may be underwater for Morpho. Seizing all the collateral is then what goes through, so
/// it is the fallback of a full repayment (the other way round cannot happen: interest only adds
/// debt).
pub fn liquidation_attempts(plan: Liquidation, pos: &Position, outstanding: u64) -> Vec<Liquidation> {
    let seizable = u64::try_from(pos.collateral.min(u128::from(outstanding))).unwrap_or(outstanding);
    match plan {
        Liquidation::RepayShares(_) if seizable > 0 => vec![plan, Liquidation::SeizeAll(seizable)],
        _ => vec![plan],
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

/// The market's borrow totals, the oracle price and the LLTV.
pub async fn market_state<P: Provider>(p: &P, d: &Deployment) -> Result<MarketState> {
    let mkt = IMorpho::new(d.morpho, p).market(d.market_id).call().await?;
    let price = IOracle::new(d.market.oracle, p).price().call().await?;
    Ok(MarketState {
        total_borrow_assets: mkt.totalBorrowAssets,
        total_borrow_shares: mkt.totalBorrowShares,
        price: u128::try_from(price).map_err(|_| anyhow!("price overflow"))?,
        lltv: u128::try_from(d.market.lltv).map_err(|_| anyhow!("lltv overflow"))?,
    })
}

/// `user`'s position in a market whose state was read with [`market_state`].
pub async fn position_in<P: Provider>(p: &P, d: &Deployment, m: &MarketState, user: Address) -> Result<Position> {
    let pos = IMorpho::new(d.morpho, p).position(d.market_id, user).call().await?;
    Ok(Position::new(pos.borrowShares, pos.collateral, m))
}

pub async fn position<P: Provider>(p: &P, d: &Deployment, user: Address) -> Result<Position> {
    let m = market_state(p, d).await?;
    position_in(p, d, &m, user).await
}

/// The raw 43-byte Orchard-protocol (Ironwood) receiver of the bot's unified address.
pub fn ironwood_receiver(zcash_address: &str) -> Result<[u8; 43]> {
    Ok(zecured_zcash_ops::keys::ironwood_receiver(&Network::TestNetwork, zcash_address)
        .context("the bot's Zcash address has no Ironwood (Orchard-protocol) receiver")?
        .to_raw_address_bytes())
}

/// `keccak256(salt32 || receiver43)` (docs/INTERFACES.md § 1).
pub fn commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> B256 {
    let mut preimage = salt.to_vec();
    preimage.extend_from_slice(receiver);
    keccak256(preimage)
}

/// RT-2: where the bot keeps the salt and receiver of every commitment it puts on Base, so that
/// it can check, and prove (F3), the payment of each of its claims (`zecured-wallet
/// watch-claims`). Set once at startup; the file is readable by its owner only.
static DESTINATIONS: OnceLock<PathBuf> = OnceLock::new();
static DESTINATIONS_LOCK: Mutex<()> = Mutex::new(());

pub fn keep_destinations_in(path: PathBuf) {
    let _ = DESTINATIONS.set(path);
}

/// Appends a destination (`zecured-wallet`'s `KeptDestination` layout) to the bot's file.
fn keep_destination(commitment: B256, salt: &[u8; 32], receiver: &[u8; 43]) -> Result<()> {
    let Some(path) = DESTINATIONS.get() else {
        return Ok(());
    };
    let _guard = DESTINATIONS_LOCK.lock().map_err(|_| anyhow!("destination file lock poisoned"))?;
    let mut kept: Vec<Value> = match fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw).context("the bot's destination file")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e).context("read the bot's destination file"),
    };
    kept.push(json!({ "commitment": commitment, "receiver": hex::encode(receiver), "salt": hex::encode(salt) }));
    write_private(path, &serde_json::to_vec_pretty(&kept)?)
}

/// Writes `data` to `path` atomically, readable by its owner only.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp)?;
    // `mode` only applies to a new file; a leftover temporary file keeps its permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Reveals a fresh (receiver, salt) for the bot's Ironwood address to the signers through
/// the backend and returns the salted commitment to put on Base, after keeping its salt (RT-2).
///
/// The commitment is computed here, and the backend's answer must be the same (M-11): a
/// backend that answered with a commitment to its own receiver would otherwise get the bot's
/// ZEC paid to it.
pub async fn fresh_commitment(backend: &str, zcash_address: &str, receiver: &[u8; 43]) -> Result<B256> {
    let mut salt = [0u8; 32];
    OsRng.fill_bytes(&mut salt);
    let expected = commitment(&salt, receiver);
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?
        .post(format!("{}/recipients", backend.trim_end_matches('/')))
        .json(&json!({ "receiver": zcash_address, "salt": hex::encode(salt) }))
        .send()
        .await
        .context("backend unreachable")?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("backend refused the recipient ({})", v["code"].as_str().unwrap_or("no code"));
    }
    let returned: B256 = v["commitment"].as_str().context("no commitment")?.parse().map_err(|e| anyhow!("{e}"))?;
    ensure!(returned == expected, "the backend answered with a commitment that is not ours; not using it");
    keep_destination(expected, &salt, receiver).context("keeping the commitment's salt")?;
    Ok(expected)
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
    use axum::{Json, Router, routing::post};

    use super::*;

    async fn backend(answer_for: [u8; 43]) -> String {
        let app = Router::new().route(
            "/recipients",
            post(move |Json(b): Json<Value>| async move {
                let salt: [u8; 32] = hex::decode(b["salt"].as_str().unwrap()).unwrap().try_into().unwrap();
                Json(json!({ "commitment": commitment(&salt, &answer_for) }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// M-11: a backend answering with a commitment to another receiver is caught.
    #[tokio::test]
    async fn the_backend_commitment_is_checked() {
        let ours = [7u8; 43];
        let honest = backend(ours).await;
        let c = fresh_commitment(&honest, "utest1ours", &ours).await.unwrap();
        assert_ne!(c, B256::ZERO);
        let malicious = backend([8u8; 43]).await;
        let e = fresh_commitment(&malicious, "utest1ours", &ours).await.unwrap_err();
        assert!(e.to_string().contains("not ours"), "{e}");
    }

    /// RT-2: the salt of every commitment the bot uses is kept, readable by its owner only, in the
    /// layout `zecured-wallet watch-claims` reads.
    #[tokio::test]
    async fn every_commitment_is_kept_with_its_salt() {
        let path = std::env::temp_dir().join(format!("zecured-bots-destinations-{}.json", std::process::id()));
        keep_destinations_in(path);
        let file = DESTINATIONS.get().unwrap();
        let ours = [9u8; 43];
        let c = fresh_commitment(&backend(ours).await, "utest1ours", &ours).await.unwrap();
        let kept: Vec<Value> = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let mine = kept.iter().find(|k| k["commitment"] == json!(c)).expect("the commitment is kept");
        let salt: [u8; 32] = hex::decode(mine["salt"].as_str().unwrap()).unwrap().try_into().unwrap();
        assert_eq!(commitment(&salt, &ours), c);
        assert_eq!(mine["receiver"], json!(hex::encode(ours)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_file(file);
    }

    #[test]
    fn commitment_is_keccak_of_salt_then_receiver() {
        let mut preimage = vec![1u8; 32];
        preimage.extend([2u8; 43]);
        assert_eq!(commitment(&[1u8; 32], &[2u8; 43]), keccak256(preimage));
    }

    #[test]
    fn rounding_matches_morpho() {
        assert_eq!(mul_div_up(10, 3, 4), 8);
        assert_eq!(mul_div_down(10, 3, 4), 7);
        // 0.01 ZEC (1e6 zat) at $40 (price = 40e34): value 40e34 * 1e6 / 1e36 = 0.4 USDC = 400_000 units.
        assert_eq!(mul_div_down(1_000_000, 40 * 10u128.pow(34), ORACLE_PRICE_SCALE), 400_000);
    }

    // ----- H-04: which liquidation the bot sends

    const LLTV: u128 = 625_000_000_000_000_000; // 62.5 %
    /// `price` in dollars per ZEC times 100 (cents), on Morpho's scale.
    const fn cents(c: u128) -> u128 {
        c * 10u128.pow(32)
    }

    /// The contracts' fork tests (ZecuredFixture): a 0.01 ZEC vault borrowing 40 % of $40 (160,000
    /// units) alone in a fresh market, so 160,000 assets for 1.6e11 shares.
    fn fork_test_loan(price: u128) -> Position {
        let market = MarketState {
            total_borrow_assets: 160_000,
            total_borrow_shares: 160_000 * VIRTUAL_SHARES,
            price,
            lltv: LLTV,
        };
        Position::new(160_000 * VIRTUAL_SHARES, 1_000_000, &market)
    }

    /// Morpho.liquidate's arithmetic and checks (morpho-blue/src/Morpho.sol), written out apart from
    /// the bot's: the collateral seized and the shares repaid, or why Morpho reverts.
    fn morpho_liquidate(
        borrow_shares: u128,
        collateral: u128,
        m: &MarketState,
        seized: u128,
        repaid_shares: u128,
    ) -> Result<(u128, u128), &'static str> {
        assert!((seized == 0) != (repaid_shares == 0), "INCONSISTENT_INPUT");
        let (assets, shares) = (m.total_borrow_assets + 1, m.total_borrow_shares + 1_000_000);
        let borrowed = mul_div_up(borrow_shares, assets, shares);
        let max_borrow = mul_div_down(mul_div_down(collateral, m.price, ORACLE_PRICE_SCALE), m.lltv, WAD);
        if max_borrow >= borrowed {
            return Err("HEALTHY_POSITION");
        }
        let cursor_term = mul_div_down(300_000_000_000_000_000, WAD - m.lltv, WAD);
        let lif = mul_div_down(WAD, WAD, WAD - cursor_term).min(1_150_000_000_000_000_000);
        let (seized, repaid_shares) = if seized > 0 {
            let quoted = mul_div_up(seized, m.price, ORACLE_PRICE_SCALE);
            (seized, mul_div_up(mul_div_up(quoted, WAD, lif), shares, assets))
        } else {
            if m.price == 0 {
                return Err("division by zero");
            }
            let repaid = mul_div_down(repaid_shares, assets, shares);
            (mul_div_down(mul_div_down(repaid, lif, WAD), ORACLE_PRICE_SCALE, m.price), repaid_shares)
        };
        if repaid_shares > borrow_shares {
            return Err("borrowShares underflow");
        }
        if seized > collateral {
            return Err("collateral underflow");
        }
        Ok((seized, repaid_shares))
    }

    fn run(pos: &Position, m: &MarketState, plan: Liquidation) -> Result<(u128, u128), &'static str> {
        let (seized, repaid) = plan.arguments();
        let (seized, repaid) = (u128::try_from(seized).unwrap(), u128::try_from(repaid).unwrap());
        morpho_liquidate(pos.borrow_shares, pos.collateral, m, seized, repaid)
    }

    /// A deterministic generator for the property test.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: u128) -> u128 {
            let mut draw = || {
                self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                u128::from(self.0 >> 11)
            };
            ((draw() << 53) | draw()) % bound
        }
    }

    #[test]
    fn the_incentive_factor_is_morphos() {
        assert_eq!(liquidation_incentive_factor(LLTV), 1_126_760_563_380_281_690);
        // The cap: a low LLTV would give more than 1.15.
        assert_eq!(liquidation_incentive_factor(0), MAX_LIQUIDATION_INCENTIVE_FACTOR);
        assert_eq!(liquidation_incentive_factor(WAD), WAD);
    }

    /// A healthy position (or one without debt) is left alone, up to Morpho's exact LLTV boundary.
    #[test]
    fn healthy_positions_are_not_liquidated() {
        assert_eq!(plan_liquidation(&fork_test_loan(cents(4_000)), 1_000_000), None);
        // At $25.60 the collateral is worth 256,000 units and 62.5 % of it is the debt exactly.
        assert_eq!(plan_liquidation(&fork_test_loan(cents(2_560)), 1_000_000), None);
        let just_below = fork_test_loan(cents(2_560) - 1);
        assert_eq!(plan_liquidation(&just_below, 1_000_000), Some(Liquidation::RepayShares(160_000 * VIRTUAL_SHARES)));
        let mut repaid = fork_test_loan(cents(1_000));
        repaid.borrow_shares = 0;
        assert_eq!(plan_liquidation(&repaid, 1_000_000), None);
    }

    /// Liquidatable but not underwater: the whole debt is repaid and part of the collateral seized,
    /// what the contracts' `test_liquidate_repayAllShares_partialSeize` asserts at $25.50.
    #[test]
    fn a_liquidatable_position_repays_every_share() {
        for (price, seized) in [(cents(2_550), 706_984), (cents(1_850), 974_491)] {
            let pos = fork_test_loan(price);
            let plan = plan_liquidation(&pos, 1_000_000).unwrap();
            assert_eq!(plan, Liquidation::RepayShares(160_000 * VIRTUAL_SHARES));
            assert_eq!(pos.seized_for_all_shares(), Some(seized));
            let market = MarketState {
                total_borrow_assets: 160_000,
                total_borrow_shares: 160_000 * VIRTUAL_SHARES,
                price,
                lltv: LLTV,
            };
            assert_eq!(run(&pos, &market, plan), Ok((seized, 160_000 * VIRTUAL_SHARES)));
        }
    }

    /// Underwater (past 1/LIF): repaying every share would seize more than the vault holds, which
    /// Morpho reverts (the review's repro at $17); seizing all of it goes through, as the contracts'
    /// `test_liquidate_wholeVault_closesTheVault` does at $18.
    #[test]
    fn an_underwater_position_seizes_all_the_collateral() {
        for price in [cents(1_800), cents(1_700), cents(100), 1] {
            let pos = fork_test_loan(price);
            let market = MarketState {
                total_borrow_assets: 160_000,
                total_borrow_shares: 160_000 * VIRTUAL_SHARES,
                price,
                lltv: LLTV,
            };
            assert_eq!(plan_liquidation(&pos, 1_000_000), Some(Liquidation::SeizeAll(1_000_000)));
            assert_eq!(
                run(&pos, &market, Liquidation::RepayShares(pos.borrow_shares)),
                Err("collateral underflow"),
                "what the bot used to send"
            );
            let (seized, repaid) = run(&pos, &market, Liquidation::SeizeAll(1_000_000)).unwrap();
            assert_eq!(seized, 1_000_000);
            assert!(repaid <= pos.borrow_shares, "the rest is bad debt");
        }
        // A zero price: Morpho cannot divide by it, but seizing everything needs no division.
        let pos = fork_test_loan(0);
        assert_eq!(plan_liquidation(&pos, 1_000_000), Some(Liquidation::SeizeAll(1_000_000)));
    }

    /// The boundary, with Morpho's rounding: repaying every share is chosen exactly while the collateral
    /// it seizes (1,001,561 at $18) is no more than the vault holds.
    #[test]
    fn the_underwater_boundary_follows_morphos_rounding() {
        let market = MarketState {
            total_borrow_assets: 160_000,
            total_borrow_shares: 160_000 * VIRTUAL_SHARES,
            price: cents(1_800),
            lltv: LLTV,
        };
        let shares = 160_000 * VIRTUAL_SHARES;
        let at = |collateral: u128| Position::new(shares, collateral, &market);
        assert_eq!(at(1_000_000).seized_for_all_shares(), Some(1_001_561));
        for collateral in [1_001_561, 1_001_562, 1_001_600] {
            let pos = at(collateral);
            let outstanding = u64::try_from(collateral).unwrap();
            assert_eq!(plan_liquidation(&pos, outstanding), Some(Liquidation::RepayShares(shares)));
            assert_eq!(run(&pos, &market, Liquidation::RepayShares(shares)), Ok((1_001_561, shares)));
        }
        for collateral in [1_001_560, 1_001_000] {
            let pos = at(collateral);
            let outstanding = u64::try_from(collateral).unwrap();
            assert_eq!(plan_liquidation(&pos, outstanding), Some(Liquidation::SeizeAll(outstanding)));
            assert_eq!(run(&pos, &market, Liquidation::RepayShares(shares)), Err("collateral underflow"));
            assert!(run(&pos, &market, Liquidation::SeizeAll(outstanding)).is_ok());
        }
        // The Registry's outstanding bounds the seizure too (the Adapter arms the zZEC transfer with it).
        let pos = at(1_001_561);
        assert_eq!(plan_liquidation(&pos, 1_001_560), Some(Liquidation::SeizeAll(1_001_560)));
        assert_eq!(plan_liquidation(&pos, 0), None);
    }

    /// Over many positions and prices: the planned liquidation always goes through Morpho's checks,
    /// and the whole collateral is seized only when repaying every share would revert.
    #[test]
    fn the_planned_liquidation_never_reverts_in_morpho() {
        let mut rng = Lcg(0x5eed_2026_1015);
        let (mut repaid_all, mut seized_all) = (0, 0);
        for _ in 0..20_000 {
            let total_assets = 1 + rng.below(10_000_000_000);
            // Share prices from Morpho's initial 1e-6 up to 2e-6 assets per share (accrued interest).
            let total_shares = (total_assets * VIRTUAL_SHARES * (1_000 - rng.below(500)) / 1_000).max(1);
            let borrow_shares = 1 + rng.below(total_shares);
            let collateral = 1 + rng.below(100_000_000_000);
            let debt = mul_div_up(borrow_shares, total_assets + VIRTUAL_ASSETS, total_shares + VIRTUAL_SHARES);
            // An LTV from 30 % to 150 % (healthy, liquidatable and underwater), or any price at all.
            let price = if rng.below(10) == 0 {
                1 + rng.below(cents(200_000))
            } else {
                let ltv_permille = 300 + rng.below(1_200);
                mul_div_down(debt * 1_000, ORACLE_PRICE_SCALE, ltv_permille * collateral).clamp(1, cents(1_000_000))
            };
            let market = MarketState {
                total_borrow_assets: total_assets,
                total_borrow_shares: total_shares,
                price,
                lltv: LLTV,
            };
            let pos = Position::new(borrow_shares, collateral, &market);
            let outstanding = u64::try_from(collateral).unwrap();
            let healthy =
                morpho_liquidate(borrow_shares, collateral, &market, 0, borrow_shares) == Err("HEALTHY_POSITION");
            match plan_liquidation(&pos, outstanding) {
                None => assert!(healthy, "{pos:?}"),
                Some(plan) => {
                    assert!(!healthy);
                    assert!(run(&pos, &market, plan).is_ok(), "{plan:?} reverts for {pos:?} at {market:?}");
                    if let Liquidation::SeizeAll(_) = plan {
                        assert!(run(&pos, &market, Liquidation::RepayShares(borrow_shares)).is_err());
                        seized_all += 1;
                    } else {
                        repaid_all += 1;
                    }
                }
            }
        }
        assert!(repaid_all > 1_000 && seized_all > 1_000, "both paths covered: {repaid_all}, {seized_all}");
    }

    /// Morpho accrues interest before it liquidates: a plan made from the stored debt that repays
    /// every share falls back on seizing everything, which then goes through.
    #[test]
    fn a_full_repayment_falls_back_on_seizing_everything() {
        let stored = MarketState {
            total_borrow_assets: 160_000,
            total_borrow_shares: 160_000 * VIRTUAL_SHARES,
            price: cents(1_800),
            lltv: LLTV,
        };
        let shares = 160_000 * VIRTUAL_SHARES;
        let pos = Position::new(shares, 1_001_561, &stored);
        let plan = plan_liquidation(&pos, 1_001_561).unwrap();
        assert_eq!(plan, Liquidation::RepayShares(shares));
        assert_eq!(liquidation_attempts(plan, &pos, 1_001_561), vec![plan, Liquidation::SeizeAll(1_001_561)]);
        // A few units of interest later, repaying every share seizes too much; seizing everything works.
        let accrued = MarketState {
            total_borrow_assets: 160_010,
            ..stored
        };
        assert_eq!(run(&pos, &accrued, plan), Err("collateral underflow"));
        assert!(run(&pos, &accrued, Liquidation::SeizeAll(1_001_561)).is_ok());
        // An underwater plan has no fallback to try.
        let under = Position::new(shares, 1_000_000, &stored);
        let plan = plan_liquidation(&under, 1_000_000).unwrap();
        assert_eq!(liquidation_attempts(plan, &under, 1_000_000), vec![Liquidation::SeizeAll(1_000_000)]);
    }
}
