//! The v4 app's routes, in a module of their own so that main.rs only merges them: P-1's demo
//! wallet (`/demo/*`, DEMO), P-2's anonymity sets (`/stats/vaults`), P-3's evidence (`/evidence`)
//! and the Zecured Swap relay (`/swap/*`). Each costly group has its own rate limit inside the
//! global one; CORS is the app's, as everywhere else.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router, middleware,
    routing::{get, post},
};
use tower_http::cors::CorsLayer;
use tracing::warn;
use zecured_coordinator::Coordinator;
use zecured_node::config::ZcashNetwork;

use crate::{
    demo::{self, DemoConfig, DemoPath},
    evidence, limits, stats, swap_relay,
};

/// These routes' settings, from the environment (the demo wallet's key and the webhook are never
/// printed).
#[derive(clap::Args, Clone, Default)]
pub struct AppArgs {
    /// DEMO (P-1, the judge deployment only): the demo wallet's Orchard-protocol spending key, 32
    /// bytes of hex (`zecured-wallet init` writes one as `spending_key`). Empty: no demo path.
    /// Ignored on Base mainnet.
    #[arg(long, env = "DEMO_WALLET_SPENDING_KEY", hide_env_values = true, default_value = "")]
    pub demo_wallet_key: String,
    /// The demo wallet's birthday height on Zcash testnet.
    #[arg(long, env = "DEMO_WALLET_BIRTHDAY", default_value_t = 0)]
    pub demo_wallet_birthday: u32,
    /// The most the demo wallet pays per UTC day, in zatoshi.
    #[arg(long, env = "DEMO_DAILY_CAP_ZAT", default_value_t = 50_000_000)]
    pub demo_daily_cap_zat: u64,
    /// Below this balance, in zatoshi, the operators are alerted.
    #[arg(long, env = "DEMO_LOW_BALANCE_ZAT", default_value_t = 20_000_000)]
    pub demo_low_balance_zat: u64,
    /// An optional URL the low-balance alarm POSTs to.
    #[arg(long, env = "DEMO_ALARM_WEBHOOK", hide_env_values = true, default_value = "")]
    pub demo_alarm_webhook: String,
    /// P-3: the evidence file (schema `zecured-evidence/1`, app/lib/evidence.ts).
    #[arg(long, env = "ZECURED_EVIDENCE")]
    pub evidence: Option<PathBuf>,
    /// Zecured Swap: the maker's quote and order service, relayed by `/swap/*` (Buy side). Empty:
    /// the routes answer `not_configured`.
    #[arg(long, env = "SWAP_MAKER_URL", default_value = "")]
    pub swap_maker_url: String,
}

/// What these routes run with.
#[derive(Clone)]
pub struct AppWiring {
    pub demo: DemoPath,
    pub evidence: evidence::EvidenceFile,
    pub stats: Option<Arc<stats::VaultStats>>,
    pub swap: swap_relay::SwapMaker,
}

impl AppWiring {
    /// Everything off: no demo wallet, no evidence, no statistics, no maker.
    #[cfg(test)]
    pub fn disabled() -> anyhow::Result<Self> {
        Ok(Self {
            demo: DemoPath::off(),
            evidence: evidence::EvidenceFile::new(None),
            stats: None,
            swap: swap_relay::SwapMaker::new(None, ZcashNetwork::Testnet.params())?,
        })
    }

