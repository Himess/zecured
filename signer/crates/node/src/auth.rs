//! Authentication of coordinator requests to a signer node (D16).
//!
//! The coordinator signs `keccak256("zecured-node-api" || method || path || sha256(body) ||
//! timestamp)` with its key; a node accepts a request only if the signature recovers to the
//! configured coordinator address, the timestamp is within the allowed skew and the same
//! request was not accepted before (a replay cache covers the skew window, L-1). This keeps
//! strangers away from the node API once nodes run on the open internet; authorization of
//! anything that moves value still rests on each node's own policy checks.

use std::{collections::HashMap, sync::Mutex};

use alloy::{
    primitives::{Address, B256, keccak256},
    signers::{Signature, SignerSync, local::PrivateKeySigner},
};
use anyhow::{Context, Result, anyhow, ensure};
use sha2::{Digest, Sha256};

/// Header carrying the Unix timestamp of the request.
pub const TS_HEADER: &str = "x-zecured-ts";
/// Header carrying the 65-byte signature, hex.
pub const SIG_HEADER: &str = "x-zecured-sig";
/// Maximum accepted clock difference, in seconds.
pub const MAX_SKEW_SECS: u64 = 60;

fn digest(method: &str, path: &str, body: &[u8], ts: u64) -> B256 {
    let body_hash = Sha256::digest(body);
    let mut buf = b"zecured-node-api".to_vec();
    buf.extend_from_slice(method.as_bytes());
    buf.push(0);
    buf.extend_from_slice(path.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&body_hash);
    buf.extend_from_slice(&ts.to_be_bytes());
    keccak256(buf)
}

/// Signs a request; returns the header values `(timestamp, signature hex)`.
pub fn sign(key: &PrivateKeySigner, method: &str, path: &str, body: &[u8], ts: u64) -> Result<(String, String)> {
    let sig = key.sign_hash_sync(&digest(method, path, body, ts)).map_err(|e| anyhow!("sign request: {e}"))?;
    Ok((ts.to_string(), hex::encode(sig.as_bytes())))
}

/// Verifies a request signed by `expected` at a time within the allowed skew of `now`.
pub fn verify(expected: Address, method: &str, path: &str, body: &[u8], ts: &str, sig: &str, now: u64) -> Result<()> {
    verify_signed(expected, method, path, body, ts, sig, now).map(|_| ())
}

/// Like [`verify`]; returns the request's digest and timestamp, for the [`ReplayCache`].
pub fn verify_signed(
    expected: Address,
    method: &str,
    path: &str,
    body: &[u8],
    ts: &str,
    sig: &str,
    now: u64,
) -> Result<(B256, u64)> {
    let ts: u64 = ts.parse().context("bad timestamp")?;
    ensure!(ts.abs_diff(now) <= MAX_SKEW_SECS, "stale request");
    let bytes = hex::decode(sig.trim_start_matches("0x")).context("bad signature encoding")?;
    let sig = Signature::try_from(bytes.as_slice()).context("bad signature")?;
    let digest = digest(method, path, body, ts);
    let signer = sig.recover_address_from_prehash(&digest).context("cannot recover signer")?;
    ensure!(signer == expected, "request not signed by the coordinator");
    Ok((digest, ts))
}

/// Signed requests already accepted (L-1). Within the allowed clock skew a captured request
/// would otherwise verify again, e.g. a replayed `/v1/sign/commit` overwriting a signing
/// session. Keyed by the signed digest, so a re-encoded signature is a replay too.
#[derive(Default)]
pub struct ReplayCache {
    seen: Mutex<HashMap<B256, u64>>,
}

impl ReplayCache {
    /// Requests remembered at most (only the coordinator can add any).
    const MAX_ENTRIES: usize = 100_000;

    /// Accepts the request `digest` signed at `ts` once; a second time it is a replay.
    pub fn check(&self, digest: B256, ts: u64, now: u64) -> Result<()> {
        let mut seen = self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // A request whose timestamp left the window can no longer verify: forget it.
        seen.retain(|_, t| t.abs_diff(now) <= MAX_SKEW_SECS);
        ensure!(!seen.contains_key(&digest), "replayed request");
        ensure!(seen.len() < Self::MAX_ENTRIES, "too many requests");
        seen.insert(digest, ts);
        Ok(())
    }
}

/// Compares a secret token in constant time (over SHA-256 digests, so neither the content nor
/// the length of the expected token leaks through timing).
pub fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (Sha256::digest(expected.as_bytes()), Sha256::digest(given.as_bytes()));
    let diff = a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| std::hint::black_box(acc | (x ^ y)));
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_requests_verify_and_tampering_fails() {
        let key = PrivateKeySigner::random();
        let (ts, sig) = sign(&key, "POST", "/v1/sign/commit", b"{\"a\":1}", 1_000).unwrap();
        verify(key.address(), "POST", "/v1/sign/commit", b"{\"a\":1}", &ts, &sig, 1_010).unwrap();
        assert!(verify(key.address(), "POST", "/v1/sign/commit", b"{\"a\":2}", &ts, &sig, 1_010).is_err());
        assert!(verify(key.address(), "POST", "/v1/sign/share", b"{\"a\":1}", &ts, &sig, 1_010).is_err());
        assert!(verify(key.address(), "POST", "/v1/sign/commit", b"{\"a\":1}", &ts, &sig, 2_000).is_err());
        assert!(verify(Address::repeat_byte(1), "POST", "/v1/sign/commit", b"{\"a\":1}", &ts, &sig, 1_010).is_err());
    }

    #[test]
    fn a_signed_request_is_accepted_once() {
        let key = PrivateKeySigner::random();
        let cache = ReplayCache::default();
        let (ts, sig) = sign(&key, "POST", "/v1/sign/commit", b"{}", 1_000).unwrap();
        let (digest, t) = verify_signed(key.address(), "POST", "/v1/sign/commit", b"{}", &ts, &sig, 1_000).unwrap();
        cache.check(digest, t, 1_000).unwrap();
        // Replayed within the window: refused, however late in it.
        assert!(cache.check(digest, t, 1_001).is_err());
        assert!(cache.check(digest, t, 1_000 + MAX_SKEW_SECS).is_err());
        // Another request is fine; an entry is forgotten only once it could no longer verify.
        let (ts2, sig2) = sign(&key, "POST", "/v1/sign/commit", b"{}", 1_001).unwrap();
        let (digest2, t2) = verify_signed(key.address(), "POST", "/v1/sign/commit", b"{}", &ts2, &sig2, 1_001).unwrap();
        cache.check(digest2, t2, 1_001).unwrap();
        assert!(verify_signed(key.address(), "POST", "/v1/sign/commit", b"{}", &ts, &sig, 1_100).is_err());
        cache.check(digest, t, 1_000 + MAX_SKEW_SECS + 1).unwrap();
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(token_matches("secret-token", "secret-token"));
        assert!(!token_matches("secret-token", "secret-tokeN"));
        assert!(!token_matches("secret-token", "secret-token "));
        assert!(!token_matches("secret-token", ""));
    }
}
