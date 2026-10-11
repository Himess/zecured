//! Choosing the signer signatures sent to the Registry (I6, M-1).
//!
//! The Registry reverts on any signature that is malformed, high-s, not from a signer or out
//! of order (`ZecuredRegistry._verifySignerSignatures`). So every signature a node returns is
//! checked against **that node's** configured attestation address first, and exactly
//! `MIN_SIGNERS` valid signatures are sent, sorted by strictly increasing signer address. One
//! misbehaving node can then neither make a submission revert nor pick the attested tag.

use std::collections::BTreeMap;

use alloy::{
    primitives::{Address, B256, Bytes, U256, uint},
    sol_types::{Eip712Domain, SolStruct},
};
use zecured_base::{DepositAttestation, recover_typed};
use zecured_node::{AttestResponse, ConfirmResponse, config::NodeInfo};

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

/// The deposit tag and exactly `need` sorted attestation signatures, from the answers that
/// sign the vault's own account and amount with a valid signature of the answering node.
/// Answers are grouped by tag; only a tag with `need` valid signatures is used.
pub fn attestation(
    nodes: &[NodeInfo],
    domain: &Eip712Domain,
    vault_id: B256,
    account: Address,
    amount: u64,
    answers: &[(u16, AttestResponse)],
    need: usize,
) -> Option<(B256, Vec<Bytes>)> {
    let mut by_tag: BTreeMap<B256, Vec<(Address, Bytes)>> = BTreeMap::new();
    for (index, a) in answers {
        let Some(eth) = node_address(nodes, *index) else {
            continue;
        };
        if a.index != *index || a.account != account || a.amount != amount {
            continue;
        }
        let msg = DepositAttestation {
            vaultId: vault_id,
            account,
            amount,
            depositTag: a.deposit_tag,
        };
        if signed_by(domain, &msg, &a.signature, eth) {
            by_tag.entry(a.deposit_tag).or_default().push((eth, a.signature.clone()));
        }
    }
    // With distinct signers, at most one tag can gather a threshold of the five.
    by_tag.into_iter().find_map(|(tag, sigs)| sorted_threshold(sigs, need).map(|s| (tag, s)))
}

/// Exactly `need` sorted signatures of `message` (a `PayoutConfirmation` or a
/// `VaultCancellation`), each checked against the answering node's configured address.
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
    use alloy::signers::local::PrivateKeySigner;
    use zecured_base::{PayoutConfirmation, claim_ids_hash, registry_domain, sign_typed};

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
    const ACCOUNT: Address = Address::repeat_byte(0x33);
    const AMOUNT: u64 = 1_000_000;

    fn attest(n: &Net, index: u16, signer: usize, tag: B256) -> (u16, AttestResponse) {
        let msg = DepositAttestation {
            vaultId: VAULT,
            account: ACCOUNT,
            amount: AMOUNT,
            depositTag: tag,
        };
        (
            index,
            AttestResponse {
                index,
                account: ACCOUNT,
                amount: AMOUNT,
                deposit_tag: tag,
                signature: sign_typed(&n.keys[signer], &n.domain, &msg).unwrap(),
            },
        )
    }

    fn signers_of(n: &Net, tag: B256, sigs: &[Bytes]) -> Vec<Address> {
        let msg = DepositAttestation {
            vaultId: VAULT,
            account: ACCOUNT,
            amount: AMOUNT,
            depositTag: tag,
        };
        sigs.iter().map(|s| recover_typed(&n.domain, &msg, s).unwrap()).collect()
    }

    #[test]
    fn exactly_three_sorted_signatures_are_sent() {
        let n = net();
        let tag = B256::repeat_byte(7);
        let answers: Vec<_> = (1..=5u16).map(|i| attest(&n, i, usize::from(i) - 1, tag)).collect();
        let (got_tag, sigs) = attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers, 3).unwrap();
        assert_eq!(got_tag, tag);
        assert_eq!(sigs.len(), 3, "never more than the threshold");
        let signers = signers_of(&n, tag, &sigs);
        assert!(signers.windows(2).all(|w| w[0] < w[1]), "strictly increasing signers");
    }

    /// M-1: a node answering first with its own tag cannot pick the tag, and a signature that
    /// does not recover to the answering node's address is never counted (nor sent).
    #[test]
    fn a_lying_or_foreign_signer_is_ignored() {
        let n = net();
        let tag = B256::repeat_byte(7);
        let answers = vec![
            attest(&n, 1, 0, B256::repeat_byte(9)), // node 1: bogus tag, answers first
            attest(&n, 2, 0, tag),                  // node 2 relays node 1's signature
            attest(&n, 3, 2, tag),
            attest(&n, 4, 3, tag),
        ];
        // Only nodes 3 and 4 are valid for the tag: below the threshold, nothing is sent.
        assert!(attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers, 3).is_none());
    }

    #[test]
    fn a_threshold_of_valid_matching_answers_is_enough() {
        let n = net();
        let tag = B256::repeat_byte(7);
        let answers = vec![
            attest(&n, 1, 0, B256::repeat_byte(9)), // first answer: wrong tag
            attest(&n, 2, 0, tag),                  // foreign signature
            attest(&n, 3, 2, tag),
            attest(&n, 4, 3, tag),
            attest(&n, 5, 4, tag),
        ];
        let (got_tag, sigs) = attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers, 3).unwrap();
        assert_eq!(got_tag, tag);
        let mut expected: Vec<Address> = n.keys[2..].iter().map(|k| k.address()).collect();
        expected.sort();
        assert_eq!(signers_of(&n, tag, &sigs), expected);
        // Without node 5 only two valid signatures remain: nothing is sent.
        assert!(attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers[..4], 3).is_none());
    }

    #[test]
    fn wrong_account_amount_or_encoding_is_refused() {
        let n = net();
        let tag = B256::repeat_byte(7);
        let mut answers: Vec<_> = (1..=3u16).map(|i| attest(&n, i, usize::from(i) - 1, tag)).collect();
        answers[0].1.account = Address::repeat_byte(0x44);
        assert!(attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers, 3).is_none());

        let mut answers: Vec<_> = (1..=3u16).map(|i| attest(&n, i, usize::from(i) - 1, tag)).collect();
        // v = 0 recovers off-chain but OpenZeppelin's ECDSA rejects it.
        let mut sig = answers[1].1.signature.to_vec();
        sig[64] -= 27;
        answers[1].1.signature = sig.into();
        assert!(attestation(&n.nodes, &n.domain, VAULT, ACCOUNT, AMOUNT, &answers, 3).is_none());
    }

    #[test]
    fn high_s_is_not_registry_compatible() {
        let n = net();
        let msg = PayoutConfirmation {
            vaultId: VAULT,
            claimIdsHash: claim_ids_hash(&[B256::repeat_byte(1)]),
        };
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
        let msg = PayoutConfirmation {
            vaultId: VAULT,
            claimIdsHash: claim_ids_hash(&[B256::repeat_byte(1), B256::repeat_byte(2)]),
        };
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
        let other = PayoutConfirmation {
            vaultId: VAULT,
            claimIdsHash: claim_ids_hash(&[B256::repeat_byte(1)]),
        };
        let wrong: Vec<_> = (1..=3u16)
            .map(|i| {
                let signature = sign_typed(&n.keys[usize::from(i) - 1], &n.domain, &other).unwrap();
                (i, ConfirmResponse { index: i, signature })
            })
            .collect();
        assert!(confirmation(&n.nodes, &n.domain, &msg, &wrong, 3).is_none());
    }
}
