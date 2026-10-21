//! The maker's HTTP API (served to the app through the backend's proxy, or directly):
//!
//! - `GET /swap/quote?side=buy|sell&zatoshi=N` (or `&usdc=N`, USDC units) `[&taker=0x..]`, header
//!   `X-Zecured-Invite: <code>` when the maker is invite-only: a signed `SwapQuote` for
//!   `ZecuredSwap.open`, with the order id it will have; on Sell also the maker's Ironwood receiver
//!   and salt, which the taker pays and proves against.
//! - `POST /swap/orders/{orderId}/destination` `{ receiver, salt }`: the taker's Ironwood receiver
//!   (a unified address, or the raw 43 bytes as 0x-hex) and the salt of the order's commitment.
//!   The commitment is on Base: a destination that does not open it is refused, so no
//!   authentication is needed. Never logged; erased once the order is done. 202 when taken; given
//!   again once taken, 200 from the book before anything is read from the chains (N-8).
//! - `GET /swap/orders/{orderId}`: the maker's view of one of its orders (no receiver, salt or
//!   txid).
//! - `GET /swap/status`: what the maker quotes now.
//!
//! And on the operators' loopback address only (`status_bind`, O-7), never through the public API
//! or the backend's proxy: `GET /inventory`, the maker's free ZEC and USDC for the watchdog.
//!
//! Errors are `{ "error": message, "code": code }` with fixed messages.

use std::sync::{Arc, atomic::Ordering};

use alloy::primitives::{Address, B256, U256};
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
    chain::ProofPath,
    quote::sign_quote,
};
use zecured_zcash_ops::keys::encode_ironwood_ua;

use crate::{
    engine::{DestinationFacts, Maker, accept_destination, destination_again},
    gate::{INVITE_HEADER, admitted},
    quoting::{Market, QuoteJson, QuoteRequest, quote, terms},
    state::{Book, GivenQuote, MakerStatus},
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
    /// W-19: under the code `busy`, which the backend's proxy maps.
    pub const TAKER_BUSY: Self =
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "This taker has as many open orders as the maker takes.");
    pub const RELAY_BEHIND: Self = Self::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "relay_behind",
        "The Zcash header relay lags: the pay window could not be met.",
    );
    pub const PRICE_STALE: Self = Self::new(StatusCode::SERVICE_UNAVAILABLE, "price_stale", "No fresh ZEC price.");
    pub const UNAVAILABLE: Self =
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "The maker cannot follow Zcash or Base right now.");
    /// W-7: under the code `unavailable`, which the backend's proxy maps.
    pub const PROOFS_STOPPED: Self = Self::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "ZecuredSwap takes no payment proof of the maker's guest now: the maker neither quotes nor pays.",
    );
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

/// What a quote needs from the chains: the price, Base's clock, whether a proof could land (W-7),
/// and the USDC the maker could lock.
struct ChainMarket {
    price: U256,
    price_age: Option<u64>,
    now: u64,
    proofs: ProofPath,
    usdc_free: U256,
}

/// The market as the quote needs it: the chains' part, and (N-10) the maker's places and inventory
/// less what its open orders and the quotes it gave hold.
fn market(m: &Maker, c: &ChainMarket, book: &Book, taker: Option<Address>) -> Market {
    let taker = taker.filter(|t| *t != Address::ZERO);
    Market {
        price: c.price,
        price_age: c.price_age,
        proofs: c.proofs,
        zcash_free: m.balance.load(Ordering::Relaxed).saturating_sub(book.reserved_zatoshi(c.now)),
        usdc_free: c.usdc_free.saturating_sub(book.quoted_usdc(c.now)),
        open_orders: book.places_taken(c.now),
        taker_open_orders: taker.map_or(0, |t| book.taker_places(t, c.now, None)),
    }
}

