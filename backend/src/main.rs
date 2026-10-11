//! `zecured-backend`: the public API of the coordinator (docs/INTERFACES.md § 4).
//!
//! Privacy: nothing here logs addresses, salts, viewing keys or client IPs. Errors returned to
//! clients are a fixed message and a short code (`{ "error", "code" }`); details are logged at
//! debug level only, scrubbed of identifiers and numbers.

use std::{future::Future, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use alloy::primitives::{Address, B256, Bytes, keccak256};
use anyhow::{Context, Result, anyhow, ensure};
use axum::{
    Json, Router,
    extract::{FromRequest, FromRequestParts, Path, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{ACCEPT, CONTENT_TYPE},
        request::Parts,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{debug, error, info, warn};
use zecured_coordinator::{
    Coordinator, MIN_SIGNERS, OpenVaultRequest,
    error::{PublicError, public_error, scrubbed},
};
use zecured_node::config::NetworkConfig;

mod allowlist;
mod limits;
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
    #[arg(
        long,
        env = "ZECURED_DEPLOYMENTS",
        default_value = "contracts/deployments/base-sepolia.json"
    )]
    deployments: PathBuf,
    #[arg(long, env = "BUNDLER_URL", hide_env_values = true, default_value = "")]
    bundler_url: String,
    #[arg(long, env = "PAYMASTER_URL", hide_env_values = true, default_value = "")]
    paymaster_url: String,
    /// Base RPC behind the public `/proxy/rpc` (its own key and quota); empty = the network
    /// config's RPC, which the coordinator uses.
    #[arg(long, env = "PROXY_RPC_URL", hide_env_values = true, default_value = "")]
    proxy_rpc_url: String,
    /// Browser origins allowed to call this API (CORS), comma-separated.
    #[arg(
        long,
        env = "APP_ORIGINS",
        default_value = "http://127.0.0.1:3000,http://localhost:3000"
    )]
    app_origins: String,
    /// The market maker's quote service (close from collateral).
    #[arg(long, env = "MARKET_MAKER_URL", default_value = "http://127.0.0.1:8090")]
    market_maker_url: String,
    /// Owner key of the MOCK price oracle, for the demo control panel's price slider.
    #[arg(long, env = "DEPLOYER_PRIVATE_KEY", hide_env_values = true, default_value = "")]
    oracle_owner_key: String,
    /// Token protecting the demo control endpoints (empty = disabled).
    #[arg(long, env = "DEMO_ADMIN_TOKEN", hide_env_values = true, default_value = "")]
    admin_token: String,
    /// Nodes the demo panel may switch to "compromised": at most two, so it can never compromise
    /// a signing threshold. Those nodes must also be started with `--demo-allow-compromise`.
    #[arg(long, env = "DEMO_COMPROMISABLE_NODES", default_value = "4,5")]
    demo_compromisable_nodes: String,
    /// Seconds between coordinator passes.
    #[arg(long, default_value_t = 10)]
    tick_secs: u64,
}

// ------------------------------------------------------------------------- errors (L-2)

/// An API error: an HTTP status, a fixed public message and a short code. Nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self { status, code, message }
    }

    fn bad_request() -> Self {
        PublicError::BadRequest.into()
    }

    const UNAUTHORIZED: Self = Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "Unauthorized.");
    const MODE_NOT_ALLOWED: Self =
        Self::new(StatusCode::FORBIDDEN, "mode_not_allowed", "The demo panel cannot switch this node to that mode.");
    const NOT_CONFIGURED: Self =
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "not_configured", "Not configured on this backend.");
    const MM_UNAVAILABLE: Self = Self::new(
        StatusCode::BAD_GATEWAY,
        "market_maker_unavailable",
        "The market maker is unavailable. Try again later.",
    );
    const MM_REFUSED: Self =
        Self::new(StatusCode::BAD_GATEWAY, "market_maker_refused", "The market maker refused the request.");
}

impl From<PublicError> for ApiError {
    fn from(p: PublicError) -> Self {
        let status = StatusCode::from_u16(p.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Self::new(status, p.code(), p.message())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let p = public_error(&e).unwrap_or(PublicError::Internal);
        debug!(code = p.code(), error = %scrubbed(&e), "request failed");
        p.into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response()
    }
}

type ApiResult = Result<Json<serde_json::Value>, ApiError>;
type App = State<Arc<Coordinator>>;

/// A JSON body; a malformed one is a plain `bad_request` (not axum's descriptive rejection).
struct Body<T>(T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Body(v))
            .map_err(|_| ApiError::bad_request())
    }
}

