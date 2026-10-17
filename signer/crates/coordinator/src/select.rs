//! Choosing the signer signatures sent to the Registry (I6, M-1).
//!
//! The Registry reverts on any signature that is malformed, high-s, not from a signer or out
//! of order (`ZecuredRegistry._verifySignerSignatures`). So every signature a node returns is
//! checked against **that node's** configured attestation address first, and exactly
//! `MIN_SIGNERS` valid signatures are sent, sorted by strictly increasing signer address. One
//! misbehaving node can then not make a submission revert.

use alloy::{
    primitives::{Address, Bytes, U256, uint},
    sol_types::{Eip712Domain, SolStruct},
};
use zecured_base::recover_typed;
use zecured_node::{ConfirmResponse, config::NodeInfo};

/// secp256k1 n / 2: OpenZeppelin's `ECDSA` rejects larger `s` (signature malleability).
const HALF_N: U256 = uint!(0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0_U256);

/// True if OpenZeppelin's `ECDSA.recover` accepts this encoding: 65 bytes `r || s || v`,
/// `v` = 27 or 28, `0 < s <= n/2`, `r != 0`.
pub fn registry_encoding_ok(sig: &[u8]) -> bool {
    if sig.len() != 65 || !(sig[64] == 27 || sig[64] == 28) {
        return false;
    }
    let r = U256::from_be_slice(&sig[..32]);
    let s = U256::from_be_slice(&sig[32..64]);
    !r.is_zero() && !s.is_zero() && s <= HALF_N
}

/// `sig` is a Registry-acceptable signature of `message` under `domain` by `expected`.
pub fn signed_by<T: SolStruct>(domain: &Eip712Domain, message: &T, sig: &[u8], expected: Address) -> bool {
    registry_encoding_ok(sig) && recover_typed(domain, message, sig).is_ok_and(|a| a == expected)
}

/// Exactly `need` signatures by distinct signers, in strictly increasing signer order.
pub fn sorted_threshold(mut valid: Vec<(Address, Bytes)>, need: usize) -> Option<Vec<Bytes>> {
    valid.sort_by_key(|(a, _)| *a);
    valid.dedup_by_key(|(a, _)| *a);
    (valid.len() >= need).then(|| valid.into_iter().take(need).map(|(_, s)| s).collect())
}

fn node_address(nodes: &[NodeInfo], index: u16) -> Option<Address> {
    nodes.iter().find(|n| n.index == index).map(|n| n.eth)
}

/// Exactly `need` sorted signatures of `message` (a `PayoutConfirmation`), each checked against
/// the answering node's configured address.
pub fn confirmation<T: SolStruct>(
    nodes: &[NodeInfo],
    domain: &Eip712Domain,
    message: &T,
    answers: &[(u16, ConfirmResponse)],
    need: usize,
) -> Option<Vec<Bytes>> {
    let valid = answers
        .iter()
        .filter(|(index, c)| c.index == *index)
        .filter_map(|(index, c)| {
            let eth = node_address(nodes, *index)?;
            signed_by(domain, message, &c.signature, eth).then(|| (eth, c.signature.clone()))
        })
        .collect();
    sorted_threshold(valid, need)
}

#[cfg(test)]
mod tests {
    use alloy::{primitives::B256, signers::local::PrivateKeySigner};
    use zecured_base::{payout_confirmation, registry_domain, sign_typed};

    use super::*;

    struct Net {
        keys: Vec<PrivateKeySigner>,
        nodes: Vec<NodeInfo>,
        domain: Eip712Domain,
    }

    fn net() -> Net {
        let keys: Vec<PrivateKeySigner> = (0..5).map(|_| PrivateKeySigner::random()).collect();
        let nodes = keys
            .iter()
            .enumerate()
            .map(|(k, key)| NodeInfo {
                index: u16::try_from(k + 1).unwrap(),
                url: String::new(),
                x25519: String::new(),
                eth: key.address(),
            })
            .collect();
        Net {
            keys,
            nodes,
            domain: registry_domain(84_532, Address::repeat_byte(0x11)),
        }
    }

