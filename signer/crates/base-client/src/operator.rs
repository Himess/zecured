//! The signers' messages outside of vault payouts, and the signing layer they share.
//!
//! - Operator messages (O-3), which `zecured-ops` prepares, signs, collects and submits:
//!   `PauseMinting` and `UnpauseMinting` (the Intake's domain, 3 of 5), `ScheduleVersion` (all 5) and
//!   `RemoveVersion` (3 of 5; all 5 for the last working deposit version, R-23) in the version
//!   registry's domain, and `ReimburseDestination` (the Registry's domain, all 5 since R-6).
//! - Liveness receipts (R-5): `DestinationReceived`, which a signer node signs once it holds and has
//!   validated a claim's destination, under the bond's domain (`ZecuredSignerBond` `1`);
//!   `SignerBondV2.armLiveness` accepts any signer's.
//!
//! Every type string matches the contracts byte for byte (`ZecuredEIP712.sol`, and v4's interface
//! freeze for `DestinationReceived`); the tests pin them with golden digests that Foundry's `cast`
//! computed independently.

use alloy::{
    primitives::{Address, B256, Bytes, FixedBytes, U256, keccak256},
    signers::Signature,
    sol,
    sol_types::{Eip712Domain, SolCall, SolStruct, eip712_domain},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::events::{IZecuredIntake, IZecuredRegistry, IZecuredVersions};

sol! {
    /// R-5: signed by one signer node once it holds the claim's destination preimage (salt and
    /// receiver) and has validated it: a valid Ironwood receiver whose commitment is the claim's.
    /// Arms the claim's liveness clock from `max(createdAt, receivedAt)`.
    #[derive(Debug, PartialEq, Eq)]
    struct DestinationReceived {
        bytes32 claimId;
        bytes32 commitment;
        uint64 receivedAt;
    }

    /// T-03: three signers pause new minting until `until`.
    #[derive(Debug, PartialEq, Eq)]
    struct PauseMinting {
        uint64 until;
        uint256 nonce;
        uint64 deadline;
    }

    /// T-03: three signers lift the pause.
    #[derive(Debug, PartialEq, Eq)]
    struct UnpauseMinting {
        uint256 nonce;
        uint64 deadline;
    }

    /// T-07a: all five signers schedule a verifier version.
    #[derive(Debug, PartialEq, Eq)]
    struct ScheduleVersion {
        uint8 kind;
        bytes4 selector;
        bytes32 imageId;
        uint32[4] branches;
        uint256 nonce;
        uint64 deadline;
    }

    /// T-07a: three signers remove a verifier version (all five for the last working deposit
    /// version, R-23).
    #[derive(Debug, PartialEq, Eq)]
    struct RemoveVersion {
        bytes32 versionId;
        uint256 nonce;
        uint64 deadline;
    }

    /// T-05, R-6: all five signers point a claim the bonds paid at the destination that reimburses
    /// them.
    #[derive(Debug, PartialEq, Eq)]
    struct ReimburseDestination {
        bytes32 claimId;
        bytes32 commitment;
        uint64 deadline;
    }
}

/// The Intake's domain (`PauseMinting`, `UnpauseMinting`).
pub fn intake_domain(chain_id: u64, intake: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredIntake",
        version: "1",
        chain_id: chain_id,
        verifying_contract: intake,
    }
}

/// The version registry's domain (`ScheduleVersion`, `RemoveVersion`).
pub fn versions_domain(chain_id: u64, versions: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredVersions",
        version: "1",
        chain_id: chain_id,
        verifying_contract: versions,
    }
}

/// `SignerBondV2`'s domain (R-5: `DestinationReceived`).
pub fn bond_domain(chain_id: u64, bond: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredSignerBond",
        version: "1",
        chain_id: chain_id,
        verifying_contract: bond,
    }
}

/// The receipt a node signs for `claim_id`, whose destination is `commitment`, held since
/// `received_at` (Unix seconds).
pub fn destination_received(claim_id: B256, commitment: B256, received_at: u64) -> DestinationReceived {
    DestinationReceived {
        claimId: claim_id,
        commitment,
        receivedAt: received_at,
    }
}

/// `ZecuredVersions.VersionKind` (v4 appends `Swap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionKind {
    Deposit,
    Violation,
    Swap,
}

impl VersionKind {
    pub fn code(self) -> u8 {
        match self {
            VersionKind::Deposit => crate::events::VERSION_DEPOSIT,
            VersionKind::Violation => crate::events::VERSION_VIOLATION,
            VersionKind::Swap => crate::events::VERSION_SWAP,
        }
    }

