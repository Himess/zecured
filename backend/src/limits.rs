//! Small in-memory rate limits (M-8): token buckets, one global, one per route group, and one
//! per key (UserOp sender) for sponsorship. Nothing here records client IPs.

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// `burst` requests at once, refilled at `burst` per `per`.
#[derive(Clone, Copy, Debug)]
pub struct Rate {
    pub burst: u32,
    pub per: Duration,
}

impl Rate {
    pub const fn new(burst: u32, per: Duration) -> Self {
        Self { burst, per }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn full(rate: Rate, now: Instant) -> Self {
        Self {
            tokens: f64::from(rate.burst),
            at: now,
        }
    }

    fn take(&mut self, rate: Rate, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        let refill = elapsed * f64::from(rate.burst) / rate.per.as_secs_f64().max(f64::EPSILON);
        self.tokens = (self.tokens + refill).min(f64::from(rate.burst));
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One shared token bucket.
pub struct TokenBucket {
    rate: Rate,
    state: Mutex<Bucket>,
}

impl TokenBucket {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            state: Mutex::new(Bucket::full(rate, Instant::now())),
        }
    }

    pub fn try_take(&self) -> bool {
        self.try_take_at(Instant::now())
    }

    pub fn try_take_at(&self, now: Instant) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).take(self.rate, now)
    }
}

/// A token bucket per key, with a bound on the number of keys tracked.
pub struct KeyedBuckets<K> {
    rate: Rate,
    max_keys: usize,
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K: Eq + Hash + Copy> KeyedBuckets<K> {
    pub fn new(rate: Rate, max_keys: usize) -> Self {
        Self {
            rate,
            max_keys,
            buckets: Mutex::default(),
        }
    }

    pub fn try_take(&self, key: K) -> bool {
        self.try_take_at(key, Instant::now())
    }

    pub fn try_take_at(&self, key: K, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if buckets.len() >= self.max_keys && !buckets.contains_key(&key) {
            // Forget keys idle for a whole period: their buckets are full again anyway.
            buckets.retain(|_, b| now.saturating_duration_since(b.at) < self.rate.per);
            if buckets.len() >= self.max_keys {
                return false; // memory stays bounded under a flood of new keys
            }
        }
        buckets.entry(key).or_insert_with(|| Bucket::full(self.rate, now)).take(self.rate, now)
    }
}

/// The `429` answer: fixed message and code.
pub fn too_many_requests() -> Response {
    let body = json!({ "error": "Too many requests. Try again later.", "code": "rate_limited" });
    (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response()
}

/// Middleware: takes one token from `bucket` or answers `429`.
pub async fn limit(State(bucket): State<Arc<TokenBucket>>, req: Request, next: Next) -> Response {
    if bucket.try_take() {
        next.run(req).await
    } else {
        too_many_requests()
    }
}

/// The backend's limits.
pub struct Limits {
    /// Every request.
    pub global: Arc<TokenBucket>,
    /// `POST /vaults`: a five-node DKG and a vault scanned until it expires. Taken by the handler,
    /// and only for a request whose stamp checks out (H-01).
    pub open_vault: Arc<TokenBucket>,
    /// `POST /recipients`: a reveal written on five nodes.
    pub recipients: Arc<TokenBucket>,
    /// Owner requests (destination, refund, viewing key): forwarded to five nodes.
    pub owner: Arc<TokenBucket>,
    /// Close quotes and close requests relayed to the market maker.
    pub market_maker: Arc<TokenBucket>,
    /// `/proxy/*` (Base RPC, bundler, paymaster).
    pub proxy: Arc<TokenBucket>,
}

impl Default for Limits {
    fn default() -> Self {
        let bucket = |burst, secs| Arc::new(TokenBucket::new(Rate::new(burst, Duration::from_secs(secs))));
        Self {
            global: bucket(400, 8),
            open_vault: bucket(3, 60),
            recipients: bucket(10, 300),
            owner: bucket(20, 60),
            market_maker: bucket(10, 60),
            proxy: bucket(200, 10),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, middleware, routing::get};
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn buckets_allow_a_burst_then_refill() {
        let b = TokenBucket::new(Rate::new(3, Duration::from_secs(30)));
        let t0 = Instant::now();
        assert!((0..3).all(|_| b.try_take_at(t0)));
        assert!(!b.try_take_at(t0));
        assert!(!b.try_take_at(t0 + Duration::from_secs(5)), "one token takes 10 s");
        assert!(b.try_take_at(t0 + Duration::from_secs(10)));
        assert!(!b.try_take_at(t0 + Duration::from_secs(10)));
        // Never more than the burst after a long pause.
        let later = t0 + Duration::from_secs(3_600);
        assert!((0..3).all(|_| b.try_take_at(later)));
        assert!(!b.try_take_at(later));
    }

    #[test]
    fn keyed_buckets_are_independent_and_bounded() {
        let k = KeyedBuckets::new(Rate::new(2, Duration::from_secs(60)), 2);
        let t0 = Instant::now();
        assert!(k.try_take_at(1u8, t0) && k.try_take_at(1u8, t0));
        assert!(!k.try_take_at(1u8, t0));
        assert!(k.try_take_at(2u8, t0), "another key has its own bucket");
        assert!(!k.try_take_at(3u8, t0), "no room for a third key while the others are active");
        assert!(k.try_take_at(3u8, t0 + Duration::from_secs(60)), "idle keys are forgotten");
    }

    #[tokio::test]
    async fn the_middleware_answers_429_with_a_fixed_message() {
        let bucket = Arc::new(TokenBucket::new(Rate::new(2, Duration::from_secs(3_600))));
        let app = Router::new()
            .route("/x", get(|| async { "ok" }))
            .route_layer(middleware::from_fn_with_state(bucket, limit));
        for _ in 0..2 {
            let r = app.clone().oneshot(axum::http::Request::get("/x").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
        let r = app.oneshot(axum::http::Request::get("/x").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "rate_limited");
    }
}
