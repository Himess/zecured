//! Tests only: a Base JSON-RPC stub of what the maker reads (its latest block, ZecuredSwap's orders,
//! relay and version registry, the oracle, USDC), answering from a state the test changes as it
//! goes, and a maker on it.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use alloy::{
    primitives::{Address, B256, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
};
use axum::{Json, Router, extract::State, routing::post};
use orchard::keys::{FullViewingKey, SpendingKey};
use serde_json::{Value, json};
use zecured_base::events::{IZcashHeaderRelay, IZecuredVersions};
use zecured_swap::abi::{IERC20, IZecPrice, IZecuredSwap};
use zecured_zcash_ops::network::ZcashNetwork;

use crate::{
    config::MakerConfig,
    engine::{Maker, Wallet},
    quoting::ChainLimits,
    state::Book,
};

/// What the stub answers.
pub struct Chain {
    /// The latest block's timestamp.
    pub now: u64,
    /// `isWorking` of every version.
    pub version_working: bool,
    pub relay_height: u64,
    /// `price()` and `priceHigh()` (fresh at `now`), in Morpho's scale.
    pub price: U256,
    pub usdc_balance: U256,
    pub usdc_allowance: U256,
    /// ZecuredSwap's orders; another id is an empty one (`OrderStatus.None`).
    pub orders: HashMap<B256, IZecuredSwap::Order>,
    /// How many calls the stub answered.
    pub calls: usize,
}

impl Default for Chain {
    fn default() -> Self {
        Self {
            now: 1_000_000,
            version_working: true,
            relay_height: 4_500_000,
            // $40 per ZEC.
            price: U256::from(40_000_000u64) * crate::pricing::price_scale() / U256::from(100_000_000u64),
            usdc_balance: U256::from(500_000_000u64),
            usdc_allowance: U256::MAX,
            orders: HashMap::new(),
            calls: 0,
        }
    }
}

pub type Shared = Arc<Mutex<Chain>>;

