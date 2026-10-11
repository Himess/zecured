//! Privacy proxy for the browser: Base RPC, ERC-4337 bundler and paymaster.
//!
//! The browser never talks to infrastructure providers directly, so they never learn the
//! user's IP. Requests are forwarded without client headers (no X-Forwarded-For), bodies are
//! never logged, and only the JSON-RPC methods the app needs are allowed, with bounded batches
//! and log ranges (L-4). Every request that carries a UserOp must pass the paymaster
//! allowlist, name EntryPoint v0.7, come from a Zecured account (`Registry.vaultOf(sender)
//! != 0`; once deployed, still the canonical SafeProxy) and stay within a per-sender rate
//! (M-3). Refusals are fixed messages with a short code (L-2).

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use alloy::{
    primitives::{Address, B256},
    sol,
    sol_types::SolCall,
};
use anyhow::Result;
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::{
    allowlist::{
        Allowlist, ENTRY_POINT_V07, FALLBACK_HANDLER_SLOT, SAFE_4337_MODULE, SAFE_PROXY_RUNTIME_CODE, SAFE_SINGLETON,
        UserOpView,
    },
    limits::{KeyedBuckets, Rate},
};

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
const PAYMASTER_METHODS: &[&str] = &[
    "pm_getPaymasterStubData",
    "pm_getPaymasterData",
    "pm_sponsorUserOperation",
];

/// Methods whose params are `[userOp, entryPoint, ...]`: the UserOp must pass the allowlist.
const USEROP_METHODS: &[&str] = &[
    "eth_sendUserOperation",
    "eth_estimateUserOperationGas",
    "pm_getPaymasterStubData",
    "pm_getPaymasterData",
    "pm_sponsorUserOperation",
];

/// Most requests in one JSON-RPC batch.
pub const MAX_BATCH: usize = 10;
/// Most blocks one `eth_getLogs` may span (the app reads 2,000-block chunks).
pub const MAX_LOG_BLOCKS: u64 = 2_000;
/// Most addresses one `eth_getLogs` may filter on.
const MAX_LOG_ADDRESSES: usize = 4;
/// UserOp-carrying requests per sender: a sponsored UserOp takes about four.
const SENDER_RATE: Rate = Rate::new(20, Duration::from_secs(60));
const MAX_SENDERS: usize = 10_000;
/// A sender found to be a Zecured Safe is not checked again for this long.
const VERIFIED_TTL: Duration = Duration::from_secs(60);
/// Timeout of the proxy's own chain reads (sponsorship checks).
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

sol! {
    function vaultOf(address account) external view returns (bytes32 vaultId);
}

/// A refused JSON-RPC request: a fixed public message and a short code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: &'static str,
}

impl Refusal {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    fn rpc_code(self) -> i64 {
        match self.code {
            "parse_error" => -32700,
            "invalid_request" | "batch_too_large" => -32600,
            "method_not_allowed" => -32601,
            "rate_limited" => -32005,
            _ => -32602,
        }
    }
}

const PARSE_ERROR: Refusal = Refusal::new("parse_error", "Invalid JSON.");
const INVALID_REQUEST: Refusal = Refusal::new("invalid_request", "Invalid JSON-RPC request.");
const BATCH_TOO_LARGE: Refusal = Refusal::new("batch_too_large", "Empty or too large JSON-RPC batch.");
const METHOD_NOT_ALLOWED: Refusal = Refusal::new("method_not_allowed", "Method not allowed.");
const LOG_RANGE: Refusal = Refusal::new(
    "log_range",
    "eth_getLogs needs a contract address and numeric fromBlock/toBlock at most 2,000 blocks apart, or a blockHash.",
);
const MALFORMED_USEROP: Refusal = Refusal::new("invalid_params", "Malformed UserOperation.");
const ENTRY_POINT: Refusal = Refusal::new("entry_point", "Only EntryPoint v0.7 is supported.");
const NOT_CONFIGURED: Refusal = Refusal::new("not_configured", "Sponsorship is not configured.");
const RATE_LIMITED: Refusal =
    Refusal::new("rate_limited", "Too many UserOperations from this account. Try again later.");
const UNKNOWN_SENDER: Refusal =
    Refusal::new("unknown_sender", "Not sponsored: the sender is not a Zecured loan account.");
const NOT_A_SAFE: Refusal =
    Refusal::new("not_a_safe", "Not sponsored: the sender is not a standard Zecured Safe account.");
