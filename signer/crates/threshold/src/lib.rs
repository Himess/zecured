//! FROST(Pallas, BLAKE2b-512) re-randomized threshold signing for vault spend authorization.
//!
//! Each vault has its own 3-of-5 key. The group verifying key is the Orchard-protocol
//! spend validating key `ak` of the vault; the shares never leave their nodes.
//!
//! The FROST ciphersuite comes from `reddsa` 0.5 (pasta_curves 0.5). Everything that
//! crosses into the Zcash transaction stack (orchard 0.16, pasta_curves 0.6) is passed
//! as canonical bytes: `ak` (32), the randomizer `alpha` (32), the sighash (32) and the
//! final RedPallas spend-auth signature (64).

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail, ensure};
use rand_core_06::{CryptoRng, RngCore};
use reddsa::frost::redpallas::{
    self as frost, Identifier, SigningPackage,
    frost::Field,
    keys::{EvenY, KeyPackage, PublicKeyPackage, dkg},
    rerandomized::{RandomizedParams, Randomizer},
    round1::{self, SigningCommitments, SigningNonces},
    round2::SignatureShare,
};

/// Number of signer nodes holding a share of every vault key.
pub const MAX_SIGNERS: u16 = 5;
/// Number of distinct signers required to authorize a spend.
pub const MIN_SIGNERS: u16 = 3;

/// Re-export of the FROST participant identifier.
pub type SignerId = Identifier;

/// The participant identifier for node `index` (1-based, as in FROST).
pub fn signer_id(index: u16) -> Result<SignerId> {
    Identifier::try_from(index).map_err(|e| anyhow!("invalid signer index {index}: {e}"))
}

/// Result of a completed distributed key generation for one vault.
pub struct VaultKeys {
    /// Secret key package per participant (in production each node only ever sees its own).
    pub key_packages: BTreeMap<SignerId, KeyPackage>,
    /// Public data shared by every participant: group key + per-signer verifying shares.
    pub public_key_package: PublicKeyPackage,
}

impl VaultKeys {
    /// The group verifying key as Orchard `ak` bytes (`I2LEOSP_256(ak)`, ỹ = 0).
    pub fn ak_bytes(&self) -> Result<[u8; 32]> {
        ak_bytes(&self.public_key_package)
    }
}

/// The group verifying key of `pkp` as Orchard `ak` bytes.
pub fn ak_bytes(pkp: &PublicKeyPackage) -> Result<[u8; 32]> {
    let bytes = pkp
        .verifying_key()
        .serialize()
        .map_err(|e| anyhow!("serialize verifying key: {e}"))?;
    let ak: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .context("verifying key is not 32 bytes")?;
    // reddsa's post-DKG hook negates all shares when needed so that ak has ỹ = 0,
    // as Orchard requires (ZIP 2005 § 4.2.3). Check it rather than trust it.
    ensure!(pkp.has_even_y(), "group key does not have an even y-coordinate");
    ensure!(ak[31] & 0x80 == 0, "ak sign bit set");
    Ok(ak)
}

