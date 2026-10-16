//! v3's minting gates for `GET /status` (design notes T-01, T-03): whether a proven deposit of each
//! vault denomination would be minted now, the pause, the halt, the bonds and the coverage cap's
//! room, read from `ZecuredIntake` and `SignerBondV2` and cached for [`TTL`]. Public state only:
//! flags, times and one total, nothing about any vault or user. None of it ever blocks an exit.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::{
    primitives::{Address, Bytes, U256},
    sol,
    sol_types::SolCall,
};
use anyhow::Result;
use serde_json::{Map, Value, json};

sol! {
    function mintingPaused() external view returns (bool);
    function pausedUntil() external view returns (uint64);
    function pausedSince() external view returns (uint64);
    function capRoom() external view returns (uint256);
    function mintBlock(uint64 amount) external view returns (uint8);
    function halted() external view returns (bool);
    function fullyBonded() external view returns (bool);
}

/// `MintBlock` (IZecuredIntake), by value; the enum is append-only.
pub const MINT_BLOCK: [&str; 5] = ["None", "Halted", "NotBonded", "Paused", "CapExceeded"];

/// How long one reading is served: `/status` never fans out to the chain per request.
pub const TTL: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Why an `eth_call` gave no result.
#[derive(Debug, PartialEq, Eq)]
enum CallError {
    /// The node answered with an error: the call reverted (`mintBlock` reverts `CapPriceStale`).
    Reverted,
    /// No usable answer: the node is unreachable or answered something else.
    Unavailable,
}

pub struct MintingGates {
    rpc_url: String,
    intake: Address,
    bond: Address,
    denominations: Vec<u64>,
    http: reqwest::Client,
    cache: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

impl MintingGates {
    pub fn new(rpc_url: String, intake: Address, bond: Address, denominations: Vec<u64>) -> Result<Self> {
        Ok(Self {
            rpc_url,
            intake,
            bond,
            denominations,
            http: reqwest::Client::builder().timeout(CALL_TIMEOUT).build()?,
            cache: tokio::sync::Mutex::new(None),
        })
    }

    /// The gates as `GET /status` shows them, read at most once per [`TTL`]; `null` while the
    /// chain cannot be read.
    pub async fn json(&self) -> Value {
        let mut cache = self.cache.lock().await;
        if let Some((at, value)) = cache.as_ref()
            && at.elapsed() < TTL
        {
            return value.clone();
        }
        let value = self.read().await.unwrap_or(Value::Null);
        *cache = Some((Instant::now(), value.clone()));
        value
    }

    /// - `paused`: `mintingPaused()`, by signatures until `pausedUntil` (0: not paused by
    ///   signatures), or by an open theft case on a deployment with `autoPauseOnCase`;
    /// - `halted`, `fullyBonded`: the bond's;
    /// - `capRoom`: zatoshi the coverage and absolute caps still admit (decimal string; 0 while
    ///   the cap's price is stale, 2^256 - 1 without a cap);
    /// - `mintBlock`: per denomination (zatoshi, decimal keys), what would stop a proven deposit
    ///   of that amount from being minted now (`None`: nothing), or `null` while the cap's price
    ///   is stale (the Intake then refuses deposit proofs until it is fresh again).
    async fn read(&self) -> Option<Value> {
        let (paused, until, since, room, halted, bonded) = tokio::join!(
            self.view::<mintingPausedCall>(self.intake, mintingPausedCall {}),
            self.view::<pausedUntilCall>(self.intake, pausedUntilCall {}),
            self.view::<pausedSinceCall>(self.intake, pausedSinceCall {}),
            self.view::<capRoomCall>(self.intake, capRoomCall {}),
            self.view::<haltedCall>(self.bond, haltedCall {}),
            self.view::<fullyBondedCall>(self.bond, fullyBondedCall {}),
        );
        let mut blocks = Map::new();
        for amount in &self.denominations {
            let block = match self.eth_call(self.intake, mintBlockCall { amount: *amount }.abi_encode()).await {
                Ok(out) => {
                    let value = mintBlockCall::abi_decode_returns(&out).ok()?;
                    Value::from(*MINT_BLOCK.get(usize::from(value))?)
                }
                Err(CallError::Reverted) => Value::Null,
                Err(CallError::Unavailable) => return None,
            };
            blocks.insert(amount.to_string(), block);
        }
        let room: U256 = room?;
        Some(json!({
            "paused": paused?,
            "pausedUntil": until?,
            "pausedSince": since?,
            "halted": halted?,
            "fullyBonded": bonded?,
            "capRoom": room.to_string(),
            "mintBlock": blocks,
            "readAt": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        }))
    }

    /// A view call that must answer.
    async fn view<C: SolCall>(&self, to: Address, call: C) -> Option<C::Return> {
        let out = self.eth_call(to, call.abi_encode()).await.ok()?;
        C::abi_decode_returns(&out).ok()
    }