/// Serves `chain` on a loopback port; returns its URL.
pub async fn base_stub(chain: Shared) -> String {
    let app = Router::new().route("/", post(rpc)).with_state(chain);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn word(v: U256) -> Vec<u8> {
    v.to_be_bytes::<32>().to_vec()
}

fn address(a: Address) -> Vec<u8> {
    word(U256::from_be_slice(a.as_slice()))
}

/// The latest block, as `eth_getBlockByNumber` gives it.
fn block(now: u64) -> Value {
    let h = B256::repeat_byte(0x42);
    json!({
        "hash": h, "parentHash": h, "sha3Uncles": h, "miner": Address::ZERO, "stateRoot": h,
        "transactionsRoot": h, "receiptsRoot": h, "logsBloom": format!("0x{}", "0".repeat(512)),
        "difficulty": "0x0", "number": "0x10", "gasLimit": "0x1c9c380", "gasUsed": "0x0",
        "timestamp": format!("0x{now:x}"), "extraData": "0x", "mixHash": h, "nonce": "0x0000000000000000",
        "baseFeePerGas": "0x1", "uncles": [], "transactions": [],
    })
}

async fn rpc(State(chain): State<Shared>, Json(req): Json<Value>) -> Json<Value> {
    let mut c = chain.lock().unwrap();
    c.calls += 1;
    let error =
        |message: &str| json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": -32000, "message": message } });
    let result = match req["method"].as_str() {
        Some("eth_chainId") => json!("0x14a34"),
        Some("eth_blockNumber") => json!("0x10"),
        Some("eth_getBlockByNumber") => block(c.now),
        Some("eth_call") => {
            let call = &req["params"][0];
            let data = hex::decode(
                call["input"].as_str().or(call["data"].as_str()).unwrap_or_default().trim_start_matches("0x"),
            )
            .unwrap_or_default();
            let (selector, args) = (data.get(..4).unwrap_or_default(), data.get(4..).unwrap_or_default());
            let out: Vec<u8> = if selector == IZecuredSwap::versionsCall::SELECTOR {
                address(Address::repeat_byte(0x57))
            } else if selector == IZecuredSwap::relayCall::SELECTOR {
                address(Address::repeat_byte(0x77))
            } else if selector == IZecuredSwap::usdcCall::SELECTOR {
                address(Address::repeat_byte(0x55))
            } else if selector == IZecuredVersions::isWorkingCall::SELECTOR {
                word(U256::from(c.version_working as u8))
            } else if selector == IZcashHeaderRelay::bestChainTipCall::SELECTOR {
                IZcashHeaderRelay::bestChainTipCall::abi_encode_returns(&IZcashHeaderRelay::bestChainTipReturn {
                    blockHash: B256::repeat_byte(0xaa),
                    height: U256::from(c.relay_height),
                    chainWork: U256::from(c.relay_height),
                })
            } else if selector == IZecPrice::priceCall::SELECTOR {
                word(c.price)
            } else if selector == IZecPrice::priceHighCall::SELECTOR {
                IZecPrice::priceHighCall::abi_encode_returns(&IZecPrice::priceHighReturn {
                    price: c.price,
                    updatedAt: U256::from(c.now),
                })
            } else if selector == IERC20::balanceOfCall::SELECTOR {
                word(c.usdc_balance)
            } else if selector == IERC20::allowanceCall::SELECTOR {
                word(c.usdc_allowance)
            } else if selector == IZecuredSwap::orderCall::SELECTOR {
                let id = B256::from_slice(&args[..32]);
                let none = IZecuredSwap::Order {
                    maker: Address::ZERO,
                    zatoshi: 0,
                    side: 0,
                    status: 0,
                    taker: Address::ZERO,
                    payDeadline: 0,
                    usdc: U256::ZERO,
                    receiverCommitment: B256::ZERO,
                };
                IZecuredSwap::orderCall::abi_encode_returns(c.orders.get(&id).unwrap_or(&none))
            } else if selector == IZecuredSwap::pendingSettlementCall::SELECTOR {
                IZecuredSwap::pendingSettlementCall::abi_encode_returns(&IZecuredSwap::PendingSettlement {
                    anchor: B256::ZERO,
                    versionId: B256::ZERO,
                    readyHeight: 0,
                    readyTime: 0,
                })
            } else {
                return Json(error("unexpected call"));
            };
            json!(Bytes::from(out))
        }
        _ => return Json(error("unexpected method")),
    };
    Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
}

/// A maker of `cfg` and `book` on the Base RPC `rpc`, with a test wallet (never scanned).
pub fn maker(rpc: &str, cfg: MakerConfig, book: Book) -> Arc<Maker> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap()).erased();
    let sk = (1u8..).find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes([b; 32]))).unwrap();
    let wallet = Wallet {
        fvk: FullViewingKey::from(&sk),
        sk,
        birthday: 1,
        scan: None,
    };
    let limits = ChainLimits {
        max_order_usdc: U256::from(200_000_000u64),
        max_order_zat: 1_000_000_000,
        min_pay_window: 7_200,
        max_pay_window: 21_600,
    };
    let domain = zecured_swap::quote::swap_domain(cfg.chain_id, cfg.swap);
    Arc::new(Maker::new(
        cfg,
        ZcashNetwork::Testnet,
        PrivateKeySigner::random(),
        provider,
        domain,
        limits,
        book,
        wallet,
        B256::ZERO,
    ))
}

/// An open order of `maker` on Base, as `order(id)` answers it.
pub fn open_order(maker: Address, side: u8, taker: Address, zatoshi: u64, pay_deadline: u64) -> IZecuredSwap::Order {
    IZecuredSwap::Order {
        maker,
        zatoshi,
        side,
        status: 1,
        taker,
        payDeadline: pay_deadline,
        usdc: U256::from(20_000_000u64),
        receiverCommitment: B256::repeat_byte(3),
    }
}
