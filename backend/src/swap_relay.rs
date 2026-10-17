//! Zecured Swap (the v4 design's section 5): the maker's quote and order service, relayed so the
//! browser never contacts the maker (its IP stays with us, as for the close-from-collateral market
//! maker). Only what the app needs passes, rebuilt from validated parameters:
//!
//! - `GET /swap/quote?side=buy&usdc=<units>|zatoshi=<zat>[&taker=<address>]` → the maker's
//!   `GET {SWAP_MAKER_URL}/swap/quote` (a signed `SwapQuote`; the app checks it against its pinned
//!   swap contract). The Sell side stays on the command line.
//! - `POST /swap/orders/:orderId/destination` `{ receiver, salt }` → the maker's same path: the
//!   Ironwood address and the salt behind the order's commitment, which the maker checks
//!   (`keccak256(salt || receiver) == C`) before it pays. The receiver is checked here first (an
//!   Ironwood receiver of the deployment's Zcash network); neither is ever logged.
//! - `GET /swap/orders/:orderId` → the maker's view of the order (`{ status }`).
//!
//! The paths, the query and the refusal codes are swap/maker's (`api.rs`). The maker's refusals
//! become fixed messages (L-2); nothing it says passes as text. On the mainnet beta the maker quotes
//! invitees only: the browser's `x-zecured-invite` header is passed on as it is, never logged.

use std::{sync::Arc, time::Duration};

use alloy::primitives::{Address, B256};
use axum::{
    Json,
    extract::{Path, Query, rejection::JsonRejection, rejection::PathRejection, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use zcash_protocol::consensus::Network;

/// The most USDC one quote is asked for (units): far above any order cap.
const MAX_USDC_UNITS: u128 = 1_000_000_000_000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// A fixed answer (L-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}

impl SwapError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self { status, code, message }
    }
}

impl IntoResponse for SwapError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response()
    }
}

pub const BAD_REQUEST: SwapError = SwapError::new(StatusCode::BAD_REQUEST, "bad_request", "Invalid request.");
pub const NOT_CONFIGURED: SwapError =
    SwapError::new(StatusCode::SERVICE_UNAVAILABLE, "not_configured", "The swap is not configured on this backend.");
pub const UNAVAILABLE: SwapError = SwapError::new(
    StatusCode::BAD_GATEWAY,
    "swap_maker_unavailable",
    "The swap's maker is unavailable. Try again later.",
);
pub const INVALID_RECEIVER: SwapError = SwapError::new(
    StatusCode::UNPROCESSABLE_ENTITY,
    "invalid_receiver",
    "Not an Ironwood address of this deployment's Zcash network.",
);

/// The header of the maker's invite code (swap/maker's `INVITE_HEADER`).
pub const INVITE_HEADER: &str = "x-zecured-invite";

