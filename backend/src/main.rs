//! `zecured-backend`: the public API of the coordinator (docs/INTERFACES.md § 4).
//!
//! Privacy: nothing here logs addresses, salts, viewing keys or client IPs; errors returned
//! to clients describe the failed check only.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};
use zecured_coordinator::{Coordinator, OpenVaultRequest};
use zecured_node::config::NetworkConfig;

mod allowlist;
mod proxy;

#[derive(Parser)]
#[command(about = "Zecured backend: coordinator + public API")]
struct Cli {
    #[arg(long, env = "ZECURED_NETWORK_CONFIG", default_value = "signer/data/network.json")]
    config: PathBuf,
    #[arg(long, env = "ZECURED_COORDINATOR_DATA", default_value = "signer/data/coordinator")]
    data: PathBuf,
    #[arg(long, env = "BACKEND_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(long, env = "COORDINATOR_PRIVATE_KEY", hide_env_values = true)]
    coordinator_key: String,
    /// Deployment file written by `contracts/script/DeployZecured.s.sol` (enables sponsorship).
    #[arg(long, env = "ZECURED_DEPLOYMENTS", default_value = "contracts/deployments/base-sepolia.json")]
    deployments: PathBuf,
    #[arg(long, env = "BUNDLER_URL", hide_env_values = true, default_value = "")]
    bundler_url: String,
    #[arg(long, env = "PAYMASTER_URL", hide_env_values = true, default_value = "")]
    paymaster_url: String,
    /// The market maker's quote service (close from collateral).
    #[arg(long, env = "MARKET_MAKER_URL", default_value = "http://127.0.0.1:8090")]
    market_maker_url: String,
    /// Owner key of the MOCK price oracle, for the demo control panel's price slider.
    #[arg(long, env = "DEPLOYER_PRIVATE_KEY", hide_env_values = true, default_value = "")]
    oracle_owner_key: String,
    /// Token protecting the demo control endpoints (empty = disabled).
    #[arg(long, env = "DEMO_ADMIN_TOKEN", hide_env_values = true, default_value = "")]
    admin_token: String,
    /// Seconds between coordinator passes.
    #[arg(long, default_value_t = 10)]
    tick_secs: u64,
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

type ApiResult = Result<Json<serde_json::Value>, ApiError>;
type App = State<Arc<Coordinator>>;

fn salt(hex_salt: &str) -> Result<[u8; 32]> {
    hex::decode(hex_salt.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("salt must be 32 bytes"))
}

/// Accepts decimal strings or JSON numbers for amounts and nonces.
fn num(v: &serde_json::Value) -> Result<u64> {
    match v {
        serde_json::Value::String(s) => s.parse().context("not a decimal number"),
        serde_json::Value::Number(n) => n.as_u64().context("not an unsigned integer"),
        _ => Err(anyhow!("expected a number")),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenBody {
    owner: Address,
    account: Address,
    amount: serde_json::Value,
    request_id: B256,
    deadline: serde_json::Value,
    signature: Bytes,
}

async fn open_vault(State(c): App, Json(b): Json<OpenBody>) -> ApiResult {
    let r = c
        .open_vault(OpenVaultRequest {
            owner: b.owner,
            account: b.account,
            amount: num(&b.amount)?,
            request_id: b.request_id,
            deadline: num(&b.deadline)?,
            signature: b.signature,
        })
        .await?;
    Ok(Json(json!({
        "vaultId": r.vault_id,
        "address": r.address,
        "zip321": r.zip321,
        "amount": r.amount.to_string(),
        "expiryHeight": r.expiry_height,
    })))
}

async fn get_vault(State(c): App, Path(id): Path<B256>) -> ApiResult {
    let v = c.view(id).await?;
    Ok(Json(json!({
        "vaultId": v.vault_id,
        "status": v.status,
        "amount": v.amount.to_string(),
        "address": v.address,
        "expiryHeight": v.expiry_height,
        "outstanding": v.outstanding.to_string(),
        "payoutInFlight": v.inflight,
        "birthdayHeight": v.birthday_height,
        "tipHeight": v.tip_height,
        "nonConforming": v.non_conforming.iter().map(|n| json!({ "noteId": n.note_id, "amount": n.amount.to_string() })).collect::<Vec<_>>(),
        "claims": v.claims.iter().map(|c| json!({ "claimId": c.claim_id, "kind": c.kind, "amount": c.amount.to_string(), "paid": c.paid })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DestinationBody {
    receiver: String,
    salt: String,
    nonce: serde_json::Value,
    deadline: serde_json::Value,
    signature: Bytes,
}

async fn destination(State(c): App, Path(id): Path<B256>, Json(b): Json<DestinationBody>) -> ApiResult {
    let commitment = c
        .add_destination(id, &b.receiver, salt(&b.salt)?, num(&b.nonce)?, num(&b.deadline)?, b.signature)
        .await?;
    Ok(Json(json!({ "commitment": commitment })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefundBody {
    note_id: B256,
    receiver: String,
    salt: String,
    nonce: serde_json::Value,
    deadline: serde_json::Value,
    signature: Bytes,
}

async fn refund(State(c): App, Path(id): Path<B256>, Json(b): Json<RefundBody>) -> ApiResult {
    c.request_refund(
        id,
        b.note_id,
        &b.receiver,
        salt(&b.salt)?,
        num(&b.nonce)?,
        num(&b.deadline)?,
        b.signature,
    )
    .await?;
    Ok(Json(json!({ "accepted": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewKeyBody {
    nonce: serde_json::Value,
    deadline: serde_json::Value,
    signature: Bytes,
}

async fn viewing_key(State(c): App, Path(id): Path<B256>, Json(b): Json<ViewKeyBody>) -> ApiResult {
    let ufvk = c.viewing_key(id, num(&b.nonce)?, num(&b.deadline)?, &b.signature).await?;
    Ok(Json(json!({ "ufvk": ufvk })))
}

#[derive(Deserialize)]
struct RecipientBody {
    receiver: String,
    salt: String,
}

async fn recipients(State(c): App, Json(b): Json<RecipientBody>) -> ApiResult {
    let commitment = c.add_recipient(&b.receiver, salt(&b.salt)?).await?;
    Ok(Json(json!({ "commitment": commitment })))
}

#[derive(Clone)]
struct AdminToken(Arc<String>);

fn check_admin(token: &AdminToken, headers: &axum::http::HeaderMap) -> Result<(), ApiError> {
    let given = headers.get("x-admin-token").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if token.0.is_empty() || given != token.0.as_str() {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct DrainBody {
    to: String,
}

async fn admin_drain(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    headers: axum::http::HeaderMap,
    Path(id): Path<B256>,
    Json(b): Json<DrainBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    Ok(Json(serde_json::to_value(c.demo_drain(id, &b.to).await?).map_err(anyhow::Error::from)?))
}

#[derive(Deserialize)]
struct ModeBody {
    mode: String,
}

async fn admin_mode(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    headers: axum::http::HeaderMap,
    Path(index): Path<u16>,
    Json(b): Json<ModeBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    Ok(Json(c.set_node_mode(index, &b.mode, &t.0).await?))
}

async fn admin_nodes(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    headers: axum::http::HeaderMap,
) -> ApiResult {
    check_admin(&t, &headers)?;
    Ok(Json(json!({ "nodes": c.node_health().await })))
}

#[derive(Clone)]
struct MarketMaker(Arc<String>);

/// Relays a market maker call so the browser never contacts the market maker directly.
async fn mm_forward(mm: &MarketMaker, req: reqwest::RequestBuilder) -> ApiResult {
    let _ = mm;
    let resp = req.send().await.map_err(|_| anyhow!("market maker unreachable"))?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|_| anyhow!("market maker returned invalid JSON"))?;
    if !status.is_success() {
        return Err(ApiError(StatusCode::BAD_GATEWAY, v["error"].as_str().unwrap_or("market maker refused").to_owned()));
    }
    Ok(Json(v))
}

async fn close_quote(axum::Extension(mm): axum::Extension<MarketMaker>, Path(id): Path<B256>) -> ApiResult {
    let req = reqwest::Client::new().get(format!("{}/quote/{id}", mm.0));
    mm_forward(&mm, req).await
}

async fn close_submit(axum::Extension(mm): axum::Extension<MarketMaker>, Json(b): Json<serde_json::Value>) -> ApiResult {
    let req = reqwest::Client::new().post(format!("{}/close", mm.0)).json(&b);
    mm_forward(&mm, req).await
}

#[derive(Clone)]
struct Oracle {
    rpc: String,
    address: Option<Address>,
    owner_key: String,
}

alloy::sol! {
    #[sol(rpc)]
    interface IMockZecOracle {
        function price() external view returns (uint256);
        function setPrice(uint256 newPrice) external;
    }
}

/// MockZecOracle scale: price = ZEC/USD * 1e34 (Morpho scale 1e36, 6-decimal loan, 8-decimal collateral).
const ORACLE_SCALE_PER_CENT: u128 = 100_000_000_000_000_000_000_000_000_000_000;

async fn admin_get_oracle(
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(o): axum::Extension<Oracle>,
    headers: axum::http::HeaderMap,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let address = o.address.ok_or_else(|| anyhow!("no deployment"))?;
    let p = alloy::providers::ProviderBuilder::new().connect_http(o.rpc.parse().map_err(anyhow::Error::from)?);
    let raw = IMockZecOracle::new(address, &p).price().call().await.map_err(anyhow::Error::from)?;
    let cents = raw / alloy::primitives::U256::from(ORACLE_SCALE_PER_CENT);
    Ok(Json(json!({ "priceUsdCents": cents.to_string(), "raw": raw.to_string() })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OracleBody {
    price_usd_cents: u64,
}

async fn admin_set_oracle(
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(o): axum::Extension<Oracle>,
    headers: axum::http::HeaderMap,
    Json(b): Json<OracleBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let address = o.address.ok_or_else(|| anyhow!("no deployment"))?;
    let key: alloy::signers::local::PrivateKeySigner =
        o.owner_key.parse().map_err(|_| anyhow!("oracle owner key not configured"))?;
    let p = alloy::providers::ProviderBuilder::new()
        .wallet(alloy::network::EthereumWallet::from(key))
        .connect_http(o.rpc.parse().map_err(anyhow::Error::from)?);
    let raw = alloy::primitives::U256::from(b.price_usd_cents) * alloy::primitives::U256::from(ORACLE_SCALE_PER_CENT);
    let receipt = IMockZecOracle::new(address, &p)
        .setPrice(raw)
        .send()
        .await
        .map_err(anyhow::Error::from)?
        .get_receipt()
        .await
        .map_err(anyhow::Error::from)?;
    if !receipt.status() {
        return Err(ApiError(StatusCode::BAD_GATEWAY, "setPrice reverted".into()));
    }
    Ok(Json(json!({ "priceUsdCents": b.price_usd_cents.to_string(), "txHash": receipt.transaction_hash })))
}

#[derive(Clone)]
struct PublicConfig(Arc<serde_json::Value>);

/// Public deployment parameters, so the app and the signers cannot drift apart.
async fn public_config(axum::Extension(c): axum::Extension<PublicConfig>) -> Json<serde_json::Value> {
    Json((*c.0).clone())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

/// `GET /config`: chain, contract addresses, market and signer-network parameters.
fn public_config_json(cfg: &NetworkConfig, deployments: &std::path::Path) -> serde_json::Value {
    let dep: serde_json::Value = std::fs::read(deployments)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or(serde_json::Value::Null);
    json!({
        "chainId": cfg.chain_id,
        "registry": cfg.registry,
        "adapter": cfg.adapter,
        "contracts": dep.get("contracts").cloned().unwrap_or(serde_json::Value::Null),
        "market": dep.get("market").cloned().unwrap_or(serde_json::Value::Null),
        "denominations": cfg.denominations.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
        "confirmations": cfg.confirmations,
        "vaultExpiryBlocks": cfg.vault_expiry_blocks,
    })
}

/// Reads the allowlist parameters from the deployment file written by the deploy script.
fn load_allowlist(path: &std::path::Path) -> Option<allowlist::Allowlist> {
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let contract = |name: &str| raw.get("contracts")?.get(name)?.as_str()?.parse::<Address>().ok();
    let market = |name: &str| raw.get("market")?.get(name)?.as_str().map(str::to_owned);
    Some(allowlist::Allowlist {
        morpho: market("morphoBlue")?.parse().ok()?,
        registry: contract("ZecuredRegistry")?,
        adapter: contract("SettlementAdapter")?,
        usdc: contract("MockUSDC")?,
        loan_token: market("loanToken")?.parse().ok()?,
        collateral_token: market("collateralToken")?.parse().ok()?,
        oracle: market("oracle")?.parse().ok()?,
        irm: market("irm")?.parse().ok()?,
        lltv: market("lltv")?.parse().ok()?,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let cfg = NetworkConfig::load(&cli.config)?;
    let allowlist = load_allowlist(&cli.deployments);
    let allowlist_oracle = allowlist.as_ref().map(|a| a.oracle);
    if allowlist.is_none() {
        warn!("no deployment file: paymaster sponsorship disabled");
    }
    let proxy = Arc::new(proxy::Proxy::new(
        cfg.base_rpc_url.clone(),
        Some(cli.bundler_url.clone()),
        Some(cli.paymaster_url.clone()),
        allowlist,
    )?);
    let coordinator = Arc::new(Coordinator::new(cfg, &cli.data, &cli.coordinator_key)?);

    let worker = coordinator.clone();
    let every = Duration::from_secs(cli.tick_secs);
    tokio::spawn(async move {
        loop {
            if let Err(e) = worker.tick().await {
                warn!("coordinator pass failed: {e:#}");
            }
            tokio::time::sleep(every).await;
        }
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/vaults", post(open_vault))
        .route("/vaults/{id}", get(get_vault))
        .route("/vaults/{id}/destination", post(destination))
        .route("/vaults/{id}/refund", post(refund))
        .route("/vaults/{id}/viewing-key", post(viewing_key))
        .route("/recipients", post(recipients))
        .route("/config", get(public_config))
        .route("/quotes/close/{id}", get(close_quote))
        .route("/closes", post(close_submit))
        .route("/admin/nodes", get(admin_nodes))
        .route("/admin/nodes/{index}/mode", post(admin_mode))
        .route("/admin/vaults/{id}/drain", post(admin_drain))
        .route("/admin/oracle", get(admin_get_oracle).post(admin_set_oracle))
        .layer(axum::Extension(PublicConfig(Arc::new(public_config_json(&coordinator.cfg, &cli.deployments)))))
        .layer(axum::Extension(AdminToken(Arc::new(cli.admin_token.clone()))))
        .layer(axum::Extension(Oracle {
            rpc: coordinator.cfg.base_rpc_url.clone(),
            address: allowlist_oracle,
            owner_key: cli.oracle_owner_key.clone(),
        }))
        .layer(axum::Extension(MarketMaker(Arc::new(cli.market_maker_url.trim_end_matches('/').to_owned()))))
        .merge(
            Router::new()
                .route("/proxy/rpc", post(proxy::rpc))
                .route("/proxy/bundler", post(proxy::bundler))
                .route("/proxy/paymaster", post(proxy::paymaster))
                .with_state(proxy),
        )
        // The app calls this API from the browser; tighten the origin when deployed.
        .layer(CorsLayer::permissive())
        .with_state(coordinator);
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    info!(bind = %cli.bind, "backend listening");
    axum::serve(listener, app).await?;
    Ok(())
}
