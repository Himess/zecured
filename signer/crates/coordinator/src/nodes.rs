//! HTTP client for the five signer nodes.
//!
//! Every POST carries the coordinator's D16 signature, including the demo mode switch.
//! Errors never embed node URLs: they say which node failed and whether it refused under its
//! policy or could not be reached.

use std::{fmt, time::Duration};

use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow};
use futures::future::join_all;
use serde::{Serialize, de::DeserializeOwned};
use tracing::debug;
use zecured_node::config::NetworkConfig;

use crate::error::{PublicError, scrub};

/// One node's answer to a broadcast request.
pub type Answer<T> = (u16, Result<T>);

/// Why a node did not give a usable answer.
#[derive(Debug)]
pub struct NodeError {
    pub index: u16,
    /// The node answered and refused under its policy (rather than failing to answer).
    pub refused: bool,
    /// The node's own reason, or what went wrong reaching it. Never a URL.
    pub reason: String,
}

impl NodeError {
    fn unavailable(index: u16, reason: impl Into<String>) -> Self {
        Self {
            index,
            refused: false,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = if self.refused { "refused" } else { "unavailable" };
        write!(f, "node {} {what}: {}", self.index, self.reason)
    }
}

impl std::error::Error for NodeError {}

/// True if `e` is a node refusing under its policy.
pub fn is_refusal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NodeError>().is_some_and(|n| n.refused)
}

/// Successful answers and failures of a broadcast.
pub fn split<T>(answers: Vec<Answer<T>>) -> (Vec<(u16, T)>, Vec<anyhow::Error>) {
    let mut ok = vec![];
    let mut failed = vec![];
    for (i, a) in answers {
        match a {
            Ok(v) => ok.push((i, v)),
            Err(e) => failed.push(e),
        }
    }
    (ok, failed)
}

/// The error for "fewer than `need` signers gave a usable answer": the public reason is a
/// refusal if any node refused under its policy, unavailability otherwise.
pub fn shortfall(what: &str, got: usize, need: usize, failures: &[anyhow::Error]) -> anyhow::Error {
    let public = if failures.iter().any(is_refusal) {
        PublicError::SignersRefused
    } else {
        PublicError::SignersUnavailable
    };
    let reasons: Vec<String> = failures.iter().map(|e| format!("{e:#}")).collect();
    anyhow!("{what}: {got} of {need} signers ({})", reasons.join("; ")).context(public)
}

#[derive(Clone)]
pub struct Nodes {
    http: reqwest::Client,
    nodes: Vec<(u16, String)>,
    /// Key that signs every request (D16).
    key: PrivateKeySigner,
}