/// The maker's refusal code (swap/maker `api.rs`), as a fixed message of ours.
pub fn maker_error(code: &str) -> SwapError {
    use StatusCode as S;
    let e = SwapError::new;
    match code {
        "bad_request" => BAD_REQUEST,
        "invite_required" => {
            e(S::FORBIDDEN, "invite_required", "The maker quotes invitees only: give your invite code.")
        }
        "side_disabled" => e(S::UNPROCESSABLE_ENTITY, "side_disabled", "The maker does not quote this side now."),
        "taker_required" => e(
            S::UNPROCESSABLE_ENTITY,
            "taker_required",
            "Connect your wallet first: the quote is bound to your address.",
        ),
        "too_small" => e(S::UNPROCESSABLE_ENTITY, "too_small", "That amount is below the maker's minimum."),
        "too_large" => e(S::UNPROCESSABLE_ENTITY, "too_large", "That amount is above the maker's maximum."),
        "no_inventory" => e(S::CONFLICT, "no_inventory", "The maker cannot fill this order now."),
        "busy" => e(S::SERVICE_UNAVAILABLE, "busy", "The maker has too many open orders. Try again later."),
        "relay_behind" => e(
            S::SERVICE_UNAVAILABLE,
            "relay_behind",
            "The Zcash header relay lags, so the maker cannot meet the pay window now. Try again later.",
        ),
        "price_stale" => {
            e(S::SERVICE_UNAVAILABLE, "price_stale", "The maker has no fresh ZEC price now. Try again later.")
        }
        "unavailable" => UNAVAILABLE,
        "unknown_order" => {
            e(S::NOT_FOUND, "unknown_order", "The maker does not know this order (yet). Try again in a minute.")
        }
        "not_buy" => e(S::CONFLICT, "not_buy", "Only a Buy order takes a destination."),
        "not_awaiting" => e(S::CONFLICT, "not_awaiting", "This order no longer takes a destination."),
        "too_late" => e(
            S::CONFLICT,
            "too_late",
            "Too late for the maker to pay and prove before the deadline: the order will be refunded.",
        ),
        "bad_destination" => e(
            S::UNPROCESSABLE_ENTITY,
            "bad_destination",
            "The address and salt do not open the order's commitment.",
        ),
        _ => e(S::BAD_GATEWAY, "swap_maker_refused", "The swap's maker refused the request."),
    }
}

/// The browser's invite code for the maker, if any: at most 128 visible ASCII characters.
fn invite(headers: &HeaderMap) -> Result<Option<String>, SwapError> {
    let Some(v) = headers.get(INVITE_HEADER) else {
        return Ok(None);
    };
    let code = v.to_str().map_err(|_| BAD_REQUEST)?.trim();
    if code.is_empty() {
        return Ok(None);
    }
    if code.len() > 128 || !code.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(BAD_REQUEST);
    }
    Ok(Some(code.to_owned()))
}

/// The relay: the maker's base URL (`SWAP_MAKER_URL`; `None` answers `not_configured`) and the
/// Zcash network receivers must belong to.
#[derive(Clone)]
pub struct SwapMaker {
    url: Option<Arc<String>>,
    http: reqwest::Client,
    network: Network,
}

impl SwapMaker {
    pub fn new(url: Option<String>, network: Network) -> anyhow::Result<Self> {
        Ok(Self {
            url: url.map(|u| Arc::new(u.trim_end_matches('/').to_owned())).filter(|u| !u.is_empty()),
            http: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            network,
        })
    }

    fn base(&self) -> Result<&str, SwapError> {
        self.url.as_deref().map(String::as_str).ok_or(NOT_CONFIGURED)
    }

    async fn forward(&self, req: reqwest::RequestBuilder) -> Result<Json<Value>, SwapError> {
        let resp = req.send().await.map_err(|_| UNAVAILABLE)?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|_| UNAVAILABLE)?;
        if !status.is_success() {
            return Err(maker_error(v.get("code").and_then(Value::as_str).unwrap_or_default()));
        }
        Ok(Json(v))
    }
}

#[derive(Deserialize)]
pub struct QuoteQuery {
    side: String,
    usdc: Option<String>,
    zatoshi: Option<String>,
    taker: Option<String>,
}

/// The maker's query, rebuilt from validated parameters (never the caller's raw query).
pub fn quote_query(q: &QuoteQuery) -> Result<String, SwapError> {
    if q.side != "buy" {
        return Err(BAD_REQUEST);
    }
    let mut out = String::from("side=buy");
    match (&q.usdc, &q.zatoshi) {
        (Some(u), None) => {
            let units: u128 = u.parse().map_err(|_| BAD_REQUEST)?;
            if units == 0 || units > MAX_USDC_UNITS {
                return Err(BAD_REQUEST);
            }
            out.push_str(&format!("&usdc={units}"));
        }
        (None, Some(z)) => {
            let zat: u64 = z.parse().map_err(|_| BAD_REQUEST)?;
            if zat == 0 {
                return Err(BAD_REQUEST);
            }
            out.push_str(&format!("&zatoshi={zat}"));
        }
        _ => return Err(BAD_REQUEST),
    }
    if let Some(t) = &q.taker {
        let taker: Address = t.parse().map_err(|_| BAD_REQUEST)?;
        out.push_str(&format!("&taker={taker}"));
    }
    Ok(out)
}