    /// From the settings and the coordinator's network config; `data` is the coordinator's data
    /// directory (the demo payer's book lives there). Starts the demo wallet's balance watch.
    pub fn new(args: &AppArgs, coordinator: Arc<Coordinator>, data: &Path) -> anyhow::Result<Self> {
        let cfg = coordinator.cfg.clone();
        // The demo wallet pays TAZ: never on Base mainnet, nor with vaults on Zcash Mainnet.
        let mainnet = cfg.chain_id == demo::BASE_MAINNET || cfg.zcash() == ZcashNetwork::Mainnet;
        let demo = if args.demo_wallet_key.is_empty() {
            DemoPath::off()
        } else if mainnet {
            warn!("DEMO_WALLET_SPENDING_KEY is ignored on Base mainnet and Zcash Mainnet: no demo path");
            DemoPath::off()
        } else {
            let payer =
                demo::LwdPayer::new(cfg.lightwalletd_url.clone(), &args.demo_wallet_key, args.demo_wallet_birthday)?;
            let config = DemoConfig {
                chain_id: cfg.chain_id,
                registry: cfg.registry,
                signers: cfg.nodes.iter().map(|n| (n.index, n.eth)).collect(),
                stamp_bits: cfg.open_vault.stamp_bits,
                daily_cap_zat: args.demo_daily_cap_zat,
                low_balance_zat: args.demo_low_balance_zat,
                alarm_webhook: Some(args.demo_alarm_webhook.clone()).filter(|u| !u.is_empty()),
                book: data.join("demo-payer.json"),
            };
            let path = DemoPath::new(config, coordinator, Arc::new(payer))?;
            warn!("DEMO: the demo path is on: the demo wallet pays opened vaults in TAZ, once each");
            path.watch();
            path
        };
        // The deployment's Zcash network (the network config's, or the one its Base chain goes with).
        let network = cfg.zcash().params();
        Ok(Self {
            demo,
            evidence: evidence::EvidenceFile::new(args.evidence.clone()),
            stats: Some(Arc::new(stats::VaultStats::new(
                cfg.base_rpc_url.clone(),
                cfg.registry,
                cfg.registry_from_block,
            )?)),
            swap: swap_relay::SwapMaker::new(Some(args.swap_maker_url.clone()), network)?,
        })
    }
}

fn bucket(burst: u32, secs: u64) -> Arc<limits::TokenBucket> {
    Arc::new(limits::TokenBucket::new(limits::Rate::new(burst, Duration::from_secs(secs))))
}