    pub fn from_code(code: u8) -> Result<Self> {
        Ok(match code {
            crate::events::VERSION_DEPOSIT => VersionKind::Deposit,
            crate::events::VERSION_VIOLATION => VersionKind::Violation,
            crate::events::VERSION_SWAP => VersionKind::Swap,
            other => bail!("unknown version kind {other}"),
        })
    }
}

/// Which contract verifies an operator message, and so whose domain it is signed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verifier {
    Intake,
    Versions,
    Registry,
}

/// One operator message, as `zecured-ops` writes it to a file for every signer to sign.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum OperatorMessage {
    /// T-03: pause new minting until `until` (Unix seconds); exits never stop.
    PauseMinting { until: u64, nonce: U256, deadline: u64 },
    /// T-03: lift the pause.
    UnpauseMinting { nonce: U256, deadline: u64 },
    /// T-07a: schedule a verifier version.
    #[serde(rename_all = "camelCase")]
    ScheduleVersion {
        kind: VersionKind,
        selector: FixedBytes<4>,
        image_id: B256,
        branches: [u32; 4],
        nonce: U256,
        deadline: u64,
    },
    /// T-07a: remove a verifier version.
    #[serde(rename_all = "camelCase")]
    RemoveVersion {
        version_id: B256,
        nonce: U256,
        deadline: u64,
    },
    /// T-05, R-6: the Reimburse destination of a claim the bonds paid.
    #[serde(rename_all = "camelCase")]
    ReimburseDestination {
        claim_id: B256,
        commitment: B256,
        deadline: u64,
    },
}

impl OperatorMessage {
    /// The contract that verifies it.
    pub fn verifier(&self) -> Verifier {
        match self {
            OperatorMessage::PauseMinting { .. } | OperatorMessage::UnpauseMinting { .. } => Verifier::Intake,
            OperatorMessage::ScheduleVersion { .. } | OperatorMessage::RemoveVersion { .. } => Verifier::Versions,
            OperatorMessage::ReimburseDestination { .. } => Verifier::Registry,
        }
    }

    /// Distinct signer signatures the contract needs at least. `RemoveVersion` of the last working
    /// deposit version needs all five (R-23); that depends on the chain (see
    /// [`removal_threshold`]).
    pub fn threshold(&self) -> usize {
        match self {
            OperatorMessage::ScheduleVersion { .. } | OperatorMessage::ReimburseDestination { .. } => 5,
            _ => 3,
        }
    }

    /// The deadline the contract checks (`now <= deadline`).
    pub fn deadline(&self) -> u64 {
        match self {
            OperatorMessage::PauseMinting { deadline, .. }
            | OperatorMessage::UnpauseMinting { deadline, .. }
            | OperatorMessage::ScheduleVersion { deadline, .. }
            | OperatorMessage::RemoveVersion { deadline, .. }
            | OperatorMessage::ReimburseDestination { deadline, .. } => *deadline,
        }
    }

    /// Its domain, for the verifying contract `contract` on chain `chain_id`.
    pub fn domain(&self, chain_id: u64, contract: Address) -> Eip712Domain {
        match self.verifier() {
            Verifier::Intake => intake_domain(chain_id, contract),
            Verifier::Versions => versions_domain(chain_id, contract),
            Verifier::Registry => crate::registry_domain(chain_id, contract),
        }
    }

    /// The EIP-712 struct hash.
    pub fn struct_hash(&self) -> B256 {
        match self {
            OperatorMessage::PauseMinting { until, nonce, deadline } => PauseMinting {
                until: *until,
                nonce: *nonce,
                deadline: *deadline,
            }
            .eip712_hash_struct(),
            OperatorMessage::UnpauseMinting { nonce, deadline } => UnpauseMinting {
                nonce: *nonce,
                deadline: *deadline,
            }
            .eip712_hash_struct(),
            OperatorMessage::ScheduleVersion {
                kind,
                selector,
                image_id,
                branches,
                nonce,
                deadline,
            } => ScheduleVersion {
                kind: kind.code(),
                selector: *selector,
                imageId: *image_id,
                branches: *branches,
                nonce: *nonce,
                deadline: *deadline,
            }
            .eip712_hash_struct(),
            OperatorMessage::RemoveVersion {
                version_id,
                nonce,
                deadline,
            } => RemoveVersion {
                versionId: *version_id,
                nonce: *nonce,
                deadline: *deadline,
            }
            .eip712_hash_struct(),
            OperatorMessage::ReimburseDestination {
                claim_id,
                commitment,
                deadline,
            } => ReimburseDestination {
                claimId: *claim_id,
                commitment: *commitment,
                deadline: *deadline,
            }
            .eip712_hash_struct(),
        }
    }