    async fn eth_call(&self, to: Address, data: Vec<u8>) -> Result<Vec<u8>, CallError> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "eth_call",
            "params": [{ "to": to, "data": Bytes::from(data) }, "latest"],
        });
        let resp = self.http.post(&self.rpc_url).json(&body).send().await.map_err(|_| CallError::Unavailable)?;
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
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{Json, Router, extract::State, routing::post};

    use super::*;

    const INTAKE: Address = Address::repeat_byte(0x55);
    const BOND: Address = Address::repeat_byte(0x22);

    /// The Intake and the bond as an RPC stub: each view's answer, `mintBlock` per amount
    /// (absent: the call reverts, as with a stale cap price).
    struct Chain {
        paused: bool,
        until: u64,
        halted: bool,
        bonded: bool,
        room: U256,
        blocks: HashMap<u64, u8>,
        calls: AtomicUsize,
    }

    async fn rpc(State(c): State<Arc<Chain>>, Json(req): Json<Value>) -> Json<Value> {
        c.calls.fetch_add(1, Ordering::SeqCst);
        let to: Address = req["params"][0]["to"].as_str().unwrap().parse().unwrap();
        let data: Bytes = serde_json::from_value(req["params"][0]["data"].clone()).unwrap();
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        let out = match (to, selector) {
            (INTAKE, mintingPausedCall::SELECTOR) => mintingPausedCall::abi_encode_returns(&c.paused),
            (INTAKE, pausedUntilCall::SELECTOR) => pausedUntilCall::abi_encode_returns(&c.until),
            (INTAKE, pausedSinceCall::SELECTOR) => pausedSinceCall::abi_encode_returns(&(c.until / 2)),
            (INTAKE, capRoomCall::SELECTOR) => capRoomCall::abi_encode_returns(&c.room),
            (INTAKE, mintBlockCall::SELECTOR) => {
                let amount = mintBlockCall::abi_decode(&data).unwrap().amount;
                match c.blocks.get(&amount) {
                    Some(b) => mintBlockCall::abi_encode_returns(b),
                    None => {
                        let error = json!({ "code": 3, "message": "execution reverted", "data": "0x8d5bcd5a" });
                        return Json(json!({ "jsonrpc": "2.0", "id": req["id"], "error": error }));
                    }
                }
            }
            (BOND, haltedCall::SELECTOR) => haltedCall::abi_encode_returns(&c.halted),
            (BOND, fullyBondedCall::SELECTOR) => fullyBondedCall::abi_encode_returns(&c.bonded),
            other => panic!("unexpected call {other:?}"),
        };
        Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": Bytes::from(out) }))
    }

    async fn serve(chain: Chain) -> (String, Arc<Chain>) {
        let chain = Arc::new(chain);
        let app = Router::new().route("/", post(rpc)).with_state(chain.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), chain)
    }

    fn chain() -> Chain {
        Chain {
            paused: true,
            until: 1_800_000_000,
            halted: false,
            bonded: true,
            room: U256::from(4_000_000u64),
            blocks: HashMap::from([(1_000_000, 3), (5_000_000, 4)]),
            calls: AtomicUsize::new(0),
        }
    }

    /// T-01, T-03: the gates read from the Intake and the bond, per denomination, then cached.
    #[tokio::test]
    async fn the_gates_are_read_per_denomination_and_cached() {
        let (url, chain) = serve(chain()).await;
        let gates = MintingGates::new(url, INTAKE, BOND, vec![1_000_000, 5_000_000, 12_500_000]).unwrap();
        let v = gates.json().await;
        assert!(v["readAt"].as_u64().unwrap() > 1_700_000_000);
        let mut v = v;
        v.as_object_mut().unwrap().remove("readAt");
        assert_eq!(
            v,
            json!({
                "paused": true,
                "pausedUntil": 1_800_000_000u64,
                "pausedSince": 900_000_000u64,
                "halted": false,
                "fullyBonded": true,
                "capRoom": "4000000",
                // 0.125 reverts: the cap's price is stale, nothing can be told now.
                "mintBlock": { "1000000": "Paused", "5000000": "CapExceeded", "12500000": null },
            })
        );
        let calls = chain.calls.load(Ordering::SeqCst);
        assert_eq!(calls, 6 + 3);
        gates.json().await;
        assert_eq!(chain.calls.load(Ordering::SeqCst), calls, "served from the cache");
    }

    /// Without a cap (tests only) `capRoom` is 2^256 - 1; an unknown `MintBlock` value or an
    /// unreachable node is no reading at all.
    #[tokio::test]
    async fn an_unreadable_chain_is_no_reading() {
        let mut c = chain();
        c.room = U256::MAX;
        c.blocks.insert(12_500_000, 9);
        let (url, _) = serve(c).await;
        let gates = MintingGates::new(url.clone(), INTAKE, BOND, vec![1_000_000]).unwrap();
        assert_eq!(gates.json().await["capRoom"], U256::MAX.to_string());
        let unknown = MintingGates::new(url, INTAKE, BOND, vec![12_500_000]).unwrap();
        assert_eq!(unknown.json().await, Value::Null);
        let offline = MintingGates::new("http://127.0.0.1:9".into(), INTAKE, BOND, vec![1_000_000]).unwrap();
        assert_eq!(offline.json().await, Value::Null);
    }
}