/// The routes, under the global limit `global` (the backend's `Limits::global`) and `cors`.
pub fn router(w: AppWiring, global: &Arc<limits::TokenBucket>, cors: CorsLayer) -> Router {
    let limited = |b: Arc<limits::TokenBucket>| middleware::from_fn_with_state(b, limits::limit);
    Router::new()
        .route("/demo/status", get(demo::status))
        .route("/evidence", get(evidence::handler))
        .route("/stats/vaults", get(stats::handler))
        // A demo payment builds and proves a Zcash transaction.
        .merge(Router::new().route("/demo/vaults/{id}/pay", post(demo::pay)).route_layer(limited(bucket(6, 600))))
        .merge(
            Router::new()
                .route("/swap/quote", get(swap_relay::quote))
                .route("/swap/orders/{id}/destination", post(swap_relay::destination))
                .route("/swap/orders/{id}", get(swap_relay::order))
                .route_layer(limited(bucket(20, 60))),
        )
        .layer(axum::Extension(w.demo))
        .layer(axum::Extension(w.evidence))
        .layer(axum::Extension(w.stats))
        .layer(axum::Extension(w.swap))
        .layer(middleware::from_fn_with_state(global.clone(), limits::limit))
        .layer(cors)
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Address, B256, keccak256};
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use zecured_node::config::NetworkConfig;

    use super::*;

    fn cors() -> CorsLayer {
        CorsLayer::new().allow_origin(axum::http::HeaderValue::from_static("http://localhost:3000"))
    }

    async fn call(app: &Router, method: Method, uri: &str, body: &str) -> (StatusCode, Value, bool) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("origin", "http://localhost:3000")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let r = app.clone().oneshot(req).await.unwrap();
        let status = r.status();
        let cors = r.headers().contains_key("access-control-allow-origin");
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default(), cors)
    }

    /// Off by default, each route a fixed answer; merged beside the main router's routes.
    #[tokio::test]
    async fn the_routes_answer_fixed_codes_when_off() {
        let global = bucket(400, 8);
        let other = Router::new().route("/health", get(|| async { "ok" }));
        let app = other.merge(router(AppWiring::disabled().unwrap(), &global, cors()));
        let (status, body, allowed) = call(&app, Method::GET, "/demo/status", "").await;
        assert_eq!((status, body, allowed), (StatusCode::OK, json!({ "enabled": false }), true));
        let pay = format!("/demo/vaults/{}/pay", B256::repeat_byte(1));
        let (status, body, _) = call(&app, Method::POST, &pay, "{}").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("demo_off")));
        for uri in ["/evidence", "/stats/vaults"] {
            let (status, body, _) = call(&app, Method::GET, uri, "").await;
            assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_configured")), "{uri}");
        }
        let (status, body, _) = call(&app, Method::GET, "/swap/quote?side=buy&usdc=1000000", "").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("not_configured")));
        let (status, body, _) = call(&app, Method::GET, "/swap/quote?side=sell&usdc=1000000", "").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        let (status, body, _) = call(&app, Method::POST, "/swap/orders/0x12/destination", "{}").await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
        let (status, _, _) = call(&app, Method::GET, "/health", "").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn demo_payments_have_their_own_limit_inside_the_global_one() {
        let global = bucket(400, 8);
        let app = router(AppWiring::disabled().unwrap(), &global, cors());
        let pay = format!("/demo/vaults/{}/pay", B256::repeat_byte(1));
        for _ in 0..6 {
            let (status, _, _) = call(&app, Method::POST, &pay, "{}").await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        let (status, body, allowed) = call(&app, Method::POST, &pay, "{}").await;
        assert_eq!(
            (status, body["code"].as_str(), allowed),
            (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"), true)
        );
        let (status, _, _) = call(&app, Method::GET, "/demo/status", "").await;
        assert_eq!(status, StatusCode::OK, "the status has no limit of its own");
        // The global bucket is the backend's: once it is empty, these routes answer 429 too.
        let tight = bucket(1, 3_600);
        let app = router(AppWiring::disabled().unwrap(), &tight, cors());
        assert_eq!(call(&app, Method::GET, "/demo/status", "").await.0, StatusCode::OK);
        assert_eq!(call(&app, Method::GET, "/demo/status", "").await.0, StatusCode::TOO_MANY_REQUESTS);
    }

    /// The demo path follows the settings and the chain: on with a key on a testnet, never on Base
    /// mainnet; the statistics read the network config's Registry.
    #[tokio::test]
    async fn the_wiring_follows_the_settings_and_the_chain() {
        let wired = |chain_id: u64, key: &str| {
            let dir = std::env::temp_dir().join(format!(
                "zecured-app-routes-test-{}-{chain_id}-{}",
                hex::encode(&keccak256(key.as_bytes())[..4]),
                std::process::id()
            ));
            let nodes: Vec<Value> = (1..=5u8)
                .map(|i| json!({ "index": i, "url": "http://127.0.0.1:9", "x25519": "00", "eth": Address::repeat_byte(i) }))
                .collect();
            let cfg: NetworkConfig = serde_json::from_value(json!({
                "lightwalletd_url": "http://127.0.0.1:9", "base_rpc_url": "http://127.0.0.1:9", "chain_id": chain_id,
                "registry": Address::repeat_byte(0x11), "adapter": Address::repeat_byte(0x22), "registry_from_block": 1,
                "confirmations": 3, "vault_expiry_blocks": 288, "denominations": [1_000_000], "nodes": nodes,
            }))
            .unwrap();
            let gas = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
            let coordinator = Arc::new(Coordinator::new(cfg, &dir, &gas).unwrap());
            let args = AppArgs {
                demo_wallet_key: key.to_owned(),
                demo_wallet_birthday: 4_400_000,
                ..AppArgs::default()
            };
            let w = AppWiring::new(&args, coordinator, &dir);
            let _ = std::fs::remove_dir_all(&dir);
            w
        };
        let valid = (0u8..=255)
            .map(|i| [i; 32])
            .find(|b| bool::from(orchard::keys::SpendingKey::from_bytes(*b).is_some()))
            .unwrap();
        let key = hex::encode(valid);
        assert!(!wired(84_532, "").unwrap().demo.enabled());
        let on = wired(84_532, &key).unwrap();
        assert!(on.demo.enabled() && on.stats.is_some());
        assert!(!wired(demo::BASE_MAINNET, &key).unwrap().demo.enabled(), "never on Base mainnet");
        assert!(wired(84_532, "not-a-key").is_err());
    }
}
