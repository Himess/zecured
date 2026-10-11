//! Authentication of coordinator requests to a signer node (D16).
//!
//! The coordinator signs `keccak256("zecured-node-api" || method || path || sha256(body) ||
//! timestamp)` with its key; a node accepts a request only if the signature recovers to the
//! configured coordinator address and the timestamp is within the allowed skew. This keeps
//! strangers away from the node API once nodes run on the open internet; authorization of
//! anything that moves value still rests on each node's own policy checks.

use alloy::{
    primitives::{Address, B256, keccak256},
    signers::{Signature, SignerSync, local::PrivateKeySigner},
};
use anyhow::{Context, Result, anyhow, ensure};

/// Header carrying the Unix timestamp of the request.
pub const TS_HEADER: &str = "x-zecured-ts";
/// Header carrying the 65-byte signature, hex.
pub const SIG_HEADER: &str = "x-zecured-sig";
/// Maximum accepted clock difference, in seconds.
pub const MAX_SKEW_SECS: u64 = 60;

fn digest(method: &str, path: &str, body: &[u8], ts: u64) -> B256 {
    use sha2::{Digest, Sha256};
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
    let ts: u64 = ts.parse().context("bad timestamp")?;
    ensure!(ts.abs_diff(now) <= MAX_SKEW_SECS, "stale request");
    let bytes = hex::decode(sig.trim_start_matches("0x")).context("bad signature encoding")?;
    let sig = Signature::try_from(bytes.as_slice()).context("bad signature")?;
    let signer = sig.recover_address_from_prehash(&digest(method, path, body, ts)).context("cannot recover signer")?;
    ensure!(signer == expected, "request not signed by the coordinator");
    Ok(())
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
}
