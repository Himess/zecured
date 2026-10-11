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
    let bytes = pkp.verifying_key().serialize().map_err(|e| anyhow!("serialize verifying key: {e}"))?;
    let ak: [u8; 32] = bytes.as_slice().try_into().context("verifying key is not 32 bytes")?;
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
    let mut r1_received: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round1::Package>> = BTreeMap::new();
    for &id in &ids {
        let (secret, package) =
            dkg::part1(id, MAX_SIGNERS, MIN_SIGNERS, &mut rng).map_err(|e| anyhow!("dkg part1: {e}"))?;
        r1_secrets.insert(id, secret);
        for &other in ids.iter().filter(|&&o| o != id) {
            r1_received.entry(other).or_default().insert(id, package.clone());
        }
    }

    // Part 2: every participant sends one private package to every other participant.
    let mut r2_secrets = BTreeMap::new();
    let mut r2_received: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round2::Package>> = BTreeMap::new();
    for &id in &ids {
        let secret = r1_secrets.remove(&id).context("missing round-1 secret")?;
        let (secret2, packages) = dkg::part2(secret, &r1_received[&id]).map_err(|e| anyhow!("dkg part2: {e}"))?;
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
            Some(existing) => ensure!(existing == &pkp, "participants disagree on the public key package"),
        }
        key_packages.insert(id, kp);
    }

    Ok(VaultKeys {
        key_packages,
        public_key_package: public_key_package.context("no participants")?,
    })
}

/// Round 1 for one signer: fresh nonces (kept secret, used once) and their commitments.
pub fn commit<R: RngCore + CryptoRng>(key_package: &KeyPackage, rng: &mut R) -> (SigningNonces, SigningCommitments) {
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
    reddsa_frost_rerandomized::sign::<frost::PallasBlake2b512>(signing_package, nonces, key_package, randomizer)
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
        bail!("need {MIN_SIGNERS} signature shares, got {}", shares.len());
    }
    let params = RandomizedParams::from_randomizer(public_key_package.verifying_key(), randomizer_from_alpha(alpha)?);
    let signature = frost::rerandomized::aggregate(signing_package, shares, public_key_package, &params)
        .map_err(|e| anyhow!("frost aggregate: {e}"))?;
    let bytes = signature.serialize().map_err(|e| anyhow!("serialize signature: {e}"))?;
    bytes.as_slice().try_into().context("signature is not 64 bytes")
}

/// Runs both FROST rounds for `signers` inside one process and returns the aggregate
/// signature over `sighash` for randomizer `alpha`.
///
/// SPIKE / TEST ONLY: deployed nodes exchange commitments and shares over the network.
pub fn sign_in_process(
    key_packages: &BTreeMap<SignerId, KeyPackage>,
    public_key_package: &PublicKeyPackage,
    signers: &[u16],
    sighash: &[u8; 32],
    alpha: &[u8; 32],
) -> Result<[u8; 64]> {
    let mut rng = rand_core_06::OsRng;
    let mut nonces = BTreeMap::new();
    let mut commitments = BTreeMap::new();
    for &i in signers {
        let id = signer_id(i)?;
        let kp = key_packages.get(&id).with_context(|| format!("no key package for signer {i}"))?;
        let (n, c) = commit(kp, &mut rng);
        nonces.insert(id, n);
        commitments.insert(id, c);
    }
    let package = signing_package(commitments, sighash)?;
    let mut shares = BTreeMap::new();
    for (id, n) in &nonces {
        shares.insert(*id, sign_share(&package, n, &key_packages[id], alpha)?);
    }
    aggregate(&package, &shares, public_key_package, alpha)
}

// ------------------------------------------------------------------ share refresh and repair
//
// Both keep the vault's group key `ak`, hence its address and viewing keys. Refresh gives every
// signer a new share (a share stolen before the refresh is useless afterwards, and old and new
// shares never combine). Repair rebuilds one signer's lost share from three others without
// anyone learning it or the group secret. Each node runs its own part; the private messages
// travel sealed node-to-node, as in the DKG.

use reddsa::frost::redpallas::frost::keys::{
    refresh,
    repairable::{self, Delta, Sigma},
};

/// Refresh part 1 (every signer): a fresh polynomial with constant term zero; the package is
/// broadcast.
pub fn refresh_part1<R: RngCore + CryptoRng>(
    index: u16,
    rng: R,
) -> Result<(dkg::round1::SecretPackage, dkg::round1::Package)> {
    refresh::refresh_dkg_part1(signer_id(index)?, MAX_SIGNERS, MIN_SIGNERS, rng)
        .map_err(|e| anyhow!("refresh part1: {e}"))
}

