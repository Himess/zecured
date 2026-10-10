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

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let cfg = NetworkConfig::load(&cli.config)?;
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
        // The app calls this API from the browser; tighten the origin when deployed.
        .layer(CorsLayer::permissive())
        .with_state(coordinator);
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    info!(bind = %cli.bind, "backend listening");
    axum::serve(listener, app).await?;
    Ok(())
}