/// A path parameter, with the same treatment.
struct Param<T>(T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Param<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(v)| Param(v))
            .map_err(|_| ApiError::bad_request())
    }
}

fn salt(hex_salt: &str) -> Result<[u8; 32], ApiError> {
    hex::decode(hex_salt.trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(ApiError::bad_request)
}

/// Accepts decimal strings or JSON numbers for amounts and nonces.
fn num(v: &serde_json::Value) -> Result<u64, ApiError> {
    match v {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
    .ok_or_else(ApiError::bad_request)
}

// ------------------------------------------------------------------------- public API

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

async fn open_vault(State(c): App, Body(b): Body<OpenBody>) -> ApiResult {
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

/// The fields the app reads (app/lib/api.ts `parseVault`); the vault's address is not among
/// them and is not served to anyone holding a vault id (L-10).
async fn get_vault(State(c): App, Param(id): Param<B256>) -> ApiResult {
    let v = c.view(id).await?;
    Ok(Json(json!({
        "vaultId": v.vault_id,
        "status": v.status,
        "amount": v.amount.to_string(),
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

async fn destination(State(c): App, Param(id): Param<B256>, Body(b): Body<DestinationBody>) -> ApiResult {
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

async fn refund(State(c): App, Param(id): Param<B256>, Body(b): Body<RefundBody>) -> ApiResult {
    c.request_refund(id, b.note_id, &b.receiver, salt(&b.salt)?, num(&b.nonce)?, num(&b.deadline)?, b.signature)
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

async fn viewing_key(State(c): App, Param(id): Param<B256>, Body(b): Body<ViewKeyBody>) -> ApiResult {
    let ufvk = c.viewing_key(id, num(&b.nonce)?, num(&b.deadline)?, &b.signature).await?;
    Ok(Json(json!({ "ufvk": ufvk })))
}

#[derive(Deserialize)]
struct RecipientBody {
    receiver: String,
    salt: String,
}

async fn recipients(State(c): App, Body(b): Body<RecipientBody>) -> ApiResult {
    let commitment = c.add_recipient(&b.receiver, salt(&b.salt)?).await?;
    Ok(Json(json!({ "commitment": commitment })))
}

// ------------------------------------------------------------------------- demo control (C-3)

#[derive(Clone)]
struct AdminToken(Arc<String>);

/// Compares hashes, so the time taken says nothing about the token (L-1, C-3).
fn token_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (keccak256(given.as_bytes()), keccak256(expected.as_bytes()));
    std::hint::black_box(a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y))) == 0
}

fn check_admin(token: &AdminToken, headers: &HeaderMap) -> Result<(), ApiError> {
    let given = headers.get("x-admin-token").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if token.0.is_empty() || !token_matches(given, &token.0) {
        return Err(ApiError::UNAUTHORIZED);
    }
    Ok(())
}

/// Which modes the demo panel may set: "normal" and "offline" anywhere, "compromised" only on
/// a fixed set of fewer than `MIN_SIGNERS` nodes, so the panel can never assemble a threshold
/// of policy-free signers.
#[derive(Clone, Debug)]
struct ModePolicy {
    compromisable: Arc<Vec<u16>>,
}

impl ModePolicy {
    fn parse(list: &str, nodes: &[u16]) -> Result<Self> {
        let mut compromisable = vec![];
        for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let index: u16 = part.parse().context("DEMO_COMPROMISABLE_NODES: expected node indices")?;
            ensure!(nodes.contains(&index), "DEMO_COMPROMISABLE_NODES: node {index} is not in the network");
            if !compromisable.contains(&index) {
                compromisable.push(index);
            }
        }
        ensure!(
            compromisable.len() < usize::from(MIN_SIGNERS),
            "DEMO_COMPROMISABLE_NODES: at most {} nodes",
            MIN_SIGNERS - 1
        );
        Ok(Self {
            compromisable: Arc::new(compromisable),
        })
    }

    fn check(&self, index: u16, mode: &str) -> Result<(), ApiError> {
        match mode {
            "normal" | "offline" => Ok(()),
            "compromised" if self.compromisable.contains(&index) => Ok(()),
            "compromised" => Err(ApiError::MODE_NOT_ALLOWED),
            _ => Err(ApiError::bad_request()),
        }
    }
}

#[derive(Deserialize)]
struct DrainBody {
    to: String,
}

async fn admin_drain(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    headers: HeaderMap,
    Param(id): Param<B256>,
    Body(b): Body<DrainBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let report = c.demo_drain(id, &b.to).await?;
    Ok(Json(serde_json::to_value(report).map_err(|e| ApiError::from(anyhow::Error::from(e)))?))
}

#[derive(Deserialize)]
struct ModeBody {
    mode: String,
}

/// Switches a node's demo mode through a coordinator-signed request (D16).
async fn admin_mode(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(policy): axum::Extension<ModePolicy>,
    headers: HeaderMap,
    Param(index): Param<u16>,
    Body(b): Body<ModeBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    policy.check(index, &b.mode)?;
    let r = c.set_node_mode(index, &b.mode, &t.0).await?;
    let mode = r.get("mode").cloned().unwrap_or(serde_json::Value::Null);
    Ok(Json(json!({ "mode": mode })))
}

async fn admin_nodes(State(c): App, axum::Extension(t): axum::Extension<AdminToken>, headers: HeaderMap) -> ApiResult {
    check_admin(&t, &headers)?;
    Ok(Json(json!({ "nodes": c.node_health().await })))
}

// ------------------------------------------------------------------------- market maker relay

#[derive(Clone)]
struct MarketMaker {
    url: Arc<String>,
    http: reqwest::Client,
}

/// Our fixed message for a market maker refusal code (anything else: a generic refusal).
fn market_maker_error(code: &str) -> ApiError {
    let refused = |code, message| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message);
    match code {
        "no_debt" => refused("no_debt", "There is no debt to close."),
        "underwater" => refused("underwater", "The position is underwater: it cannot be closed from collateral."),
        "user_amount_too_small" => {
            refused("user_amount_too_small", "What would come back to you does not cover the Zcash payout fee.")
        }
        "quote_mismatch" => refused("quote_mismatch", "The request does not match the market maker's quote."),
        "close_rejected" => {
            refused("close_rejected", "The close would fail on Base (expired, already used, or the position changed).")
        }
        "bad_request" => ApiError::bad_request(),
        _ => ApiError::MM_REFUSED,
    }
}

/// Relays a market maker call so the browser never contacts the market maker directly. Its
/// errors are mapped to fixed messages; nothing it says is passed through as text.
async fn mm_forward(req: reqwest::RequestBuilder) -> ApiResult {
    let resp = req.send().await.map_err(|_| ApiError::MM_UNAVAILABLE)?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|_| ApiError::MM_UNAVAILABLE)?;
    if !status.is_success() {
        return Err(market_maker_error(v.get("code").and_then(|c| c.as_str()).unwrap_or_default()));
    }
    Ok(Json(v))
}

async fn close_quote(axum::Extension(mm): axum::Extension<MarketMaker>, Param(id): Param<B256>) -> ApiResult {
    mm_forward(mm.http.get(format!("{}/quote/{id}", mm.url))).await
}

async fn close_submit(
    axum::Extension(mm): axum::Extension<MarketMaker>,
    Body(b): Body<serde_json::Value>,
) -> ApiResult {
    mm_forward(mm.http.post(format!("{}/close", mm.url)).json(&b)).await
}

// ------------------------------------------------------------------------- MOCK oracle

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

fn chain_error(e: impl Into<anyhow::Error>) -> ApiError {
    e.into().context(PublicError::ChainUnavailable).into()
}

async fn admin_get_oracle(
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(o): axum::Extension<Oracle>,
    headers: HeaderMap,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let address = o.address.ok_or(ApiError::NOT_CONFIGURED)?;
    let p = alloy::providers::ProviderBuilder::new().connect_http(o.rpc.parse().map_err(chain_error)?);
    let raw = IMockZecOracle::new(address, &p).price().call().await.map_err(chain_error)?;
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
    headers: HeaderMap,
    Body(b): Body<OracleBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let address = o.address.ok_or(ApiError::NOT_CONFIGURED)?;
    let key: alloy::signers::local::PrivateKeySigner = o.owner_key.parse().map_err(|_| ApiError::NOT_CONFIGURED)?;
    let p = alloy::providers::ProviderBuilder::new()
        .wallet(alloy::network::EthereumWallet::from(key))
        .connect_http(o.rpc.parse().map_err(chain_error)?);
    let raw = alloy::primitives::U256::from(b.price_usd_cents) * alloy::primitives::U256::from(ORACLE_SCALE_PER_CENT);
    let receipt = IMockZecOracle::new(address, &p)
        .setPrice(raw)
        .send()
        .await
        .map_err(chain_error)?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .map_err(chain_error)?;
    if !receipt.status() {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "reverted", "setPrice reverted."));
    }
    Ok(Json(json!({ "priceUsdCents": b.price_usd_cents.to_string(), "txHash": receipt.transaction_hash })))
}

// ------------------------------------------------------------------------- config and wiring

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
/// `sponsorship`: the bundler, the paymaster and the paymaster allowlist are all configured, so the
/// app can send sponsored UserOps.
fn public_config_json(cfg: &NetworkConfig, deployments: &std::path::Path, sponsorship: bool) -> serde_json::Value {
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
        "sponsorship": sponsorship,
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

/// CORS for the app's origins only (M-8): `APP_ORIGINS`, e.g. `https://app.example`.
fn cors_layer(origins: &str) -> Result<CorsLayer> {
    let mut list = vec![];
    for origin in origins.split(',').map(|o| o.trim().trim_end_matches('/')).filter(|o| !o.is_empty()) {
        let host = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://"));
        ensure!(
            host.is_some_and(|h| !h.is_empty() && !h.contains(['/', '*', ' '])),
            "APP_ORIGINS: {origin} is not an origin (scheme://host[:port])"
        );
        list.push(HeaderValue::from_str(origin).with_context(|| format!("APP_ORIGINS: {origin}"))?);
    }
    ensure!(!list.is_empty(), "APP_ORIGINS is empty");
    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(list))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([CONTENT_TYPE, ACCEPT, HeaderName::from_static("x-admin-token")])
        .max_age(Duration::from_secs(600)))
}

/// Per-request context besides the coordinator.
struct Wiring {
    public_config: PublicConfig,
    admin_token: AdminToken,
    modes: ModePolicy,
    oracle: Oracle,
    market_maker: MarketMaker,
}

/// The API: per-route rate limits on the expensive routes, a global one on everything, CORS
/// for the app's origins outermost (preflights cost no tokens).
fn router(
    coordinator: Arc<Coordinator>,
    proxy: Arc<proxy::Proxy>,
    w: Wiring,
    limits: &limits::Limits,
    cors: CorsLayer,
) -> Router {
    let limited = |bucket: &Arc<limits::TokenBucket>| middleware::from_fn_with_state(bucket.clone(), limits::limit);
    Router::new()
        .route("/health", get(health))
        .route("/config", get(public_config))
        .route("/vaults/{id}", get(get_vault))
        .route("/admin/nodes", get(admin_nodes))
        .route("/admin/nodes/{index}/mode", post(admin_mode))
        .route("/admin/vaults/{id}/drain", post(admin_drain))
        .route("/admin/oracle", get(admin_get_oracle).post(admin_set_oracle))
        .merge(Router::new().route("/vaults", post(open_vault)).route_layer(limited(&limits.open_vault)))
        .merge(Router::new().route("/recipients", post(recipients)).route_layer(limited(&limits.recipients)))
        .merge(
            Router::new()
                .route("/vaults/{id}/destination", post(destination))
                .route("/vaults/{id}/refund", post(refund))
                .route("/vaults/{id}/viewing-key", post(viewing_key))
                .route_layer(limited(&limits.owner)),
        )
        .merge(
            Router::new()
                .route("/quotes/close/{id}", get(close_quote))
                .route("/closes", post(close_submit))
                .route_layer(limited(&limits.market_maker)),
        )
        .layer(axum::Extension(w.public_config))
        .layer(axum::Extension(w.admin_token))
        .layer(axum::Extension(w.modes))
        .layer(axum::Extension(w.oracle))
        .layer(axum::Extension(w.market_maker))
        .merge(
            Router::new()
                .route("/proxy/rpc", post(proxy::rpc))
                .route("/proxy/bundler", post(proxy::bundler))
                .route("/proxy/paymaster", post(proxy::paymaster))
                .route_layer(limited(&limits.proxy))
                .with_state(proxy),
        )
        .layer(middleware::from_fn_with_state(limits.global.clone(), limits::limit))
        .layer(cors)
        .with_state(coordinator)
}

/// Runs `pass` every `every`, each pass in its own task: a pass that fails or panics is logged
/// and the next one runs as scheduled (M-1).
async fn supervise<F, Fut>(every: Duration, mut pass: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    loop {
        match tokio::spawn(pass()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(code = public_error(&e).unwrap_or(PublicError::Internal).code(), "coordinator pass failed");
                debug!(error = %scrubbed(&e), "coordinator pass failed");
            }
            Err(e) if e.is_panic() => error!("coordinator pass panicked; the next pass runs as scheduled"),
            Err(_) => warn!("coordinator pass was cancelled"),
        }
        tokio::time::sleep(every).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let cfg = NetworkConfig::load(&cli.config)?;
    let indices: Vec<u16> = cfg.nodes.iter().map(|n| n.index).collect();
    let modes = ModePolicy::parse(&cli.demo_compromisable_nodes, &indices)?;
    let cors = cors_layer(&cli.app_origins)?;
    let allowlist = load_allowlist(&cli.deployments);
    let allowlist_oracle = allowlist.as_ref().map(|a| a.oracle);
    if allowlist.is_none() {
        warn!("no deployment file: paymaster sponsorship disabled");
    }
    let proxy_rpc = if cli.proxy_rpc_url.is_empty() {
        cfg.base_rpc_url.clone()
    } else {
        cli.proxy_rpc_url.clone()
    };
    let proxy = Arc::new(proxy::Proxy::new(
        proxy_rpc,
        cfg.base_rpc_url.clone(),
        Some(cli.bundler_url.clone()),
        Some(cli.paymaster_url.clone()),
        allowlist,
    )?);
    let sponsorship = proxy.bundler_url.is_some() && proxy.paymaster_url.is_some() && proxy.allowlist.is_some();
    let coordinator = Arc::new(Coordinator::new(cfg, &cli.data, &cli.coordinator_key)?);

    let worker = coordinator.clone();
    tokio::spawn(supervise(Duration::from_secs(cli.tick_secs), move || {
        let worker = worker.clone();
        async move { worker.tick().await }
    }));

    let wiring = Wiring {
        public_config: PublicConfig(Arc::new(public_config_json(&coordinator.cfg, &cli.deployments, sponsorship))),
        admin_token: AdminToken(Arc::new(cli.admin_token.clone())),
        modes,
        oracle: Oracle {
            rpc: coordinator.cfg.base_rpc_url.clone(),
            address: allowlist_oracle,
            owner_key: cli.oracle_owner_key.clone(),
        },
        market_maker: MarketMaker {
            url: Arc::new(cli.market_maker_url.trim_end_matches('/').to_owned()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(180)).build()?,
        },
    };
    let app = router(coordinator, proxy, wiring, &limits::Limits::default(), cors);
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    info!(bind = %cli.bind, "backend listening");
    axum::serve(listener, app).await.map_err(|e| anyhow!("server: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body as HttpBody;
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn admin_tokens_compare_by_value() {
        assert!(token_matches("s3cret-token", "s3cret-token"));
        assert!(!token_matches("s3cret-tokem", "s3cret-token"));
        assert!(!token_matches("", "s3cret-token"));
        let t = AdminToken(Arc::new("tok".into()));
        let mut h = HeaderMap::new();
        assert_eq!(check_admin(&t, &h), Err(ApiError::UNAUTHORIZED));
        h.insert("x-admin-token", HeaderValue::from_static("tok"));
        assert_eq!(check_admin(&t, &h), Ok(()));
        // An empty configured token disables the endpoints, whatever is sent.
        let off = AdminToken(Arc::new(String::new()));
        h.insert("x-admin-token", HeaderValue::from_static(""));
        assert_eq!(check_admin(&off, &h), Err(ApiError::UNAUTHORIZED));
    }

    /// C-3: the panel can never put a signing threshold of nodes into "compromised" mode.
    #[test]
    fn compromise_is_limited_to_two_fixed_nodes() {
        let nodes = [1, 2, 3, 4, 5];
        let p = ModePolicy::parse("4,5", &nodes).unwrap();
        assert_eq!(p.check(4, "compromised"), Ok(()));
        assert_eq!(p.check(5, "compromised"), Ok(()));
        for i in 1..=3 {
            assert_eq!(p.check(i, "compromised"), Err(ApiError::MODE_NOT_ALLOWED));
            assert_eq!(p.check(i, "offline"), Ok(()));
            assert_eq!(p.check(i, "normal"), Ok(()));
        }
        assert_eq!(p.check(4, "evil"), Err(ApiError::bad_request()));
        assert!(ModePolicy::parse("1,2,3", &nodes).is_err(), "three nodes would reach the threshold");
        assert!(ModePolicy::parse("4,5,4", &nodes).is_ok(), "duplicates count once");
        assert!(ModePolicy::parse("6", &nodes).is_err());
        assert!(ModePolicy::parse("four", &nodes).is_err());
        let none = ModePolicy::parse("", &nodes).unwrap();
        assert_eq!(none.check(4, "compromised"), Err(ApiError::MODE_NOT_ALLOWED));
    }

    /// L-2: callers only ever get a fixed message and a code.
    #[test]
    fn errors_are_fixed_messages_with_a_code() {
        let e =
            anyhow!("node 2 unavailable: http://10.0.0.7:9102/v1/sign/commit").context(PublicError::SignersUnavailable);
        let api = ApiError::from(e);
        assert_eq!(api.code, "signers_unavailable");
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        let api = ApiError::from(anyhow!("permission denied (os error 13) at /var/lib/zecured"));
        assert_eq!((api.code, api.message), ("internal", "Internal error."));
        assert_eq!(market_maker_error("no_debt").code, "no_debt");
        assert_eq!(market_maker_error("close would revert: 0xdeadbeef ...").code, "market_maker_refused");
        assert_eq!(salt("0x12"), Err(ApiError::bad_request()));
        assert_eq!(num(&json!("12x")), Err(ApiError::bad_request()));
    }

    #[tokio::test]
    async fn malformed_bodies_get_the_fixed_bad_request() {
        async fn echo(Body(b): Body<RecipientBody>) -> String {
            b.receiver
        }
        let app = Router::new().route("/r", post(echo));
        let req = |body: &'static str| {
            axum::http::Request::post("/r")
                .header("content-type", "application/json")
                .body(HttpBody::from(body))
                .unwrap()
        };
        let r = app.clone().oneshot(req(r#"{"receiver":"x","salt":"00"}"#)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let r = app.oneshot(req(r#"{"receiver":1}"#)).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v, json!({ "error": "Invalid request.", "code": "bad_request" }));
    }

    /// M-8: only the app's origins may call the API from a browser.
    #[tokio::test]
    async fn cors_allows_only_the_app_origins() {
        let app = Router::new()
            .route("/health", get(health))
            .layer(cors_layer("http://127.0.0.1:3000, http://localhost:3000/").unwrap());
        let from = |origin: &'static str| {
            axum::http::Request::get("/health").header("origin", origin).body(HttpBody::empty()).unwrap()
        };
        let r = app.clone().oneshot(from("http://localhost:3000")).await.unwrap();
        assert_eq!(r.headers().get("access-control-allow-origin").unwrap(), "http://localhost:3000");
        let r = app.clone().oneshot(from("https://evil.example")).await.unwrap();
        assert!(r.headers().get("access-control-allow-origin").is_none());
        let preflight = axum::http::Request::builder()
            .method(Method::OPTIONS)
            .uri("/health")
            .header("origin", "http://127.0.0.1:3000")
            .header("access-control-request-method", "POST")
            .body(HttpBody::empty())
            .unwrap();
        let r = app.oneshot(preflight).await.unwrap();
        assert_eq!(r.headers().get("access-control-allow-origin").unwrap(), "http://127.0.0.1:3000");

        assert!(cors_layer("*").is_err());
        assert!(cors_layer("").is_err());
        assert!(cors_layer("localhost:3000").is_err());
        assert!(cors_layer("https://app.example/path").is_err());
    }

    /// The wired API: fixed errors, per-route limits, the demo mode policy and CORS, with a
    /// coordinator whose nodes are unreachable (nothing here may need them).
    #[tokio::test]
    async fn the_api_is_wired_with_limits_and_fixed_errors() {
        let dir = std::env::temp_dir().join(format!("zecured-backend-test-{}", hex::encode(keccak256(b"wiring"))));
        let nodes: Vec<serde_json::Value> = (1..=5u8)
            .map(|i| json!({ "index": i, "url": "http://127.0.0.1:9", "x25519": "00", "eth": Address::repeat_byte(i) }))
            .collect();
        let cfg: NetworkConfig = serde_json::from_value(json!({
            "lightwalletd_url": "http://127.0.0.1:9", "base_rpc_url": "http://127.0.0.1:9", "chain_id": 84_532,
            "registry": Address::repeat_byte(0x11), "adapter": Address::repeat_byte(0x22), "registry_from_block": 1,
            "confirmations": 3, "vault_expiry_blocks": 288, "denominations": [1_000_000], "nodes": nodes,
        }))
        .unwrap();
        let key = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
        let coordinator = Arc::new(Coordinator::new(cfg, &dir, &key).unwrap());
        let proxy = Arc::new(
            proxy::Proxy::new("http://127.0.0.1:9".into(), "http://127.0.0.1:9".into(), None, None, None).unwrap(),
        );
        let wiring = Wiring {
            public_config: PublicConfig(Arc::new(json!({}))),
            admin_token: AdminToken(Arc::new("tok".into())),
            modes: ModePolicy::parse("4,5", &[1, 2, 3, 4, 5]).unwrap(),
            oracle: Oracle {
                rpc: "http://127.0.0.1:9".into(),
                address: None,
                owner_key: String::new(),
            },
            market_maker: MarketMaker {
                url: Arc::new("http://127.0.0.1:9".into()),
                http: reqwest::Client::new(),
            },
        };
        let app = router(
            coordinator,
            proxy,
            wiring,
            &limits::Limits::default(),
            cors_layer("http://localhost:3000").unwrap(),
        );
        let call = |method: Method, uri: &str, body: &'static str| {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("origin", "http://localhost:3000")
                .header("x-admin-token", "tok")
                .body(HttpBody::from(body))
                .unwrap();
            let app = app.clone();
            async move {
                let r = app.oneshot(req).await.unwrap();
                let status = r.status();
                let cors = r.headers().contains_key("access-control-allow-origin");
                let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
                (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default(), cors)
            }
        };

        let (status, _, cors) = call(Method::GET, "/health", "").await;
        assert_eq!((status, cors), (StatusCode::OK, true));
        let unknown = format!("/vaults/{}", B256::repeat_byte(9));
        let (status, body, _) = call(Method::GET, &unknown, "").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("unknown_vault")));
        assert!(body.get("address").is_none());
        // C-3: no third compromised node, whatever the token.
        let (status, body, _) = call(Method::POST, "/admin/nodes/1/mode", r#"{"mode":"compromised"}"#).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("mode_not_allowed")));
        // M-8: POST /vaults allows a burst of 3, then answers 429.
        for _ in 0..3 {
            let (status, body, _) = call(Method::POST, "/vaults", "{}").await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        }
        let (status, body, cors) = call(Method::POST, "/vaults", "{}").await;
        assert_eq!((status, body["code"].as_str(), cors), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"), true));
        // Other routes have their own buckets.
        let (status, _, _) = call(Method::POST, "/recipients", "{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-1: a panicking pass is logged and the loop goes on.
    #[tokio::test]
    async fn the_coordinator_loop_survives_a_panic() {
        let passes = Arc::new(AtomicUsize::new(0));
        let counter = passes.clone();
        let loop_task = tokio::spawn(supervise(Duration::from_millis(5), move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                match n {
                    0 => panic!("index out of bounds (test)"),
                    1 => Err(anyhow!("node 1 unavailable")),
                    _ => Ok(()),
                }
            }
        }));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while passes.load(Ordering::SeqCst) < 4 {
            assert!(tokio::time::Instant::now() < deadline, "the loop stopped");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        loop_task.abort();
    }
}