const CHECK_UNAVAILABLE: Refusal =
    Refusal::new("check_unavailable", "Sponsorship checks are unavailable. Try again later.");
const UPSTREAM: Refusal = Refusal::new("upstream_unavailable", "The upstream service is unavailable. Try again later.");

fn rpc_error(id: Option<&Value>, r: Refusal) -> Response {
    let id = id.cloned().unwrap_or(Value::Null);
    let body =
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": r.rpc_code(), "message": r.message, "data": r.code } });
    (StatusCode::OK, Json(body)).into_response()
}

pub struct Proxy {
    http: reqwest::Client,
    /// Upstream of the public `/proxy/rpc` (its own key and quota if `PROXY_RPC_URL` is set).
    pub rpc_url: String,
    /// The backend's own RPC, for sponsorship checks.
    check_rpc_url: String,
    pub bundler_url: Option<String>,
    pub paymaster_url: Option<String>,
    pub allowlist: Option<Allowlist>,
    senders: KeyedBuckets<Address>,
    /// Senders recently found to be Zecured Safes.
    verified: Mutex<HashMap<Address, Instant>>,
}

pub type ProxyState = State<Arc<Proxy>>;

/// A `0x` hex quantity.
fn quantity(v: Option<&Value>) -> Option<u64> {
    u64::from_str_radix(v?.as_str()?.strip_prefix("0x")?, 16).ok()
}

/// L-4: an `eth_getLogs` filter must name its contract(s) and either one block hash or an
/// explicit numeric range of at most `MAX_LOG_BLOCKS` blocks (tags like "latest" are refused).
fn check_log_filter(params: Option<&Value>) -> Result<(), Refusal> {
    let filter = params.and_then(|p| p.get(0)).and_then(Value::as_object).ok_or(LOG_RANGE)?;
    let addresses = match filter.get("address") {
        Some(Value::String(_)) => 1,
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a.len(),
        _ => 0,
    };
    if addresses == 0 || addresses > MAX_LOG_ADDRESSES {
        return Err(LOG_RANGE);
    }
    if filter.get("blockHash").is_some_and(|h| !h.is_null()) {
        return if filter.contains_key("fromBlock") || filter.contains_key("toBlock") {
            Err(LOG_RANGE)
        } else {
            Ok(())
        };
    }
    let (Some(from), Some(to)) = (quantity(filter.get("fromBlock")), quantity(filter.get("toBlock"))) else {
        return Err(LOG_RANGE);
    };
    if to < from || to - from >= MAX_LOG_BLOCKS {
        return Err(LOG_RANGE);
    }
    Ok(())
}

/// M-3: the EntryPoint parameter of every UserOp method must be v0.7.
fn check_entry_point(params: Option<&Value>) -> Result<(), Refusal> {
    let entry_point = params.and_then(|p| p.get(1)).and_then(Value::as_str).and_then(|s| s.parse::<Address>().ok());
    if entry_point == Some(ENTRY_POINT_V07) {
        Ok(())
    } else {
        Err(ENTRY_POINT)
    }
}

fn hex_bytes(v: &Value) -> Option<Vec<u8>> {
    hex::decode(v.as_str()?.strip_prefix("0x")?).ok()
}

/// The address in a 32-byte storage word, if it holds exactly an address.
fn word_address(word: &[u8]) -> Option<Address> {
    (word.len() == 32 && word[..12].iter().all(|b| *b == 0)).then(|| Address::from_slice(&word[12..]))
}

