//! The maker's HTTP API (served to the app through the backend's proxy, or directly):
//!
//! - `GET /swap/quote?side=buy|sell&zatoshi=N` (or `&usdc=N`, USDC units) `[&taker=0x..]`, header
//!   `X-Zecured-Invite: <code>` when the maker is invite-only: a signed `SwapQuote` for
//!   `ZecuredSwap.open`, with the order id it will have; on Sell also the maker's Ironwood receiver
//!   and salt, which the taker pays and proves against.
//! - `POST /swap/orders/{orderId}/destination` `{ receiver, salt }`: the taker's Ironwood receiver
//!   (a unified address, or the raw 43 bytes as 0x-hex) and the salt of the order's commitment.
//!   The commitment is on Base: a destination that does not open it is refused, so no
//!   authentication is needed. Never logged; erased once the order is done.
//! - `GET /swap/orders/{orderId}`: the maker's view of one of its orders (no receiver, salt or
//!   txid).
//! - `GET /swap/status`: what the maker quotes now.
//!
//! And on the operators' loopback address only (`status_bind`, O-7), never through the public API
//! or the backend's proxy: `GET /inventory`, the maker's free ZEC and USDC for the watchdog.
//!
//! Errors are `{ "error": message, "code": code }` with fixed messages.

use std::sync::{Arc, atomic::Ordering};

use alloy::primitives::{B256, U256};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use orchard::keys::Scope;
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info};
use zecured_swap::{
    Side,
    abi::{IERC20, IZecPrice},
    quote::sign_quote,
};
use zecured_zcash_ops::keys::encode_ironwood_ua;

use crate::{
    engine::{Maker, accept_destination},
    gate::{INVITE_HEADER, admitted},
    quoting::{Market, QuoteJson, QuoteRequest, quote, terms},
    state::MakerStatus,
};

/// An error of the maker API: a fixed message and a short code.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}

impl ApiError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self { status, code, message }
    }

    pub const BAD_REQUEST: Self = Self::new(StatusCode::BAD_REQUEST, "bad_request", "Invalid request.");
    pub const INVITE_REQUIRED: Self =
        Self::new(StatusCode::FORBIDDEN, "invite_required", "Quotes are for invitees only.");
    pub const SIDE_DISABLED: Self =
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "side_disabled", "The maker does not quote this side.");
    pub const TAKER_REQUIRED: Self = Self::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "taker_required",
        "Give the taker's address: the quote is bound to it.",
    );
    pub const TOO_SMALL: Self =
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "too_small", "The order is below the minimum.");
    pub const TOO_LARGE: Self =
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "too_large", "The order is above the maximum.");
    pub const NO_INVENTORY: Self =
        Self::new(StatusCode::CONFLICT, "no_inventory", "The maker cannot fill this order now.");
    pub const BUSY: Self = Self::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "The maker has too many open orders.");
    pub const RELAY_BEHIND: Self = Self::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "relay_behind",
        "The Zcash header relay lags: the pay window could not be met.",
    );
    pub const PRICE_STALE: Self = Self::new(StatusCode::SERVICE_UNAVAILABLE, "price_stale", "No fresh ZEC price.");
    pub const UNAVAILABLE: Self =
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "The maker cannot follow Zcash or Base right now.");
    pub const UNKNOWN_ORDER: Self = Self::new(StatusCode::NOT_FOUND, "unknown_order", "No such order of this maker.");
    pub const NOT_BUY: Self =
        Self::new(StatusCode::CONFLICT, "not_buy", "Only a Buy order takes a destination: on Sell the taker pays.");
    pub const NOT_AWAITING: Self =
        Self::new(StatusCode::CONFLICT, "not_awaiting", "This order no longer takes a destination.");
    pub const TOO_LATE: Self = Self::new(
        StatusCode::CONFLICT,
        "too_late",
        "Too late to pay and prove before the deadline: the order will be refunded.",
    );
    pub const BAD_DESTINATION: Self = Self::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "bad_destination",
        "Not an Ironwood receiver and salt that open the order's commitment.",
    );
    pub const INTERNAL: Self = Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "Internal error.");
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        debug!("swap maker request failed: {e:#}");
        Self::INTERNAL
    }
}

