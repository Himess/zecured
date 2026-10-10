//! Privacy proxy for the browser: Base RPC, ERC-4337 bundler and paymaster.
//!
//! The browser never talks to infrastructure providers directly, so they never learn the
//! user's IP. Requests are forwarded without client headers (no X-Forwarded-For), bodies are
//! never logged, and only the JSON-RPC methods the app needs are allowed. Every request that
//! carries a UserOp is checked against the paymaster allowlist before it is forwarded.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::allowlist::{Allowlist, UserOpView};

/// Read-only Base RPC methods the app uses.
const RPC_METHODS: &[&str] = &[
    "eth_chainId",
    "eth_blockNumber",
    "eth_call",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getLogs",
    "eth_getBlockByNumber",
    "eth_getTransactionReceipt",
    "eth_getTransactionCount",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_feeHistory",
    "eth_estimateGas",
];

/// ERC-4337 bundler methods (v0.7) the app uses.
const BUNDLER_METHODS: &[&str] = &[
    "eth_chainId",
    "eth_supportedEntryPoints",
    "eth_estimateUserOperationGas",
    "eth_sendUserOperation",
    "eth_getUserOperationByHash",
    "eth_getUserOperationReceipt",
    "pimlico_getUserOperationGasPrice",
    "pimlico_getUserOperationStatus",
];

/// Paymaster methods: ERC-7677 and Pimlico's sponsorship call.
const PAYMASTER_METHODS: &[&str] = &["pm_getPaymasterStubData", "pm_getPaymasterData", "pm_sponsorUserOperation"];

/// Methods whose first parameter is a UserOp that must pass the allowlist.
const USEROP_METHODS: &[&str] = &[
    "eth_sendUserOperation",
    "eth_estimateUserOperationGas",
    "pm_getPaymasterStubData",
    "pm_getPaymasterData",
    "pm_sponsorUserOperation",
];

pub struct Proxy {
    http: reqwest::Client,
    pub rpc_url: String,
    pub bundler_url: Option<String>,
    pub paymaster_url: Option<String>,
    pub allowlist: Option<Allowlist>,
}

pub type ProxyState = State<Arc<Proxy>>;

impl Proxy {
    pub fn new(rpc_url: String, bundler_url: Option<String>, paymaster_url: Option<String>, allowlist: Option<Allowlist>) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            rpc_url,
            bundler_url: bundler_url.filter(|u| !u.is_empty()),
            paymaster_url: paymaster_url.filter(|u| !u.is_empty()),
            allowlist,
        })
    }

    fn check(&self, req: &Value, methods: &[&str]) -> Result<()> {
        let method = req.get("method").and_then(Value::as_str).context("missing method")?;
        if !methods.contains(&method) {
            bail!("method {method} not allowed");
        }
        if USEROP_METHODS.contains(&method) {
            let op = req
                .get("params")
                .and_then(|p| p.get(0))
                .cloned()
                .context("missing UserOp")?;
            let op: UserOpView = serde_json::from_value(op).context("malformed UserOp")?;
            self.allowlist
                .as_ref()
                .context("sponsorship not configured")?
                .check(&op)?;
        }
        Ok(())
    }

    async fn forward(&self, url: &str, body: &Value, methods: &[&str]) -> Response {
        // Single request or batch; every element must be allowed.
        let checked = match body {
            Value::Array(items) => items.iter().try_for_each(|r| self.check(r, methods)),
            other => self.check(other, methods),
        };
        if let Err(e) = checked {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            return (
                StatusCode::OK,
                Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{e:#}") } })),
            )
                .into_response();
        }
        match self.post(url, body).await {
            Ok(v) => (StatusCode::OK, Json(v)).into_response(),
            Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("{e:#}") }))).into_response(),
        }
    }

    async fn post(&self, url: &str, body: &Value) -> Result<Value> {
        let resp = self.http.post(url).json(body).send().await.map_err(|_| anyhow!("upstream unreachable"))?;
        resp.json().await.map_err(|_| anyhow!("upstream returned invalid JSON"))
    }
}

pub async fn rpc(State(p): ProxyState, Json(body): Json<Value>) -> Response {
    let url = p.rpc_url.clone();
    p.forward(&url, &body, RPC_METHODS).await
}

pub async fn bundler(State(p): ProxyState, Json(body): Json<Value>) -> Response {
    match p.bundler_url.clone() {
        Some(url) => p.forward(&url, &body, BUNDLER_METHODS).await,
        None => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "bundler not configured" }))).into_response(),
    }
}

pub async fn paymaster(State(p): ProxyState, Json(body): Json<Value>) -> Response {
    match p.paymaster_url.clone() {
        Some(url) => p.forward(&url, &body, PAYMASTER_METHODS).await,
        None => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "paymaster not configured" }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> Proxy {
        Proxy::new("http://127.0.0.1:1".into(), None, None, None).unwrap()
    }

    #[test]
    fn write_methods_are_refused_on_the_rpc_proxy() {
        let p = proxy();
        assert!(p.check(&json!({"method": "eth_call", "params": []}), RPC_METHODS).is_ok());
        assert!(p.check(&json!({"method": "eth_sendRawTransaction", "params": []}), RPC_METHODS).is_err());
        assert!(p.check(&json!({"method": "eth_accounts"}), RPC_METHODS).is_err());
    }

    #[test]
    fn userops_need_a_configured_allowlist() {
        let p = proxy();
        let req = json!({"method": "pm_getPaymasterData", "params": [{"sender": "0x0000000000000000000000000000000000000001", "callData": "0x"}]});
        assert!(p.check(&req, PAYMASTER_METHODS).is_err());
    }
}
