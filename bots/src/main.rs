//! `zecured-bots`: the liquidator and the market maker used in the demo.
//!
//! Both roles are permissionless; anyone can run them. Each liquidation or close uses a
//! fresh salt, so the bot's commitments on Base cannot be linked to each other.

mod chain;

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, anyhow, ensure};
use axum::{
    Json, Router,
    extract::{
        Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info, warn};
use zecured_base::events::{self, RegistryEvent};
use zecured_zcash_ops::spend::zip317_fee;

use crate::chain::{CloseRequest, Deployment, IRegistryView, ISettlementAdapter, ORACLE_PRICE_SCALE};

#[derive(Parser)]
#[command(about = "Zecured demo bots")]
struct Cli {
    #[arg(
        long,
        env = "ZECURED_DEPLOYMENTS",
        default_value = "contracts/deployments/base-sepolia.json",
        global = true
    )]
    deployments: PathBuf,
    #[arg(
        long,
        env = "BASE_SEPOLIA_RPC_URL",
        default_value = "https://sepolia.base.org",
        global = true
    )]
    rpc: String,
    #[arg(
        long,
        env = "ZECURED_BACKEND",
        default_value = "http://127.0.0.1:8080",
        global = true
    )]
    backend: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Liquidate unhealthy Zecured positions through the SettlementAdapter.
    Liquidator {
        #[arg(long, env = "BOT_LIQUIDATOR_PRIVATE_KEY", hide_env_values = true)]
        key: String,
        /// The bot's Ironwood-only unified address (receives the seized ZEC).
        #[arg(long, env = "BOT_LIQUIDATOR_ZCASH_ADDRESS")]
        zcash_address: String,
        #[arg(long, default_value_t = 15)]
        every_secs: u64,
    },
    /// Quote and execute close-from-collateral requests.
    MarketMaker {
        #[arg(long, env = "BOT_MARKET_MAKER_PRIVATE_KEY", hide_env_values = true)]
        key: String,
        #[arg(long, env = "BOT_MARKET_MAKER_ZCASH_ADDRESS")]
        zcash_address: String,
        #[arg(long, default_value = "127.0.0.1:8090")]
        bind: SocketAddr,
        /// Margin over the oracle price taken in ZEC, in basis points.
        #[arg(long, default_value_t = 100)]
        margin_bps: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let d = Deployment::load(&cli.deployments)?;
    match cli.cmd {
        Cmd::Liquidator {
            key,
            zcash_address,
            every_secs,
        } => liquidator(&cli.rpc, &cli.backend, d, key.parse()?, &zcash_address, every_secs).await,
        Cmd::MarketMaker {
            key,
            zcash_address,
            bind,
            margin_bps,
        } => market_maker(cli.rpc, cli.backend, d, key.parse()?, zcash_address, bind, margin_bps).await,
    }
}

// ------------------------------------------------------------------------- liquidator

