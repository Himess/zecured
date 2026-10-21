//! The verifier versions (design notes T-07a) for `GET /status`: which RISC Zero verifiers
//! (router selectors) and guest image ids the Intake takes deposit proofs from and the slasher
//! violation proofs from, read from `ZecuredVersions` and cached for [`TTL`], so that anyone can
//! check the running system against the submitted guests (review G5) after a version change too
//! (the deployment file only names the initial ones). Public state only.

use std::time::Duration;

use alloy::{
    primitives::{Address, B256},
    sol,
};
use anyhow::Result;
use serde_json::{Value, json};

use crate::onchain::{Cached, view};

sol! {
    /// IZecuredVersions `Version`; its `VersionKind` is ABI-encoded as uint8.
    struct Version {
        uint8 kind;
        bytes4 selector;
        bytes32 imageId;
        uint32[4] branches;
        uint64 activeAt;
        bool removed;
    }

    function versionIds(uint8 kind) external view returns (bytes32[] memory);
    function version(bytes32 versionId) external view returns (Version memory);
    function isActive(bytes32 versionId) external view returns (bool);
}

/// `VersionKind` (IZecuredVersions), by value, with the name `/status` gives each.
const KINDS: [(u8, &str); 2] = [(0, "deposit"), (1, "violation")];
/// Versions change rarely, and only after a public delay.
pub const TTL: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Most versions of one kind listed (the newest): the list only ever grows.
const MAX_LISTED: usize = 16;

pub struct VerifierVersions {
    rpc_url: String,
    registry: Address,
    http: reqwest::Client,
    cache: Cached,
}

impl VerifierVersions {
    pub fn new(rpc_url: String, registry: Address) -> Result<Self> {
        Ok(Self {
            rpc_url,
            registry,
            http: reqwest::Client::builder().timeout(CALL_TIMEOUT).build()?,
            cache: Cached::new(TTL),
        })
    }

    /// `{ deposit: [...], violation: [...] }`, each version `{ versionId, selector, imageId,
    /// branches, activeAt, active, removed }` in the order it was scheduled (the newest
    /// [`MAX_LISTED`]); `null` while the chain cannot be read.
    pub async fn json(&self) -> Value {
        self.cache.get(|| self.read()).await
    }

    async fn read(&self) -> Option<Value> {
        let (http, url) = (&self.http, self.rpc_url.as_str());
        let mut out = serde_json::Map::new();
        for (kind, name) in KINDS {
            let ids: Vec<B256> = view(http, url, self.registry, versionIdsCall { kind }).await?;
            let mut listed = vec![];
            for id in ids.iter().skip(ids.len().saturating_sub(MAX_LISTED)) {
                let (v, active) = tokio::join!(
                    view(http, url, self.registry, versionCall { versionId: *id }),
                    view(http, url, self.registry, isActiveCall { versionId: *id }),
                );
                let v = v?;
                listed.push(json!({
                    "versionId": id,
                    "selector": v.selector,
                    "imageId": v.imageId,
                    "branches": v.branches,
                    "activeAt": v.activeAt,
                    "active": active?,
                    "removed": v.removed,
                }));
            }
            out.insert(name.to_owned(), listed.into());
        }
        Some(out.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use alloy::{primitives::Bytes, sol_types::SolCall};
    use axum::{Json, Router, extract::State, routing::post};

    use super::*;

    const REGISTRY: Address = Address::repeat_byte(0x9b);

    /// ZecuredVersions as an RPC stub: one deposit version (active) and two violation versions
    /// (the first removed).
    async fn rpc(State(calls): State<Arc<AtomicUsize>>, Json(req): Json<Value>) -> Json<Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        let data: Bytes = serde_json::from_value(req["params"][0]["data"].clone()).unwrap();
        let id = |b: u8| B256::repeat_byte(b);
        let out = if let Ok(c) = versionIdsCall::abi_decode(&data) {
            versionIdsCall::abi_encode_returns(&if c.kind == 0 { vec![id(1)] } else { vec![id(2), id(3)] })
        } else if let Ok(c) = versionCall::abi_decode(&data) {
            let n = c.versionId[0];
            let v = Version {
                kind: u8::from(n != 1),
                selector: [0x73, 0xc4, 0x57, 0xba].into(),
                imageId: B256::repeat_byte(n + 0x10),
                branches: [0x77190ad9, 0, 0, 0],
                activeAt: 1_792_000_000 + u64::from(n),
                removed: n == 2,
            };
            versionCall::abi_encode_returns(&v)
        } else {
            let c = isActiveCall::abi_decode(&data).unwrap();
            isActiveCall::abi_encode_returns(&(c.versionId[0] != 2))
        };
        Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": Bytes::from(out) }))
    }

    /// T-07a: every version of each kind, in order, with whether it verifies now; cached.
    #[tokio::test]
    async fn the_versions_are_listed_per_kind_and_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/", post(rpc)).with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let versions = VerifierVersions::new(format!("http://{addr}"), REGISTRY).unwrap();
        let v = versions.json().await;
        let version = |n: u8, active: bool, removed: bool| {
            json!({
                "versionId": B256::repeat_byte(n),
                "selector": "0x73c457ba",
                "imageId": B256::repeat_byte(n + 0x10),
                "branches": [0x77190ad9u32, 0, 0, 0],
                "activeAt": 1_792_000_000 + u64::from(n),
                "active": active,
                "removed": removed,
            })
        };
        assert_eq!(
            v,
            json!({
                "deposit": [version(1, true, false)],
                "violation": [version(2, false, true), version(3, true, false)],
            })
        );
        let read = calls.load(Ordering::SeqCst);
        assert_eq!(read, 2 + 3 * 2);
        versions.json().await;
        assert_eq!(calls.load(Ordering::SeqCst), read, "served from the cache");
        let offline = VerifierVersions::new("http://127.0.0.1:9".into(), REGISTRY).unwrap();
        assert_eq!(offline.json().await, Value::Null);
    }
}