pub fn router(maker: Arc<Maker>) -> Router {
    Router::new()
        .route("/swap/quote", get(get_quote))
        .route("/swap/orders/{order_id}/destination", post(post_destination))
        .route("/swap/orders/{order_id}", get(get_order))
        .route("/swap/status", get(get_status))
        .with_state(maker)
}

/// The market as the quote needs it: price, relay lag, inventory.
async fn market(m: &Maker, side: Side) -> Result<Market, ApiError> {
    let oracle = IZecPrice::new(m.cfg.oracle, &m.provider);
    let now = m.now().await?;
    let low = oracle.price().call().await.map_err(|_| ApiError::PRICE_STALE)?;
    // A ZecuredPriceFeed also gives its high side and when it was last fresh; a plain Morpho oracle
    // (a local fork's MOCK) does not.
    let (price, price_age) = match oracle.priceHigh().call().await {
        Ok(high) => {
            let age = now.saturating_sub(u64::try_from(high.updatedAt).unwrap_or(0));
            (if side == Side::Buy { high.price.max(low) } else { low }, Some(age))
        }
        Err(_) if m.cfg.local_fork => (low, None),
        Err(_) => return Err(ApiError::PRICE_STALE),
    };
    // The engine's last pass: it must be recent (the maker follows both chains), and its view of
    // the relay gives the lag.
    let (zcash_tip, relay_height, pass_at) = m.last_pass();
    if crate::engine::unix_now().saturating_sub(pass_at) > 3 * m.cfg.poll_secs + 60 {
        return Err(ApiError::UNAVAILABLE);
    }
    let (reserved, open_orders) = {
        let book = m.book.lock().await;
        (book.reserved_zatoshi(), book.open_orders().count())
    };
    let me = m.signer.address();
    let usdc = IERC20::new(m.swap().usdc().call().await.map_err(anyhow::Error::from)?, &m.provider);
    let balance = usdc.balanceOf(me).call().await.map_err(anyhow::Error::from)?;
    let allowance = usdc.allowance(me, m.cfg.swap).call().await.map_err(anyhow::Error::from)?;
    Ok(Market {
        price,
        price_age,
        now,
        relay_lag: zcash_tip.saturating_sub(relay_height),
        zcash_free: m.balance.load(Ordering::Relaxed).saturating_sub(reserved),
        usdc_free: balance.min(allowance),
        open_orders,
    })
}