    const VAULT: B256 = B256::repeat_byte(0x55);

    /// Exactly three signatures are sent, by distinct signers in strictly increasing order.
    #[test]
    fn exactly_three_sorted_signatures_are_sent() {
        let n = net();
        let msg = payout_confirmation(VAULT, B256::repeat_byte(0xaa), &[B256::repeat_byte(1)]);
        let answers: Vec<_> = (1..=5u16)
            .map(|i| {
                let signature = sign_typed(&n.keys[usize::from(i) - 1], &n.domain, &msg).unwrap();
                (i, ConfirmResponse { index: i, signature })
            })
            .collect();
        let sigs = confirmation(&n.nodes, &n.domain, &msg, &answers, 3).unwrap();
        assert_eq!(sigs.len(), 3, "never more than the threshold");
        let signers: Vec<Address> = sigs.iter().map(|s| recover_typed(&n.domain, &msg, s).unwrap()).collect();
        assert!(signers.windows(2).all(|w| w[0] < w[1]), "strictly increasing signers");
    }

    #[test]
    fn high_s_is_not_registry_compatible() {
        let n = net();
        let msg = payout_confirmation(VAULT, B256::repeat_byte(0xaa), &[B256::repeat_byte(1)]);
        let sig = sign_typed(&n.keys[0], &n.domain, &msg).unwrap();
        assert!(registry_encoding_ok(&sig));
        // (r, n - s, v ^ 1) is the same signature in its high-s form.
        let order: U256 = uint!(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141_U256);
        let s = U256::from_be_slice(&sig[32..64]);
        let mut high = sig.to_vec();
        high[32..64].copy_from_slice(&(order - s).to_be_bytes::<32>());
        high[64] = if sig[64] == 27 { 28 } else { 27 };
        assert!(!registry_encoding_ok(&high));
    }

    #[test]
    fn confirmations_are_checked_per_node() {
        let n = net();
        let msg = payout_confirmation(VAULT, B256::repeat_byte(0xaa), &[B256::repeat_byte(1), B256::repeat_byte(2)]);
        let answer = |index: u16, signer: usize| {
            (
                index,
                ConfirmResponse {
                    index,
                    signature: sign_typed(&n.keys[signer], &n.domain, &msg).unwrap(),
                },
            )
        };
        let all: Vec<_> = (1..=5u16).map(|i| answer(i, usize::from(i) - 1)).collect();
        let sigs = confirmation(&n.nodes, &n.domain, &msg, &all, 3).unwrap();
        assert_eq!(sigs.len(), 3);
        // Nodes 1-3 answer, node 2 with node 5's signature: only two are valid.
        let bad = vec![answer(1, 0), answer(2, 4), answer(3, 2)];
        assert!(confirmation(&n.nodes, &n.domain, &msg, &bad, 3).is_none());
        // A signature over other claims never counts.
        let other = payout_confirmation(VAULT, B256::repeat_byte(0xaa), &[B256::repeat_byte(1)]);
        let wrong: Vec<_> = (1..=3u16)
            .map(|i| {
                let signature = sign_typed(&n.keys[usize::from(i) - 1], &n.domain, &other).unwrap();
                (i, ConfirmResponse { index: i, signature })
            })
            .collect();
        assert!(confirmation(&n.nodes, &n.domain, &msg, &wrong, 3).is_none());
        // R-1: nor a signature of the same claims for another payout (a replay after a reopening).
        let replay = payout_confirmation(VAULT, B256::repeat_byte(0xab), &[B256::repeat_byte(1), B256::repeat_byte(2)]);
        let replayed: Vec<_> = (1..=3u16)
            .map(|i| {
                let signature = sign_typed(&n.keys[usize::from(i) - 1], &n.domain, &replay).unwrap();
                (i, ConfirmResponse { index: i, signature })
            })
            .collect();
        assert!(confirmation(&n.nodes, &n.domain, &msg, &replayed, 3).is_none());
    }
}