async fn liquidator(
    rpc: &str,
    backend: &str,
    d: Deployment,
    key: PrivateKeySigner,
    zaddr: &str,
    every: u64,
) -> Result<()> {
    let me = key.address();
    let receiver = chain::ironwood_receiver(zaddr)?;
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    chain::ensure_allowance(&provider, d.usdc, me, d.adapter).await?;
    let mut accounts: BTreeMap<B256, Address> = BTreeMap::new();
    let mut cursor = d.block;
    info!("liquidator running");
    loop {
        if let Err(e) = liquidation_pass(&provider, backend, &d, zaddr, &receiver, &mut accounts, &mut cursor).await {
            warn!("liquidation pass failed: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(every)).await;
    }
}

async fn liquidation_pass<P: Provider>(
    p: &P,
    backend: &str,
    d: &Deployment,
    zaddr: &str,
    receiver: &[u8; 43],
    accounts: &mut BTreeMap<B256, Address>,
    cursor: &mut u64,
) -> Result<()> {
    let head = p.get_block_number().await?;
    if head > *cursor {
        for ev in events::fetch(p, d.registry, *cursor + 1, head).await? {
            if let RegistryEvent::VaultActivated { account, .. } = ev.event {
                accounts.insert(ev.vault_id, account);
            }
        }
        *cursor = head;
    }
    for (vault_id, account) in accounts.clone() {
        let pos = chain::position(p, d, account).await?;
        if pos.borrow_shares == 0 || pos.healthy() {
            continue;
        }
        info!(vault = %hex::encode(&vault_id[..4]), "unhealthy position: liquidating");
        let commitment = chain::fresh_commitment(backend, zaddr, receiver).await?;
        let adapter = ISettlementAdapter::new(d.adapter, p);
        let call = adapter.liquidate(account, U256::ZERO, U256::from(pos.borrow_shares), commitment);
        // Simulate first: the adapter enforces the dust floor and may revert.
        if let Err(e) = call.call().await {
            warn!("liquidation would revert: {e}");
            continue;
        }
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await?);
        let receipt = call.gas(gas).send().await?.get_receipt().await?;
        ensure!(receipt.status(), "liquidation reverted");
        info!(tx = %receipt.transaction_hash, "liquidated");
        if pos.collateral > 0 {
            accounts.remove(&vault_id);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------- market maker

struct Mm {
    d: Deployment,
    rpc: String,
    backend: String,
    key: PrivateKeySigner,
    zaddr: String,
    /// Raw Ironwood receiver of `zaddr`, to check the commitments the backend returns (M-11).
    receiver: [u8; 43],
    margin_bps: u16,
    /// Quotes given, by vault. A close repays at most the quoted debt plus `REPAY_SLACK_BPS`
    /// (contracts review L-2: the borrower may borrow more after signing) and takes at least the
    /// quoted ZEC minus `PRICE_SLACK_BPS` (Y7: the price may rise before the close lands).
    quotes: std::sync::Mutex<std::collections::HashMap<B256, Quote>>,
}

#[derive(Clone, Copy, Debug)]
struct Quote {
    debt: u128,
    mm_amount: u64,
    /// Unix seconds.
    expires_at: u64,
}

/// Interest that may accrue between a quote and the close transaction (10 minutes at most),
/// in basis points of the quoted debt.
const REPAY_SLACK_BPS: u128 = 10;
/// How far the ZEC price may rise between a quote and the close before the market maker
/// refuses it (Y7), in basis points of the quoted ZEC.
const PRICE_SLACK_BPS: u64 = 100;

/// Y7: the fewest zatoshi the market maker accepts for a quote's USDC.
fn min_mm_amount(quoted: u64) -> u64 {
    // At most 2^64 * 10^4 < 2^128: no overflow in u128.
    u64::try_from(u128::from(quoted) * u128::from(10_000 - PRICE_SLACK_BPS) / 10_000).unwrap_or(u64::MAX)
}

/// Smallest borrower share of a close the market maker takes on (H-3).
///
/// The signers pay a close as one transaction whose fee the borrower's output bears: one
/// vault note in, and up to three outputs (borrower, market maker, and change when earlier
/// claims are still unpaid in the vault), i.e. `zip317_fee(1, 3)` = 15,000 zat. A borrower
/// share at or below that is dust: the signers keep it in the vault, and the market maker's
/// claim, which is paid together with (or after) the borrower's, would never be paid.
///
/// Remaining risk, not checkable here: a borrower can sign a close whose `userCommitment` was
/// never revealed to the signers. Its claim is then set aside and, until the signers' ledger
/// pays the market maker alone after the grace period, the market maker's claim waits too.
fn min_close_user_amount() -> u64 {
    zip317_fee(1, 3)
}

/// An error of the market maker API: a fixed message and a short code (the backend relays
/// only known codes, with its own messages).
#[derive(Debug, PartialEq, Eq)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    const fn refuse(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message,
        }
    }

    const BAD_REQUEST: Self = Self {
        status: StatusCode::BAD_REQUEST,
        code: "bad_request",
        message: "Invalid request.",
    };
    const NO_DEBT: Self = Self::refuse("no_debt", "There is no debt to close.");
    const UNDERWATER: Self = Self::refuse("underwater", "The position is underwater.");
    const USER_AMOUNT_TOO_SMALL: Self =
        Self::refuse("user_amount_too_small", "The borrower's share would not cover the Zcash payout fee.");
    const QUOTE_MISMATCH: Self = Self::refuse("quote_mismatch", "The request does not match our quote.");
    const CLOSE_REJECTED: Self = Self::refuse("close_rejected", "The close would revert.");
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        debug!("market maker request failed: {e:#}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: "Internal error.",
        }
    }
}