impl Proxy {
    pub fn new(
        rpc_url: String,
        check_rpc_url: String,
        bundler_url: Option<String>,
        paymaster_url: Option<String>,
        allowlist: Option<Allowlist>,
    ) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?,
            rpc_url,
            check_rpc_url,
            bundler_url: bundler_url.filter(|u| !u.is_empty()),
            paymaster_url: paymaster_url.filter(|u| !u.is_empty()),
            allowlist,
            senders: KeyedBuckets::new(SENDER_RATE, MAX_SENDERS),
            verified: Mutex::default(),
        })
    }

    async fn check(&self, req: &Value, methods: &[&str]) -> Result<(), Refusal> {
        let method = req.get("method").and_then(Value::as_str).ok_or(INVALID_REQUEST)?;
        if !methods.contains(&method) {
            return Err(METHOD_NOT_ALLOWED);
        }
        let params = req.get("params");
        if method == "eth_getLogs" {
            check_log_filter(params)?;
        }
        if USEROP_METHODS.contains(&method) {
            let allowlist = self.allowlist.as_ref().ok_or(NOT_CONFIGURED)?;
            let op = params.and_then(|p| p.get(0)).ok_or(MALFORMED_USEROP)?;
            let op: UserOpView = serde_json::from_value(op.clone()).map_err(|_| MALFORMED_USEROP)?;
            check_entry_point(params)?;
            allowlist.check(&op).map_err(|reason| Refusal::new("not_sponsorable", reason))?;
            if !self.senders.try_take(op.sender) {
                return Err(RATE_LIMITED);
            }
            self.check_sender(allowlist, &op).await?;
        }
        Ok(())
    }

    fn recently_verified(&self, sender: Address) -> bool {
        let verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        verified.get(&sender).is_some_and(|t| t.elapsed() < VERIFIED_TTL)
    }

    fn mark_verified(&self, sender: Address) {
        let mut verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        if verified.len() >= MAX_SENDERS {
            verified.retain(|_, t| t.elapsed() < VERIFIED_TTL);
            if verified.len() >= MAX_SENDERS {
                verified.clear();
            }
        }
        verified.insert(sender, Instant::now());
    }

    /// M-3: the sender is a Zecured account (bound to a vault by `attestDeposit`, which comes
    /// before its first UserOp) and, once deployed, still the canonical SafeProxy with the
    /// Safe 1.4.1 singleton and the Safe 4337 module as fallback handler, so its `callData`
    /// means what the allowlist decoded. Fails closed if the chain cannot be read.
    async fn check_sender(&self, allowlist: &Allowlist, op: &UserOpView) -> Result<(), Refusal> {
        if self.recently_verified(op.sender) {
            return Ok(());
        }
        let call = vaultOfCall { account: op.sender }.abi_encode();
        let call = json!([{ "to": allowlist.registry, "data": format!("0x{}", hex::encode(call)) }, "latest"]);
        let vault = self.chain_read("eth_call", call).await?;
        let vault = hex_bytes(&vault).filter(|b| b.len() == 32).ok_or(CHECK_UNAVAILABLE)?;
        if B256::from_slice(&vault).is_zero() {
            return Err(UNKNOWN_SENDER);
        }
        // An undeployed sender is checked by the allowlist: its factory data CREATE2-derives it.
        if op.factory.is_none() {
            let code = self.chain_read("eth_getCode", json!([op.sender, "latest"])).await?;
            if hex_bytes(&code).as_deref() != Some(SAFE_PROXY_RUNTIME_CODE) {
                return Err(NOT_A_SAFE);
            }
            for (slot, expected) in [(B256::ZERO, SAFE_SINGLETON), (FALLBACK_HANDLER_SLOT, SAFE_4337_MODULE)] {
                let word = self.chain_read("eth_getStorageAt", json!([op.sender, slot, "latest"])).await?;
                if hex_bytes(&word).as_deref().and_then(word_address) != Some(expected) {
                    return Err(NOT_A_SAFE);
                }
            }
        }
        self.mark_verified(op.sender);
        Ok(())
    }

    /// One JSON-RPC read on the backend's own RPC.
    async fn chain_read(&self, method: &str, params: Value) -> Result<Value, Refusal> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self.http.post(&self.check_rpc_url).timeout(CHECK_TIMEOUT).json(&body).send().await;
        let v: Value = resp.map_err(|_| CHECK_UNAVAILABLE)?.json().await.map_err(|_| CHECK_UNAVAILABLE)?;
        v.get("result").cloned().ok_or(CHECK_UNAVAILABLE)
    }

    async fn check_all(&self, body: &Value, methods: &[&str]) -> Result<(), Refusal> {
        match body {
            Value::Array(items) if items.is_empty() || items.len() > MAX_BATCH => Err(BATCH_TOO_LARGE),
            Value::Array(items) => {
                for item in items {
                    self.check(item, methods).await?;
                }
                Ok(())
            }
            Value::Object(_) => self.check(body, methods).await,
            _ => Err(INVALID_REQUEST),
        }
    }

    async fn forward(&self, url: &str, body: Result<Json<Value>, JsonRejection>, methods: &[&str]) -> Response {
        let Ok(Json(body)) = body else {
            return rpc_error(None, PARSE_ERROR);
        };
        if let Err(refusal) = self.check_all(&body, methods).await {
            return rpc_error(body.get("id"), refusal);
        }
        match self.post(url, &body).await {
            Some(v) => (StatusCode::OK, Json(v)).into_response(),
            None => {
                let body = json!({ "error": UPSTREAM.message, "code": UPSTREAM.code });
                (StatusCode::BAD_GATEWAY, Json(body)).into_response()
            }
        }
    }

    async fn post(&self, url: &str, body: &Value) -> Option<Value> {
        self.http.post(url).json(body).send().await.ok()?.json().await.ok()
    }
}