async fn get_quote(
    State(m): State<Arc<Maker>>,
    headers: HeaderMap,
    query: Result<Query<QuoteRequest>, QueryRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let code = headers.get(INVITE_HEADER).and_then(|v| v.to_str().ok());
    if !admitted(&m.cfg, code) {
        return Err(ApiError::INVITE_REQUIRED);
    }
    let Ok(Query(req)) = query else {
        return Err(ApiError::BAD_REQUEST);
    };
    let side = Side::parse(&req.side).map_err(|_| ApiError::BAD_REQUEST)?;
    let market = market(&m, side).await?;
    let t = terms(&m.cfg, &m.limits, &market, &req)?;
    // A random nonce: unguessable order ids, and none reused across restarts.
    let nonce = U256::from_be_bytes(zecured_swap::random_bytes32());
    // Sell: a fresh diversified receiver of the maker's wallet for each quote, unlinkable to the
    // maker's other receipts; the wallet's incoming viewing key finds the payment.
    let maker_receiver = if t.side == Side::Sell {
        let w = m.wallet.lock().await;
        let index = u32::from_be_bytes(zecured_swap::random_bytes32()[..4].try_into().expect("4 bytes"));
        let address = w.fvk.address_at(index, Scope::External);
        let salt = zecured_swap::random_bytes32();
        Some((
            encode_ironwood_ua(&m.network, address)?,
            salt,
            zecured_swap::receiver_commitment(&salt, &address.to_raw_address_bytes()),
        ))
    } else {
        None
    };
    let q = quote(&m.cfg, &t, m.signer.address(), market.now, nonce, maker_receiver.as_ref().map(|r| r.2));
    let signature = sign_quote(&m.signer, &m.domain, &q)?;
    let order_id = zecured_swap::order_id(m.cfg.chain_id, m.cfg.swap, q.maker, q.nonce);
    let mut body = json!({
        "quote": QuoteJson::from(&q),
        "signature": signature,
        "swap": m.cfg.swap,
        "chainId": m.cfg.chain_id,
        "orderId": order_id,
        "expiresAt": q.quoteDeadline.to_string(),
    });
    if let Some((ua, salt, _)) = maker_receiver {
        body["makerReceiver"] = json!(ua);
        body["makerSalt"] = json!(format!("0x{}", hex::encode(salt)));
    }
    info!(side = ?t.side, zatoshi = t.zatoshi, "quoted");
    Ok(Json(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationBody {
    receiver: String,
    salt: String,
}

async fn post_destination(
    State(m): State<Arc<Maker>>,
    Path(order_id): Path<B256>,
    body: Result<Json<DestinationBody>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let Ok(Json(body)) = body else {
        return Err(ApiError::BAD_REQUEST);
    };
    let now = m.now().await?;
    // An order opened since the engine's last pass is read from Base directly.
    let known = m.book.lock().await.orders.contains_key(&order_id);
    let fresh = if known {
        None
    } else {
        m.record_from_chain(order_id).await?
    };
    let mut book = m.book.lock().await;
    if let Some(rec) = fresh {
        book.orders.entry(order_id).or_insert(rec);
    }
    let rec = book.orders.get(&order_id).ok_or(ApiError::UNKNOWN_ORDER)?.clone();
    let d = accept_destination(&m.cfg, &m.network, &rec, now, &body.receiver, &body.salt)?;
    let entry = book.orders.get_mut(&order_id).expect("read above");
    if entry.status == MakerStatus::AwaitingDestination {
        entry.receiver = Some(hex::encode(d.receiver));
        entry.salt = Some(hex::encode(d.salt));
        entry.status = MakerStatus::DestinationReceived;
        book.save(&m.cfg.state)?;
        info!(order = %order_id, "destination received");
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "orderId": order_id, "status": "destination_received" }))))
}

async fn get_order(
    State(m): State<Arc<Maker>>,
    Path(order_id): Path<B256>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let book = m.book.lock().await;
    let rec = book.orders.get(&order_id).ok_or(ApiError::UNKNOWN_ORDER)?;
    Ok(Json(json!({
        "orderId": rec.order_id,
        "side": rec.side,
        "status": rec.status,
        "zatoshi": rec.zatoshi.to_string(),
        "usdc": rec.usdc.to_string(),
        "payDeadline": rec.pay_deadline.to_string(),
        "note": rec.note,
    })))
}

/// O-7: what the maker can still fill: ZEC (the last scan's spendable balance less what its open
/// Buy orders reserved) and USDC (its balance within its allowance to ZecuredSwap, which Sell orders
/// lock), in zatoshi and USDC units.
pub async fn inventory(m: &Maker) -> anyhow::Result<(u64, U256)> {
    let reserved = m.book.lock().await.reserved_zatoshi();
    let zec_free = m.balance.load(Ordering::Relaxed).saturating_sub(reserved);
    let me = m.signer.address();
    let usdc = IERC20::new(m.swap().usdc().call().await?, &m.provider);
    let balance = usdc.balanceOf(me).call().await?;
    let allowance = usdc.allowance(me, m.cfg.swap).call().await?;
    Ok((zec_free, balance.min(allowance)))
}

/// O-7: the operators' local endpoint, served on `status_bind` (a loopback address) only:
/// `GET /inventory` answers `{ "zecFreeZat", "usdcFree" }` (decimal strings). Never part of
/// [`router`], so neither the public API nor the backend's `/swap/*` proxy can reach it.
pub fn status_router(maker: Arc<Maker>) -> Router {
    Router::new().route("/inventory", get(get_inventory)).with_state(maker)
}

async fn get_inventory(State(m): State<Arc<Maker>>) -> Result<Json<serde_json::Value>, ApiError> {
    let (zec_free, usdc_free) = inventory(&m).await?;
    Ok(Json(json!({ "zecFreeZat": zec_free.to_string(), "usdcFree": usdc_free.to_string() })))
}