async fn market_maker(
    rpc: String,
    backend: String,
    d: Deployment,
    key: PrivateKeySigner,
    zaddr: String,
    bind: SocketAddr,
    margin_bps: u16,
) -> Result<()> {
    let receiver = chain::ironwood_receiver(&zaddr)?;
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(key.clone())).connect_http(rpc.parse()?);
    chain::ensure_allowance(&provider, d.usdc, key.address(), d.adapter).await?;
    let mm = Arc::new(Mm {
        d,
        rpc,
        backend,
        key,
        zaddr,
        receiver,
        margin_bps,
        quotes: Default::default(),
    });
    let app = Router::new().route("/quote/{vault_id}", get(quote)).route("/close", post(close)).with_state(mm);
    info!(%bind, "market maker listening");
    axum::serve(tokio::net::TcpListener::bind(bind).await?, app).await?;
    Ok(())
}

/// `ceil(a * b / d)` over 256 bits.
fn mul_div_up(a: U256, b: U256, d: U256) -> U256 {
    (a * b + d - U256::from(1u8)) / d
}

async fn quote(
    State(mm): State<Arc<Mm>>,
    vault_id: Result<Path<B256>, PathRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Ok(Path(vault_id)) = vault_id else {
        return Err(ApiError::BAD_REQUEST);
    };
    let p = ProviderBuilder::new().connect_http(mm.rpc.parse().map_err(anyhow::Error::from)?);
    let v = IRegistryView::new(mm.d.registry, &p).vault(vault_id).call().await.map_err(anyhow::Error::from)?;
    let pos = chain::position(&p, &mm.d, v.account).await?;
    if pos.debt == 0 {
        return Err(ApiError::NO_DEBT);
    }
    // Same formula as the adapter (INTERFACES § 3.3), on the current debt.
    let mm_amount = mul_div_up(
        U256::from(pos.debt) * U256::from(10_000u32 + u32::from(mm.margin_bps)),
        U256::from(ORACLE_PRICE_SCALE),
        U256::from(10_000u32) * U256::from(pos.price),
    );
    let mm_amount = u64::try_from(mm_amount).map_err(|_| anyhow!("amount overflow"))?;
    let user_amount = v.outstanding.checked_sub(mm_amount).ok_or(ApiError::UNDERWATER)?;
    check_user_amount(user_amount)?;
    let expires_at = zecured_now() + 600;
    mm.quotes.lock().map_err(|_| anyhow!("quote book poisoned"))?.insert(
        vault_id,
        Quote {
            debt: pos.debt,
            mm_amount,
            expires_at,
        },
    );
    Ok(Json(json!({
        "executor": mm.key.address(),
        "marginBps": mm.margin_bps,
        "repayAssets": pos.debt.to_string(),
        "mmAmount": mm_amount.to_string(),
        "userAmount": user_amount.to_string(),
        "expiresAt": expires_at,
    })))
}

/// H-3: refuses a close whose borrower share would be dust for the signers.
fn check_user_amount(user_amount: u64) -> Result<(), ApiError> {
    if user_amount <= min_close_user_amount() {
        return Err(ApiError::USER_AMOUNT_TOO_SMALL);
    }
    Ok(())
}