    /// The digest every signer signs: `keccak256(0x1901 || domainSeparator || structHash)`.
    pub fn signing_hash(&self, chain_id: u64, contract: Address) -> B256 {
        let mut buf = Vec::with_capacity(66);
        buf.extend_from_slice(&[0x19, 0x01]);
        buf.extend_from_slice(self.domain(chain_id, contract).separator().as_slice());
        buf.extend_from_slice(self.struct_hash().as_slice());
        keccak256(buf)
    }

    /// The transaction's calldata with `sigs` (already sorted by signer address).
    pub fn calldata(&self, sigs: Vec<Bytes>) -> Bytes {
        let encoded = match self {
            OperatorMessage::PauseMinting { until, nonce, deadline } => IZecuredIntake::pauseMintingCall {
                until: *until,
                nonce: *nonce,
                deadline: *deadline,
                sigs,
            }
            .abi_encode(),
            OperatorMessage::UnpauseMinting { nonce, deadline } => IZecuredIntake::unpauseMintingCall {
                nonce: *nonce,
                deadline: *deadline,
                sigs,
            }
            .abi_encode(),
            OperatorMessage::ScheduleVersion {
                kind,
                selector,
                image_id,
                branches,
                nonce,
                deadline,
            } => IZecuredVersions::scheduleVersionCall {
                kind: kind.code(),
                selector: *selector,
                imageId: *image_id,
                branches: *branches,
                nonce: *nonce,
                deadline: *deadline,
                sigs,
            }
            .abi_encode(),
            OperatorMessage::RemoveVersion {
                version_id,
                nonce,
                deadline,
            } => IZecuredVersions::removeVersionCall {
                versionId: *version_id,
                nonce: *nonce,
                deadline: *deadline,
                sigs,
            }
            .abi_encode(),
            OperatorMessage::ReimburseDestination {
                claim_id,
                commitment,
                deadline,
            } => IZecuredRegistry::setReimburseDestinationCall {
                claimId: *claim_id,
                commitment: *commitment,
                deadline: *deadline,
                sigs,
            }
            .abi_encode(),
        };
        Bytes::from(encoded)
    }

    /// One line for the operator: what is being signed (ids, times and nonces only).
    pub fn describe(&self) -> String {
        match self {
            OperatorMessage::PauseMinting { until, nonce, deadline } => {
                format!("PauseMinting until {until} (nonce {nonce}, signatures valid until {deadline})")
            }
            OperatorMessage::UnpauseMinting { nonce, deadline } => {
                format!("UnpauseMinting (nonce {nonce}, signatures valid until {deadline})")
            }
            OperatorMessage::ScheduleVersion {
                kind,
                selector,
                image_id,
                branches,
                nonce,
                deadline,
            } => format!(
                "ScheduleVersion {kind:?} selector {selector} image {image_id} branches {branches:x?} (nonce {nonce}, signatures valid until {deadline})"
            ),
            OperatorMessage::RemoveVersion {
                version_id,
                nonce,
                deadline,
            } => format!("RemoveVersion {version_id} (nonce {nonce}, signatures valid until {deadline})"),
            OperatorMessage::ReimburseDestination {
                claim_id,
                commitment,
                deadline,
            } => format!(
                "ReimburseDestination of claim {claim_id} to commitment {commitment} (signatures valid until {deadline})"
            ),
        }
    }
}

/// R-23: signatures a `RemoveVersion` of a `kind` version needs, given the other versions of that
/// kind that are active and working (`others_working`): five for the last working deposit version,
/// three otherwise. The last working violation version cannot go at all (R-22): `None`.
pub fn removal_threshold(kind: VersionKind, others_working: usize) -> Option<usize> {
    match (kind, others_working) {
        (VersionKind::Deposit, 0) => Some(5),
        (VersionKind::Violation, 0) => None,
        _ => Some(3),
    }
}

/// secp256k1 n / 2: OpenZeppelin's `ECDSA` rejects larger `s` (malleability).
const HALF_N: U256 = alloy::primitives::uint!(0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0_U256);

