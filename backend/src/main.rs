//! `zecured-backend`: the public API of the coordinator (docs/INTERFACES.md § 4).
//!
//! Privacy: nothing here logs addresses, salts, viewing keys or client IPs. Errors returned to
//! clients are a fixed message and a short code (`{ "error", "code" }`); details are logged at
//! debug level only, scrubbed of identifiers and numbers.

use std::{
    future::Future,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
    Coordinator, MIN_SIGNERS, OpenVaultRequest, OpenVaultResponse,
    error::{PublicError, public_error, scrubbed},
    proofs::{DepositProofs, SealFile},
};
use zecured_node::config::NetworkConfig;

mod allowlist;
mod limits;
mod minting;
mod onchain;
mod paymaster;
mod proxy;
mod versions;

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
    /// RQ-05: our own SingletonPaymasterV7 (contracts/deployments/<name>.paymaster.json); with
    /// its signer key, the backend answers ERC-7677 itself instead of forwarding to PAYMASTER_URL.
    #[arg(long, env = "PAYMASTER_ADDRESS", default_value = "")]
    paymaster_address: String,
    #[arg(
        long,
        env = "PAYMASTER_SIGNER_PRIVATE_KEY",
        hide_env_values = true,
        default_value = ""
    )]
    paymaster_signer_key: String,
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
    /// Owner key of the market's DemoShockOracle (and of the base MockZecOracle, if the base is a
    /// MOCK), for the demo control panel.
    #[arg(long, env = "DEPLOYER_PRIVATE_KEY", hide_env_values = true, default_value = "")]
    oracle_owner_key: String,
    /// Token protecting the demo control endpoints (empty = disabled).
    #[arg(long, env = "DEMO_ADMIN_TOKEN", hide_env_values = true, default_value = "")]
    admin_token: String,
    /// Nodes the demo panel may switch to "compromised": at most two, so it can never compromise
    /// a signing threshold (three only in the separate theft demo's `demo-tier3` build). Those
    /// nodes must also be started with `--demo-allow-compromise`.
    #[arg(long, env = "DEMO_COMPROMISABLE_NODES", default_value = "4,5")]
    demo_compromisable_nodes: String,
    /// Seconds between coordinator passes.
    #[arg(long, default_value_t = 10)]
    tick_secs: u64,
    /// Tier 2 (D22): activate vaults on deposit proofs made by this coordinator, using the zkVM
    /// host at this path (`tier2/target/release/zecured-tier2-host`), instead of attestations.
    #[arg(long, env = "DEPOSIT_PROOF_HOST")]
    deposit_proof_host: Option<PathBuf>,
    /// With `--deposit-proof-host`: MOCK seals for a Registry deployed with MockRiscZeroVerifier
    /// (local fork only); otherwise succinct proofs wrapped in Groth16 (Docker on x86).
    #[arg(long, env = "DEPOSIT_PROOF_MOCK", default_value_t = false)]
    deposit_proof_mock: bool,
    /// Header file and per-block summary cache of the deposit witness builder.
    #[arg(long, env = "DEPOSIT_PROOF_HEADERS", default_value = "data/tier2/headers_nu7.json")]
    deposit_proof_headers: PathBuf,
    #[arg(long, env = "DEPOSIT_PROOF_SUMMARIES", default_value = "data/tier2/summaries.json")]
    deposit_proof_summaries: PathBuf,
    /// Deposit proofs run at once, 1 or 2 (review H-12: a proof takes about 10 GB of RAM on a CPU,
    /// 13 GB of GPU memory); the others wait in the proving queue, one job per vault.
    #[arg(long, env = "DEPOSIT_PROOF_CONCURRENCY", default_value_t = 1)]
    deposit_proof_concurrency: usize,
    /// Mainnet beta (section 6): the operator's invite file (keccak256 hashes of the invite codes
    /// with their uses, `inviteOnly`, and the `openings` switch); unset: openings are open to all.
    #[arg(long, env = "ZECURED_INVITES")]
    invites: Option<PathBuf>,
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
    /// The same answer as the rate-limit middleware's (`limits::too_many_requests`).
    const RATE_LIMITED: Self =
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Too many requests. Try again later.");
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
    /// H-01: the nonce of the request's hashcash stamp (decimal string or number).
    #[serde(default)]
    stamp: serde_json::Value,
    /// Mainnet beta: the invite code, when openings are invite-only (never logged).
    #[serde(default)]
    invite: Option<String>,
}

/// `POST /vaults`'s rate limit. Only a request whose stamp checks out takes a token (H-01), so
/// requests without the work can no longer use the limit up for everyone.
#[derive(Clone)]
struct OpenVaultLimiter(Arc<limits::TokenBucket>);

async fn open_vault(
    State(c): App,
    axum::Extension(limiter): axum::Extension<OpenVaultLimiter>,
    Body(b): Body<OpenBody>,
) -> ApiResult {
    let req = OpenVaultRequest {
        owner: b.owner,
        account: b.account,
        amount: num(&b.amount)?,
        request_id: b.request_id,
        deadline: num(&b.deadline)?,
        signature: b.signature,
        stamp: if b.stamp.is_null() { 0 } else { num(&b.stamp)? },
        invite: b.invite,
    };
    // Section 6: closed openings are refused before the stamp is even looked at.
    if !c.openings().open {
        return Err(PublicError::OpeningsClosed.into());
    }
    c.check_open_stamp(&req)?;
    if !limiter.0.try_take() {
        return Err(ApiError::RATE_LIMITED);
    }
    let r = c.open_vault(req).await?;
    Ok(Json(open_vault_json(&r)))
}

