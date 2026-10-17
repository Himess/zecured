//! HTTP API of a signer node (called by the coordinator only; bind it to a private address).
//!
//! Every route except `/v1/health` requires the coordinator's signature (D16), the demo mode
//! switch included (C-3), and a signed request is accepted only once (L-1).

use std::sync::Arc;

use alloy::primitives::B256;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ConfirmIn, DestinationIn, Mode, ModeError, Node, ReceiptIn, ReceiptOut, RecipientIn, RefundIn, dkg, signing,
};

/// An error returned to the coordinator. Messages describe the policy decision only and
/// never echo addresses, keys or salts.
pub struct ApiError(StatusCode, String);

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

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn online(node: &Node) -> Result<(), ApiError> {
    if node.mode().await == Mode::Offline {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "node offline (demo)".into()));
    }
    Ok(())
}

#[derive(Serialize)]
struct Health {
    index: u16,
    mode: Mode,
    vaults: usize,
    /// O-2: the last Base block whose Registry events this node applied (its view is current up to
    /// there): the coordinator waits for it before acting on a newer event.
    base_block: u64,
}

async fn health(State(n): State<Arc<Node>>) -> Json<Health> {
    let store = n.store.lock().await;
    Json(Health {
        index: n.id.index,
        mode: n.mode().await,
        vaults: store.vaults.len(),
        base_block: store.base_cursor,
    })
}

/// R-5: this node's liveness receipt for a claim whose destination it holds.
async fn receipt(State(n): State<Arc<Node>>, Json(b): Json<ReceiptIn>) -> ApiResult<ReceiptOut> {
    online(&n).await?;
    Ok(Json(n.receipt(b.claim_id).await?))
}

async fn round1(State(n): State<Arc<Node>>, Json(b): Json<dkg::VaultOpen>) -> ApiResult<dkg::Round1Out> {
    online(&n).await?;
    Ok(Json(n.dkg_round1(b).await?))
}

async fn round2(State(n): State<Arc<Node>>, Json(b): Json<dkg::Round2In>) -> ApiResult<dkg::Round2Out> {
    online(&n).await?;
    Ok(Json(n.dkg_round2(b).await?))
}

async fn round3(State(n): State<Arc<Node>>, Json(b): Json<dkg::Round3In>) -> ApiResult<dkg::Round3Out> {
    online(&n).await?;
    Ok(Json(n.dkg_round3(b).await?))
}

async fn recipients(State(n): State<Arc<Node>>, Json(b): Json<RecipientIn>) -> ApiResult<serde_json::Value> {
    online(&n).await?;
    Ok(Json(json!({ "commitment": n.add_recipient(b).await? })))
}

async fn destination(
    State(n): State<Arc<Node>>,
    Path(id): Path<B256>,
    Json(b): Json<DestinationIn>,
) -> ApiResult<serde_json::Value> {
    online(&n).await?;
    Ok(Json(json!({ "commitment": n.destination(id, b).await? })))
}

async fn refund(
    State(n): State<Arc<Node>>,
    Path(id): Path<B256>,
    Json(b): Json<RefundIn>,
) -> ApiResult<serde_json::Value> {
    online(&n).await?;
    n.refund_request(id, b).await?;
    Ok(Json(json!({ "accepted": true })))
}

async fn commit(
    State(n): State<Arc<Node>>,
    Json(b): Json<signing::CommitRequest>,
) -> ApiResult<signing::CommitResponse> {
    online(&n).await?;
    Ok(Json(n.sign_commit(b).await?))
}

async fn share(State(n): State<Arc<Node>>, Json(b): Json<signing::ShareRequest>) -> ApiResult<signing::ShareResponse> {
    online(&n).await?;
    Ok(Json(n.sign_share(b).await?))
}

async fn confirm(
    State(n): State<Arc<Node>>,
    Path(id): Path<B256>,
    Json(b): Json<ConfirmIn>,
) -> ApiResult<crate::ConfirmResponse> {
    online(&n).await?;
    Ok(Json(n.confirm(id, b).await?))
}

async fn sync(State(n): State<Arc<Node>>) -> ApiResult<serde_json::Value> {
    online(&n).await?;
    Ok(Json(json!({ "applied": n.sync_base().await? })))
}

#[derive(Deserialize)]
struct ModeIn {
    mode: Mode,
    token: String,
}

async fn set_mode(State(n): State<Arc<Node>>, Json(b): Json<ModeIn>) -> ApiResult<serde_json::Value> {
    let mode = n.set_mode(b.mode, &b.token).await.map_err(|e| {
        let status = match e {
            ModeError::Unauthorized => StatusCode::UNAUTHORIZED,
            ModeError::NotAllowed => StatusCode::FORBIDDEN,
        };
        ApiError(status, e.to_string())
    })?;
    Ok(Json(json!({ "mode": mode })))
}

/// Largest request body a node accepts (PCZTs are a few hundred kB at most).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// D16: every request except the health probe must be signed by the configured coordinator,
/// the demo mode switch included (C-3), and is accepted once (L-1).
async fn authenticate(State(n): State<Arc<Node>>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    if path == "/v1/health" {
        return next.run(req).await;
    }
    // No coordinator configured: only possible with --insecure-no-auth (development).
    let Some(coordinator) = n.cfg.coordinator else {
        return next.run(req).await;
    };
    let method = req.method().as_str().to_owned();
    let (ts, sig) = {
        let headers = req.headers();
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        (get(crate::auth::TS_HEADER), get(crate::auth::SIG_HEADER))
    };
    let (Some(ts), Some(sig)) = (ts, sig) else {
        return ApiError(StatusCode::UNAUTHORIZED, "unsigned request".into()).into_response();
    };
    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY_BYTES).await else {
        return ApiError(StatusCode::PAYLOAD_TOO_LARGE, "body too large".into()).into_response();
    };
    let now = crate::now();
    let checked = crate::auth::verify_signed(coordinator, &method, &path, &bytes, &ts, &sig, now)
        .and_then(|(digest, ts)| n.replay.check(digest, ts, now));
    if let Err(e) = checked {
        return ApiError(StatusCode::UNAUTHORIZED, e.to_string()).into_response();
    }
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/dkg/round1", post(round1))
        .route("/v1/dkg/round2", post(round2))
        .route("/v1/dkg/round3", post(round3))
        .route("/v1/vaults/{id}/destination", post(destination))
        .route("/v1/vaults/{id}/refund", post(refund))
        .route("/v1/vaults/{id}/confirm", post(confirm))
        .route("/v1/receipts", post(receipt))
        .route("/v1/recipients", post(recipients))
        .route("/v1/sign/commit", post(commit))
        .route("/v1/sign/share", post(share))
        .route("/v1/sync", post(sync))
        .route("/v1/admin/mode", post(set_mode))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(node.clone(), authenticate))
        .with_state(node)
}