/// Whether OpenZeppelin's `ECDSA.recover` accepts the encoding: 65 bytes `r || s || v`, `v` 27 or 28,
/// non-zero `r` and `s`, low `s`.
pub fn signature_encoding_ok(sig: &[u8]) -> bool {
    if sig.len() != 65 || !(sig[64] == 27 || sig[64] == 28) {
        return false;
    }
    let r = U256::from_be_slice(&sig[..32]);
    let s = U256::from_be_slice(&sig[32..64]);
    !r.is_zero() && !s.is_zero() && s <= HALF_N
}

/// The signer of `sig` over `digest`, if the contracts accept the encoding.
pub fn recover_digest(digest: B256, sig: &[u8]) -> Result<Address> {
    ensure!(signature_encoding_ok(sig), "not a 65-byte low-s signature with v = 27 or 28");
    let signature = Signature::try_from(sig).context("malformed signature")?;
    signature.recover_address_from_prehash(&digest).context("cannot recover the signer")
}

/// The signatures of `digest` by members of `signers`, at most one per signer, sorted by strictly
/// increasing signer address as the contracts require. A signature by anyone else, or unreadable,
/// is an error: a bundle never mixes in a stranger's.
pub fn sorted_signatures(digest: B256, sigs: &[Bytes], signers: &[Address]) -> Result<Vec<(Address, Bytes)>> {
    let mut out: Vec<(Address, Bytes)> = Vec::with_capacity(sigs.len());
    for (i, sig) in sigs.iter().enumerate() {
        let signer = recover_digest(digest, sig).with_context(|| format!("signature {}", i + 1))?;
        if !signers.contains(&signer) {
            return Err(anyhow!("signature {} is not by a signer of the Registry's set", i + 1));
        }
        if out.iter().any(|(a, _)| *a == signer) {
            continue; // the same signer twice counts once
        }
        out.push((signer, sig.clone()));
    }
    out.sort_by_key(|(a, _)| *a);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloy::signers::{SignerSync, local::PrivateKeySigner};

    use super::*;
    use crate::{claim_id_of, registry_domain};

    const CHAIN: u64 = crate::BASE_SEPOLIA_CHAIN_ID;
    const INTAKE: Address = Address::repeat_byte(0x66);
    const VERSIONS: Address = Address::repeat_byte(0x99);
    const BOND: Address = Address::repeat_byte(0xcc);
    const REGISTRY: Address = Address::repeat_byte(0x11);
    const DEADLINE: u64 = 1_800_000_000;

    fn b(h: &str) -> B256 {
        h.parse().unwrap()
    }

    fn image() -> B256 {
        b("0x1a2ba0a861d63a6be1a6cac73141244fa7a178419df41a6d4b0b92cc06d02599")
    }

    /// The type strings of `ZecuredEIP712.sol` and the v4 freeze (§ 3 for `DestinationReceived`).
    #[test]
    fn type_strings_match_the_contracts() {
        assert_eq!(
            DestinationReceived::eip712_encode_type(),
            "DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)"
        );
        assert_eq!(PauseMinting::eip712_encode_type(), "PauseMinting(uint64 until,uint256 nonce,uint64 deadline)");
        assert_eq!(UnpauseMinting::eip712_encode_type(), "UnpauseMinting(uint256 nonce,uint64 deadline)");
        assert_eq!(
            ScheduleVersion::eip712_encode_type(),
            "ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)"
        );
        assert_eq!(
            RemoveVersion::eip712_encode_type(),
            "RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)"
        );
        assert_eq!(
            ReimburseDestination::eip712_encode_type(),
            "ReimburseDestination(bytes32 claimId,bytes32 commitment,uint64 deadline)"
        );
    }

    /// Golden vectors, computed independently with Foundry's `cast keccak` and `cast abi-encode` from
    /// the type strings (domains: chain 84532, Intake 0x6666..66, ZecuredVersions 0x9999..99,
    /// SignerBondV2 0xcccc..cc, Registry 0x1111..11; claim id `claimIdOf(0x5555..55, 0)`, commitment
    /// 0x7777..77, deadline 1,800,000,000).
    #[test]
    fn digests_match_the_golden_vectors() {
        assert_eq!(
            intake_domain(CHAIN, INTAKE).separator(),
            b("0x9fd12188572b48f7b8b30bdaa49c6badb5aa0297e5c00eb7a3b57e52dc7668d3")
        );
        assert_eq!(
            versions_domain(CHAIN, VERSIONS).separator(),
            b("0x251a4f038712200d785975c4afffaf2937e75f72e32a0c95ea04c90e9f747843")
        );
        assert_eq!(
            bond_domain(CHAIN, BOND).separator(),
            b("0x00078ee823a391fcf68eda3215eee753cd755459cdb3bc119891d2c342fceda7")
        );
        let claim = claim_id_of(B256::repeat_byte(0x55), 0);
        assert_eq!(claim, b("0x16cb43d034ed6e80566ab57c4dd5697e92d5a8866db3f0ec1bfc47ad711ba7c9"));
        let commitment = B256::repeat_byte(0x77);

        let receipt = destination_received(claim, commitment, DEADLINE);
        assert_eq!(
            receipt.eip712_hash_struct(),
            b("0xd8b114405dbd6fb2200d17716e4ab5570bba574f27d3a789c6865ce5ad5a7bfa")
        );
        assert_eq!(
            receipt.eip712_signing_hash(&bond_domain(CHAIN, BOND)),
            b("0x8e63ed8155098182bd32ef741ef6320f61c750efc9efa3c4bb6a6328e31d78b5")
        );

        let cases: Vec<(OperatorMessage, Address, &str)> = vec![
            (
                OperatorMessage::PauseMinting {
                    until: 1_800_086_400,
                    nonce: U256::from(3),
                    deadline: DEADLINE,
                },
                INTAKE,
                "0xea44651610d4d110b0f497487790eddb187ee3a65aa793c9270c45830434b74f",
            ),
            (
                OperatorMessage::UnpauseMinting {
                    nonce: U256::from(4),
                    deadline: DEADLINE,
                },
                INTAKE,
                "0xb0cc1f6efb6478b29fdada1adfd100aa8064fb23d0b79302632b2ac1e384cb56",
            ),
            (
                OperatorMessage::ScheduleVersion {
                    kind: VersionKind::Swap,
                    selector: FixedBytes([0x73, 0xc4, 0x57, 0xba]),
                    image_id: image(),
                    branches: [0x7719_0ad9, 0, 0, 0],
                    nonce: U256::from(5),
                    deadline: DEADLINE,
                },
                VERSIONS,
                "0xbb9a6d44554528da532f3110c46d73c38bf75ae9cba1fa34bb0570147bfa5e96",
            ),
            (
                OperatorMessage::RemoveVersion {
                    version_id: b("0xcdeaed45bb5cbded667b6b4c68534550bf17c141440d0da9ffacb180b46848b1"),
                    nonce: U256::from(6),
                    deadline: DEADLINE,
                },
                VERSIONS,
                "0xcad1bfcab49b38c83799a56c4d63569843348e3ab2bcca4f0452b180ed313465",
            ),
            (
                OperatorMessage::ReimburseDestination {
                    claim_id: claim,
                    commitment,
                    deadline: DEADLINE,
                },
                REGISTRY,
                "0x1b7140e43386510d466173985e8bc361ec549c85d79905e5dfaed888ca342f68",
            ),
        ];
        for (message, contract, digest) in cases {
            assert_eq!(message.signing_hash(CHAIN, contract), b(digest), "{}", message.describe());
            // The same digest as alloy's typed-data signing hash under the message's domain.
            let domain = message.domain(CHAIN, contract);
            let via_alloy = match &message {
                OperatorMessage::ReimburseDestination {
                    claim_id,
                    commitment,
                    deadline,
                } => ReimburseDestination {
                    claimId: *claim_id,
                    commitment: *commitment,
                    deadline: *deadline,
                }
                .eip712_signing_hash(&domain),
                _ => message.signing_hash(CHAIN, contract),
            };
            assert_eq!(via_alloy, b(digest));
        }
        // The swap version id these vectors remove: `versionIdOf(Swap, selector, imageId)`.
        assert_eq!(
            crate::events::version_id(VersionKind::Swap.code(), [0x73, 0xc4, 0x57, 0xba], image()),
            b("0xcdeaed45bb5cbded667b6b4c68534550bf17c141440d0da9ffacb180b46848b1")
        );
        assert_eq!(registry_domain(CHAIN, REGISTRY).separator(), crate::registry_domain(CHAIN, REGISTRY).separator());
    }

    /// The message file round-trips, names its fields as an operator reads them, and every message
    /// is signed under its own contract's domain with its own threshold.
    #[test]
    fn messages_serialize_and_know_their_verifier() {
        let schedule = OperatorMessage::ScheduleVersion {
            kind: VersionKind::Deposit,
            selector: FixedBytes([1, 2, 3, 4]),
            image_id: image(),
            branches: [0x7719_0ad9, 0, 0, 0],
            nonce: U256::from(9),
            deadline: DEADLINE,
        };
        let json = serde_json::to_value(&schedule).unwrap();
        assert_eq!(json["type"], "schedule-version");
        assert_eq!(json["kind"], "deposit");
        assert_eq!(json["imageId"], serde_json::json!(image()));
        assert_eq!(serde_json::from_value::<OperatorMessage>(json).unwrap(), schedule);
        assert_eq!((schedule.verifier(), schedule.threshold()), (Verifier::Versions, 5));
        let pause = OperatorMessage::PauseMinting {
            until: 1,
            nonce: U256::ZERO,
            deadline: 2,
        };
        assert_eq!((pause.verifier(), pause.threshold(), pause.deadline()), (Verifier::Intake, 3, 2));
        let reimburse = OperatorMessage::ReimburseDestination {
            claim_id: B256::ZERO,
            commitment: B256::ZERO,
            deadline: 3,
        };
        assert_eq!((reimburse.verifier(), reimburse.threshold()), (Verifier::Registry, 5), "R-6: all five");
        // A message for one contract never verifies under another's domain.
        assert_ne!(pause.signing_hash(CHAIN, INTAKE), pause.signing_hash(CHAIN, VERSIONS));
        assert_ne!(pause.signing_hash(CHAIN, INTAKE), pause.signing_hash(1, INTAKE));
    }

    /// R-22 and R-23 as the contracts apply them.
    #[test]
    fn removal_thresholds() {
        assert_eq!(removal_threshold(VersionKind::Deposit, 0), Some(5));
        assert_eq!(removal_threshold(VersionKind::Deposit, 1), Some(3));
        assert_eq!(removal_threshold(VersionKind::Violation, 0), None);
        assert_eq!(removal_threshold(VersionKind::Violation, 1), Some(3));
        assert_eq!(removal_threshold(VersionKind::Swap, 0), Some(3), "no last-version rule for swaps");
    }

    /// Signatures are checked against the signer set, counted once per signer and sorted by address.
    #[test]
    fn signature_sets_are_checked_and_sorted() {
        let keys: Vec<PrivateKeySigner> = (0..5).map(|_| PrivateKeySigner::random()).collect();
        let set: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let message = OperatorMessage::UnpauseMinting {
            nonce: U256::from(1),
            deadline: DEADLINE,
        };
        let digest = message.signing_hash(CHAIN, INTAKE);
        let sign = |k: &PrivateKeySigner| Bytes::from(k.sign_hash_sync(&digest).unwrap().as_bytes().to_vec());
        let mut sigs: Vec<Bytes> = keys[..3].iter().map(sign).collect();
        sigs.push(sign(&keys[0])); // the same signer again
        let sorted = sorted_signatures(digest, &sigs, &set).unwrap();
        assert_eq!(sorted.len(), 3);
        assert!(sorted.windows(2).all(|w| w[0].0 < w[1].0));
        // A stranger's signature, or one over another message, is refused.
        let stranger = PrivateKeySigner::random();
        assert!(sorted_signatures(digest, &[sign(&stranger)], &set).is_err());
        let other = OperatorMessage::UnpauseMinting {
            nonce: U256::from(2),
            deadline: DEADLINE,
        }
        .signing_hash(CHAIN, INTAKE);
        let wrong = Bytes::from(keys[1].sign_hash_sync(&other).unwrap().as_bytes().to_vec());
        assert!(sorted_signatures(digest, &[wrong], &set).is_err());
        // High-s encodings are refused before recovery.
        let mut high = sigs[0].to_vec();
        high[64] = 29;
        assert!(recover_digest(digest, &high).is_err());
    }

    /// The calldata is the contract's function with the sorted signatures.
    #[test]
    fn calldata_encodes_the_contract_call() {
        let message = OperatorMessage::PauseMinting {
            until: 10,
            nonce: U256::from(2),
            deadline: 20,
        };
        let data = message.calldata(vec![Bytes::from(vec![1u8; 65])]);
        assert_eq!(&data[..4], IZecuredIntake::pauseMintingCall::SELECTOR.as_slice());
        let decoded = IZecuredIntake::pauseMintingCall::abi_decode(&data).unwrap();
        assert_eq!((decoded.until, decoded.nonce, decoded.deadline), (10, U256::from(2), 20));
        assert_eq!(decoded.sigs.len(), 1);
    }
}