/// Refresh part 2 (every signer): one private package per other signer.
pub fn refresh_part2(
    secret: dkg::round1::SecretPackage,
    round1: &BTreeMap<SignerId, dkg::round1::Package>,
) -> Result<(dkg::round2::SecretPackage, BTreeMap<SignerId, dkg::round2::Package>)> {
    refresh::refresh_dkg_part2(secret, round1).map_err(|e| anyhow!("refresh part2: {e}"))
}

/// Refresh part 3 (every signer): the new key package and public key package. Refused unless
/// the group key is unchanged and our new verifying share is the one everybody computes for us.
pub fn refresh_finish(
    secret: &dkg::round2::SecretPackage,
    round1: &BTreeMap<SignerId, dkg::round1::Package>,
    round2: &BTreeMap<SignerId, dkg::round2::Package>,
    old_public: &PublicKeyPackage,
    old_key: &KeyPackage,
) -> Result<(KeyPackage, PublicKeyPackage)> {
    let (kp, pkp) = refresh::refresh_dkg_shares(secret, round1, round2, old_public.clone(), old_key.clone())
        .map_err(|e| anyhow!("refresh shares: {e}"))?;
    ensure!(pkp.verifying_key() == old_public.verifying_key(), "the refresh changed the group key");
    ensure!(
        pkp.verifying_shares().get(kp.identifier()) == Some(kp.verifying_share()),
        "our refreshed share does not match the public key package"
    );
    ensure!(kp.signing_share() != old_key.signing_share(), "the refresh left our share unchanged");
    Ok((kp, pkp))
}

/// Repair part 1 (each helper): random deltas summing to the helper's contribution to the
/// lost share, one for each helper (itself included).
pub fn repair_deltas<R: RngCore + CryptoRng>(
    helpers: &[u16],
    key: &KeyPackage,
    lost: u16,
    rng: &mut R,
) -> Result<BTreeMap<SignerId, Delta<reddsa::frost::redpallas::PallasBlake2b512>>> {
    let ids: Vec<SignerId> = helpers.iter().map(|&i| signer_id(i)).collect::<Result<_>>()?;
    ensure!(!helpers.contains(&lost), "a signer cannot help repair its own share");
    repairable::repair_share_part1(&ids, key, rng, signer_id(lost)?).map_err(|e| anyhow!("repair part1: {e}"))
}

/// Repair part 2 (each helper): the sum of the deltas it received, for the repaired signer.
pub fn repair_sigma(
    deltas: &[Delta<reddsa::frost::redpallas::PallasBlake2b512>],
) -> Sigma<reddsa::frost::redpallas::PallasBlake2b512> {
    repairable::repair_share_part2(deltas)
}

/// Repair part 3 (the repaired signer): its key package from the helpers' sigmas, refused
/// unless it matches its verifying share in the public key package.
pub fn repair_finish(
    sigmas: &[Sigma<reddsa::frost::redpallas::PallasBlake2b512>],
    index: u16,
    public: &PublicKeyPackage,
) -> Result<KeyPackage> {
    let id = signer_id(index)?;
    let kp = repairable::repair_share_part3(sigmas, id, public).map_err(|e| anyhow!("repair part3: {e}"))?;
    ensure!(
        public.verifying_shares().get(&id) == Some(kp.verifying_share()),
        "the repaired share does not match the public key package"
    );
    Ok(kp)
}

/// Refreshes every share of `keys` inside one process. SPIKE / TEST ONLY, like
/// [`dkg_in_process`].
pub fn refresh_in_process<R: RngCore + CryptoRng>(keys: &VaultKeys, mut rng: R) -> Result<VaultKeys> {
    let ids: Vec<u16> = (1..=MAX_SIGNERS).collect();
    let mut secrets = BTreeMap::new();
    let mut received1: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round1::Package>> = BTreeMap::new();
    for &i in &ids {
        let (secret, package) = refresh_part1(i, &mut rng)?;
        secrets.insert(signer_id(i)?, secret);
        for &j in ids.iter().filter(|&&j| j != i) {
            received1.entry(signer_id(j)?).or_default().insert(signer_id(i)?, package.clone());
        }
    }
    let mut secrets2 = BTreeMap::new();
    let mut received2: BTreeMap<SignerId, BTreeMap<SignerId, dkg::round2::Package>> = BTreeMap::new();
    for (id, secret) in secrets {
        let (secret2, packages) = refresh_part2(secret, &received1[&id])?;
        secrets2.insert(id, secret2);
        for (to, package) in packages {
            received2.entry(to).or_default().insert(id, package);
        }
    }
    let mut key_packages = BTreeMap::new();
    let mut public: Option<PublicKeyPackage> = None;
    for (id, secret2) in &secrets2 {
        let (kp, pkp) =
            refresh_finish(secret2, &received1[id], &received2[id], &keys.public_key_package, &keys.key_packages[id])?;
        match &public {
            None => public = Some(pkp),
            Some(p) => ensure!(p == &pkp, "signers disagree on the refreshed public key package"),
        }
        key_packages.insert(*id, kp);
    }
    Ok(VaultKeys {
        key_packages,
        public_key_package: public.context("no signers")?,
    })
}