fn zecured_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseBody {
    request: CloseRequestJson,
    signature: Bytes,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseRequestJson {
    vault_id: B256,
    account: Address,
    executor: Address,
    margin_bps: u16,
    min_user_amount: serde_json::Value,
    user_commitment: B256,
    nonce: serde_json::Value,
    deadline: serde_json::Value,
}

fn num(v: &serde_json::Value) -> Result<U256> {
    match v {
        serde_json::Value::String(s) => s.parse().map_err(|e| anyhow!("{e}")),
        serde_json::Value::Number(n) => Ok(U256::from(n.as_u64().context("not an integer")?)),
        _ => Err(anyhow!("expected a number")),
    }
}

async fn close(
    State(mm): State<Arc<Mm>>,
    body: Result<Json<CloseBody>, JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Ok(Json(b)) = body else {
        return Err(ApiError::BAD_REQUEST);
    };
    let r = &b.request;
    if r.executor != mm.key.address() || r.margin_bps != mm.margin_bps {
        return Err(ApiError::QUOTE_MISMATCH);
    }
    let field = |v: &serde_json::Value| num(v).map_err(|_| ApiError::BAD_REQUEST);
    let small = |v: U256| u64::try_from(v).map_err(|_| ApiError::BAD_REQUEST);
    let req = CloseRequest {
        vaultId: r.vault_id,
        account: r.account,
        executor: r.executor,
        marginBps: r.margin_bps,
        minUserAmount: small(field(&r.min_user_amount)?)?,
        userCommitment: r.user_commitment,
        nonce: field(&r.nonce)?,
        deadline: small(field(&r.deadline)?)?,
    };
    // The adapter guarantees the borrower at least `minUserAmount`, so this bounds the
    // executed share, not just the quoted one.
    check_user_amount(req.minUserAmount)?;
    let mm_commitment = chain::fresh_commitment(&mm.backend, &mm.zaddr, &mm.receiver).await?;
    let p = ProviderBuilder::new()
        .wallet(EthereumWallet::from(mm.key.clone()))
        .connect_http(mm.rpc.parse().map_err(anyhow::Error::from)?);
    // Repay at most what we quoted for this vault (plus interest since), never what the borrower
    // may have borrowed after signing.
    let quoted = mm.quotes.lock().map_err(|_| anyhow!("quote book poisoned"))?.get(&r.vault_id).copied();
    let Some(quote) = quoted else {
        return Err(ApiError::QUOTE_MISMATCH);
    };
    if zecured_now() > quote.expires_at {
        return Err(ApiError::QUOTE_MISMATCH);
    }
    let max_repay = U256::from(quote.debt) * U256::from(10_000 + REPAY_SLACK_BPS) / U256::from(10_000u32);
    let adapter = ISettlementAdapter::new(mm.d.adapter, &p);
    let call =
        adapter.closeWithCollateral(req, b.signature.clone(), mm_commitment, max_repay, min_mm_amount(quote.mm_amount));
    if let Err(e) = call.call().await {
        debug!("close would revert: {e}");
        return Err(ApiError::CLOSE_REJECTED);
    }
    let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.map_err(anyhow::Error::from)?);
    let receipt = call
        .gas(gas)
        .send()
        .await
        .map_err(anyhow::Error::from)?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .map_err(anyhow::Error::from)?;
    if !receipt.status() {
        return Err(ApiError::CLOSE_REJECTED);
    }
    info!(tx = %receipt.transaction_hash, "closed from collateral");
    Ok(Json(json!({ "txHash": receipt.transaction_hash })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H-3: a borrower share that would be dust for the signers is refused, at quote time
    /// and on the signed minimum at execution time.
    #[test]
    fn dust_borrower_shares_are_refused() {
        assert_eq!(min_close_user_amount(), 15_000);
        assert_eq!(check_user_amount(0), Err(ApiError::USER_AMOUNT_TOO_SMALL));
        assert_eq!(check_user_amount(10_000), Err(ApiError::USER_AMOUNT_TOO_SMALL));
        assert_eq!(check_user_amount(15_000), Err(ApiError::USER_AMOUNT_TOO_SMALL));
        assert_eq!(check_user_amount(15_001), Ok(()));
    }

    /// Y7: the close takes at least the quoted ZEC minus 1 %.
    #[test]
    fn the_zec_floor_allows_a_one_percent_price_rise() {
        assert_eq!(min_mm_amount(3_800_000), 3_762_000);
        assert_eq!(min_mm_amount(0), 0);
        assert_eq!(min_mm_amount(u64::MAX), 18_262_276_632_972_456_098); // floor((2^64 - 1) * 0.99)
    }

    #[test]
    fn api_errors_are_fixed() {
        let e = ApiError::from(anyhow!("error sending request for url (https://rpc.example/v2/SECRETKEY)"));
        assert_eq!((e.code, e.message), ("internal", "Internal error."));
    }
}
