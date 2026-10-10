//! HTTP API of a signer node (called by the coordinator only; bind it to a private address).

use std::sync::Arc;

use alloy::primitives::B256;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ConfirmIn, DestinationIn, Mode, Node, RecipientIn, RefundIn, dkg, signing};

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
}

async fn health(State(n): State<Arc<Node>>) -> Json<Health> {
    Json(Health {
        index: n.id.index,
        mode: n.mode().await,
        vaults: n.store.lock().await.vaults.len(),
    })
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

async fn attest(State(n): State<Arc<Node>>, Path(id): Path<B256>) -> ApiResult<crate::AttestResponse> {
    online(&n).await?;
    Ok(Json(n.attest(id).await?))
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

async fn refund(State(n): State<Arc<Node>>, Path(id): Path<B256>, Json(b): Json<RefundIn>) -> ApiResult<serde_json::Value> {
    online(&n).await?;
    n.refund_request(id, b).await?;
    Ok(Json(json!({ "accepted": true })))
}

async fn commit(State(n): State<Arc<Node>>, Json(b): Json<signing::CommitRequest>) -> ApiResult<signing::CommitResponse> {
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
    let mode = n
        .set_mode(b.mode, &b.token)
        .await
        .map_err(|e| ApiError(StatusCode::UNAUTHORIZED, e.to_string()))?;
    Ok(Json(json!({ "mode": mode })))
}

pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/dkg/round1", post(round1))
        .route("/v1/dkg/round2", post(round2))
        .route("/v1/dkg/round3", post(round3))
        .route("/v1/vaults/{id}/attest", post(attest))
        .route("/v1/vaults/{id}/destination", post(destination))
        .route("/v1/vaults/{id}/refund", post(refund))
        .route("/v1/vaults/{id}/confirm", post(confirm))
        .route("/v1/recipients", post(recipients))
        .route("/v1/sign/commit", post(commit))
        .route("/v1/sign/share", post(share))
        .route("/v1/sync", post(sync))
        .route("/v1/admin/mode", post(set_mode))
        .with_state(node)
}