/// Rebuilds signer `lost`'s key package from `helpers` inside one process. TEST ONLY.
pub fn repair_in_process<R: RngCore + CryptoRng>(
    keys: &VaultKeys,
    helpers: &[u16],
    lost: u16,
    rng: &mut R,
) -> Result<KeyPackage> {
    let mut to_helper: BTreeMap<SignerId, Vec<Delta<reddsa::frost::redpallas::PallasBlake2b512>>> = BTreeMap::new();
    for &h in helpers {
        for (to, delta) in repair_deltas(helpers, &keys.key_packages[&signer_id(h)?], lost, rng)? {
            to_helper.entry(to).or_default().push(delta);
        }
    }
    let sigmas: Vec<_> = to_helper.values().map(|d| repair_sigma(d)).collect();
    repair_finish(&sigmas, lost, &keys.public_key_package)
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
        sign_in_process(&keys.key_packages, &keys.public_key_package, signers, msg, alpha)
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

    /// Refresh: same group key (so the same vault address), every share new; the new shares
    /// sign, and an old share no longer combines with new ones.
    #[test]
    fn refreshed_shares_keep_the_key_and_retire_the_old_shares() {
        let keys = dkg_in_process(OsRng).unwrap();
        let fresh = refresh_in_process(&keys, OsRng).unwrap();
        assert_eq!(fresh.ak_bytes().unwrap(), keys.ak_bytes().unwrap());
        for (id, kp) in &fresh.key_packages {
            assert_ne!(kp.signing_share(), keys.key_packages[id].signing_share());
        }
        let (msg, alpha) = ([9u8; 32], random_alpha());
        let sig = sign_with(&fresh, &[2, 4, 5], &msg, &alpha).unwrap();
        let ak = reddsa::VerificationKey::<reddsa::orchard::SpendAuth>::try_from(keys.ak_bytes().unwrap()).unwrap();
        let rk = ak.randomize(&<frost::PallasScalarField as Field>::deserialize(&alpha).unwrap());
        rk.verify(&msg, &reddsa::Signature::from(sig)).unwrap();

        // An attacker holding one old share and two new ones cannot sign.
        let mut mixed = fresh.key_packages.clone();
        let one = signer_id(1).unwrap();
        mixed.insert(one, keys.key_packages[&one].clone());
        let forged = sign_in_process(&mixed, &fresh.public_key_package, &[1, 2, 3], &msg, &alpha);
        assert!(forged.is_err(), "an old share must not combine with refreshed ones");
    }

    /// Repair: three helpers rebuild a lost share exactly, and it signs.
    #[test]
    fn a_lost_share_is_repaired_by_three_helpers() {
        let keys = dkg_in_process(OsRng).unwrap();
        let mut rng = OsRng;
        let repaired = repair_in_process(&keys, &[1, 3, 4], 2, &mut rng).unwrap();
        let two = signer_id(2).unwrap();
        assert_eq!(repaired.signing_share(), keys.key_packages[&two].signing_share());
        let mut packages = keys.key_packages.clone();
        packages.insert(two, repaired);
        sign_in_process(&packages, &keys.public_key_package, &[2, 3, 5], &[3u8; 32], &random_alpha()).unwrap();
        // Two helpers are not enough, and nobody helps repair itself.
        assert!(repair_in_process(&keys, &[1, 3], 2, &mut rng).is_err());
        assert!(repair_deltas(&[1, 2, 3], &keys.key_packages[&signer_id(1).unwrap()], 2, &mut rng).is_err());
    }

    #[test]
    fn two_signers_cannot_sign() {
        let keys = dkg_in_process(OsRng).unwrap();
        let err = sign_with(&keys, &[2, 4], &[1u8; 32], &random_alpha()).unwrap_err();
        assert!(err.to_string().contains("at least 3"), "{err}");
    }
}
