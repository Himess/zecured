//! Deterministic derivations every signer (and the coordinator, where it can) agrees on.
//!
//! All use BLAKE2b with a 16-byte personalization so their outputs are domain-separated.

use crate::classify::NotePosition;

const SK_COMMIT: &[u8; 16] = b"Zecured_SkCommit";
const VAULT_SK: &[u8; 16] = b"Zecured_VaultSk_";
const TAG_KEY: &[u8; 16] = b"Zecured_TagKey__";
const DEPOSIT_TAG: &[u8; 16] = b"Zecured_DepTag__";
const PAYOUT_REF: &[u8; 16] = b"Zecured_PayRef__";
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

/// The vault's tag key `K = BLAKE2b-256("Zecured_TagKey__", UFVK string)` (D24): every value Base
/// sees about a vault's notes and payouts is a PRF under it, unlinkable to Zcash without the
/// viewing key.
fn tag_key(ufvk: &str) -> [u8; 32] {
    blake2b_256(TAG_KEY, None, &[ufvk.as_bytes()])
}

/// The keyed tag the Registry stores to refuse a second mint for the same note (I5):
/// BLAKE2b-256 keyed with the tag key over the note commitment `cmx`. A Tier 2 deposit proof
/// computes exactly this (tier2/statement `deposit_tag`), so an attestation and a proof of the
/// same note carry the same tag. Unlinkable to the note without the vault's viewing key (leak
/// vector L9). Tier 3 uses it for every lineage note, change notes included (`noteTag`,
/// docs/TIER3.md § 2).
pub fn deposit_tag(ufvk: &str, cmx: &[u8; 32]) -> [u8; 32] {
    blake2b_256(DEPOSIT_TAG, Some(&tag_key(ufvk)), &[cmx])
}

/// Tier 3 `payoutRef` (docs/TIER3.md § 2): BLAKE2b-256 keyed with the tag key over the payout's
/// txid in internal byte order (the ZIP 244 digest itself, which for these transactions is also
/// the sighash the signers sign). The one value that names a payout on Base.
pub fn payout_ref(ufvk: &str, txid: &[u8; 32]) -> [u8; 32] {
    blake2b_256(PAYOUT_REF, Some(&tag_key(ufvk)), &[txid])
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
        let cmx = [8u8; 32];
        assert_ne!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1b", &cmx));
        assert_eq!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1a", &cmx));
    }

    /// The same value as tier2/statement's `deposit_tag` (and Python's hashlib.blake2b with the
    /// same key and personalization).
    #[test]
    fn deposit_tag_vector() {
        let tag = deposit_tag("uviewtest1a", &[7u8; 32]);
        assert_eq!(hex::encode(tag), "add8548e79e01888ca52988bc44b01d0be125fdc5569db7597bd040f335f25aa");
    }

    /// Python's hashlib.blake2b(txid, digest_size=32, key=K, person=b"Zecured_PayRef__"), with
    /// K = hashlib.blake2b(b"uviewtest1a", digest_size=32, person=b"Zecured_TagKey__").
    #[test]
    fn payout_ref_vector() {
        let r = payout_ref("uviewtest1a", &[0x42u8; 32]);
        assert_eq!(hex::encode(r), "fe5ff98dd5dd2edcc9663d2ce6c4ed236521d441c34d44ebf2329198d80366df");
    }

    /// Same key, same input, different personalization: a payout reference never equals a note
    /// tag, so a txid can never pass for a note commitment on Base.
    #[test]
    fn payout_ref_is_domain_separated_from_note_tags() {
        let x = [0x42u8; 32];
        assert_ne!(payout_ref("uviewtest1a", &x), deposit_tag("uviewtest1a", &x));
        assert_ne!(payout_ref("uviewtest1a", &x), payout_ref("uviewtest1b", &x));
    }
}