/// Runs the three-part FROST DKG for all `MAX_SIGNERS` participants inside one process.
///
/// SPIKE / TEST ONLY: in the deployed system every node runs its own part and the
/// round-2 packages travel over confidential, authenticated channels.
pub fn dkg_in_process<R: RngCore + CryptoRng>(mut rng: R) -> Result<VaultKeys> {
    let ids: Vec<SignerId> = (1..=MAX_SIGNERS).map(signer_id).collect::<Result<_>>()?;

    // Part 1: every participant broadcasts a commitment package.
    let mut r1_secrets = BTreeMap::new();
    let mut r1_received: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round1::Package>> =
        BTreeMap::new();
    for &id in &ids {
        let (secret, package) = dkg::part1(id, MAX_SIGNERS, MIN_SIGNERS, &mut rng)
            .map_err(|e| anyhow!("dkg part1: {e}"))?;
        r1_secrets.insert(id, secret);
        for &other in ids.iter().filter(|&&o| o != id) {
            r1_received
                .entry(other)
                .or_default()
                .insert(id, package.clone());
        }
    }

    // Part 2: every participant sends one private package to every other participant.
    let mut r2_secrets = BTreeMap::new();
    let mut r2_received: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round2::Package>> =
        BTreeMap::new();
    for &id in &ids {
        let secret = r1_secrets.remove(&id).context("missing round-1 secret")?;
        let (secret2, packages) =
            dkg::part2(secret, &r1_received[&id]).map_err(|e| anyhow!("dkg part2: {e}"))?;
        r2_secrets.insert(id, secret2);
        for (recipient, package) in packages {
            r2_received.entry(recipient).or_default().insert(id, package);
        }
    }

    // Part 3: every participant derives its key package and the shared public package.
    let mut key_packages = BTreeMap::new();
    let mut public_key_package: Option<PublicKeyPackage> = None;
    for &id in &ids {
        let (kp, pkp) = dkg::part3(&r2_secrets[&id], &r1_received[&id], &r2_received[&id])
            .map_err(|e| anyhow!("dkg part3: {e}"))?;
        match &public_key_package {
            None => public_key_package = Some(pkp),
            Some(existing) => ensure!(
                existing == &pkp,
                "participants disagree on the public key package"
            ),
        }
        key_packages.insert(id, kp);
    }

    Ok(VaultKeys {
        key_packages,
        public_key_package: public_key_package.context("no participants")?,
    })
}

/// Round 1 for one signer: fresh nonces (kept secret, used once) and their commitments.
pub fn commit<R: RngCore + CryptoRng>(
    key_package: &KeyPackage,
    rng: &mut R,
) -> (SigningNonces, SigningCommitments) {
    round1::commit(key_package.signing_share(), rng)
}

/// Builds the signing package for `sighash` from the commitments of the chosen signers.
pub fn signing_package(
    commitments: BTreeMap<SignerId, SigningCommitments>,
    sighash: &[u8; 32],
) -> Result<SigningPackage> {
    ensure!(
        commitments.len() >= usize::from(MIN_SIGNERS),
        "need at least {MIN_SIGNERS} signers, got {}",
        commitments.len()
    );
    Ok(SigningPackage::new(commitments, sighash))
}

/// The FROST randomizer for an Orchard-protocol spend with randomizer `alpha`
/// (the PCZT spend's `alpha`, little-endian Pallas scalar encoding).
pub fn randomizer_from_alpha(alpha: &[u8; 32]) -> Result<Randomizer> {
    let scalar = <frost::PallasScalarField as Field>::deserialize(alpha)
        .map_err(|e| anyhow!("alpha is not a canonical Pallas scalar: {e}"))?;
    Ok(Randomizer::from_scalar(scalar))
}

/// Round 2 for one signer: its signature share over the signing package, for the key
/// re-randomized by `alpha` (so the aggregate verifies under `rk = ak + [alpha] G`).
pub fn sign_share(
    signing_package: &SigningPackage,
    nonces: &SigningNonces,
    key_package: &KeyPackage,
    alpha: &[u8; 32],
) -> Result<SignatureShare> {
    let randomizer = randomizer_from_alpha(alpha)?;
    // `frost_rerandomized::sign` is deprecated in favour of commitment-derived
    // randomizers, but a Zcash spend's randomizer is fixed by the transaction (rk is part
    // of the sighash) before round 1, exactly as in ZF's zcash-sign / frost-client.
    #[allow(deprecated)]
    let share = frost_rerandomized_sign(signing_package, nonces, key_package, randomizer)?;
    Ok(share)
}

#[allow(deprecated)]
fn frost_rerandomized_sign(
    signing_package: &SigningPackage,
    nonces: &SigningNonces,
    key_package: &KeyPackage,
    randomizer: Randomizer,
) -> Result<SignatureShare> {
    reddsa_frost_rerandomized::sign::<frost::PallasBlake2b512>(
        signing_package,
        nonces,
        key_package,
        randomizer,
    )
    .map_err(|e| anyhow!("frost round 2: {e}"))
}

