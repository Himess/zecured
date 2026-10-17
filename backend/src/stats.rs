//! P-2, the status page's anonymity sets: how many vaults of each deposit size are open on Base
//! now. A vault's size is the `amount` of its `VaultActivated` event (the deposit, before any opening
//! fee), and it counts while `Registry.vault(vaultId).status` is Active. Read from the chain alone
//! (public events and views, nothing from the coordinator's books): the events from the deployment's
//! block in [`LOG_SPAN`]-block windows with a cursor, the statuses of the vaults still Active once
//! every [`TTL`] (a vault that left Active never returns to it: Closed is final). `GET /stats/vaults`
//! answers `{ byAmount: { <zatoshi>: count }, total, complete, readAt }`; `complete` is false while
//! the event scan has not reached the latest block yet.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, B256, Bytes},
    sol,
    sol_types::{SolCall, SolEvent},
};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

sol! {
    event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
    function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
}

/// `VaultStatus.Active` (IZecuredRegistry, append-only).
const STATUS_ACTIVE: u8 = 1;
/// Blocks per `eth_getLogs` (public RPCs refuse larger ranges).
pub const LOG_SPAN: u64 = 2_000;
/// Windows read per refresh, so that one request never hammers the RPC.
const MAX_WINDOWS_PER_REFRESH: u64 = 50;
/// Vaults followed at most.
const MAX_VAULTS: usize = 10_000;
/// How long one reading is served.
pub const TTL: Duration = Duration::from_secs(120);
/// How long an unreadable chain is left alone before the next try.
const RETRY: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct VaultStats {
    rpc_url: String,
    registry: Address,
    http: reqwest::Client,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The next block whose events are not read yet.
    next_block: u64,
    /// Every activated vault seen: its size, and whether it was Active at the last read.
    vaults: HashMap<B256, (u64, bool)>,
    complete: bool,
    /// When the last reading succeeded (Unix seconds).
    read_at: Option<u64>,
    /// When to read the chain again.
    next_try: Option<Instant>,
}

/// Base could not be read before any reading succeeded: a fixed answer (L-2).
#[derive(Debug, PartialEq, Eq)]
pub struct Unavailable;