/// `POST /vaults` response: the vault, the hash of its tag key and every signer's `VaultOpened`
/// signature for them, which the app and the CLI check against the signer set pinned in their
/// build before showing the address (C-4).
fn open_vault_json(r: &OpenVaultResponse) -> serde_json::Value {
    json!({
        "vaultId": r.vault_id,
        "address": r.address,
        "zip321": r.zip321,
        "amount": r.amount.to_string(),
        "expiryHeight": r.expiry_height,
        "tagKeyHash": r.tag_key_hash,
        "signatures": r.signatures.iter().map(|s| json!({ "index": s.index, "signature": s.signature })).collect::<Vec<_>>(),
    })
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
        // Deposits that have not landed on Base: each is refunded once a proof lands it (T-02), or,
        // when no guest of the deployment can land it (`offChain`), off-chain on request (P5, I30).
        "nonConforming": v.non_conforming.iter().map(|n| json!({ "noteId": n.note_id, "amount": n.amount.to_string(), "offChain": n.offchain })).collect::<Vec<_>>(),
        // A Refund claim waits for its account's destination (`destinationSet`, T-02). v4: `proven`
        // (section 3: its payout's delivery is proven on Base, final), `disputableUntil` (confirmed
        // by the signers, disputable until then, Unix seconds; null otherwise) and `livenessArmed`
        // (R-5: its liveness clock is armed for its current destination).
        "claims": v.claims.iter().map(|c| json!({
            "claimId": c.claim_id, "kind": c.kind, "amount": c.amount.to_string(), "paid": c.paid,
            "status": zecured_coordinator::claim_status(c.state), "destinationSet": c.destination_set,
            "proven": c.proven, "disputableUntil": c.disputable_until, "livenessArmed": c.liveness_armed,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProveRefundBody {
    nonce: serde_json::Value,
    deadline: serde_json::Value,
    signature: Bytes,
}

/// R-11.1: the vault's owner asks for its deposit back when the coverage cap leaves no room for its
/// activation (`ProveRefund`, signed like the other owner requests): the coordinator proves the
/// deposit in Refund mode. `{ queued: true }`, or a fixed refusal: `not_blocked`, `no_deposit`,
/// `already_landed`, `refund_unavailable`, `not_owner`, `expired`, `nonce_used`, `not_configured`.
async fn prove_refund(State(c): App, Param(id): Param<B256>, Body(b): Body<ProveRefundBody>) -> ApiResult {
    c.request_refund_proof(id, num(&b.nonce)?, num(&b.deadline)?, &b.signature).await?;
    Ok(Json(json!({ "queued": true })))
}

/// R-5 (v4 freeze § 3): the first valid liveness receipt any signer gives for the claim, for its
/// current destination: `{ claimId, commitment, receivedAt, signer, signature }`, which the claim's
/// claimant passes to `SignerBondV2.armLiveness(claimId, receivedAt, signature)`; `404 no_receipt`
/// while no signer holds a validated destination for it. Public (the receipt only says a signer
/// holds the destination; the claim and its commitment are on Base) and rate-limited.
async fn claim_receipt(State(c): App, Param(claim_id): Param<B256>) -> ApiResult {
    let r = c.claim_receipt(claim_id).await?;
    Ok(Json(json!({
        "claimId": r.claim_id,
        "commitment": r.commitment,
        "receivedAt": r.received_at,
        "signer": r.signer,
        "signature": r.signature,
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

/// P5: the off-chain refund of a deposit that no guest of the deployment can land on Base (I30);
/// any other deposit is refunded on Base, as a Refund claim.
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

/// Y10: a deposit proof made on the borrower's own machine (`zecured-wallet loan prove`),
/// relayed with the coordinator's gas: proving one's own deposit needs no ETH. Nothing about it
/// needs trust: the Registry checks the proof, the anchor and the signatures.
async fn deposit_proof(State(c): App, Param(id): Param<B256>, Body(b): Body<SealFile>) -> ApiResult {
    c.submit_external_proof(id, b).await?;
    Ok(Json(json!({ "accepted": true })))
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

/// How many nodes the demo panel may compromise: fewer than `MIN_SIGNERS`, so it can never
/// assemble a threshold of policy-free signers, except in the separate theft demo's build
/// (`demo-tier3`, review T3-6), whose whole point is a real theft that is then slashed.
fn max_compromisable() -> usize {
    if cfg!(feature = "demo-tier3") {
        usize::from(MIN_SIGNERS)
    } else {
        usize::from(MIN_SIGNERS) - 1
    }
}

/// Which modes the demo panel may set: "normal" and "offline" anywhere, "compromised" only on
/// a fixed set of nodes ([`max_compromisable`]).
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
            compromisable.len() <= max_compromisable(),
            "DEMO_COMPROMISABLE_NODES: at most {} nodes",
            max_compromisable()
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

    /// Adds `compromisable` to each node of a `node_health` list, so the demo panel offers
    /// "compromised" only where it is allowed.
    fn annotate(&self, mut nodes: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
        for node in &mut nodes {
            let index = node.get("index").and_then(serde_json::Value::as_u64).and_then(|i| u16::try_from(i).ok());
            if let Some(fields) = node.as_object_mut() {
                let allowed = index.is_some_and(|i| self.compromisable.contains(&i));
                fields.insert("compromisable".into(), allowed.into());
            }
        }
        nodes
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

/// DEMO ONLY (`demo-tier3`, the theft demo, T3-6): three compromised nodes sweep the vault to
/// `to` without a commitment on Base, and the transaction is broadcast (an F1 violation).
#[cfg(feature = "demo-tier3")]
async fn admin_steal(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    headers: HeaderMap,
    Param(id): Param<B256>,
    Body(b): Body<DrainBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let txid = c.demo_steal(id, &b.to).await?;
    Ok(Json(json!({ "txid": txid })))
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

async fn admin_nodes(
    State(c): App,
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(policy): axum::Extension<ModePolicy>,
    headers: HeaderMap,
) -> ApiResult {
    check_admin(&t, &headers)?;
    Ok(Json(json!({ "nodes": policy.annotate(c.node_health().await) })))
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
        "repay_above_debt" => refused("repay_above_debt", "The repayment is more than the debt."),
        "deleverage_too_small" => {
            refused("deleverage_too_small", "The collateral for that repayment is below the 0.001 ZEC minimum.")
        }
        "deleverage_too_large" => {
            refused("deleverage_too_large", "That would leave less than 0.001 ZEC in the vault: close it instead.")
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

#[derive(serde::Deserialize)]
struct RepayQuery {
    repay: String,
}

/// S18: a market maker quote for repaying part of the debt from collateral.
async fn deleverage_quote(
    axum::Extension(mm): axum::Extension<MarketMaker>,
    Param(id): Param<B256>,
    query: Result<axum::extract::Query<RepayQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    let Ok(axum::extract::Query(q)) = query else {
        return Err(ApiError::bad_request());
    };
    // Only a decimal amount is passed on.
    let repay: u128 = q.repay.parse().map_err(|_| ApiError::bad_request())?;
    mm_forward(mm.http.get(format!("{}/deleverage-quote/{id}?repay={repay}", mm.url))).await
}

async fn deleverage_submit(
    axum::Extension(mm): axum::Extension<MarketMaker>,
    Body(b): Body<serde_json::Value>,
) -> ApiResult {
    mm_forward(mm.http.post(format!("{}/deleverage", mm.url)).json(&b)).await
}

async fn close_submit(
    axum::Extension(mm): axum::Extension<MarketMaker>,
    Body(b): Body<serde_json::Value>,
) -> ApiResult {
    mm_forward(mm.http.post(format!("{}/close", mm.url)).json(&b)).await
}

// ------------------------------------------------------------------------- demo price controls

/// RQ-06: off Base mainnet the market's oracle is a DemoShockOracle (MOCK / DEMO) around a base
/// ZEC/USD price; on Base mainnet the market prices with the ZecuredPriceFeed itself (T-10), and
/// the demo price controls refuse with [`NOT_DEMO_ORACLE`].
#[derive(Clone)]
struct Oracle {
    rpc: String,
    /// The market's oracle.
    market: Option<Address>,
    /// The market's oracle when it is the deployment's DemoShockOracle.
    demo: Option<Address>,
    /// The base oracle, when it is the owner-set MockZecOracle (local fork, or no real feed yet).
    base_mock: Option<Address>,
    owner_key: String,
}

impl Oracle {
    /// From the deployment file: `market.oracle`, `contracts.DemoShockOracle` (written only when
    /// it is the market's oracle, which is checked here too) and `contracts.MockZecOracle`.
    fn from_deployment(rpc: String, deployment: &serde_json::Value, owner_key: String) -> Self {
        let address = |section: &str, name: &str| deployment.get(section)?.get(name)?.as_str()?.parse::<Address>().ok();
        let market = address("market", "oracle");
        Self {
            rpc,
            market,
            demo: address("contracts", "DemoShockOracle").filter(|demo| Some(*demo) == market),
            base_mock: address("contracts", "MockZecOracle"),
            owner_key,
        }
    }

    /// The DemoShockOracle, if the market's oracle is one; a fixed refusal otherwise, before any
    /// chain access.
    fn demo(&self) -> Result<Address, ApiError> {
        self.market.ok_or(ApiError::NOT_CONFIGURED)?;
        self.demo.ok_or(NOT_DEMO_ORACLE)
    }
}

/// The market's oracle is a real price feed (Base mainnet): no demo price control.
const NOT_DEMO_ORACLE: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "not_demo_oracle",
    "The market's oracle is a real price feed: the demo price controls are off.",
);

alloy::sol! {
    /// Morpho's oracle interface, which every market oracle has.
    #[sol(rpc)]
    interface IMarketOracle {
        function price() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IMockZecOracle {
        function price() external view returns (uint256);
        function setPrice(uint256 newPrice) external;
    }

    #[sol(rpc)]
    interface IDemoShockOracle {
        function price() external view returns (uint256);
        function base() external view returns (address);
        function shockBps() external view returns (uint256);
        function setShock(uint256 newShockBps) external;
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
    let market = o.market.ok_or(ApiError::NOT_CONFIGURED)?;
    let p = alloy::providers::ProviderBuilder::new().connect_http(o.rpc.parse().map_err(chain_error)?);
    // The market's price, which Morpho uses: any market oracle has it.
    let effective = IMarketOracle::new(market, &p).price().call().await.map_err(chain_error)?;
    let cents = |raw: alloy::primitives::U256| (raw / alloy::primitives::U256::from(ORACLE_SCALE_PER_CENT)).to_string();
    // The demo shock and the price below it, only on a DemoShockOracle (none on Base mainnet).
    let (base, shock_bps) = match o.demo {
        Some(demo) => {
            let shock = IDemoShockOracle::new(demo, &p);
            let base_address = shock.base().call().await.map_err(chain_error)?;
            let base = IMockZecOracle::new(base_address, &p).price().call().await.map_err(chain_error)?;
            let shock_bps = shock.shockBps().call().await.map_err(chain_error)?;
            (json!(cents(base)), json!(shock_bps.to_string()))
        }
        None => (serde_json::Value::Null, serde_json::Value::Null),
    };
    Ok(Json(json!({
        "priceUsdCents": cents(effective),
        "raw": effective.to_string(),
        "demo": o.demo.is_some(),
        "basePriceUsdCents": base,
        "baseIsMock": o.base_mock.is_some(),
        "shockBps": shock_bps,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OracleBody {
    price_usd_cents: u64,
}

fn owner_provider(o: &Oracle) -> Result<impl alloy::providers::Provider, ApiError> {
    let key: alloy::signers::local::PrivateKeySigner = o.owner_key.parse().map_err(|_| ApiError::NOT_CONFIGURED)?;
    Ok(alloy::providers::ProviderBuilder::new()
        .wallet(alloy::network::EthereumWallet::from(key))
        .connect_http(o.rpc.parse().map_err(chain_error)?))
}

/// MOCK base price (only when the base oracle is the MockZecOracle below a DemoShockOracle).
async fn admin_set_oracle(
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(o): axum::Extension<Oracle>,
    headers: HeaderMap,
    Body(b): Body<OracleBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    o.demo()?;
    let address = o.base_mock.ok_or(ApiError::new(
        StatusCode::CONFLICT,
        "real_price",
        "The base price is a real feed: shock it with /admin/shock instead.",
    ))?;
    let p = owner_provider(&o)?;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShockBody {
    shock_bps: u64,
}

/// DEMO: the market's shock (DemoShockOracle.setShock: a lowering keeps at least 75 % of the
/// current shock and comes at least two minutes after the last one). Refused when the market's
/// oracle is not a DemoShockOracle (Base mainnet).
async fn admin_set_shock(
    axum::Extension(t): axum::Extension<AdminToken>,
    axum::Extension(o): axum::Extension<Oracle>,
    headers: HeaderMap,
    Body(b): Body<ShockBody>,
) -> ApiResult {
    check_admin(&t, &headers)?;
    let demo = o.demo()?;
    let p = owner_provider(&o)?;
    let shock = IDemoShockOracle::new(demo, &p);
    let call = shock.setShock(alloy::primitives::U256::from(b.shock_bps));
    // A refused step (too deep, too soon) is the contract's answer, not an outage.
    call.call().await.map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "shock_refused",
            "Shock refused: at most 25 % lower per step, two minutes apart.",
        )
    })?;
    let receipt = call
        .send()
        .await
        .map_err(chain_error)?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .map_err(chain_error)?;
    if !receipt.status() {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "reverted", "setShock reverted."));
    }
    Ok(Json(json!({ "shockBps": b.shock_bps.to_string(), "txHash": receipt.transaction_hash })))
}

// ------------------------------------------------------------------------- config and wiring

#[derive(Clone)]
struct PublicConfig(Arc<serde_json::Value>);

/// Public deployment parameters, so the app and the signers cannot drift apart; `openings` (section
/// 6) is read on every request, as the operator may switch it at any time.
async fn public_config(
    State(coordinator): App,
    axum::Extension(c): axum::Extension<PublicConfig>,
) -> Json<serde_json::Value> {
    let mut out = (*c.0).clone();
    out["openings"] = serde_json::to_value(coordinator.openings()).unwrap_or_default();
    Json(out)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

/// What `GET /status` shows that never changes while the backend runs: the commit it was built
/// from, the deployment it serves (review G5) and the privacy delays it runs with (H-05).
#[derive(Clone)]
struct StatusInfo {
    fixed: Arc<serde_json::Value>,
    /// The signer nodes' modes, cached so that `/status` never fans out to the nodes per request.
    nodes: Arc<tokio::sync::Mutex<Option<NodeModes>>>,
    /// v3's minting gates (T-01, T-03), read from the Intake and the bond and cached; `None`
    /// without a v3 deployment file.
    minting: Option<Arc<minting::MintingGates>>,
    /// The verifier versions (T-07a), read from ZecuredVersions and cached; `None` without one.
    versions: Option<Arc<versions::VerifierVersions>>,
}

/// When the nodes' modes were read, and what they were.
type NodeModes = (Instant, Vec<serde_json::Value>);

const STATUS_NODES_TTL: Duration = Duration::from_secs(15);

fn status_info(dep: &serde_json::Value, cfg: &NetworkConfig) -> StatusInfo {
    let field = |path: &[&str]| path.iter().fold(dep, |v, k| &v[*k]).clone();
    let fixed = json!({
        "commit": env!("ZECURED_COMMIT"),
        "treeClean": env!("ZECURED_TREE_CLEAN") == "true",
        "startedAt": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "deployment": {
            "chainId": field(&["chainId"]),
            "block": field(&["block"]),
            "registry": field(&["contracts", "ZecuredRegistry"]),
            "intake": field(&["contracts", "ZecuredIntake"]),
            "slasher": field(&["contracts", "ZecuredSlasher"]),
            "signerBond": field(&["contracts", "SignerBond"]),
            "compensator": field(&["contracts", "ZecuredCompensator"]),
            "versions": field(&["contracts", "ZecuredVersions"]),
            "mintMode": field(&["depositProofs", "mintMode"]),
            "verifierIsMock": field(&["depositProofs", "verifierIsMock"]),
            // The guests the deployment started with; `versions` below lists the current ones.
            "depositImageId": field(&["depositProofs", "imageId"]),
            "violationImageId": field(&["tier3", "violationImageId"]),
            "challengeWindow": field(&["tier3", "challengeWindow"]),
            "theftDemo": field(&["tier3", "theftDemo"]),
            "v3": v3_parameters(dep),
        },
        // L5 and L23: the random delays that hide when a deposit is minted and when a payout is broadcast.
        "privacyDelays": {
            "attestMaxBlocks": cfg.attest_delay_max_blocks,
            "payoutMaxBlocks": cfg.payout_delay_max_blocks,
            "localFork": cfg.local_fork,
        },
    });
    let contract = |name: &str| dep.get("contracts")?.get(name)?.as_str()?.parse::<Address>().ok();
    let minting = contract("ZecuredIntake").zip(contract("SignerBond")).and_then(|(intake, bond)| {
        minting::MintingGates::new(cfg.base_rpc_url.clone(), intake, bond, cfg.denominations.clone())
            .ok()
            .map(Arc::new)
    });
    let versions = contract("ZecuredVersions")
        .and_then(|registry| versions::VerifierVersions::new(cfg.base_rpc_url.clone(), registry).ok().map(Arc::new));
    StatusInfo {
        fixed: Arc::new(fixed),
        nodes: Arc::new(tokio::sync::Mutex::new(None)),
        minting,
        versions,
    }
}

/// The deployment file's public v3 parameters that the app and anyone checking the deployment
/// read (design notes T-01 to T-06: the opening fee, the refund floor and abandon period, the
/// pause, the liveness penalty, the coverage cap, the incident window); `null` before v3.
fn v3_parameters(dep: &serde_json::Value) -> serde_json::Value {
    const KEYS: [&str; 12] = [
        "feeBps",
        "minFeeZat",
        "minRefundZat",
        "refundAbandonPeriod",
        "pendingRefundDelay",
        "maxPause",
        "livenessPeriod",
        "commitGrace",
        "livenessPremiumBps",
        "coverageBps",
        "maxTotalZat",
        "incidentWindow",
    ];
    let Some(v3) = dep.get("v3").and_then(serde_json::Value::as_object) else {
        return serde_json::Value::Null;
    };
    KEYS.iter()
        .filter_map(|k| Some(((*k).to_owned(), v3.get(*k)?.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// `GET /status`: the running commit and image ids (G5), the Zcash tip of the last coordinator
/// pass, each signer node's mode, the deposit-proof queue's counts (H-12), v3's minting gates
/// (cached 15 s) and the verifier versions (T-07a, cached 60 s), each `null` while unreadable or
/// without v3. Nothing about any vault or user.
async fn status(State(c): App, axum::Extension(s): axum::Extension<StatusInfo>) -> Json<serde_json::Value> {
    let nodes = {
        let mut cache = s.nodes.lock().await;
        match cache.as_ref() {
            Some((at, nodes)) if at.elapsed() < STATUS_NODES_TTL => nodes.clone(),
            _ => {
                let nodes: Vec<serde_json::Value> = c
                    .node_health()
                    .await
                    .into_iter()
                    .map(|n| json!({ "index": n["index"], "mode": n["mode"] }))
                    .collect();
                *cache = Some((Instant::now(), nodes.clone()));
                nodes
            }
        }
    };
    let mut out = (*s.fixed).clone();
    out["zcashTip"] = c.zcash_tip().into();
    out["nodes"] = nodes.into();
    // Counts only (`null` without deposit proofs): proofs waiting, running, and run at once.
    out["depositProofs"] = serde_json::to_value(c.proving_status()).unwrap_or_default();
    out["minting"] = match &s.minting {
        Some(gates) => gates.json().await,
        None => serde_json::Value::Null,
    };
    out["versions"] = match &s.versions {
        Some(versions) => versions.json().await,
        None => serde_json::Value::Null,
    };
    Json(out)
}

/// The deployment file written by the deploy script (`Null` if there is none).
fn load_deployment(path: &std::path::Path) -> serde_json::Value {
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// The deployment's signer set (the Registry's), ascending; `None` if it lists none or a
/// malformed one.
fn deployment_signers(dep: &serde_json::Value) -> Option<Vec<Address>> {
    let mut signers: Vec<Address> =
        dep.get("signers")?.as_array()?.iter().map(|s| s.as_str()?.parse().ok()).collect::<Option<_>>()?;
    signers.sort();
    Some(signers)
}

/// `GET /config`: chain, contract addresses, market, signer set and signer-network parameters.
/// The app checks the addresses and the signer set against the ones pinned in its build.
/// `sponsorship`: the bundler, the paymaster and the paymaster allowlist are all configured, so the
/// app can send sponsored UserOps. `intake` and `v3`: the deployment's ZecuredIntake and its public
/// v3 parameters (`null` before v3).
fn public_config_json(cfg: &NetworkConfig, dep: &serde_json::Value, sponsorship: bool) -> serde_json::Value {
    json!({
        "chainId": cfg.chain_id,
        "registry": cfg.registry,
        "adapter": cfg.adapter,
        "intake": dep.pointer("/contracts/ZecuredIntake").cloned().unwrap_or(serde_json::Value::Null),
        "v3": v3_parameters(dep),
        "contracts": dep.get("contracts").cloned().unwrap_or(serde_json::Value::Null),
        "market": dep.get("market").cloned().unwrap_or(serde_json::Value::Null),
        "signers": deployment_signers(dep),
        "denominations": cfg.denominations.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
        "confirmations": cfg.confirmations,
        "vaultExpiryBlocks": cfg.vault_expiry_blocks,
        // H-01: the zero bits `POST /vaults`'s stamp needs.
        "openVaultStampBits": cfg.open_vault.stamp_bits,
        "sponsorship": sponsorship,
    })
}

/// True unless the deployment's signer set differs from the network's node addresses: owners
/// check every vault's `VaultOpened` signatures against the former, nodes sign with the latter.
fn signer_sets_agree(cfg: &NetworkConfig, dep: &serde_json::Value) -> bool {
    let mut nodes: Vec<Address> = cfg.nodes.iter().map(|n| n.eth).collect();
    nodes.sort();
    deployment_signers(dep).is_none_or(|s| s == nodes)
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
        // v3: `refundPending` (T-03). A deployment file without it (v2) sponsors nothing.
        intake: contract("ZecuredIntake")?,
        // v3: `slashLiveness` (T-05); the proxy confirms it is the Registry's `signerBond()`.
        bond: contract("SignerBond")?,
        // RQ-06: Circle's testnet USDC on Base Sepolia, a MockUSDC on the local fork.
        usdc: contract("LoanToken").or_else(|| contract("MockUSDC"))?,
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
    status: StatusInfo,
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
    let router = Router::new();
    // DEMO ONLY: the theft demo's sweep exists only in its own build (T3-6).
    #[cfg(feature = "demo-tier3")]
    let router = router.route("/admin/vaults/{id}/steal", post(admin_steal));
    router
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/config", get(public_config))
        .route("/vaults/{id}", get(get_vault))
        .route("/admin/nodes", get(admin_nodes))
        .route("/admin/nodes/{index}/mode", post(admin_mode))
        .route("/admin/vaults/{id}/drain", post(admin_drain))
        .route("/admin/oracle", get(admin_get_oracle).post(admin_set_oracle))
        .route("/admin/shock", post(admin_set_shock))
        // H-01: the open-vault limit is taken inside the handler, once the request's stamp checks out.
        .merge(
            Router::new()
                .route("/vaults", post(open_vault))
                .route_layer(axum::Extension(OpenVaultLimiter(limits.open_vault.clone()))),
        )
        .merge(Router::new().route("/recipients", post(recipients)).route_layer(limited(&limits.recipients)))
        .merge(
            Router::new()
                .route("/vaults/{id}/destination", post(destination))
                .route("/vaults/{id}/refund", post(refund))
                .route("/vaults/{id}/viewing-key", post(viewing_key))
                .route("/vaults/{id}/deposit-proof", post(deposit_proof))
                .route("/vaults/{id}/prove-refund", post(prove_refund))
                .route_layer(limited(&limits.owner)),
        )
        .merge(
            Router::new()
                .route("/claims/{claimId}/receipt", post(claim_receipt))
                .route_layer(limited(&limits.receipts)),
        )
        .merge(
            Router::new()
                .route("/quotes/close/{id}", get(close_quote))
                .route("/closes", post(close_submit))
                .route("/quotes/deleverage/{id}", get(deleverage_quote))
                .route("/deleverages", post(deleverage_submit))
                .route_layer(limited(&limits.market_maker)),
        )
        .layer(axum::Extension(w.public_config))
        .layer(axum::Extension(w.status))
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
    #[cfg(feature = "demo-tier3")]
    {
        // T3-6: the theft demo's backend never runs against the judge deployment.
        let signers: Vec<Address> = cfg.nodes.iter().map(|n| n.eth).collect();
        zecured_base::theft_demo_guard::check(cfg.registry, &signers)?;
        zecured_base::theft_demo_guard::check_on_chain(&cfg.base_rpc_url, cfg.registry).await?;
        warn!("DEMO (theft demo build): three nodes of this network can be compromised and steal");
    }
    let indices: Vec<u16> = cfg.nodes.iter().map(|n| n.index).collect();
    let modes = ModePolicy::parse(&cli.demo_compromisable_nodes, &indices)?;
    let cors = cors_layer(&cli.app_origins)?;
    let deployment = load_deployment(&cli.deployments);
    if !signer_sets_agree(&cfg, &deployment) {
        warn!(
            "the deployment file's signers are not the network config's node addresses: owners will refuse new vaults"
        );
    }
    let allowlist = load_allowlist(&cli.deployments);
    if allowlist.is_none() {
        warn!("no v3 deployment file: paymaster sponsorship disabled");
    }
    let proxy_rpc = if cli.proxy_rpc_url.is_empty() {
        cfg.base_rpc_url.clone()
    } else {
        cli.proxy_rpc_url.clone()
    };
    let own_paymaster = match (cli.paymaster_address.is_empty(), cli.paymaster_signer_key.is_empty()) {
        (true, true) => None,
        (false, false) => {
            let own = paymaster::OwnPaymaster::new(
                cli.paymaster_address.parse().context("PAYMASTER_ADDRESS")?,
                cli.paymaster_signer_key.parse().context("PAYMASTER_SIGNER_PRIVATE_KEY")?,
                cfg.base_rpc_url.clone(),
            )?;
            info!(paymaster = %own.address, "sponsoring with our own paymaster");
            Some(own)
        }
        _ => anyhow::bail!("set both PAYMASTER_ADDRESS and PAYMASTER_SIGNER_PRIVATE_KEY, or neither"),
    };
    let proxy = Arc::new(proxy::Proxy::new(
        proxy_rpc,
        cfg.base_rpc_url.clone(),
        Some(cli.bundler_url.clone()),
        Some(cli.paymaster_url.clone()),
        own_paymaster,
        allowlist,
    )?);
    let sponsorship = proxy.bundler_url.is_some()
        && (proxy.paymaster_url.is_some() || proxy.own_paymaster.is_some())
        && proxy.allowlist.is_some();
    let mut coordinator = Coordinator::new(cfg, &cli.data, &cli.coordinator_key)?;
    if let Some(host) = &cli.deposit_proof_host {
        coordinator = coordinator.with_deposit_proofs(
            DepositProofs {
                host: host.clone(),
                mock: cli.deposit_proof_mock,
                headers: cli.deposit_proof_headers.clone(),
                summaries: cli.deposit_proof_summaries.clone(),
                concurrency: cli.deposit_proof_concurrency,
            },
            &cli.data,
        )?;
        info!(
            mock = cli.deposit_proof_mock,
            concurrency = cli.deposit_proof_concurrency,
            "deposits are activated with Tier 2 proofs"
        );
    }
    if let Some(file) = &cli.invites {
        coordinator = coordinator.with_invites(file, &cli.data)?;
        let o = coordinator.openings();
        info!(open = o.open, invite_only = o.invite_only, "vault openings follow the operator's invite file");
    }
    let coordinator = Arc::new(coordinator);

    let worker = coordinator.clone();
    tokio::spawn(supervise(Duration::from_secs(cli.tick_secs), move || {
        let worker = worker.clone();
        async move { worker.tick().await }
    }));

    let wiring = Wiring {
        public_config: PublicConfig(Arc::new(public_config_json(&coordinator.cfg, &deployment, sponsorship))),
        status: status_info(&deployment, &coordinator.cfg),
        admin_token: AdminToken(Arc::new(cli.admin_token.clone())),
        modes,
        oracle: Oracle::from_deployment(
            coordinator.cfg.base_rpc_url.clone(),
            &deployment,
            cli.oracle_owner_key.clone(),
        ),
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
        assert_eq!(
            ModePolicy::parse("1,2,3", &nodes).is_ok(),
            cfg!(feature = "demo-tier3"),
            "three nodes reach the threshold: only the theft demo's build allows it"
        );
        assert!(ModePolicy::parse("1,2,3,4", &nodes).is_err());
        assert!(ModePolicy::parse("4,5,4", &nodes).is_ok(), "duplicates count once");
        assert!(ModePolicy::parse("6", &nodes).is_err());
        assert!(ModePolicy::parse("four", &nodes).is_err());
        let none = ModePolicy::parse("", &nodes).unwrap();
        assert_eq!(none.check(4, "compromised"), Err(ApiError::MODE_NOT_ALLOWED));
    }

    /// The demo panel learns which nodes it may offer "compromised" for.
    #[test]
    fn node_health_says_which_nodes_are_compromisable() {
        let p = ModePolicy::parse("4,5", &[1, 2, 3, 4, 5]).unwrap();
        let nodes = p.annotate(vec![
            json!({ "index": 1, "mode": "normal", "vaults": 2 }),
            json!({ "index": 4, "mode": "unreachable" }),
            json!({ "index": 5, "mode": "compromised", "vaults": 2 }),
        ]);
        let flags: Vec<bool> = nodes.iter().map(|n| n["compromisable"].as_bool().unwrap()).collect();
        assert_eq!(flags, [false, true, true]);
        assert_eq!(nodes[0], json!({ "index": 1, "mode": "normal", "vaults": 2, "compromisable": false }));
    }

    /// C-4: the vault comes with the hash of its tag key and every signer's signature.
    #[test]
    fn open_vault_answers_with_the_signers_signatures() {
        let signature = |b: u8| Bytes::from(vec![b; 65]);
        let r = OpenVaultResponse {
            vault_id: B256::repeat_byte(7),
            address: "utest1vault".into(),
            zip321: "zcash:utest1vault?amount=0.01".into(),
            amount: 1_000_000,
            expiry_height: 4_486_288,
            tag_key_hash: B256::repeat_byte(9),
            signatures: (1..=5u8)
                .map(|i| zecured_coordinator::NodeSignature {
                    index: u16::from(i),
                    signature: signature(i),
                })
                .collect(),
        };
        let v = open_vault_json(&r);
        assert_eq!(v["vaultId"], json!(B256::repeat_byte(7)));
        assert_eq!((v["amount"].as_str(), v["expiryHeight"].as_u64()), (Some("1000000"), Some(4_486_288)));
        assert_eq!(v["tagKeyHash"], json!(format!("0x{}", "09".repeat(32))));
        assert!(v.get("ufvkHash").is_none(), "VaultOpened v2 signs no hash of the viewing key");
        let sigs = v["signatures"].as_array().unwrap();
        assert_eq!(sigs.len(), 5);
        assert_eq!(sigs[1], json!({ "index": 2, "signature": format!("0x{}", "02".repeat(65)) }));
    }

    /// `GET /config` reports the deployment's signer set (ascending), for the app to compare
    /// with the one pinned in its build.
    #[test]
    fn config_reports_the_deployments_signer_set() {
        let keys: Vec<Address> = (1..=5u8).map(Address::repeat_byte).collect();
        let nodes: Vec<serde_json::Value> = keys
            .iter()
            .zip(1u16..)
            .map(|(eth, index)| json!({ "index": index, "url": "http://127.0.0.1:9", "x25519": "00", "eth": eth }))
            .collect();
        let cfg: NetworkConfig = serde_json::from_value(json!({
            "lightwalletd_url": "http://127.0.0.1:9", "base_rpc_url": "http://127.0.0.1:9", "chain_id": 84_532,
            "registry": Address::repeat_byte(0x11), "adapter": Address::repeat_byte(0x22), "registry_from_block": 1,
            "confirmations": 3, "vault_expiry_blocks": 288, "denominations": [1_000_000], "nodes": nodes,
        }))
        .unwrap();
        let reversed: Vec<String> = keys.iter().rev().map(|a| a.to_string()).collect();
        let dep = json!({ "contracts": {}, "signers": reversed });
        let c = public_config_json(&cfg, &dep, true);
        assert_eq!(c["signers"], json!(keys));
        // H-01: the stamp's bits, the default when the network config sets none.
        assert_eq!(c["openVaultStampBits"], 20);
        assert!(signer_sets_agree(&cfg, &dep));
        // Another set: reported as is (the app then refuses it), and flagged at startup.
        let other = json!({ "signers": [Address::repeat_byte(9), keys[1], keys[2], keys[3], keys[4]] });
        assert!(!signer_sets_agree(&cfg, &other));
        // No deployment file, or no or malformed signers: none reported.
        for dep in [
            serde_json::Value::Null,
            json!({ "contracts": {} }),
            json!({ "signers": ["0x12"] }),
        ] {
            assert_eq!(public_config_json(&cfg, &dep, false)["signers"], serde_json::Value::Null);
            assert!(signer_sets_agree(&cfg, &dep));
        }
    }

    /// A v3 deployment file as the deploy script writes it (`contracts/script/DeployZecured.s.sol`
    /// `_writeJson`; addresses of a simulated deployment on a Base Sepolia fork), the parts the
    /// backend reads.
    fn v3_deployment() -> serde_json::Value {
        json!({
            "chainId": 84_532,
            "block": 48_198_682,
            "contracts": {
                "BaseOracle": "0xC2f3515C272383D2b5e055D4CF39c9d719D232e1",
                "CapPrice": "0x2860F0aB28D7ADa29CFB4D74B60bCD29C414BD20",
                "DemoShockOracle": "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
                "LoanToken": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
                "MarketOracle": "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
                "MockZecOracle": "0xC2f3515C272383D2b5e055D4CF39c9d719D232e1",
                "SettlementAdapter": "0x3074AcAEa758A41C42f6EbCf5686f4a7f24ac957",
                "SignerBond": "0xf42821eF06F68a01bba08c91a16368A83C002586",
                "ZZEC": "0xbB63C296eb88567318C36fa29a26be77D8695b84",
                "ZecuredCompensator": "0xEe045dF461915D9E4C919DAFbB2Df2af7e62922b",
                "ZecuredIntake": "0x5197D7051ac19aD80dbD8829E46062B8d6d451aE",
                "ZecuredRegistry": "0x2849a4FA18C4bD42687bD9B8eBDC2F4D89ECfB2F",
                "ZecuredSlasher": "0xCcF6091bB163431D3C51C3B48CC853549df003E2",
                "ZecuredSpendLogic": "0xDB0768AE34e9926ddF58eF1aBC4C82547616a542",
                "ZecuredVersions": "0x9BfbaDc8108F2e1959A2a58a647a6DAf07C99527",
            },
            "market": {
                "collateralToken": "0xbB63C296eb88567318C36fa29a26be77D8695b84",
                "irm": "0x46415998764C29aB2a25CbeA6254146D50D22687",
                "lltv": "625000000000000000",
                "loanToken": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
                "marketId": "0xabd581e8f7e88ea8e3e7abde60d9aea72986782f9b5d3fb3518b42968a89817e",
                "morphoBlue": "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
                "oracle": "0x6065991f617AF84f4BC48B9B42A14c2CEfFfBF58",
            },
            "depositProofs": {
                "intake": "0x5197D7051ac19aD80dbD8829E46062B8d6d451aE",
                "versions": "0x9BfbaDc8108F2e1959A2a58a647a6DAf07C99527",
                "imageId": format!("0x{}", "11".repeat(32)),
                "mintMode": "proof-only",
                "verifierIsMock": false,
            },
            "tier3": { "challengeWindow": 86_400, "theftDemo": false, "violationImageId": format!("0x{}", "22".repeat(32)) },
            "v3": {
                "autoPauseOnCase": false, "bountyBps": 1_000, "bountyCap": "50000000", "capPriceMaxAge": 3_600,
                "commitGrace": 7_200, "coverageBps": 10_000, "feeBps": 50,
                "feeCommitment": format!("0x{}", "33".repeat(32)), "incidentWindow": 259_200,
                "leftoverSink": "0x000000000000000000000000000000000000dEaD", "livenessPeriod": 86_400,
                "livenessPremiumBps": 500, "maxPause": 2_592_000, "maxTotalZat": 150_000_000, "minBond": "100000000",
                "minFeeZat": 20_000, "minRefundZat": 20_000, "minSeizeBps": 0, "pendingRefundDelay": 86_400,
                "refundAbandonPeriod": 2_592_000, "unbondingPeriod": 194_400,
            },
        })
    }

    /// `v3_deployment()` as on Base mainnet: the market prices with the ZecuredPriceFeed itself
    /// (T-10), so the file has no DemoShockOracle and no MockZecOracle.
    fn mainnet_deployment() -> serde_json::Value {
        let mut dep = v3_deployment();
        let feed = json!(Address::repeat_byte(0xf0));
        let contracts = dep["contracts"].as_object_mut().unwrap();
        contracts.remove("DemoShockOracle");
        contracts.remove("MockZecOracle");
        contracts.insert("BaseOracle".into(), feed.clone());
        contracts.insert("MarketOracle".into(), feed.clone());
        dep["market"]["oracle"] = feed;
        dep["chainId"] = json!(8_453);
        dep
    }

    /// RQ-06: the demo price controls act only on the market's own DemoShockOracle; without one
    /// (Base mainnet) they refuse with a fixed error.
    #[test]
    fn the_demo_price_controls_need_the_markets_demo_oracle() {
        let address = |v: &serde_json::Value| v.as_str().unwrap().parse::<Address>().unwrap();
        let oracle = |dep: &serde_json::Value| Oracle::from_deployment(String::new(), dep, String::new());
        let dep = v3_deployment();
        let o = oracle(&dep);
        assert_eq!(o.market, Some(address(&dep["market"]["oracle"])));
        assert_eq!(o.demo(), Ok(address(&dep["contracts"]["DemoShockOracle"])));
        assert_eq!(o.base_mock, Some(address(&dep["contracts"]["MockZecOracle"])));
        let mainnet = mainnet_deployment();
        let o = oracle(&mainnet);
        assert_eq!(o.market, Some(Address::repeat_byte(0xf0)));
        assert_eq!((o.demo(), o.base_mock), (Err(NOT_DEMO_ORACLE), None));
        // A DemoShockOracle that is not the market's is never shocked.
        let mut stray = dep.clone();
        stray["market"]["oracle"] = json!(Address::repeat_byte(0xf0));
        assert_eq!(oracle(&stray).demo(), Err(NOT_DEMO_ORACLE));
        assert_eq!(oracle(&serde_json::Value::Null).demo(), Err(ApiError::NOT_CONFIGURED));
    }

    /// The paymaster allowlist's contracts come from the deployment file, the Intake and the bond
    /// included; a file without them (v2) sponsors nothing.
    #[test]
    fn the_allowlist_reads_the_v3_deployment_file() {
        let dir = std::env::temp_dir().join(format!("zecured-backend-test-{}", hex::encode(keccak256(b"allowlist"))));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deployment.json");
        let dep = v3_deployment();
        std::fs::write(&path, dep.to_string()).unwrap();
        let l = load_allowlist(&path).expect("a v3 deployment file");
        let address = |v: &serde_json::Value| v.as_str().unwrap().parse::<Address>().unwrap();
        assert_eq!(l.intake, address(&dep["contracts"]["ZecuredIntake"]));
        assert_eq!(l.bond, address(&dep["contracts"]["SignerBond"]));
        assert_eq!(l.registry, address(&dep["contracts"]["ZecuredRegistry"]));
        assert_eq!(l.adapter, address(&dep["contracts"]["SettlementAdapter"]));
        // RQ-06: the loan token, Circle's USDC here; the market as Morpho knows it.
        assert_eq!(l.usdc, address(&dep["contracts"]["LoanToken"]));
        assert_eq!(l.oracle, address(&dep["market"]["oracle"]));
        assert_eq!(l.lltv, alloy::primitives::U256::from(625_000_000_000_000_000u64));
        for v3_contract in ["ZecuredIntake", "SignerBond"] {
            let mut v2 = dep.clone();
            v2["contracts"].as_object_mut().unwrap().remove(v3_contract);
            std::fs::write(&path, v2.to_string()).unwrap();
            assert!(load_allowlist(&path).is_none(), "no {v3_contract}, no sponsorship");
        }
        assert!(load_allowlist(&dir.join("missing.json")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(market_maker_error("deleverage_too_large").code, "deleverage_too_large");
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
            "open_vault": { "stamp_bits": 8 },
        }))
        .unwrap();
        let key = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
        let coordinator = Arc::new(Coordinator::new(cfg, &dir, &key).unwrap());
        let proxy = Arc::new(
            proxy::Proxy::new("http://127.0.0.1:9".into(), "http://127.0.0.1:9".into(), None, None, None, None)
                .unwrap(),
        );
        let status = status_info(&serde_json::Value::Null, &coordinator.cfg);
        let wiring = Wiring {
            public_config: PublicConfig(Arc::new(json!({}))),
            status,
            admin_token: AdminToken(Arc::new("tok".into())),
            modes: ModePolicy::parse("4,5", &[1, 2, 3, 4, 5]).unwrap(),
            // Base mainnet: the market prices with a real feed.
            oracle: Oracle::from_deployment("http://127.0.0.1:9".into(), &mainnet_deployment(), String::new()),
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
        // G5: the status page names the running commit and each node's mode, nothing about vaults.
        let (status, body, _) = call(Method::GET, "/status", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["commit"], env!("ZECURED_COMMIT"));
        // H-05: the privacy delays the network runs with are public.
        assert_eq!(body["privacyDelays"], json!({ "attestMaxBlocks": 0, "payoutMaxBlocks": 0, "localFork": false }));
        // H-12: no deposit-proof queue on this backend.
        assert_eq!(body["depositProofs"], serde_json::Value::Null);
        // No v3 deployment file: no minting gates, no verifier versions.
        assert_eq!(body["minting"], serde_json::Value::Null);
        assert_eq!(body["versions"], serde_json::Value::Null);
        let nodes = body["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 5);
        assert!(nodes.iter().all(|n| n.as_object().is_some_and(|o| o.len() == 2) && n["mode"] == "unreachable"));
        let unknown = format!("/vaults/{}", B256::repeat_byte(9));
        let (status, body, _) = call(Method::GET, &unknown, "").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("unknown_vault")));
        assert!(body.get("address").is_none());
        // RQ-06: on a real feed the demo price controls refuse with a fixed error, before any chain
        // access (this backend's RPC is unreachable).
        for (uri, body) in [
            ("/admin/shock", r#"{"shockBps":500}"#),
            ("/admin/oracle", r#"{"priceUsdCents":100000}"#),
        ] {
            let (status, body, _) = call(Method::POST, uri, body).await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("not_demo_oracle")), "{uri}");
        }
        // C-3: no third compromised node, whatever the token.
        let (status, body, _) = call(Method::POST, "/admin/nodes/1/mode", r#"{"mode":"compromised"}"#).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("mode_not_allowed")));
        // The panel is told where "compromised" is allowed (the nodes themselves are unreachable).
        let (status, body, _) = call(Method::GET, "/admin/nodes", "").await;
        assert_eq!(status, StatusCode::OK);
        let compromisable: Vec<(u64, bool)> = body["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["index"].as_u64().unwrap(), n["compromisable"].as_bool().unwrap()))
            .collect();
        assert_eq!(compromisable, [(1, false), (2, false), (3, false), (4, true), (5, true)]);
        // M-8, H-01: POST /vaults allows a burst of 3 stamped requests, then answers 429. A request
        // without a valid stamp never spends the limit: malformed, unstamped or under-stamped ones.
        for _ in 0..5 {
            let (status, body, _) = call(Method::POST, "/vaults", "{}").await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        }
        let open_body = |owner: Address, stamped: bool| -> &'static str {
            let request_id = B256::repeat_byte(0x44);
            let valid = |nonce: u64| zecured_base::stamp::is_valid(&request_id, &owner, nonce, 8);
            let stamp = (0u64..).find(|n| valid(*n) == stamped).unwrap();
            let body = json!({
                "owner": owner, "account": Address::repeat_byte(0x55), "amount": "1000000", "requestId": request_id,
                "deadline": "99999999999", "signature": "0x", "stamp": stamp.to_string(),
            });
            Box::leak(body.to_string().into_boxed_str())
        };
        for i in 0..5u8 {
            let (status, body, _) = call(Method::POST, "/vaults", open_body(Address::repeat_byte(i), false)).await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_stamp")));
        }
        // Stamped but not signed by the owner: each takes a token, then the coordinator refuses it.
        for i in 0..3u8 {
            let (status, body, _) = call(Method::POST, "/vaults", open_body(Address::repeat_byte(i), true)).await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("not_owner")));
        }
        let (status, body, cors) = call(Method::POST, "/vaults", open_body(Address::repeat_byte(9), true)).await;
        assert_eq!((status, body["code"].as_str(), cors), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"), true));
        // Other routes have their own buckets.
        let (status, _, _) = call(Method::POST, "/recipients", "{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v4: the invite gate's switches in `/config` and on `POST /vaults` (section 6), the public
    /// liveness receipt with its own limit (R-5), and the owner's refund-proof request (R-11.1), each
    /// refusing with its fixed code on a coordinator whose nodes and chains are unreachable.
    #[tokio::test]
    async fn the_v4_routes_are_wired() {
        let dir = std::env::temp_dir().join(format!("zecured-backend-test-{}", hex::encode(keccak256(b"v4-routes"))));
        std::fs::create_dir_all(&dir).unwrap();
        let invites = dir.join("invites.json");
        let write = |v: serde_json::Value| std::fs::write(&invites, v.to_string()).unwrap();
        write(
            json!({ "openings": false, "inviteOnly": true, "codes": [{ "hash": keccak256(b"beta-7f3k"), "uses": 1 }] }),
        );
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
        let coordinator = Arc::new(Coordinator::new(cfg, &dir, &key).unwrap().with_invites(&invites, &dir).unwrap());
        let proxy = Arc::new(
            proxy::Proxy::new("http://127.0.0.1:9".into(), "http://127.0.0.1:9".into(), None, None, None, None)
                .unwrap(),
        );
        let wiring = Wiring {
            public_config: PublicConfig(Arc::new(json!({ "chainId": 84_532 }))),
            status: status_info(&serde_json::Value::Null, &coordinator.cfg),
            admin_token: AdminToken(Arc::new(String::new())),
            modes: ModePolicy::parse("4,5", &[1, 2, 3, 4, 5]).unwrap(),
            oracle: Oracle::from_deployment(String::new(), &serde_json::Value::Null, String::new()),
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
        let call = |method: Method, uri: String, body: String| {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(HttpBody::from(body))
                .unwrap();
            let app = app.clone();
            async move {
                let r = app.oneshot(req).await.unwrap();
                let status = r.status();
                let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
                (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default())
            }
        };

        // Section 6: `/config` says whether openings are open and invite-only, read on each request.
        let (status, body) = call(Method::GET, "/config".into(), String::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["chainId"], 84_532, "the deployment's parameters stay");
        assert_eq!(body["openings"], json!({ "open": false, "inviteOnly": true }));
        // Closed: refused before anything else, an unstamped request included.
        let open = json!({
            "owner": Address::repeat_byte(1), "account": Address::repeat_byte(2), "amount": "1000000",
            "requestId": B256::repeat_byte(3), "deadline": "99999999999", "signature": "0x", "invite": "beta-7f3k",
        });
        let (status, body) = call(Method::POST, "/vaults".into(), open.to_string()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::FORBIDDEN, Some("openings_closed")));
        write(json!({ "openings": true }));
        let (_, body) = call(Method::GET, "/config".into(), String::new()).await;
        assert_eq!(body["openings"], json!({ "open": true, "inviteOnly": false }));
        let (status, body) = call(Method::POST, "/vaults".into(), open.to_string()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_stamp")));

        // R-5: no receipt for a claim this coordinator does not know; a malformed id is a bad request.
        let receipt = format!("/claims/{}/receipt", B256::repeat_byte(0x77));
        let (status, body) = call(Method::POST, receipt.clone(), String::new()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("no_receipt")));
        let (status, body) = call(Method::POST, "/claims/0x12/receipt".into(), String::new()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        // Its own bucket: 20 a minute, then 429, while the owner routes still answer.
        for _ in 0..18 {
            call(Method::POST, receipt.clone(), String::new()).await;
        }
        let (status, body) = call(Method::POST, receipt.clone(), String::new()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));

        // R-11.1: a malformed request is a bad request; a backend that makes no deposit proofs
        // cannot prove a refund either.
        let prove = format!("/vaults/{}/prove-refund", B256::repeat_byte(9));
        let (status, body) = call(Method::POST, prove.clone(), "{}".into()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        let signed = json!({ "nonce": "1", "deadline": "99999999999", "signature": format!("0x{}", "11".repeat(65)) });
        let (status, body) = call(Method::POST, prove, signed.to_string()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("not_configured")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// H-12: `/status` shows the deposit-proof queue's counts, and nothing else about it.
    #[tokio::test]
    async fn status_counts_the_deposit_proofs() {
        let dir = std::env::temp_dir().join(format!("zecured-backend-test-{}", hex::encode(keccak256(b"proof-queue"))));
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
        let proofs = DepositProofs {
            host: PathBuf::from("zecured-tier2-host"),
            mock: true,
            headers: PathBuf::new(),
            summaries: PathBuf::new(),
            concurrency: 2,
        };
        let coordinator = Coordinator::new(cfg, &dir, &key).unwrap().with_deposit_proofs(proofs, &dir).unwrap();
        let info = status_info(&serde_json::Value::Null, &coordinator.cfg);
        let Json(body) = status(State(Arc::new(coordinator)), axum::Extension(info)).await;
        assert_eq!(body["depositProofs"], json!({ "queued": 0, "running": 0, "concurrency": 2 }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v3: `/config` names the Intake and the deployment's public v3 parameters; `/status` too,
    /// with the minting gates once the chain answers (`null` here: the node is unreachable).
    #[tokio::test]
    async fn status_and_config_report_the_v3_deployment() {
        let dir = std::env::temp_dir().join(format!("zecured-backend-test-{}", hex::encode(keccak256(b"v3-status"))));
        let nodes: Vec<serde_json::Value> = (1..=5u8)
            .map(|i| json!({ "index": i, "url": "http://127.0.0.1:9", "x25519": "00", "eth": Address::repeat_byte(i) }))
            .collect();
        let cfg: NetworkConfig = serde_json::from_value(json!({
            "lightwalletd_url": "http://127.0.0.1:9", "base_rpc_url": "http://127.0.0.1:9", "chain_id": 84_532,
            "registry": Address::repeat_byte(0x11), "adapter": Address::repeat_byte(0x22), "registry_from_block": 1,
            "confirmations": 3, "vault_expiry_blocks": 288, "denominations": [1_000_000, 5_000_000], "nodes": nodes,
        }))
        .unwrap();
        let dep = v3_deployment();
        let parameters = json!({
            "feeBps": 50, "minFeeZat": 20_000, "minRefundZat": 20_000, "refundAbandonPeriod": 2_592_000,
            "pendingRefundDelay": 86_400, "maxPause": 2_592_000, "livenessPeriod": 86_400, "commitGrace": 7_200,
            "livenessPremiumBps": 500, "coverageBps": 10_000, "maxTotalZat": 150_000_000, "incidentWindow": 259_200,
        });
        let c = public_config_json(&cfg, &dep, true);
        assert_eq!(c["intake"], dep["contracts"]["ZecuredIntake"]);
        assert_eq!(c["v3"], parameters, "only the parameters the app reads");
        assert_eq!(c["contracts"]["ZecuredCompensator"], dep["contracts"]["ZecuredCompensator"]);
        let before = public_config_json(&cfg, &serde_json::Value::Null, false);
        assert_eq!((&before["intake"], &before["v3"]), (&serde_json::Value::Null, &serde_json::Value::Null));

        let info = status_info(&dep, &cfg);
        assert!(info.minting.is_some(), "a v3 file names the Intake and the bond");
        assert!(info.versions.is_some(), "and ZecuredVersions");
        let none = status_info(&serde_json::Value::Null, &cfg);
        assert!(none.minting.is_none() && none.versions.is_none());
        let key = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
        let coordinator = Coordinator::new(cfg, &dir, &key).unwrap();
        let Json(body) = status(State(Arc::new(coordinator)), axum::Extension(info)).await;
        assert_eq!(body["deployment"]["intake"], dep["contracts"]["ZecuredIntake"]);
        assert_eq!(body["deployment"]["compensator"], dep["contracts"]["ZecuredCompensator"]);
        assert_eq!(body["deployment"]["versions"], dep["contracts"]["ZecuredVersions"]);
        assert_eq!(body["deployment"]["v3"], parameters);
        assert_eq!(body["minting"], serde_json::Value::Null);
        assert_eq!(body["versions"], serde_json::Value::Null);
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
