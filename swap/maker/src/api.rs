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