impl IntoResponse for Unavailable {
    fn into_response(self) -> Response {
        let body = json!({ "error": "Base could not be read. Try again later.", "code": "chain_unavailable" });
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

impl VaultStats {
    pub fn new(rpc_url: String, registry: Address, from_block: u64) -> anyhow::Result<Self> {
        Ok(Self {
            rpc_url,
            registry,
            http: reqwest::Client::builder().timeout(CALL_TIMEOUT).build()?,
            state: Mutex::new(State {
                next_block: from_block,
                ..State::default()
            }),
        })
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, Unavailable> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await.map_err(|_| Unavailable)?;
        let v: Value = resp.json().await.map_err(|_| Unavailable)?;
        v.get("result").cloned().ok_or(Unavailable)
    }

    async fn latest_block(&self) -> Result<u64, Unavailable> {
        let n = self.rpc("eth_blockNumber", json!([])).await?;
        let hex = n.as_str().and_then(|s| s.strip_prefix("0x")).ok_or(Unavailable)?;
        u64::from_str_radix(hex, 16).map_err(|_| Unavailable)
    }

    async fn activations(&self, from: u64, to: u64) -> Result<Vec<(B256, u64)>, Unavailable> {
        let filter = json!([{
            "address": self.registry,
            "topics": [VaultActivated::SIGNATURE_HASH],
            "fromBlock": format!("0x{from:x}"),
            "toBlock": format!("0x{to:x}"),
        }]);
        let logs = self.rpc("eth_getLogs", filter).await?;
        let mut out = vec![];
        for log in logs.as_array().ok_or(Unavailable)? {
            let topics: Vec<B256> = serde_json::from_value(log["topics"].clone()).map_err(|_| Unavailable)?;
            let data: Bytes = serde_json::from_value(log["data"].clone()).map_err(|_| Unavailable)?;
            let e = VaultActivated::decode_raw_log(topics, &data).map_err(|_| Unavailable)?;
            out.push((e.vaultId, e.amount));
        }
        Ok(out)
    }

    async fn is_active(&self, vault_id: B256) -> Result<bool, Unavailable> {
        let data = Bytes::from(vaultCall { vaultId: vault_id }.abi_encode());
        let out = self.rpc("eth_call", json!([{ "to": self.registry, "data": data }, "latest"])).await?;
        let out: Bytes = serde_json::from_value(out).map_err(|_| Unavailable)?;
        let r = vaultCall::abi_decode_returns(&out).map_err(|_| Unavailable)?;
        Ok(r.status == STATUS_ACTIVE)
    }

    /// Reads new events (a bounded number of windows) and the statuses of the vaults still Active.
    async fn refresh(&self, s: &mut State) -> Result<(), Unavailable> {
        let latest = self.latest_block().await?;
        let mut windows = 0;
        while s.next_block <= latest && windows < MAX_WINDOWS_PER_REFRESH {
            let to = (s.next_block + LOG_SPAN - 1).min(latest);
            for (vault, amount) in self.activations(s.next_block, to).await? {
                if s.vaults.len() < MAX_VAULTS || s.vaults.contains_key(&vault) {
                    s.vaults.entry(vault).or_insert((amount, true));
                }
            }
            s.next_block = to + 1;
            windows += 1;
        }
        s.complete = s.next_block > latest;
        let active: Vec<B256> = s.vaults.iter().filter(|(_, (_, a))| *a).map(|(id, _)| *id).collect();
        for id in active {
            let still = self.is_active(id).await?;
            if let Some(v) = s.vaults.get_mut(&id) {
                v.1 = still;
            }
        }
        s.read_at = Some(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
        Ok(())
    }

    /// The counts, read again once the last reading is [`TTL`] old. An unreadable chain keeps the
    /// last reading (with its `readAt`), or answers [`Unavailable`] before the first.
    pub async fn json(&self) -> Result<Value, Unavailable> {
        let mut s = self.state.lock().await;
        if s.next_try.is_none_or(|t| Instant::now() >= t) {
            let ok = self.refresh(&mut s).await.is_ok();
            s.next_try = Some(Instant::now() + if ok { TTL } else { RETRY });
        }
        if s.read_at.is_none() {
            return Err(Unavailable);
        }
        let mut by_amount: BTreeMap<u64, u64> = BTreeMap::new();
        for (amount, active) in s.vaults.values() {
            *by_amount.entry(*amount).or_default() += u64::from(*active);
        }
        let total: u64 = by_amount.values().sum();
        let by_amount: serde_json::Map<String, Value> =
            by_amount.iter().map(|(a, n)| (a.to_string(), json!(n))).collect();
        Ok(json!({ "byAmount": by_amount, "total": total, "complete": s.complete, "readAt": s.read_at }))
    }
}

/// `GET /stats/vaults` (`None`: no deployment file to read the Registry from).
pub async fn handler(axum::Extension(stats): axum::Extension<Option<Arc<VaultStats>>>) -> Response {
    match stats {
        Some(stats) => match stats.json().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => e.into_response(),
        },
        None => {
            let body = json!({ "error": "Not configured on this backend.", "code": "not_configured" });
            (StatusCode::NOT_FOUND, Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Router, extract::State as AxState, routing::post};

    use super::*;

    const REGISTRY: Address = Address::repeat_byte(0x22);

    /// The Registry on a stub: activations at given blocks, and the vaults still Active.
    struct Chain {
        latest: u64,
        activations: Vec<(u64, B256, u64)>,
        active: Vec<B256>,
        log_ranges: StdMutex<Vec<(u64, u64)>>,
        status_reads: StdMutex<Vec<B256>>,
        calls: AtomicUsize,
    }

    impl Chain {
        fn new(latest: u64, activations: Vec<(u64, B256, u64)>, active: Vec<B256>) -> Self {
            Self {
                latest,
                activations,
                active,
                log_ranges: StdMutex::default(),
                status_reads: StdMutex::default(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    async fn rpc(AxState(c): AxState<Arc<Chain>>, Json(req): Json<Value>) -> Json<Value> {
        c.calls.fetch_add(1, Ordering::SeqCst);
        let p = &req["params"];
        let hex = |v: &Value| u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
        let result = match req["method"].as_str().unwrap() {
            "eth_blockNumber" => json!(format!("0x{:x}", c.latest)),
            "eth_getLogs" => {
                let (from, to) = (hex(&p[0]["fromBlock"]), hex(&p[0]["toBlock"]));
                assert!(to - from < LOG_SPAN);
                assert_eq!(p[0]["address"].as_str().unwrap().parse::<Address>().unwrap(), REGISTRY);
                c.log_ranges.lock().unwrap().push((from, to));
                let logs: Vec<Value> = c
                    .activations
                    .iter()
                    .filter(|(b, _, _)| (from..=to).contains(b))
                    .map(|(b, id, amount)| {
                        let e = VaultActivated {
                            vaultId: *id,
                            account: Address::repeat_byte(0x33),
                            amount: *amount,
                            depositTag: B256::ZERO,
                        };
                        let log = e.encode_log_data();
                        json!({ "address": REGISTRY, "topics": log.topics(), "data": log.data, "blockNumber": format!("0x{b:x}") })
                    })
                    .collect();
                json!(logs)
            }
            "eth_call" => {
                let data: Bytes = serde_json::from_value(p[0]["data"].clone()).unwrap();
                let id = vaultCall::abi_decode(&data).unwrap().vaultId;
                c.status_reads.lock().unwrap().push(id);
                let status = if c.active.contains(&id) { STATUS_ACTIVE } else { 2 };
                json!(Bytes::from(vaultCall::abi_encode_returns(&vaultReturn {
                    account: Address::ZERO,
                    outstanding: 0,
                    status
                })))
            }
            m => panic!("unexpected {m}"),
        };
        Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
    }

    async fn serve(chain: Chain) -> (String, Arc<Chain>) {
        let chain = Arc::new(chain);
        let app = Router::new().route("/", post(rpc)).with_state(chain.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), chain)
    }

    #[tokio::test]
    async fn counts_the_open_vaults_of_each_size_from_the_events_and_the_statuses() {
        let v = |b: u8| B256::repeat_byte(b);
        let activations = vec![
            (100_010, v(1), 1_000_000),
            (101_999, v(2), 1_000_000),
            (102_000, v(3), 5_000_000),
            (104_400, v(4), 5_000_000),
        ];
        let (url, chain) = serve(Chain::new(104_500, activations, vec![v(1), v(3), v(4)])).await;
        let stats = VaultStats::new(url, REGISTRY, 100_000).unwrap();
        let out = stats.json().await.unwrap();
        assert_eq!(out["byAmount"], json!({ "1000000": 1, "5000000": 2 }), "the second 0.01 vault closed");
        assert_eq!((&out["total"], &out["complete"]), (&json!(3), &json!(true)));
        assert!(out["readAt"].as_u64().unwrap() > 1_700_000_000);
        let windows = vec![(100_000, 101_999), (102_000, 103_999), (104_000, 104_500)];
        assert_eq!(*chain.log_ranges.lock().unwrap(), windows);
        // Served from the cache within the TTL: no call reaches the chain.
        let calls = chain.calls.load(Ordering::SeqCst);
        assert_eq!(stats.json().await.unwrap(), out);
        assert_eq!(chain.calls.load(Ordering::SeqCst), calls);
        // The next reading asks only the vaults that were still Active.
        chain.status_reads.lock().unwrap().clear();
        stats.state.lock().await.next_try = None;
        stats.json().await.unwrap();
        let mut read = chain.status_reads.lock().unwrap().clone();
        read.sort();
        assert_eq!(read, vec![v(1), v(3), v(4)]);
    }

    #[tokio::test]
    async fn an_unreadable_chain_is_a_fixed_answer_before_the_first_reading() {
        let offline = VaultStats::new("http://127.0.0.1:9".into(), REGISTRY, 1).unwrap();
        assert_eq!(offline.json().await, Err(Unavailable));
        let r = handler(axum::Extension(None)).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_long_history_is_read_a_bounded_number_of_windows_at_a_time() {
        let (url, chain) = serve(Chain::new(1_000_000, vec![], vec![])).await;
        let stats = VaultStats::new(url, REGISTRY, 0).unwrap();
        let out = stats.json().await.unwrap();
        assert_eq!((&out["complete"], &out["total"]), (&json!(false), &json!(0)));
        assert_eq!(chain.log_ranges.lock().unwrap().len() as u64, MAX_WINDOWS_PER_REFRESH);
    }
}
