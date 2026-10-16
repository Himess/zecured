//! Small read-only helpers for what `GET /status` reads from Base itself (minting.rs,
//! versions.rs): one `eth_call`, and a JSON value read at most once per period.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use alloy::{
    primitives::{Address, Bytes},
    sol_types::SolCall,
};
use serde_json::{Value, json};

/// Why an `eth_call` gave no result.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The node answered with an error: the call reverted.
    Reverted,
    /// No usable answer: the node is unreachable or answered something else.
    Unavailable,
}

/// One `eth_call` at the latest block; nothing about the caller is sent.
pub async fn eth_call(http: &reqwest::Client, rpc_url: &str, to: Address, data: Vec<u8>) -> Result<Vec<u8>, CallError> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{ "to": to, "data": Bytes::from(data) }, "latest"],
    });
    let resp = http.post(rpc_url).json(&body).send().await.map_err(|_| CallError::Unavailable)?;
    let v: Value = resp.json().await.map_err(|_| CallError::Unavailable)?;
    if let Some(result) = v.get("result") {
        let out: Bytes = serde_json::from_value(result.clone()).map_err(|_| CallError::Unavailable)?;
        return Ok(out.to_vec());
    }
    if v.get("error").is_some() {
        return Err(CallError::Reverted);
    }
    Err(CallError::Unavailable)
}

/// A view call that must answer (`None` if it reverts, the node is unreachable, or the answer
/// does not decode).
pub async fn view<C: SolCall>(http: &reqwest::Client, rpc_url: &str, to: Address, call: C) -> Option<C::Return> {
    let out = eth_call(http, rpc_url, to, call.abi_encode()).await.ok()?;
    C::abi_decode_returns(&out).ok()
}

/// A JSON value read from the chain at most once per `ttl` (a failed read is cached as `null`
/// for the same time, so an unreachable node is not hammered by `/status`).
pub struct Cached {
    ttl: Duration,
    value: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

impl Cached {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            value: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn get<F, Fut>(&self, read: F) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<Value>>,
    {
        let mut cache = self.value.lock().await;
        if let Some((at, value)) = cache.as_ref()
            && at.elapsed() < self.ttl
        {
            return value.clone();
        }
        let value = read().await.unwrap_or(Value::Null);
        *cache = Some((Instant::now(), value.clone()));
        value
    }
}
