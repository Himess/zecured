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
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};
use zecured_base::events::{self, RegistryEvent};

use crate::chain::{CloseRequest, Deployment, IRegistryView, ISettlementAdapter, ORACLE_PRICE_SCALE};

#[derive(Parser)]
#[command(about = "Zecured demo bots")]
struct Cli {
    #[arg(long, env = "ZECURED_DEPLOYMENTS", default_value = "contracts/deployments/base-sepolia.json", global = true)]
    deployments: PathBuf,
    #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org", global = true)]
    rpc: String,
    #[arg(long, env = "ZECURED_BACKEND", default_value = "http://127.0.0.1:8080", global = true)]
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
        Cmd::Liquidator { key, zcash_address, every_secs } => {
            liquidator(&cli.rpc, &cli.backend, d, key.parse()?, &zcash_address, every_secs).await
        }
        Cmd::MarketMaker { key, zcash_address, bind, margin_bps } => {
            market_maker(cli.rpc, cli.backend, d, key.parse()?, zcash_address, bind, margin_bps).await
        }
    }
}

// ------------------------------------------------------------------------- liquidator

async fn liquidator(rpc: &str, backend: &str, d: Deployment, key: PrivateKeySigner, zaddr: &str, every: u64) -> Result<()> {
    let me = key.address();
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(rpc.parse()?);
    chain::ensure_allowance(&provider, d.usdc, me, d.adapter).await?;
    let mut accounts: BTreeMap<B256, Address> = BTreeMap::new();
    let mut cursor = d.block;
    info!("liquidator running");
    loop {
        if let Err(e) = liquidation_pass(&provider, backend, &d, zaddr, &mut accounts, &mut cursor).await {
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
        let commitment = chain::fresh_commitment(backend, zaddr).await?;
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
    margin_bps: u16,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}"))
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
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(key.clone())).connect_http(rpc.parse()?);
    chain::ensure_allowance(&provider, d.usdc, key.address(), d.adapter).await?;
    let mm = Arc::new(Mm {
        d,
        rpc,
        backend,
        key,
        zaddr,
        margin_bps,
    });
    let app = Router::new()
        .route("/quote/{vault_id}", get(quote))
        .route("/close", post(close))
        .with_state(mm);
    info!(%bind, "market maker listening");
    axum::serve(tokio::net::TcpListener::bind(bind).await?, app).await?;
    Ok(())
}

/// `ceil(a * b / d)` over 256 bits.
fn mul_div_up(a: U256, b: U256, d: U256) -> U256 {
    (a * b + d - U256::from(1u8)) / d
}

async fn quote(State(mm): State<Arc<Mm>>, Path(vault_id): Path<B256>) -> Result<Json<serde_json::Value>, ApiError> {
    let p = ProviderBuilder::new().connect_http(mm.rpc.parse().map_err(anyhow::Error::from)?);
    let v = IRegistryView::new(mm.d.registry, &p).vault(vault_id).call().await.map_err(anyhow::Error::from)?;
    let pos = chain::position(&p, &mm.d, v.account).await?;
    if pos.debt == 0 {
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, "no debt to close".into()));
    }
    // Same formula as the adapter (INTERFACES § 3.3), on the current debt.
    let mm_amount = mul_div_up(
        U256::from(pos.debt) * U256::from(10_000u32 + u32::from(mm.margin_bps)),
        U256::from(ORACLE_PRICE_SCALE),
        U256::from(10_000u32) * U256::from(pos.price),
    );
    let mm_amount = u64::try_from(mm_amount).map_err(|_| anyhow!("amount overflow"))?;
    let user_amount = v.outstanding.checked_sub(mm_amount).context("position is underwater")?;
    Ok(Json(json!({
        "executor": mm.key.address(),
        "marginBps": mm.margin_bps,
        "repayAssets": pos.debt.to_string(),
        "mmAmount": mm_amount.to_string(),
        "userAmount": user_amount.to_string(),
        "expiresAt": zecured_now() + 600,
    })))
}

fn zecured_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

async fn close(State(mm): State<Arc<Mm>>, Json(b): Json<CloseBody>) -> Result<Json<serde_json::Value>, ApiError> {
    let r = &b.request;
    if r.executor != mm.key.address() || r.margin_bps != mm.margin_bps {
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, "request does not match our quote".into()));
    }
    let req = CloseRequest {
        vaultId: r.vault_id,
        account: r.account,
        executor: r.executor,
        marginBps: r.margin_bps,
        minUserAmount: u64::try_from(num(&r.min_user_amount)?).map_err(|_| anyhow!("minUserAmount"))?,
        userCommitment: r.user_commitment,
        nonce: num(&r.nonce)?,
        deadline: u64::try_from(num(&r.deadline)?).map_err(|_| anyhow!("deadline"))?,
    };
    let mm_commitment = chain::fresh_commitment(&mm.backend, &mm.zaddr).await?;
    let p = ProviderBuilder::new()
        .wallet(EthereumWallet::from(mm.key.clone()))
        .connect_http(mm.rpc.parse().map_err(anyhow::Error::from)?);
    let adapter = ISettlementAdapter::new(mm.d.adapter, &p);
    let call = adapter.closeWithCollateral(req, b.signature.clone(), mm_commitment);
    call.call().await.map_err(|e| anyhow!("close would revert: {e}"))?;
    let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.map_err(anyhow::Error::from)?);
    let receipt = call.gas(gas).send().await.map_err(anyhow::Error::from)?.get_receipt().await.map_err(anyhow::Error::from)?;
    if !receipt.status() {
        return Err(ApiError(StatusCode::BAD_GATEWAY, "close reverted".into()));
    }
    info!(tx = %receipt.transaction_hash, "closed from collateral");
    Ok(Json(json!({ "txHash": receipt.transaction_hash })))
}