async fn chain_market(m: &Maker, side: Side) -> Result<ChainMarket, ApiError> {
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
    // W-7: the Swap version, read now; the relay's lag from the engine's last pass, which must be
    // recent (the maker follows both chains).
    let proofs = m.proof_path().await?;
    let me = m.signer.address();
    let usdc = IERC20::new(m.swap().usdc().call().await.map_err(anyhow::Error::from)?, &m.provider);
    let balance = usdc.balanceOf(me).call().await.map_err(anyhow::Error::from)?;
    let allowance = usdc.allowance(me, m.cfg.swap).call().await.map_err(anyhow::Error::from)?;
    Ok(ChainMarket {
        price,
        price_age,
        now,
        proofs,
        usdc_free: balance.min(allowance),
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
    let chain = chain_market(&m, side).await?;
    // A random nonce: unguessable order ids, and none reused across restarts.
    let nonce = U256::from_be_bytes(zecured_swap::random_bytes32());
    let order_id = zecured_swap::order_id(m.cfg.chain_id, m.cfg.swap, m.signer.address(), nonce);
    // N-10: decided and kept under one lock of the book: from here on the quote holds one of the
    // maker's places, and its ZEC or USDC, until its order is read from Base or it can no longer
    // be opened and read.
    let t = {
        let mut book = m.book.lock().await;
        book.prune_quotes(chain.now);
        let t = terms(&m.cfg, &m.limits, &market(&m, &chain, &book, req.taker), &req)?;
        let holds_until = chain.now.saturating_add(m.cfg.quote_ttl_secs).saturating_add(m.cfg.pass_stale_secs());
        let given = GivenQuote {
            side: t.side,
            taker: t.taker,
            zatoshi: t.zatoshi,
            usdc: t.usdc,
            holds_until,
        };
        book.quotes.insert(order_id, given);
        book.save(&m.cfg.state)?;
        t
    };
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
    let q = quote(&m.cfg, &t, m.signer.address(), chain.now, nonce, maker_receiver.as_ref().map(|r| r.2));
    let signature = sign_quote(&m.signer, &m.domain, &q)?;
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
    let taken = json!({ "orderId": order_id, "status": "destination_received" });
    // N-8: a destination given again once taken is acknowledged from the book (200), before
    // anything is read from the chains: neither a stale view of them nor a busy engine refuses it.
    let again = {
        let book = m.book.lock().await;
        let rec = book.orders.get(&order_id);
        rec.and_then(|rec| destination_again(&m.network, rec, &body.receiver, &body.salt))
    };
    if let Some(again) = again {
        again?;
        return Ok((StatusCode::OK, Json(taken)));
    }
    let now = m.now().await?;
    // W-7: whether a proof could land, read before the book is locked.
    let proofs = m.proof_path().await?;
    // An order opened since the engine's last pass is read from Base directly.
    let known = m.book.lock().await.orders.contains_key(&order_id);
    let fresh = if known {
        None
    } else {
        m.record_from_chain(order_id).await?
    };
    let mut book = m.book.lock().await;
    let balance = m.balance.load(Ordering::Relaxed);
    if let Some(rec) = fresh
        && !book.orders.contains_key(&order_id)
    {
        // N-10: recorded as the engine would have: in its quote's place, with its reservation.
        book.record_opened(rec, now, balance, &m.cfg);
    }
    let rec = book.orders.get(&order_id).ok_or(ApiError::UNKNOWN_ORDER)?.clone();
    // W-19, N-10: what the maker could still pay this order (its own reservation back in), and the
    // places taken, by the others and by this order's taker.
    let holds_place = rec.holds_place(now);
    let facts = DestinationFacts {
        now,
        proofs,
        zcash_free: balance.saturating_sub(book.reserved_zatoshi(now).saturating_sub(rec.reserved_zatoshi(now))),
        holds_place,
        places_taken: book.places_taken(now).saturating_sub(usize::from(holds_place)),
        taker_places: book.taker_places(rec.taker, now, Some(order_id)),
    };
    let d = accept_destination(&m.cfg, &m.network, &rec, &facts, &body.receiver, &body.salt)?;
    let entry = book.orders.get_mut(&order_id).expect("read above");
    if entry.status != MakerStatus::AwaitingDestination {
        // Taken by an earlier request meanwhile: this same destination (it opens the commitment).
        return Ok((StatusCode::OK, Json(taken)));
    }
    entry.receiver = Some(hex::encode(d.receiver));
    entry.salt = Some(hex::encode(d.salt));
    entry.status = MakerStatus::DestinationReceived;
    book.save(&m.cfg.state)?;
    info!(order = %order_id, "destination received");
    Ok((StatusCode::ACCEPTED, Json(taken)))
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
/// Buy orders and Buy quotes hold, N-10) and USDC (its balance within its allowance to
/// ZecuredSwap, which Sell orders lock, less what its Sell quotes would lock), in zatoshi and USDC
/// units.
pub async fn inventory(m: &Maker) -> anyhow::Result<(u64, U256)> {
    let now = m.now().await?;
    let (reserved, quoted) = {
        let book = m.book.lock().await;
        (book.reserved_zatoshi(now), book.quoted_usdc(now))
    };
    let zec_free = m.balance.load(Ordering::Relaxed).saturating_sub(reserved);
    let me = m.signer.address();
    let usdc = IERC20::new(m.swap().usdc().call().await?, &m.provider);
    let balance = usdc.balanceOf(me).call().await?;
    let allowance = usdc.allowance(me, m.cfg.swap).call().await?;
    Ok((zec_free, balance.min(allowance).saturating_sub(quoted)))
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
    // N-10: the places taken (orders holding one, quotes given), on Base's clock of the last pass.
    let open = m.book.lock().await.places_taken(m.last_pass_base_now());
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
        "maxOpenOrdersPerTaker": m.cfg.max_open_orders_per_taker,
    }))
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use axum::body::Body;
    use orchard::keys::{FullViewingKey, SpendingKey};
    use tower::ServiceExt;

    use super::*;
    use crate::{config::tests::judge_like, engine::new_record, state::Book};

    /// N-8: a destination given again once the maker took it is acknowledged from the book (200)
    /// before anything is read from the chains: a stale view of them (a long pass, a proof) never
    /// turns it into a refusal. One that does not open the order's commitment is still refused.
    #[tokio::test]
    async fn n8_a_destination_given_again_is_acknowledged_from_the_book() {
        use std::sync::Mutex;

        use crate::testkit::{Chain, base_stub, maker};
        let sk = (1u8..).find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes([b; 32]))).unwrap();
        let address = FullViewingKey::from(&sk).address_at(9u32, Scope::External);
        let ua = encode_ironwood_ua(&zcash_protocol::consensus::Network::TestNetwork, address).unwrap();
        let salt = [0x5a; 32];
        let id = B256::repeat_byte(7);
        let mut rec = new_record(
            id,
            Side::Buy,
            Address::repeat_byte(2),
            50_000_000,
            U256::from(20_000_000u64),
            1_000_000 + 21_600,
            zecured_swap::receiver_commitment(&salt, &address.to_raw_address_bytes()),
        );
        rec.status = MakerStatus::DestinationReceived;
        rec.receiver = Some(hex::encode(address.to_raw_address_bytes()));
        rec.salt = Some(hex::encode(salt));
        let mut book = Book::default();
        book.orders.insert(id, rec);
        let chain = Arc::new(Mutex::new(Chain::default()));
        // No pass yet: the maker's view of the chains is stale.
        let m = maker(&base_stub(chain.clone()).await, judge_like(), book);
        let post = |salt: [u8; 32]| {
            let body = json!({ "receiver": ua, "salt": hex::encode(salt) }).to_string();
            let app = router(m.clone());
            async move {
                let req = axum::http::Request::post(format!("/swap/orders/{id}/destination"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap();
                let r = app.oneshot(req).await.unwrap();
                let status = r.status();
                let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
                (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default())
            }
        };
        let (status, body) = post(salt).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({ "orderId": id, "status": "destination_received" }));
        let (status, body) = post([0x5b; 32]).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_destination")));
        assert_eq!(chain.lock().unwrap().calls, 0, "nothing read from the chains");
        // Paid since: still acknowledged.
        m.book.lock().await.orders.get_mut(&id).unwrap().status = MakerStatus::Paid;
        assert_eq!(post(salt).await.0, StatusCode::OK);
    }

    /// O-7: the operators' endpoint answers the free ZEC (the scanned balance less what open Buy
    /// orders and quotes hold) and the lockable USDC (balance within the allowance, less what Sell
    /// quotes would lock); the public API never serves it.
    #[tokio::test]
    async fn the_inventory_is_served_on_the_status_router_only() {
        use std::sync::Mutex;

        use crate::{
            state::GivenQuote,
            testkit::{Chain, base_stub, forget, maker},
        };
        let chain = Arc::new(Mutex::new(Chain {
            usdc_balance: U256::from(250_000_000u64),
            usdc_allowance: U256::from(200_000_000u64),
            ..Chain::default()
        }));
        let mut book = Book::default();
        let buy = |id: u8| {
            new_record(
                B256::repeat_byte(id),
                Side::Buy,
                Address::repeat_byte(2),
                30_000_000,
                U256::from(1u64),
                2_000_000_000,
                B256::repeat_byte(3),
            )
        };
        // One Buy order given its destination reserves its ZEC; one still without reserves none (W-19).
        let mut given = buy(1);
        given.status = MakerStatus::DestinationReceived;
        book.orders.insert(given.order_id, given);
        book.orders.insert(B256::repeat_byte(4), buy(4));
        let m = maker(&base_stub(chain).await, judge_like(), book);
        m.balance.store(100_000_000, Ordering::Relaxed);
        let get = |app: Router| async move {
            let r = app.oneshot(axum::http::Request::get("/inventory").body(Body::empty()).unwrap()).await.unwrap();
            let status = r.status();
            let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            (status, serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default())
        };
        let (status, body) = get(status_router(m.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "zecFreeZat": "70000000", "usdcFree": "200000000" }));
        // N-10: quotes given hold their ZEC or USDC.
        let quote = |side, zatoshi, usdc| GivenQuote {
            side,
            taker: Address::ZERO,
            zatoshi,
            usdc: U256::from(usdc),
            holds_until: 1_000_225,
        };
        let mut book = m.book.lock().await;
        book.quotes.insert(B256::repeat_byte(5), quote(Side::Buy, 20_000_000, 8_000_000u64));
        book.quotes.insert(B256::repeat_byte(6), quote(Side::Sell, 10_000_000, 3_900_000u64));
        drop(book);
        let (_, body) = get(status_router(m.clone())).await;
        assert_eq!(body, json!({ "zecFreeZat": "50000000", "usdcFree": "196100000" }));
        let (status, _) = get(router(m.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "never on the public API");
        forget(&m);
    }
}