#[derive(Deserialize)]
pub struct DestinationBody {
    receiver: String,
    salt: String,
}

/// The destination as the maker gets it: an Ironwood receiver of `network` and a 32-byte salt.
pub fn check_destination(b: &DestinationBody, network: &Network) -> Result<Value, SwapError> {
    let salt = b.salt.strip_prefix("0x").ok_or(BAD_REQUEST)?;
    if salt.len() != 64 || !salt.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(BAD_REQUEST);
    }
    let receiver = b.receiver.trim();
    if receiver.is_empty() || receiver.len() > 1_000 {
        return Err(BAD_REQUEST);
    }
    zecured_zcash_ops::keys::ironwood_receiver(network, receiver).map_err(|_| INVALID_RECEIVER)?;
    Ok(json!({ "receiver": receiver, "salt": format!("0x{}", salt.to_ascii_lowercase()) }))
}

/// `GET /swap/quote`.
pub async fn quote(
    axum::Extension(m): axum::Extension<SwapMaker>,
    headers: HeaderMap,
    query: Result<Query<QuoteQuery>, QueryRejection>,
) -> Result<Json<Value>, SwapError> {
    let Query(q) = query.map_err(|_| BAD_REQUEST)?;
    let query = quote_query(&q)?;
    let invite = invite(&headers)?;
    let url = format!("{}/swap/quote?{query}", m.base()?);
    let mut req = m.http.get(url);
    if let Some(code) = invite {
        req = req.header(INVITE_HEADER, code);
    }
    m.forward(req).await
}

/// `POST /swap/orders/:orderId/destination`.
pub async fn destination(
    axum::Extension(m): axum::Extension<SwapMaker>,
    id: Result<Path<B256>, PathRejection>,
    body: Result<Json<DestinationBody>, JsonRejection>,
) -> Result<Json<Value>, SwapError> {
    let Path(order) = id.map_err(|_| BAD_REQUEST)?;
    let Json(b) = body.map_err(|_| BAD_REQUEST)?;
    let checked = check_destination(&b, &m.network)?;
    let url = format!("{}/swap/orders/{order}/destination", m.base()?);
    m.forward(m.http.post(url).json(&checked)).await
}