pub async fn rpc(State(p): ProxyState, body: Result<Json<Value>, JsonRejection>) -> Response {
    p.forward(&p.rpc_url, body, RPC_METHODS).await
}

pub async fn bundler(State(p): ProxyState, body: Result<Json<Value>, JsonRejection>) -> Response {
    match p.bundler_url.as_deref() {
        Some(url) => p.forward(url, body, BUNDLER_METHODS).await,
        None => rpc_error(None, NOT_CONFIGURED),
    }
}

pub async fn paymaster(State(p): ProxyState, body: Result<Json<Value>, JsonRejection>) -> Response {
    match p.paymaster_url.as_deref() {
        Some(url) => p.forward(url, body, PAYMASTER_METHODS).await,
        None => rpc_error(None, NOT_CONFIGURED),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy::primitives::{Bytes, U256};
    use axum::{Router, routing::post};

    use super::*;
    use crate::allowlist::tests::{list, op, single};

    /// A Base RPC stub: `bound` senders have a vault; `deployed` senders have the given code
    /// and storage (singleton slot, fallback handler slot).
    #[derive(Default)]
    struct Chain {
        bound: Vec<Address>,
        deployed: HashMap<Address, (Vec<u8>, Address, Address)>,
        calls: AtomicUsize,
    }

    async fn chain_rpc(State(c): State<Arc<Chain>>, Json(req): Json<Value>) -> Json<Value> {
        c.calls.fetch_add(1, Ordering::SeqCst);
        let params = &req["params"];
        let word = |a: Address| format!("0x{}{}", "00".repeat(12), hex::encode(a));
        let result = match req["method"].as_str().unwrap() {
            "eth_call" => {
                let data = hex_bytes(&params[0]["data"]).unwrap();
                let account = vaultOfCall::abi_decode(&data).unwrap().account;
                let id = if c.bound.contains(&account) {
                    B256::repeat_byte(7)
                } else {
                    B256::ZERO
                };
                json!(id)
            }
            "eth_getCode" => {
                let a: Address = params[0].as_str().unwrap().parse().unwrap();
                json!(format!("0x{}", c.deployed.get(&a).map(|d| hex::encode(&d.0)).unwrap_or_default()))
            }
            "eth_getStorageAt" => {
                let a: Address = params[0].as_str().unwrap().parse().unwrap();
                let slot: B256 = params[1].as_str().unwrap().parse().unwrap();
                let d = c.deployed.get(&a);
                let v = match d {
                    Some(d) if slot == B256::ZERO => word(d.1),
                    Some(d) if slot == FALLBACK_HANDLER_SLOT => word(d.2),
                    _ => word(Address::ZERO),
                };
                json!(v)
            }
            m => panic!("unexpected {m}"),
        };
        Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn proxy_with(chain: Chain) -> (Proxy, Arc<Chain>) {
        let chain = Arc::new(chain);
        let url = serve(Router::new().route("/", post(chain_rpc)).with_state(chain.clone())).await;
        let p = Proxy::new(url.clone(), url, Some("http://127.0.0.1:9".into()), None, Some(list())).unwrap();
        (p, chain)
    }

    fn safe(singleton: Address, handler: Address) -> (Vec<u8>, Address, Address) {
        (SAFE_PROXY_RUNTIME_CODE.to_vec(), singleton, handler)
    }

    fn release_op(sender: Address) -> Value {
        let l = list();
        let data = crate::allowlist::requestReleaseCall {
            vaultId: B256::repeat_byte(1),
            destCommitment: B256::repeat_byte(2),
        }
        .abi_encode();
        serde_json::to_value(UserOpJson::from(&op(sender, single(l.registry, data)))).unwrap()
    }

    /// The JSON the app sends for a UserOp (the fields the allowlist reads).
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct UserOpJson {
        sender: Address,
        call_data: Bytes,
        call_gas_limit: U256,
    }

    impl From<&UserOpView> for UserOpJson {
        fn from(o: &UserOpView) -> Self {
            Self {
                sender: o.sender,
                call_data: o.call_data.clone(),
                call_gas_limit: U256::from(200_000u64),
            }
        }
    }

    fn request(method: &str, user_op: Value, entry_point: Address) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": [user_op, entry_point, "0x14a34", {}] })
    }

    #[test]
    fn write_methods_are_refused_on_the_rpc_proxy() {
        let p = Proxy::new("http://127.0.0.1:1".into(), "http://127.0.0.1:1".into(), None, None, None).unwrap();
        let run = |r: Value| futures_lite(p.check(&r, RPC_METHODS));
        assert!(run(json!({"method": "eth_call", "params": []})).is_ok());
        assert_eq!(run(json!({"method": "eth_sendRawTransaction", "params": []})), Err(METHOD_NOT_ALLOWED));
        assert_eq!(run(json!({"method": "eth_accounts"})), Err(METHOD_NOT_ALLOWED));
    }

    /// Runs a future that never awaits I/O (checks that fail before any network access).
    fn futures_lite<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn userops_need_a_configured_allowlist() {
        let p = Proxy::new("http://127.0.0.1:1".into(), "http://127.0.0.1:1".into(), None, None, None).unwrap();
        let req = request("pm_getPaymasterData", release_op(Address::repeat_byte(1)), ENTRY_POINT_V07);
        assert_eq!(futures_lite(p.check(&req, PAYMASTER_METHODS)), Err(NOT_CONFIGURED));
    }

    /// M-3: only Zecured accounts are sponsored; a deployed one must still be our Safe.
    #[tokio::test]
    async fn only_zecured_safes_are_sponsored() {
        let ours = Address::repeat_byte(0x91);
        let stranger = Address::repeat_byte(0x92);
        let custom = Address::repeat_byte(0x93);
        let swapped = Address::repeat_byte(0x94);
        let rehandled = Address::repeat_byte(0x95);
        let mut chain = Chain {
            bound: vec![ours, custom, swapped, rehandled],
            ..Default::default()
        };
        chain.deployed.insert(ours, safe(SAFE_SINGLETON, SAFE_4337_MODULE));
        chain.deployed.insert(custom, (vec![0x60, 0x00], SAFE_SINGLETON, SAFE_4337_MODULE));
        chain.deployed.insert(swapped, safe(Address::repeat_byte(0xee), SAFE_4337_MODULE));
        chain.deployed.insert(rehandled, safe(SAFE_SINGLETON, Address::repeat_byte(0xee)));
        let (p, _) = proxy_with(chain).await;
        assert_eq!(stub_data(&p, ours).await, Ok(()));
        assert_eq!(stub_data(&p, stranger).await, Err(UNKNOWN_SENDER));
        assert_eq!(stub_data(&p, custom).await, Err(NOT_A_SAFE), "a custom 4337 account bound to a vault");
        assert_eq!(stub_data(&p, swapped).await, Err(NOT_A_SAFE), "another singleton");
        assert_eq!(stub_data(&p, rehandled).await, Err(NOT_A_SAFE), "another fallback handler");
    }

    async fn stub_data(p: &Proxy, sender: Address) -> Result<(), Refusal> {
        let req = request("pm_getPaymasterStubData", release_op(sender), ENTRY_POINT_V07);
        p.check(&req, PAYMASTER_METHODS).await
    }

    /// M-3: the EntryPoint parameter is pinned to v0.7 for every UserOp method.
    #[tokio::test]
    async fn the_entry_point_is_pinned_to_v07() {
        let ours = Address::repeat_byte(0x91);
        let mut chain = Chain {
            bound: vec![ours],
            ..Default::default()
        };
        chain.deployed.insert(ours, safe(SAFE_SINGLETON, SAFE_4337_MODULE));
        let (p, chain) = proxy_with(chain).await;
        let v06: Address = "0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789".parse().unwrap();
        for method in USEROP_METHODS {
            let methods = if method.starts_with("pm_") {
                PAYMASTER_METHODS
            } else {
                BUNDLER_METHODS
            };
            let req = request(method, release_op(ours), v06);
            assert_eq!(p.check(&req, methods).await, Err(ENTRY_POINT), "{method}");
            let mut missing = request(method, release_op(ours), ENTRY_POINT_V07);
            missing["params"].as_array_mut().unwrap().truncate(1);
            assert_eq!(p.check(&missing, methods).await, Err(ENTRY_POINT), "{method}");
            assert_eq!(p.check(&request(method, release_op(ours), ENTRY_POINT_V07), methods).await, Ok(()));
        }
        assert_eq!(chain.calls.load(Ordering::SeqCst), 4, "one verification, then cached");
    }

    /// M-3: a sender cannot flood the paymaster.
    #[tokio::test]
    async fn userops_are_rate_limited_per_sender() {
        let ours = Address::repeat_byte(0x91);
        let mut chain = Chain {
            bound: vec![ours],
            ..Default::default()
        };
        chain.deployed.insert(ours, safe(SAFE_SINGLETON, SAFE_4337_MODULE));
        let (p, _) = proxy_with(chain).await;
        let req = request("pm_getPaymasterData", release_op(ours), ENTRY_POINT_V07);
        for _ in 0..SENDER_RATE.burst {
            assert_eq!(p.check(&req, PAYMASTER_METHODS).await, Ok(()));
        }
        assert_eq!(p.check(&req, PAYMASTER_METHODS).await, Err(RATE_LIMITED));
    }

    /// Sponsorship fails closed when the chain cannot be read.
    #[tokio::test]
    async fn sponsorship_checks_fail_closed() {
        let p = Proxy::new("http://127.0.0.1:9".into(), "http://127.0.0.1:9".into(), None, None, Some(list())).unwrap();
        let req = request("pm_getPaymasterData", release_op(Address::repeat_byte(1)), ENTRY_POINT_V07);
        assert_eq!(p.check(&req, PAYMASTER_METHODS).await, Err(CHECK_UNAVAILABLE));
    }

    /// L-4: getLogs ranges and batch sizes are bounded.
    #[test]
    fn log_ranges_and_batches_are_bounded() {
        let logs = |filter: Value| check_log_filter(Some(&json!([filter])));
        let registry = Address::repeat_byte(0x22);
        assert_eq!(logs(json!({ "address": registry, "fromBlock": "0x1", "toBlock": "0x7d0" })), Ok(()));
        assert_eq!(logs(json!({ "address": registry, "fromBlock": "0x1", "toBlock": "0x7d1" })), Err(LOG_RANGE));
        assert_eq!(logs(json!({ "address": registry, "fromBlock": "0x10", "toBlock": "0x1" })), Err(LOG_RANGE));
        assert_eq!(logs(json!({ "address": registry, "fromBlock": "earliest", "toBlock": "latest" })), Err(LOG_RANGE));
        assert_eq!(logs(json!({ "address": registry, "fromBlock": "0x1" })), Err(LOG_RANGE));
        assert_eq!(logs(json!({ "fromBlock": "0x1", "toBlock": "0x2" })), Err(LOG_RANGE), "no address");
        assert_eq!(logs(json!({ "address": registry, "blockHash": B256::repeat_byte(1) })), Ok(()));
        let addresses = vec![registry; MAX_LOG_ADDRESSES + 1];
        assert_eq!(logs(json!({ "address": addresses, "fromBlock": "0x1", "toBlock": "0x2" })), Err(LOG_RANGE));

        let p = Proxy::new("http://127.0.0.1:1".into(), "http://127.0.0.1:1".into(), None, None, None).unwrap();
        let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_chainId" });
        assert_eq!(futures_lite(p.check_all(&json!(vec![call.clone(); MAX_BATCH]), RPC_METHODS)), Ok(()));
        let too_many = json!(vec![call; MAX_BATCH + 1]);
        assert_eq!(futures_lite(p.check_all(&too_many, RPC_METHODS)), Err(BATCH_TOO_LARGE));
        assert_eq!(futures_lite(p.check_all(&json!([]), RPC_METHODS)), Err(BATCH_TOO_LARGE));
    }

    /// L-2: refusals are fixed JSON-RPC errors and never reach the upstream.
    #[tokio::test]
    async fn refusals_are_fixed_errors_and_not_forwarded() {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let upstream = serve(Router::new().route(
            "/",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({ "jsonrpc": "2.0", "id": 1, "result": "0x1" })) }
            }),
        ))
        .await;
        let p = Proxy::new(upstream.clone(), upstream.clone(), None, None, None).unwrap();
        let refused = p
            .forward(&upstream, Ok(Json(json!({ "id": 5, "method": "eth_sendRawTransaction" }))), RPC_METHODS)
            .await;
        let body = axum::body::to_bytes(refused.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], 5);
        assert_eq!(v["error"]["message"], METHOD_NOT_ALLOWED.message);
        assert_eq!(v["error"]["data"], "method_not_allowed");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let ok = p.forward(&upstream, Ok(Json(json!({ "id": 1, "method": "eth_chainId" }))), RPC_METHODS).await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