/// Aggregates signature shares into the 64-byte RedPallas spend authorization signature
/// valid under `rk = ak + [alpha] G`.
pub fn aggregate(
    signing_package: &SigningPackage,
    shares: &BTreeMap<SignerId, SignatureShare>,
    public_key_package: &PublicKeyPackage,
    alpha: &[u8; 32],
) -> Result<[u8; 64]> {
    if shares.len() < usize::from(MIN_SIGNERS) {
        bail!(
            "need {MIN_SIGNERS} signature shares, got {}",
            shares.len()
        );
    }
    let params =
        RandomizedParams::from_randomizer(public_key_package.verifying_key(), randomizer_from_alpha(alpha)?);
    let signature = frost::rerandomized::aggregate(signing_package, shares, public_key_package, &params)
        .map_err(|e| anyhow!("frost aggregate: {e}"))?;
    let bytes = signature
        .serialize()
        .map_err(|e| anyhow!("serialize signature: {e}"))?;
    bytes
        .as_slice()
        .try_into()
        .context("signature is not 64 bytes")
}

// `reddsa` does not re-export `frost_rerandomized::sign`; depend on the same crate
// version reddsa uses so the types unify.
use frost_rerandomized as reddsa_frost_rerandomized;

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core_06::OsRng;

    #[test]
    fn dkg_produces_even_y_group_key_shared_by_all() {
        let keys = dkg_in_process(OsRng).unwrap();
        assert_eq!(keys.key_packages.len(), usize::from(MAX_SIGNERS));
        let ak = keys.ak_bytes().unwrap();
        assert_eq!(ak[31] & 0x80, 0);
        for kp in keys.key_packages.values() {
            assert_eq!(kp.verifying_key(), keys.public_key_package.verifying_key());
            assert_eq!(*kp.min_signers(), MIN_SIGNERS);
        }
    }

    fn sign_with(keys: &VaultKeys, signers: &[u16], msg: &[u8; 32], alpha: &[u8; 32]) -> Result<[u8; 64]> {
        let mut rng = OsRng;
        let mut nonces = BTreeMap::new();
        let mut commitments = BTreeMap::new();
        for &i in signers {
            let id = signer_id(i)?;
            let (n, c) = commit(&keys.key_packages[&id], &mut rng);
            nonces.insert(id, n);
            commitments.insert(id, c);
        }
        let package = signing_package(commitments, msg)?;
        let mut shares = BTreeMap::new();
        for (id, n) in &nonces {
            shares.insert(*id, sign_share(&package, n, &keys.key_packages[id], alpha)?);
        }
        aggregate(&package, &shares, &keys.public_key_package, alpha)
    }

    fn random_alpha() -> [u8; 32] {
        let s = <frost::PallasScalarField as Field>::random(&mut OsRng);
        <frost::PallasScalarField as Field>::serialize(&s)
    }

    #[test]
    fn three_of_five_signs_and_verifies_under_randomized_key() {
        let keys = dkg_in_process(OsRng).unwrap();
        let msg = [7u8; 32];
        let alpha = random_alpha();
        let sig = sign_with(&keys, &[1, 3, 5], &msg, &alpha).unwrap();

        // Verify as a plain RedPallas SpendAuth signature under rk = ak + [alpha]G,
        // the same check a Zcash node performs.
        let ak = reddsa::VerificationKey::<reddsa::orchard::SpendAuth>::try_from(keys.ak_bytes().unwrap()).unwrap();
        let alpha_scalar = <frost::PallasScalarField as Field>::deserialize(&alpha).unwrap();
        let rk = ak.randomize(&alpha_scalar);
        rk.verify(&msg, &reddsa::Signature::from(sig)).unwrap();
        // ... and not under the unrandomized key.
        assert!(ak.verify(&msg, &reddsa::Signature::from(sig)).is_err());
    }

    #[test]
    fn two_signers_cannot_sign() {
        let keys = dkg_in_process(OsRng).unwrap();
        let err = sign_with(&keys, &[2, 4], &[1u8; 32], &random_alpha()).unwrap_err();
        assert!(err.to_string().contains("at least 3"), "{err}");
    }
}
