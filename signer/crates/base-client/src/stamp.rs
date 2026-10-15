//! Review H-01: the hashcash stamp of a vault opening.
//!
//! Opening a vault is free for its requester and costs the five signers a DKG, plus a slot among
//! the vaults waiting for a deposit. Each `POST /vaults` therefore carries a nonce whose stamp
//!
//! ```text
//! sha256("zecured-open-vault-v1" || requestId (32 bytes) || owner (20 bytes) || nonce (8 bytes, big-endian))
//! ```
//!
//! starts with the network's number of zero bits: a few seconds of work in a browser, one hash to
//! check. The stamp binds the request id and the owner, which the owner's `OpenVault` signature
//! binds to the rest of the request, and the coordinator accepts one stamp per request id.

use alloy::primitives::{Address, B256};
use sha2::{Digest, Sha256};

/// Domain separation of the stamp's preimage.
pub const DOMAIN: &[u8] = b"zecured-open-vault-v1";
/// Most zero bits a network may require (about 4 billion hashes on average).
pub const MAX_BITS: u32 = 32;
/// The bits a network requires unless its config says otherwise: about a million hashes on
/// average, a second or two in a desktop browser.
pub const DEFAULT_BITS: u32 = 20;

/// The stamp of `nonce` for the request `request_id` of `owner`.
pub fn digest(request_id: &B256, owner: &Address, nonce: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(request_id);
    h.update(owner);
    h.update(nonce.to_be_bytes());
    h.finalize().into()
}

/// Zero bits at the start of `d`, most significant bit first.
pub fn leading_zero_bits(d: &[u8; 32]) -> u32 {
    let mut bits = 0;
    for byte in d {
        bits += byte.leading_zeros();
        if *byte != 0 {
            break;
        }
    }
    bits
}

/// Whether `nonce` stamps the request with at least `bits` zero bits.
pub fn is_valid(request_id: &B256, owner: &Address, nonce: u64, bits: u32) -> bool {
    leading_zero_bits(&digest(request_id, owner, nonce)) >= bits
}

/// The first nonce from `start` that stamps the request with `bits` zero bits.
pub fn solve(request_id: &B256, owner: &Address, bits: u32, start: u64) -> u64 {
    let mut nonce = start;
    while !is_valid(request_id, owner, nonce, bits) {
        nonce = nonce.wrapping_add(1);
    }
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: B256 = B256::repeat_byte(0x33);
    const OWNER: Address = Address::repeat_byte(0x11);

    /// Cross-language vectors (app/lib/stamp.test.ts checks the same values; both were also checked
    /// with .NET's SHA-256 over the 81-byte preimage).
    #[test]
    fn stamp_vectors() {
        let mut preimage = b"zecured-open-vault-v1".to_vec();
        preimage.extend([0x33; 32]);
        preimage.extend([0x11; 20]);
        preimage.extend(7u64.to_be_bytes());
        assert_eq!(preimage.len(), 81);
        assert_eq!(digest(&REQUEST, &OWNER, 7), <[u8; 32]>::from(Sha256::digest(&preimage)));
        assert_eq!(
            hex::encode(digest(&REQUEST, &OWNER, 0)),
            "aa84c1fd702563a0fd72199fdac42b0b6b923e9a84ce8d52be66821802369c27"
        );
        // The first nonce with 12 zero bits (its stamp has 14: 0x0003638f...).
        assert_eq!(solve(&REQUEST, &OWNER, 12, 0), 8_729);
        assert_eq!(hex::encode(&digest(&REQUEST, &OWNER, 8_729)[..4]), "0003638f");
    }

    #[test]
    fn leading_zero_bits_count_from_the_first_byte() {
        let mut d = [0u8; 32];
        assert_eq!(leading_zero_bits(&d), 256);
        d[0] = 0x80;
        assert_eq!(leading_zero_bits(&d), 0);
        d[0] = 0x01;
        assert_eq!(leading_zero_bits(&d), 7);
        d[0] = 0;
        d[1] = 0x10;
        assert_eq!(leading_zero_bits(&d), 11);
        d[2] = 0xff;
        assert_eq!(leading_zero_bits(&d), 11, "later bytes do not count");
    }

    /// A solved stamp has the bits, binds its request id and owner, and 0 bits accept any nonce.
    #[test]
    fn a_stamp_binds_its_request_and_owner() {
        for bits in [0, 1, 8, 14] {
            let nonce = solve(&REQUEST, &OWNER, bits, 1_000);
            assert!(nonce >= 1_000 && is_valid(&REQUEST, &OWNER, nonce, bits));
            assert!(leading_zero_bits(&digest(&REQUEST, &OWNER, nonce)) >= bits);
        }
        assert!(is_valid(&REQUEST, &OWNER, 12_345, 0));
        let nonce = solve(&REQUEST, &OWNER, 16, 0);
        let other_request = B256::repeat_byte(0x34);
        let other_owner = Address::repeat_byte(0x12);
        assert!(
            !is_valid(&other_request, &OWNER, nonce, 16) || !is_valid(&REQUEST, &other_owner, nonce, 16),
            "a stamp is (with overwhelming probability) worth nothing for another request or owner"
        );
    }
}
