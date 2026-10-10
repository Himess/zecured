//! Vault viewing keys and addresses for FROST-controlled Orchard-protocol keys.
//!
//! A vault's spend authorizing key `ask` exists only as FROST shares; its public
//! counterpart `ak` is the DKG group key. ZIP 2005 § "Usage with FROST" requires such keys
//! to use `use_qsk = true`: the signers privately agree on a 32-byte `sk` and derive
//!
//! ```text
//! nk   = ToBase(PRF^expand_sk([0x07]))
//! qsk  = truncate_32(PRF^expand_sk([0x0C]))
//! qk   = BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)
//! rivk = ToScalar(PRF^expand_qk([0x0D] || I2LEOSP_256(ak) || I2LEOSP_256(nk)))
//! ```
//!
//! giving the full viewing key `(ak, nk, rivk)`. No Zcash library implements this
//! derivation yet, so it lives here (see docs/ref/FACTS.md).
//!
//! Receivers are scoped to the Orchard *protocol*, not to a pool (ZIP 326): after NU6.3 an
//! Orchard-protocol receiver receives Ironwood-pool funds. A vault address is therefore a
//! unified address whose only receiver is the Orchard-protocol receiver.

use anyhow::{Context, Result, anyhow, ensure};
use orchard::keys::{FullViewingKey, Scope};
use pasta_curves::{
    group::ff::{FromUniformBytes, PrimeField},
    pallas,
};
use zcash_keys::{address::UnifiedAddress, keys::UnifiedFullViewingKey};
use zcash_protocol::consensus::Parameters;

/// BLAKE2b personalization of `PRF^expand` (Zcash protocol spec § 5.4.2).
const PRF_EXPAND_PERSONALIZATION: &[u8; 16] = b"Zcash_ExpandSeed";
/// `PRF^expand` domain byte for the nullifier deriving key `nk` (spec § 4.2.3).
const DOMAIN_NK: u8 = 0x07;
/// `PRF^expand` domain byte for the legacy (`use_qsk = false`) `rivk` (spec § 4.2.3).
#[cfg(test)]
const DOMAIN_RIVK_LEGACY: u8 = 0x08;
/// `PRF^expand` domain byte for the quantum spending key `qsk` (ZIP 2005 § 4.2.3).
const DOMAIN_QSK: u8 = 0x0C;
/// `PRF^expand` domain byte for `rivk_ext` under `use_qsk = true` (ZIP 2005 § 4.2.3).
const DOMAIN_RIVK_EXT: u8 = 0x0D;
/// BLAKE3 `derive_key` context string for `qk` (ZIP 2005 § 4.2.3).
const QK_CONTEXT: &str = "Zcash ZIP 2005 qk-derivation v1";
/// Length in bytes of `qsk` (`ℓ_qsk / 8`).
const QSK_LEN: usize = 32;

/// Diversifier index of the vault's single published address.
pub const VAULT_ADDRESS_INDEX: u32 = 0;

/// `PRF^expand_key(t)` = BLAKE2b-512("Zcash_ExpandSeed", key || t).
fn prf_expand(key: &[u8; 32], t: &[&[u8]]) -> [u8; 64] {
    let mut state = blake2b_simd::Params::new()
        .hash_length(64)
        .personal(PRF_EXPAND_PERSONALIZATION)
        .to_state();
    state.update(key);
    for part in t {
        state.update(part);
    }
    *state.finalize().as_array()
}

/// `ToBase^Orchard(x) = LEOS2IP_512(x) mod q_P`.
fn to_base(x: &[u8; 64]) -> pallas::Base {
    pallas::Base::from_uniform_bytes(x)
}

/// `ToScalar^Orchard(x) = LEOS2IP_512(x) mod r_P`.
fn to_scalar(x: &[u8; 64]) -> pallas::Scalar {
    pallas::Scalar::from_uniform_bytes(x)
}

/// The secret the signers privately agree on for one vault (ZIP 2005 `sk`).
///
/// It yields the viewing keys and `qsk`, but no spend authority: `ask` exists only as
/// FROST shares. ZIP 2005 still requires it to be held as securely as `ask`, because a
/// quantum adversary holding `qsk` could steal recoverable notes.
#[derive(Clone)]
pub struct VaultSecret([u8; 32]);

impl VaultSecret {
    /// Wraps an agreed 32-byte secret.
    pub fn from_bytes(sk: [u8; 32]) -> Self {
        Self(sk)
    }

    /// The raw secret, for encrypted storage on a signer node only.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// `nk = H^nk(sk)`.
    fn nk(&self) -> pallas::Base {
        to_base(&prf_expand(&self.0, &[&[DOMAIN_NK]]))
    }

    /// `qsk = H^qsk(sk)`.
    fn qsk(&self) -> [u8; QSK_LEN] {
        let expanded = prf_expand(&self.0, &[&[DOMAIN_QSK]]);
        let mut qsk = [0u8; QSK_LEN];
        qsk.copy_from_slice(&expanded[..QSK_LEN]);
        qsk
    }

    /// `qk = H^qk(qsk)`.
    fn qk(&self) -> [u8; 32] {
        blake3::derive_key(QK_CONTEXT, &self.qsk())
    }