/// `GET /swap/orders/:orderId`.
pub async fn order(
    axum::Extension(m): axum::Extension<SwapMaker>,
    id: Result<Path<B256>, PathRejection>,
) -> Result<Json<Value>, SwapError> {
    let Path(order) = id.map_err(|_| BAD_REQUEST)?;
    let url = format!("{}/swap/orders/{order}", m.base()?);
    m.forward(m.http.get(url)).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{Router, extract::State, routing::get, routing::post};

    use super::*;

    fn q(side: &str, usdc: Option<&str>, zatoshi: Option<&str>, taker: Option<&str>) -> QuoteQuery {
        QuoteQuery {
            side: side.into(),
            usdc: usdc.map(Into::into),
            zatoshi: zatoshi.map(Into::into),
            taker: taker.map(Into::into),
        }
    }

    #[test]
    fn quotes_are_asked_for_with_validated_parameters_only() {
        assert_eq!(quote_query(&q("buy", Some("12000000"), None, None)).unwrap(), "side=buy&usdc=12000000");
        assert_eq!(
            quote_query(&q("buy", None, Some("10000000"), Some("0x3333333333333333333333333333333333333333"))).unwrap(),
            "side=buy&zatoshi=10000000&taker=0x3333333333333333333333333333333333333333"
        );
        for bad in [
            q("sell", Some("1"), None, None),
            q("buy", None, None, None),
            q("buy", Some("1"), Some("1"), None),
            q("buy", Some("0"), None, None),
            q("buy", Some("1000000000001"), None, None),
            q("buy", Some("1&admin=1"), None, None),
            q("buy", None, Some("0"), None),
            q("buy", Some("5"), None, Some("0x12")),
        ] {
            assert_eq!(quote_query(&bad), Err(BAD_REQUEST));
        }
    }

    /// The first ZIP 2005 use_qsk vector's Ironwood-only testnet address (app/lib/signers.test.ts).
    const TESTNET_UA: &str = "utest1yv0hq77h4th2c882kdmcgt47xrm5zmhkfnfpw6dcack8e6kgfmg5new7vxed0trhkc67cmf2tjea8pnd09key4v85kmqdnz33yuas40f";

    #[test]
    fn destinations_need_an_ironwood_receiver_of_the_network_and_a_salt() {
        let salt = format!("0x{}", "5A".repeat(32));
        let ok = check_destination(
            &DestinationBody {
                receiver: format!(" {TESTNET_UA} "),
                salt: salt.clone(),
            },
            &Network::TestNetwork,
        )
        .unwrap();
        assert_eq!(ok, json!({ "receiver": TESTNET_UA, "salt": format!("0x{}", "5a".repeat(32)) }));
        assert_eq!(
            check_destination(
                &DestinationBody {
                    receiver: TESTNET_UA.into(),
                    salt: salt.clone()
                },
                &Network::MainNetwork
            ),
            Err(INVALID_RECEIVER)
        );
        assert_eq!(
            check_destination(
                &DestinationBody {
                    receiver: "utest1nope".into(),
                    salt: salt.clone()
                },
                &Network::TestNetwork
            ),
            Err(INVALID_RECEIVER)
        );
        for bad_salt in [
            "5a".repeat(32),
            format!("0x{}", "5a".repeat(31)),
            format!("0x{}", "zz".repeat(32)),
        ] {
            assert_eq!(
                check_destination(
                    &DestinationBody {
                        receiver: TESTNET_UA.into(),
                        salt: bad_salt
                    },
                    &Network::TestNetwork
                ),
                Err(BAD_REQUEST)
            );
        }
    }

    /// swap/maker's refusal codes (`api.rs`), each a fixed message of ours; anything else is one code.
    #[test]
    fn the_makers_refusals_are_fixed_messages() {
        for (code, status) in [
            ("invite_required", StatusCode::FORBIDDEN),
            ("side_disabled", StatusCode::UNPROCESSABLE_ENTITY),
            ("taker_required", StatusCode::UNPROCESSABLE_ENTITY),
            ("too_small", StatusCode::UNPROCESSABLE_ENTITY),
            ("too_large", StatusCode::UNPROCESSABLE_ENTITY),
            ("no_inventory", StatusCode::CONFLICT),
            ("busy", StatusCode::SERVICE_UNAVAILABLE),
            ("relay_behind", StatusCode::SERVICE_UNAVAILABLE),
            ("price_stale", StatusCode::SERVICE_UNAVAILABLE),
            ("unknown_order", StatusCode::NOT_FOUND),
            ("not_buy", StatusCode::CONFLICT),
            ("not_awaiting", StatusCode::CONFLICT),
            ("too_late", StatusCode::CONFLICT),
            ("bad_destination", StatusCode::UNPROCESSABLE_ENTITY),
        ] {
            assert_eq!((maker_error(code).code, maker_error(code).status), (code, status));
        }
        assert_eq!(maker_error("unavailable"), UNAVAILABLE);
        assert_eq!(maker_error("bad_request"), BAD_REQUEST);
        assert_eq!(maker_error("panic at maker.rs:12 with 10.0.0.7").code, "swap_maker_refused");
    }

    #[test]
    fn invite_codes_are_passed_on_only_when_they_look_like_one() {
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(INVITE_HEADER, v.parse().unwrap());
            h
        };
        assert_eq!(invite(&HeaderMap::new()), Ok(None));
        assert_eq!(invite(&with("  ")), Ok(None));
        assert_eq!(invite(&with(" beta-7f3k ")), Ok(Some("beta-7f3k".into())));
        assert_eq!(invite(&with("two words")), Err(BAD_REQUEST));
        assert_eq!(invite(&with(&"x".repeat(129))), Err(BAD_REQUEST));
    }

    /// A maker stub recording what reached it (the invite header too).
    async fn serve(seen: Arc<Mutex<Vec<String>>>) -> String {
        async fn quote(
            State(seen): State<Arc<Mutex<Vec<String>>>>,
            headers: HeaderMap,
            uri: axum::http::Uri,
        ) -> Json<Value> {
            let code = headers.get(INVITE_HEADER).map(|v| v.to_str().unwrap().to_owned());
            seen.lock().unwrap().push(format!("{uri} invite={code:?}"));
            Json(
                json!({ "quote": { "side": 0 }, "signature": "0x12", "swap": "0x7777777777777777777777777777777777777777" }),
            )
        }
        async fn dest(State(seen): State<Arc<Mutex<Vec<String>>>>, Json(b): Json<Value>) -> (StatusCode, Json<Value>) {
            seen.lock().unwrap().push(b.to_string());
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "code": "bad_destination", "error": "C mismatch for 10.0.0.7" })),
            )
        }
        let app = Router::new()
            .route("/swap/quote", get(quote))
            .route("/swap/orders/{id}/destination", post(dest))
            .with_state(seen);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn relays_rebuilt_requests_and_maps_refusals() {
        let seen = Arc::new(Mutex::new(vec![]));
        let url = serve(seen.clone()).await;
        let m = SwapMaker::new(Some(format!("{url}/")), Network::TestNetwork).unwrap();
        let none = HeaderMap::new();
        let r = quote(axum::Extension(m.clone()), none.clone(), Ok(Query(q("buy", Some("25000000"), None, None))))
            .await
            .unwrap();
        assert_eq!(r.0["swap"], "0x7777777777777777777777777777777777777777");
        let mut invited = HeaderMap::new();
        invited.insert(INVITE_HEADER, "beta-7f3k".parse().unwrap());
        let r = quote(axum::Extension(m.clone()), invited, Ok(Query(q("buy", Some("1"), None, None))))
            .await
            .unwrap();
        assert_eq!(r.0["signature"], "0x12");
        let body = DestinationBody {
            receiver: TESTNET_UA.into(),
            salt: format!("0x{}", "5a".repeat(32)),
        };
        let e = destination(axum::Extension(m.clone()), Ok(Path(B256::repeat_byte(0x0d))), Ok(Json(body)))
            .await
            .unwrap_err();
        assert_eq!(e.code, "bad_destination");
        assert_eq!(e.message, "The address and salt do not open the order's commitment.");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0], "/swap/quote?side=buy&usdc=25000000 invite=None");
        assert_eq!(seen[1], "/swap/quote?side=buy&usdc=1 invite=Some(\"beta-7f3k\")");
        assert!(seen[2].contains(TESTNET_UA) && seen[2].contains(&"5a".repeat(32)));
        let off = SwapMaker::new(None, Network::TestNetwork).unwrap();
        assert_eq!(
            quote(axum::Extension(off), none, Ok(Query(q("buy", Some("1"), None, None)))).await.unwrap_err(),
            NOT_CONFIGURED
        );
        let dead = SwapMaker::new(Some("http://127.0.0.1:9".into()), Network::TestNetwork).unwrap();
        assert_eq!(order(axum::Extension(dead), Ok(Path(B256::ZERO))).await.unwrap_err(), UNAVAILABLE);
    }
}