async fn get_status(State(m): State<Arc<Maker>>) -> Json<serde_json::Value> {
    let open = m.book.lock().await.open_orders().count();
    Json(json!({
        "maker": m.signer.address(),
        "swap": m.cfg.swap,
        "chainId": m.cfg.chain_id,
        "sides": m.cfg.sides,
        "payWindow": m.cfg.pay_window_secs,
        "minOrderZat": m.cfg.min_order_zat.to_string(),
        "maxOrderZat": m.cfg.max_order_zat.min(m.limits.max_order_zat).to_string(),
        "maxOrderUsdc": U256::from(m.cfg.max_order_usdc).min(m.limits.max_order_usdc).to_string(),
        "spreadBps": m.cfg.spread_bps,
        "inviteOnly": m.cfg.invite_only,
        "openOrders": open,
        "maxOpenOrders": m.cfg.max_open_orders,
    }))
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::Address,
        providers::{Provider, ProviderBuilder},
        signers::local::PrivateKeySigner,
        sol_types::SolCall,
    };
    use axum::body::Body;
    use orchard::keys::{FullViewingKey, SpendingKey};
    use tower::ServiceExt;
    use zecured_swap::abi::IZecuredSwap;

    use super::*;
    use crate::{
        config::tests::judge_like,
        engine::{Wallet, new_record},
        quoting::ChainLimits,
        state::Book,
    };

    /// A Base RPC stub answering the three reads `inventory` makes, by function selector.
    async fn base_stub(usdc: Address) -> String {
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| async move {
                let call = &req["params"][0];
                let data = call["input"].as_str().or(call["data"].as_str()).unwrap_or_default();
                let selector = hex::decode(data.trim_start_matches("0x").get(..8).unwrap_or_default()).unwrap();
                let word = |v: U256| format!("0x{}", hex::encode(v.to_be_bytes::<32>()));
                let result = if selector == IZecuredSwap::usdcCall::SELECTOR {
                    word(U256::from_be_slice(usdc.as_slice()))
                } else if selector == IERC20::balanceOfCall::SELECTOR {
                    word(U256::from(250_000_000u64))
                } else if selector == IERC20::allowanceCall::SELECTOR {
                    word(U256::from(200_000_000u64))
                } else {
                    "0x14a34".to_owned()
                };
                Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn wallet() -> Wallet {
        let sk = (1u8..).find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes([b; 32]))).unwrap();
        Wallet {
            fvk: FullViewingKey::from(&sk),
            sk,
            birthday: 1,
            scan: None,
        }
    }

    /// O-7: the operators' endpoint answers the free ZEC (the scanned balance less what open Buy
    /// orders reserved) and the lockable USDC (balance within the allowance); the public API never
    /// serves it.
    #[tokio::test]
    async fn the_inventory_is_served_on_the_status_router_only() {
        let rpc = base_stub(Address::repeat_byte(0x55)).await;
        let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap()).erased();
        let cfg = judge_like();
        let mut book = Book::default();
        let order = B256::repeat_byte(1);
        book.orders.insert(
            order,
            new_record(
                order,
                Side::Buy,
                Address::repeat_byte(2),
                30_000_000,
                U256::from(1u64),
                2_000_000_000,
                B256::repeat_byte(3),
            ),
        );
        let limits = ChainLimits {
            max_order_usdc: U256::from(1u64),
            max_order_zat: 1,
            min_pay_window: 1,
            max_pay_window: 2,
        };
        let domain = zecured_swap::quote::swap_domain(cfg.chain_id, cfg.swap);
        let maker = Arc::new(Maker::new(
            cfg,
            zecured_zcash_ops::network::ZcashNetwork::Testnet,
            PrivateKeySigner::random(),
            provider,
            domain,
            limits,
            book,
            wallet(),
        ));
        maker.balance.store(100_000_000, Ordering::Relaxed);
        let get = |app: Router| async move {
            let r = app.oneshot(axum::http::Request::get("/inventory").body(Body::empty()).unwrap()).await.unwrap();
            let status = r.status();
            let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default())
        };
        let (status, body) = get(status_router(maker.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "zecFreeZat": "70000000", "usdcFree": "200000000" }));
        let (status, _) = get(router(maker)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "never on the public API");
    }
}