    /// The vault's full viewing key `(ak, nk, rivk_ext)` for the DKG group key `ak`.
    pub fn full_viewing_key(&self, ak: &[u8; 32]) -> Result<FullViewingKey> {
        ensure!(ak[31] & 0x80 == 0, "ak must have ỹ = 0");
        let nk = self.nk();
        let nk_bytes = nk.to_repr();
        let rivk = to_scalar(&prf_expand(&self.qk(), &[&[DOMAIN_RIVK_EXT], ak, &nk_bytes]));

        let mut raw = [0u8; 96];
        raw[..32].copy_from_slice(ak);
        raw[32..64].copy_from_slice(&nk_bytes);
        raw[64..].copy_from_slice(&rivk.to_repr());
        FullViewingKey::from_bytes(&raw).context("derived vault FVK is invalid")
    }
}

/// The vault's published payment address (external scope, index 0).
pub fn vault_address(fvk: &FullViewingKey) -> orchard::Address {
    fvk.address_at(VAULT_ADDRESS_INDEX, Scope::External)
}

/// Encodes `addr` as a unified address whose only receiver is the Orchard-protocol
/// receiver (paid in the Ironwood pool after NU6.3).
pub fn encode_ironwood_ua<P: Parameters>(params: &P, addr: orchard::Address) -> Result<String> {
    let ua = UnifiedAddress::from_receivers(Some(addr), None, None, None, None)
        .context("unified address needs a receiver")?;
    Ok(ua.encode(params))
}

/// Encodes the vault FVK as an Orchard-only UFVK (given to the borrower to verify the vault).
pub fn encode_ufvk<P: Parameters>(params: &P, fvk: &FullViewingKey) -> Result<String> {
    let ufvk = UnifiedFullViewingKey::from_orchard_fvk(fvk.clone())
        .map_err(|e| anyhow!("build UFVK: {e:?}"))?;
    ufvk.encode(params).map_err(|e| anyhow!("encode UFVK: {e:?}"))
}

/// Parses a unified address and returns its Orchard-protocol receiver, rejecting
/// addresses without one. Transparent and Sapling receivers are ignored: Zecured only
/// ever pays the Orchard-protocol receiver, in the Ironwood pool.
pub fn ironwood_receiver<P: Parameters>(params: &P, encoded: &str) -> Result<orchard::Address> {
    let ua = UnifiedAddress::decode(params, encoded)
        .map_err(|e| anyhow!("not a unified address for this network: {e:?}"))?;
    ua.orchard()
        .copied()
        .context("address has no Orchard-protocol (Ironwood) receiver")
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard::keys::{SpendValidatingKey, SpendingKey};
    use zcash_protocol::consensus::Network;

    /// Some 32-byte value that is a valid Orchard spending key.
    fn sample_sk() -> [u8; 32] {
        (0u8..=255)
            .map(|i| [i; 32])
            .find(|b| bool::from(SpendingKey::from_bytes(*b).is_some()))
            .unwrap()
    }

    /// Our PRF^expand / ToBase / ToScalar plumbing must reproduce orchard's own legacy
    /// derivation; `nk = H^nk(sk)` is identical in both ZIP 2005 branches.
    #[test]
    fn primitives_match_orchard_legacy_derivation() {
        let sk_bytes = sample_sk();
        let sk = SpendingKey::from_bytes(sk_bytes).unwrap();
        let reference = FullViewingKey::from(&sk).to_bytes();

        let ours = VaultSecret::from_bytes(sk_bytes);
        assert_eq!(ours.nk().to_repr(), reference[32..64]);
        let legacy_rivk = to_scalar(&prf_expand(&sk_bytes, &[&[DOMAIN_RIVK_LEGACY]]));
        assert_eq!(legacy_rivk.to_repr(), reference[64..96]);
    }

    #[test]
    fn use_qsk_fvk_keeps_ak_and_nk_and_changes_rivk() {
        let sk_bytes = sample_sk();
        let reference = FullViewingKey::from(&SpendingKey::from_bytes(sk_bytes).unwrap()).to_bytes();
        let ak: [u8; 32] = reference[..32].try_into().unwrap();
        assert!(SpendValidatingKey::from_bytes(&ak).is_some());

        let fvk = VaultSecret::from_bytes(sk_bytes).full_viewing_key(&ak).unwrap();
        let raw = fvk.to_bytes();
        assert_eq!(raw[..64], reference[..64], "ak and nk are shared with the legacy path");
        assert_ne!(raw[64..], reference[64..], "use_qsk = true derives a different rivk");
    }

    #[test]
    fn rejects_odd_ak() {
        let mut ak = [1u8; 32];
        ak[31] |= 0x80;
        assert!(VaultSecret::from_bytes([3u8; 32]).full_viewing_key(&ak).is_err());
    }

    #[test]
    fn ua_round_trips_and_has_only_the_orchard_receiver() {
        let sk_bytes = sample_sk();
        let reference = FullViewingKey::from(&SpendingKey::from_bytes(sk_bytes).unwrap()).to_bytes();
        let ak: [u8; 32] = reference[..32].try_into().unwrap();
        let fvk = VaultSecret::from_bytes([9u8; 32]).full_viewing_key(&ak).unwrap();
        let addr = vault_address(&fvk);

        let params = Network::TestNetwork;
        let encoded = encode_ironwood_ua(&params, addr).unwrap();
        assert!(encoded.starts_with("utest1"), "{encoded}");
        let ua = UnifiedAddress::decode(&params, &encoded).unwrap();
        assert!(ua.sapling().is_none() && ua.transparent().is_none());
        assert_eq!(ironwood_receiver(&params, &encoded).unwrap(), addr);
        assert_eq!(fvk.scope_for_address(&addr), Some(Scope::External));

        let ufvk = encode_ufvk(&params, &fvk).unwrap();
        assert!(ufvk.starts_with("uviewtest1"), "{ufvk}");
    }
}
