//! Deterministic derivations every signer (and the coordinator, where it can) agrees on.
//!
//! All use BLAKE2b with a 16-byte personalization so their outputs are domain-separated.

use crate::classify::NotePosition;

const SK_COMMIT: &[u8; 16] = b"Zecured_SkCommit";
const VAULT_SK: &[u8; 16] = b"Zecured_VaultSk_";
const TAG_KEY: &[u8; 16] = b"Zecured_TagKey__";
const DEPOSIT_TAG: &[u8; 16] = b"Zecured_DepTag__";
const NOTE_ID: &[u8; 16] = b"Zecured_NoteId__";

fn blake2b_256(personal: &[u8; 16], key: Option<&[u8; 32]>, parts: &[&[u8]]) -> [u8; 32] {
    let mut params = blake2b_simd::Params::new();
    params.hash_length(32).personal(personal);
    if let Some(k) = key {
        params.key(k);
    }
    let mut state = params.to_state();
    for p in parts {
        state.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(state.finalize().as_bytes());
    out
}

/// Commitment a signer publishes in DKG round 1 to its secret contribution `r` to the
/// vault's ZIP 2005 `sk` (revealed only to the other signers in round 2).
pub fn sk_commitment(vault_id: &[u8; 32], contribution: &[u8; 32]) -> [u8; 32] {
    blake2b_256(SK_COMMIT, None, &[vault_id, contribution])
}

/// The vault's ZIP 2005 `sk`: a hash of every signer's contribution, in signer order. No
/// single signer (and not the coordinator) can choose it.
pub fn vault_sk(vault_id: &[u8; 32], contributions_in_signer_order: &[[u8; 32]]) -> [u8; 32] {
    let mut parts: Vec<&[u8]> = vec![vault_id];
    for c in contributions_in_signer_order {
        parts.push(c);
    }
    blake2b_256(VAULT_SK, None, &parts)
}

/// The keyed tag the Registry stores to refuse a second mint for the same note (I5).
/// Unlinkable to the note's nullifier without the vault secret (leak vector L9).
pub fn deposit_tag(vault_sk: &[u8; 32], nullifier: &[u8; 32]) -> [u8; 32] {
    let key = blake2b_256(TAG_KEY, None, &[vault_sk]);
    blake2b_256(DEPOSIT_TAG, Some(&key), &[nullifier])
}

/// Opaque id of a received note, used off-chain (e.g. in refund requests).
pub fn note_id(vault_id: &[u8; 32], position: &NotePosition) -> [u8; 32] {
    blake2b_256(
        NOTE_ID,
        None,
        &[
            vault_id,
            &position.height.to_le_bytes(),
            &position.tx_index.to_le_bytes(),
            &position.action_index.to_le_bytes(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sk_depends_on_every_contribution_and_the_vault() {
        let v = [1u8; 32];
        let c = [[2u8; 32], [3u8; 32], [4u8; 32], [5u8; 32], [6u8; 32]];
        let sk = vault_sk(&v, &c);
        let mut c2 = c;
        c2[4] = [7u8; 32];
        assert_ne!(sk, vault_sk(&v, &c2));
        assert_ne!(sk, vault_sk(&[9u8; 32], &c));
        assert_eq!(sk, vault_sk(&v, &c));
    }

    #[test]
    fn deposit_tag_is_keyed() {
        let nf = [8u8; 32];
        assert_ne!(deposit_tag(&[1u8; 32], &nf), deposit_tag(&[2u8; 32], &nf));
        assert_eq!(deposit_tag(&[1u8; 32], &nf), deposit_tag(&[1u8; 32], &nf));
    }
}