impl Nodes {
    pub fn new(cfg: &NetworkConfig, key: PrivateKeySigner) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build().context("HTTP client")?;
        Ok(Self {
            http,
            nodes: cfg.nodes.iter().map(|n| (n.index, n.url.trim_end_matches('/').to_owned())).collect(),
            key,
        })
    }

    pub fn indices(&self) -> Vec<u16> {
        self.nodes.iter().map(|(i, _)| *i).collect()
    }

    pub fn contains(&self, index: u16) -> bool {
        self.nodes.iter().any(|(i, _)| *i == index)
    }

    fn base(&self, index: u16) -> Result<&str> {
        self.nodes
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, u)| u.as_str())
            .ok_or_else(|| NodeError::unavailable(index, "not in the network configuration").into())
    }

    /// POSTs `body` to `path` on node `index`, signed with the coordinator key (D16).
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, index: u16, path: &str, body: &B) -> Result<T> {
        let base = self.base(index)?;
        let bytes = serde_json::to_vec(body)?;
        let (ts, sig) = zecured_node::auth::sign(&self.key, "POST", path, &bytes, zecured_node::now())?;
        let sent = self
            .http
            .post(format!("{base}{path}"))
            .header("content-type", "application/json")
            .header(zecured_node::auth::TS_HEADER, ts)
            .header(zecured_node::auth::SIG_HEADER, sig)
            .body(bytes)
            .send()
            .await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                debug!(index, error = %scrub(&e.to_string()), "node request failed");
                let why = if e.is_timeout() {
                    "timed out"
                } else if e.is_connect() {
                    "unreachable"
                } else {
                    "request failed"
                };
                return Err(NodeError::unavailable(index, why).into());
            }
        };
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            let reason = value.get("error").and_then(|e| e.as_str()).unwrap_or("no reason given");
            // 4xx is a policy decision, except authentication, timeouts and rate limits.
            let refused = status.is_client_error() && !matches!(status.as_u16(), 401 | 408 | 429);
            return Err(NodeError {
                index,
                refused,
                reason: if refused {
                    reason.to_owned()
                } else {
                    format!("HTTP {}: {reason}", status.as_u16())
                },
            }
            .into());
        }
        serde_json::from_value(value).map_err(|_| NodeError::unavailable(index, "unexpected response").into())
    }

    /// GETs `path` on node `index` (unsigned: only `/v1/health` is open on a node).
    pub async fn get_json(&self, index: u16, path: &str) -> Result<serde_json::Value> {
        let base = self.base(index)?;
        let resp = self.http.get(format!("{base}{path}")).timeout(Duration::from_secs(3)).send().await;
        let resp = resp.map_err(|_| NodeError::unavailable(index, "unreachable"))?;
        resp.json().await.map_err(|_| NodeError::unavailable(index, "unexpected response").into())
    }

    /// POSTs the same body to `indices` concurrently.
    pub async fn post_many<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        indices: &[u16],
        path: &str,
        body: &B,
    ) -> Vec<Answer<T>> {
        join_all(indices.iter().map(|&i| async move { (i, self.post(i, path, body).await) })).await
    }

    /// POSTs a per-node body to each listed node concurrently.
    pub async fn post_each<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        bodies: &[(u16, B)],
        path: &str,
    ) -> Vec<Answer<T>> {
        join_all(bodies.iter().map(|(i, b)| async move { (*i, self.post(*i, path, b).await) })).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use alloy::primitives::Address;
    use axum::{
        Json, Router,
        body::Bytes,
        http::{HeaderMap, StatusCode, Uri},
        routing::post,
    };
    use serde_json::json;
    use zecured_node::config::NodeInfo;

    use super::*;
    use crate::error::public_error;

    /// A network config whose node `i` is served at `urls[i - 1]` and attests with
    /// `Address::repeat_byte(i)` (built from JSON so optional config fields keep their defaults).
    pub fn config(urls: &[String]) -> NetworkConfig {
        let nodes: Vec<NodeInfo> = urls
            .iter()
            .enumerate()
            .map(|(k, url)| NodeInfo {
                index: u16::try_from(k + 1).unwrap(),
                url: url.clone(),
                x25519: hex::encode([0u8; 32]),
                eth: Address::repeat_byte(u8::try_from(k + 1).unwrap()),
            })
            .collect();
        serde_json::from_value(json!({
            "lightwalletd_url": "http://127.0.0.1:1",
            "base_rpc_url": "http://127.0.0.1:1",
            "chain_id": 84_532,
            "registry": Address::repeat_byte(0x11),
            "adapter": Address::repeat_byte(0x22),
            "registry_from_block": 1,
            "confirmations": 3,
            "vault_expiry_blocks": 288,
            "denominations": [1_000_000, 5_000_000],
            "nodes": nodes,
        }))
        .unwrap()
    }

    /// Serves `app` on an ephemeral local port; returns its base URL.
    pub async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// C-3: every POST, the demo mode switch included, carries a valid D16 signature.
    #[tokio::test]
    async fn every_post_is_signed_by_the_coordinator() {
        let key = PrivateKeySigner::random();
        let coordinator = key.address();
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let log = seen.clone();
        let app = Router::new().route(
            "/v1/admin/mode",
            post(move |uri: Uri, headers: HeaderMap, body: Bytes| {
                let log = log.clone();
                async move {
                    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
                    let ok = zecured_node::auth::verify(
                        coordinator,
                        "POST",
                        uri.path(),
                        &body,
                        &get(zecured_node::auth::TS_HEADER),
                        &get(zecured_node::auth::SIG_HEADER),
                        zecured_node::now(),
                    )
                    .is_ok();
                    log.lock().unwrap().push(ok);
                    if ok {
                        (StatusCode::OK, Json(json!({ "mode": "offline" })))
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unsigned request" })))
                    }
                }
            }),
        );
        let url = serve(app).await;
        let nodes = Nodes::new(&config(&[url]), key).unwrap();
        let r: serde_json::Value =
            nodes.post(1, "/v1/admin/mode", &json!({ "mode": "offline", "token": "t" })).await.unwrap();
        assert_eq!(r["mode"], "offline");
        assert_eq!(*seen.lock().unwrap(), vec![true]);
    }

    #[tokio::test]
    async fn refusals_and_outages_are_told_apart_without_urls() {
        let app = Router::new()
            .route(
                "/refuse",
                post(|| async { (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "nonce already used" }))) }),
            )
            .route(
                "/offline",
                post(|| async { (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "node offline (demo)" }))) }),
            );
        let url = serve(app).await;
        // Node 2 points at a closed port.
        let nodes = Nodes::new(&config(&[url, "http://127.0.0.1:9".into()]), PrivateKeySigner::random()).unwrap();

        let e = nodes.post::<_, serde_json::Value>(1, "/refuse", &json!({})).await.unwrap_err();
        assert!(is_refusal(&e));
        assert_eq!(format!("{e:#}"), "node 1 refused: nonce already used");
        let e = nodes.post::<_, serde_json::Value>(1, "/offline", &json!({})).await.unwrap_err();
        assert!(!is_refusal(&e));
        let e = nodes.post::<_, serde_json::Value>(2, "/x", &json!({})).await.unwrap_err();
        assert!(!is_refusal(&e));
        assert!(!format!("{e:#}").contains("127.0.0.1"), "{e:#}");

        let answers: Vec<Answer<serde_json::Value>> = nodes.post_many(&[1, 2], "/refuse", &json!({})).await;
        let (ok, failed) = split(answers);
        assert!(ok.is_empty());
        let e = shortfall("test", 0, 3, &failed);
        assert_eq!(public_error(&e), Some(PublicError::SignersRefused));
        let e = shortfall("test", 0, 3, &failed[1..]);
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
    }
}
