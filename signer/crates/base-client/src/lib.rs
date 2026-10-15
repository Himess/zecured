//! Base side of the signer network: EIP-712 messages, Registry ABI and event ingestion.
//!
//! Every type string here must match docs/INTERFACES.md § 2 and the Solidity contracts
//! byte for byte; the tests pin the type strings.

use alloy::{
    primitives::{Address, B256, Bytes, U256, keccak256},
    signers::{Signature, SignerSync, local::PrivateKeySigner},
    sol,
    sol_types::{Eip712Domain, SolStruct, eip712_domain},
};
use anyhow::{Context, Result, anyhow, ensure};

pub mod events;
pub mod journal;
pub mod relay;
pub mod theft_demo_guard;
pub mod verify;

/// Base Sepolia chain id.
pub const BASE_SEPOLIA_CHAIN_ID: u64 = 84_532;

sol! {
    /// Signed by signer nodes; verified by the Registry.
    #[derive(Debug, PartialEq, Eq)]
    struct DepositAttestation {
        bytes32 vaultId;
        address account;
        uint64 amount;
        bytes32 depositTag;
    }

    /// Signed by signer nodes; verified by the Registry.
    #[derive(Debug, PartialEq, Eq)]
    struct PayoutConfirmation {
        bytes32 vaultId;
        bytes32 claimIdsHash;
    }

    /// Signed by signer nodes before they refund the conforming deposit of a vault that was
    /// never activated (D23, L-8); verified by the Registry (`cancelVault`).
    #[derive(Debug, PartialEq, Eq)]
    struct VaultCancellation {
        bytes32 vaultId;
    }

    /// Off-chain, signed by the vault owner key; verified by every signer node.
    #[derive(Debug, PartialEq, Eq)]
    struct OpenVault {
        address owner;
        address account;
        uint64 amount;
        bytes32 requestId;
        uint64 deadline;
    }

    /// Off-chain, signed by the vault owner key: pre-validates a payout destination.
    #[derive(Debug, PartialEq, Eq)]
    struct Destination {
        bytes32 vaultId;
        bytes32 commitment;
        uint64 nonce;
        uint64 deadline;
    }

    /// Off-chain, signed by the vault owner key: refund of a non-conforming note (P5).
    #[derive(Debug, PartialEq, Eq)]
    struct RefundRequest {
        bytes32 vaultId;
        bytes32 noteId;
        bytes32 commitment;
        uint64 nonce;
        uint64 deadline;
    }

    /// Off-chain, signed by the vault owner key: release of the vault's viewing key.
    #[derive(Debug, PartialEq, Eq)]
    struct ViewKeyRequest {
        bytes32 vaultId;
        uint64 nonce;
        uint64 deadline;
    }

    /// Signed by every signer node once the vault's DKG is complete, with values the node derived
    /// or checked itself; verified by the owner (app, CLI) against its own pinned signer set before
    /// the vault's address is shown (security review C-4), and on Base against the struct hash a
    /// deposit proof computes. v2 (guest round, T-09): `tagKeyHash` binds the vault's tag key, so a
    /// proof can derive its deposit tag with that key only.
    #[derive(Debug, PartialEq, Eq)]
    struct VaultOpened {
        bytes32 vaultId;
        address owner;
        address account;
        uint64 amount;
        uint32 expiryHeight;
        bytes32 addressHash;
        bytes32 tagKeyHash;
    }

    /// Tier 3 (docs/TIER3.md § 3): signed by signer nodes in FROST round 1, before any share is
    /// released; recorded by the Registry's `commitSpend`. `claimIds[0]` bears the fee;
    /// `claimRefs[i] = keccak256(salt_i || txid)` belongs to `claimIds[i]`, which the payout's
    /// action `claimOutputs[i]` pays (E1).
    #[derive(Debug, PartialEq, Eq)]
    struct SpendCommit {
        bytes32 vaultId;
        bytes32 inputTag;
        bytes32[] claimIds;
        bytes32[] claimRefs;
        uint8[] claimOutputs;
        bytes32 payoutRef;
        bytes32 changeTag;
        uint64 changeValue;
    }

    /// S18 self-deleverage: signed by the vault owner through the smart account (ERC-1271),
    /// verified by the Adapter's `deleverageWithCollateral`.
    #[derive(Debug, PartialEq, Eq)]
    struct DeleverageRequest {
        bytes32 vaultId;
        address account;
        address executor;
        uint16 marginBps;
        uint256 repayAssets;
        uint64 maxCollateral;
        uint256 nonce;
        uint64 deadline;
    }

    /// Signed by the vault owner through the smart account (ERC-1271), verified by the Adapter.
    #[derive(Debug, PartialEq, Eq)]
    struct CloseRequest {
        bytes32 vaultId;
        address account;
        address executor;
        uint16 marginBps;
        uint64 minUserAmount;
        bytes32 userCommitment;
        uint256 nonce;
        uint64 deadline;
    }
}

/// The Registry's EIP-712 domain (attestations, payout confirmations, owner requests).
pub fn registry_domain(chain_id: u64, registry: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredRegistry",
        version: "1",
        chain_id: chain_id,
        verifying_contract: registry,
    }
}

/// The SettlementAdapter's EIP-712 domain (close requests).
pub fn adapter_domain(chain_id: u64, adapter: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredSettlementAdapter",
        version: "1",
        chain_id: chain_id,
        verifying_contract: adapter,
    }
}

/// `keccak256(abi.encodePacked(claimIds))`.
pub fn claim_ids_hash(claim_ids: &[B256]) -> B256 {
    let mut packed = Vec::with_capacity(claim_ids.len() * 32);
    for id in claim_ids {
        packed.extend_from_slice(id.as_slice());
    }
    keccak256(packed)
}

/// `keccak256` of a string's UTF-8 bytes: `VaultOpened.addressHash` is this of the vault's unified
/// address, exactly as the owner gets it.
pub fn text_hash(text: &str) -> B256 {
    keccak256(text.as_bytes())
}

/// `VaultOpened.tagKeyHash`: `keccak256(K)` of the vault's tag key
/// `K = BLAKE2b-256("Zecured_TagKey__", UFVK string)` (D24), the UFVK exactly as the owner gets it.
/// Binding K rather than the viewing key lets a deposit proof carry K alone, and makes every tag
/// it derives the vault's own (design review, Claude Web § 2).
pub fn tag_key_hash(ufvk: &str) -> B256 {
    keccak256(zecured_policy::derive::tag_key(ufvk))
}

/// The `VaultOpened` message for a vault's address and viewing key (C-4).
pub fn vault_opened(
    vault_id: B256,
    owner: Address,
    account: Address,
    amount: u64,
    expiry_height: u32,
    address: &str,
    ufvk: &str,
) -> VaultOpened {
    VaultOpened {
        vaultId: vault_id,
        owner,
        account,
        amount,
        expiryHeight: expiry_height,
        addressHash: text_hash(address),
        tagKeyHash: tag_key_hash(ufvk),
    }
}

/// Checks that every member of `signers` (node index, attestation address) signed `message`
/// under `domain`: exactly one signature per index, recovering to that node's address, and no
/// signature from anyone else (C-4: a vault counts only when all five signers vouch for it).
pub fn check_signed_by_all<T: SolStruct>(
    domain: &Eip712Domain,
    message: &T,
    signers: &[(u16, Address)],
    signatures: &[(u16, Bytes)],
) -> Result<()> {
    ensure!(!signers.is_empty(), "no signer set to check against");
    for (k, (index, address)) in signers.iter().enumerate() {
        ensure!(
            signers[..k].iter().all(|(i, a)| i != index && a != address),
            "the signer set lists signer {index} or its address twice"
        );
    }
    if let Some((index, _)) = signatures.iter().find(|(i, _)| !signers.iter().any(|(s, _)| s == i)) {
        return Err(anyhow!("a signature claims to come from signer {index}, who is not in the signer set"));
    }
    for (index, expected) in signers {
        let mut theirs = signatures.iter().filter(|(i, _)| i == index);
        let (_, signature) = theirs.next().with_context(|| format!("signer {index} did not sign"))?;
        ensure!(theirs.next().is_none(), "signer {index} answered twice");
        let signer = recover_typed(domain, message, signature)
            .with_context(|| format!("signer {index}: unreadable signature"))?;
        ensure!(
            signer == *expected,
            "signer {index}: the signature is not by its attestation key, or for other values"
        );
    }
    Ok(())
}

/// `claimIdOf(vaultId, sequence) = keccak256(abi.encode(vaultId, uint64(sequence)))`.
pub fn claim_id_of(vault_id: B256, sequence: u64) -> B256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(vault_id.as_slice());
    buf[56..].copy_from_slice(&sequence.to_be_bytes());
    keccak256(buf)
}

/// Signs `message` under `domain`; returns the 65-byte `r || s || v` (v = 27/28) form that
/// OpenZeppelin's `ECDSA.recover` accepts.
pub fn sign_typed<T: SolStruct>(signer: &PrivateKeySigner, domain: &Eip712Domain, message: &T) -> Result<Bytes> {
    let digest = message.eip712_signing_hash(domain);
    let sig = signer.sign_hash_sync(&digest).map_err(|e| anyhow!("sign EIP-712 digest: {e}"))?;
    Ok(Bytes::from(sig.as_bytes().to_vec()))
}

/// Recovers the EOA that signed `message` under `domain`.
pub fn recover_typed<T: SolStruct>(domain: &Eip712Domain, message: &T, signature: &[u8]) -> Result<Address> {
    ensure!(signature.len() == 65, "signature must be 65 bytes");
    let sig = Signature::try_from(signature).context("malformed signature")?;
    let digest = message.eip712_signing_hash(domain);
    sig.recover_address_from_prehash(&digest).context("cannot recover signer")
}

/// Orders signatures by strictly increasing signer address, as the Registry requires.
/// Fails on duplicate signers.
pub fn order_signatures<T: SolStruct>(domain: &Eip712Domain, message: &T, signatures: &[Bytes]) -> Result<Vec<Bytes>> {
    let mut with_signer = signatures
        .iter()
        .map(|s| Ok((recover_typed(domain, message, s)?, s.clone())))
        .collect::<Result<Vec<_>>>()?;
    with_signer.sort_by_key(|(a, _)| *a);
    ensure!(with_signer.windows(2).all(|w| w[0].0 < w[1].0), "duplicate signer");
    Ok(with_signer.into_iter().map(|(_, s)| s).collect())
}

/// Gas limit with headroom over an `eth_estimateGas` result.
///
/// Estimates run against the latest block, but the transaction lands in a later one: Morpho
/// then accrues interest (calling the IRM), which costs more gas, and nested calls fail under
/// the 63/64 forwarding rule. Observed on Base Sepolia (2 s blocks) with `requestRelease`.
pub fn gas_with_headroom(estimate: u64) -> u64 {
    estimate.saturating_mul(13) / 10 + 50_000
}

/// Converts a zatoshi amount to the ABI's `uint64`.
pub fn zat(value: u64) -> u64 {
    value
}

/// Converts a 32-byte array to a `bytes32`.
pub fn b32(bytes: [u8; 32]) -> B256 {
    B256::from(bytes)
}

/// Converts a `uint256` nonce to `u128` when it fits (nonces are random 128-bit values).
impl From<&SpendCommit> for events::IZecuredRegistry::SpendCommit {
    fn from(c: &SpendCommit) -> Self {
        Self {
            vaultId: c.vaultId,
            inputTag: c.inputTag,
            claimIds: c.claimIds.clone(),
            claimRefs: c.claimRefs.clone(),
            claimOutputs: c.claimOutputs.clone(),
            payoutRef: c.payoutRef,
            changeTag: c.changeTag,
            changeValue: c.changeValue,
        }
    }
}

/// Bit `i` set for each signer `i` (index in the sorted set) among `signers`; the Registry records
/// the same bitmap for a commitment (`SpendRecord.signerBitmap`).
pub fn signer_bitmap(set: &[Address], signers: &[Address]) -> u8 {
    set.iter().enumerate().filter(|(_, a)| signers.contains(a)).fold(0u8, |m, (i, _)| m | (1 << i))
}

pub fn nonce_u128(n: U256) -> Result<u128> {
    u128::try_from(n).map_err(|_| anyhow!("nonce does not fit in 128 bits"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolStruct;

    #[test]
    fn type_strings_match_interfaces_md() {
        assert_eq!(
            DepositAttestation::eip712_encode_type(),
            "DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)"
        );
        assert_eq!(
            PayoutConfirmation::eip712_encode_type(),
            "PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)"
        );
        assert_eq!(VaultCancellation::eip712_encode_type(), "VaultCancellation(bytes32 vaultId)");
        assert_eq!(
            OpenVault::eip712_encode_type(),
            "OpenVault(address owner,address account,uint64 amount,bytes32 requestId,uint64 deadline)"
        );
        assert_eq!(
            Destination::eip712_encode_type(),
            "Destination(bytes32 vaultId,bytes32 commitment,uint64 nonce,uint64 deadline)"
        );
        assert_eq!(
            RefundRequest::eip712_encode_type(),
            "RefundRequest(bytes32 vaultId,bytes32 noteId,bytes32 commitment,uint64 nonce,uint64 deadline)"
        );
        assert_eq!(
            ViewKeyRequest::eip712_encode_type(),
            "ViewKeyRequest(bytes32 vaultId,uint64 nonce,uint64 deadline)"
        );
        assert_eq!(
            CloseRequest::eip712_encode_type(),
            "CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)"
        );
        assert_eq!(
            VaultOpened::eip712_encode_type(),
            "VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 tagKeyHash)"
        );
        assert_eq!(
            SpendCommit::eip712_encode_type(),
            "SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)"
        );
    }

    /// The Solidity library's golden vector (`GOLDEN_DELEVERAGE_REQUEST_DIGEST`).
    #[test]
    fn deleverage_request_golden_vector() {
        assert_eq!(
            DeleverageRequest::eip712_encode_type(),
            "DeleverageRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint256 repayAssets,uint64 maxCollateral,uint256 nonce,uint64 deadline)"
        );
        let msg = DeleverageRequest {
            vaultId: B256::repeat_byte(0x55),
            account: Address::repeat_byte(0x33),
            executor: Address::repeat_byte(0x44),
            marginBps: 100,
            repayAssets: U256::from(500_000),
            maxCollateral: 1_262_500,
            nonce: U256::from(42),
            deadline: 1_800_000_000,
        };
        let ad = adapter_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x22));
        assert_eq!(
            msg.eip712_signing_hash(&ad),
            "0x54c59f01064cf8a39e3c1036566f3e83c7cd9c5b21d3256c70f4adc4f1297211".parse::<B256>().unwrap()
        );
    }

    /// The Solidity library's golden vector (contracts/test/unit/ZecuredEIP712.t.sol,
    /// `GOLDEN_SPEND_COMMIT_DIGEST`, itself checked against Foundry's EIP-712 implementation).
    #[test]
    fn spend_commit_golden_vector() {
        let b = |h: &str| h.parse::<B256>().unwrap();
        let vault_id = B256::repeat_byte(0x55);
        let claim_id = |sequence: u64| {
            let mut encoded = [0u8; 64];
            encoded[..32].copy_from_slice(vault_id.as_slice());
            encoded[56..].copy_from_slice(&sequence.to_be_bytes());
            keccak256(encoded)
        };
        let msg = SpendCommit {
            vaultId: vault_id,
            inputTag: B256::repeat_byte(0x66),
            claimIds: vec![claim_id(0), claim_id(1)],
            claimRefs: vec![B256::repeat_byte(0x88), B256::repeat_byte(0x99)],
            claimOutputs: vec![2, 0],
            payoutRef: B256::repeat_byte(0xaa),
            changeTag: B256::repeat_byte(0xbb),
            changeValue: 3_800_000,
        };
        assert_eq!(msg.eip712_hash_struct(), b("0xc129e8c0fbddba658c07dded97e6c32a7c3af4a5ba4a24045faac75db662efe6"));
        let rd = registry_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        assert_eq!(
            msg.eip712_signing_hash(&rd),
            b("0x3dd9f2e94c96d5aa0a5c016684697f2283f0447f4d0eb81739052c0a07217d7a")
        );
    }

    /// A real Ironwood-only testnet UA and the UFVK of the first ZIP 2005 `use_qsk` vector
    /// (signer/crates/zcash-ops/tests/fixtures/zip2005_use_qsk.json; the UA encodes its default
    /// address with `encode_ironwood_ua`).
    const VECTOR_ADDRESS: &str = "utest1yv0hq77h4th2c882kdmcgt47xrm5zmhkfnfpw6dcack8e6kgfmg5new7vxed0trhkc67cmf2tjea8pnd09key4v85kmqdnz33yuas40f";
    const VECTOR_UFVK: &str = "uviewtest1rl58wrlhq2eqws08m2jxqg0gjpn2yd7me5f9cfkwmf4axnq0vjvjgvjs5t70tsny62rxklz3ha8t5cr8k30jltjmjr0xxsdhpqeqpf55w3dx0mracz99uvrvyf4hpsfxs2nc5jykraqvnt22zpkdce6tcj85lkty9lcpduhl72vz80l8ygn0q3sxn04w8";
    /// Foundry / anvil test account #0: a publicly known key, for test vectors only.
    const TEST_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn vector_vault_opened() -> VaultOpened {
        vault_opened(
            B256::repeat_byte(0x55),
            Address::repeat_byte(0x88),
            Address::repeat_byte(0x33),
            1_000_000,
            4_486_288,
            VECTOR_ADDRESS,
            VECTOR_UFVK,
        )
    }

    /// Cross-language vector (C-4, VaultOpened v2), asserted byte for byte by the app
    /// (app/lib/signers.test.ts), tier2/statement and contracts/test/unit/ZecuredEIP712.t.sol: the
    /// tag key and the hashes (also computed with Python's hashlib and pycryptodome), the digest
    /// and the signature of Foundry's test key.
    #[test]
    fn vault_opened_golden_vector() {
        let b = |h: &str| h.parse::<B256>().unwrap();
        let rd = registry_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        let msg = vector_vault_opened();
        assert_eq!(
            B256::from(zecured_policy::derive::tag_key(VECTOR_UFVK)),
            b("0x2f30a290051344422e07e110c75334c79f0fe84e23db3ea0d7666c0bedab8066")
        );
        assert_eq!(msg.addressHash, b("0x4871c7074d79798b7991d7f71f57e723823958fec9d143805fd5127ccfa55f59"));
        assert_eq!(msg.tagKeyHash, b("0x4998e90f251bfc9d596388325cf58132bb23a71bf6e71f6ff15275e11ad308b6"));
        assert_eq!(msg.eip712_hash_struct(), b("0xd63b647d9eba421dde278de2765b71900dc7372c7013dc925ae63dd3c28f5d13"));
        assert_eq!(
            msg.eip712_signing_hash(&rd),
            b("0x9b9497f4504df5fad1cae863057ab04daad3c761561ca930f7da90fddbcf080f")
        );
        let key: PrivateKeySigner = TEST_KEY.parse().unwrap();
        assert_eq!(key.address(), "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".parse::<Address>().unwrap());
        let signature = sign_typed(&key, &rd, &msg).unwrap();
        // Identical to `cast wallet sign --data` (Foundry 1.8.5) of the same typed data, and to
        // `cast wallet sign --no-hash` of the digest.
        assert_eq!(
            signature,
            "0x71640f0953e8ee9f7149ff3245a7aa8af5d3035d573ae9b85b23f2c04403798d75bb84946c4620fa2d3519759d95713b2269c2cc3cc0ca9905895d5145876f7d1b"
                .parse::<Bytes>()
                .unwrap()
        );
        assert_eq!(recover_typed(&rd, &msg, &signature).unwrap(), key.address());
    }

    /// `tagKeyHash` is keccak256 of the 32 bytes of K, not of the viewing key's text.
    #[test]
    fn tag_key_hash_is_keccak_of_the_tag_key() {
        let k = zecured_policy::derive::tag_key(VECTOR_UFVK);
        assert_eq!(tag_key_hash(VECTOR_UFVK), keccak256(k));
        assert_ne!(tag_key_hash(VECTOR_UFVK), text_hash(VECTOR_UFVK));
        assert_ne!(tag_key_hash(VECTOR_UFVK), tag_key_hash(&format!("{VECTOR_UFVK} ")));
    }

    #[test]
    fn text_hash_is_keccak_of_the_exact_utf8_bytes() {
        assert_eq!(text_hash(""), keccak256(b""));
        assert_eq!(text_hash("utest1x"), keccak256(b"utest1x"));
        // No normalization: the hash is of the string exactly as the owner gets it.
        assert_ne!(text_hash("utest1x"), text_hash("utest1x "));
        assert_ne!(text_hash("utest1x"), text_hash("UTEST1X"));
    }

    /// C-4: the vault counts only if each signer of the set signed the very values shown.
    #[test]
    fn every_signer_of_the_set_must_sign_the_same_vault() {
        let domain = registry_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        let keys: Vec<PrivateKeySigner> = (0..5).map(|_| PrivateKeySigner::random()).collect();
        let set: Vec<(u16, Address)> = keys.iter().zip(1u16..).map(|(k, i)| (i, k.address())).collect();
        let msg = vector_vault_opened();
        let sign = |k: &PrivateKeySigner, m: &VaultOpened| sign_typed(k, &domain, m).unwrap();
        let all: Vec<(u16, Bytes)> = keys.iter().zip(1u16..).map(|(k, i)| (i, sign(k, &msg))).collect();
        check_signed_by_all(&domain, &msg, &set, &all).unwrap();
        // Order does not matter.
        let reversed: Vec<(u16, Bytes)> = all.iter().rev().cloned().collect();
        check_signed_by_all(&domain, &msg, &set, &reversed).unwrap();

        let refused = |sigs: &[(u16, Bytes)], m: &VaultOpened| check_signed_by_all(&domain, m, &set, sigs).is_err();
        // One signer missing (four of five is not enough), or answering twice.
        assert!(refused(&all[..4], &msg));
        let mut twice = all.clone();
        twice.push(all[0].clone());
        assert!(refused(&twice, &msg));
        // Node 2 relays node 5's signature; a signature from outside the set.
        let mut swapped = all.clone();
        swapped[1].1 = all[4].1.clone();
        assert!(refused(&swapped, &msg));
        let mut stranger = all[..4].to_vec();
        stranger.push((5, sign(&PrivateKeySigner::random(), &msg)));
        assert!(refused(&stranger, &msg));
        let mut extra = all.clone();
        extra.push((6, sign(&keys[0], &msg)));
        assert!(refused(&extra, &msg));
        // The same signatures do not vouch for another address, tag key, expiry or owner.
        let other = |f: fn(&mut VaultOpened)| {
            let mut m = vector_vault_opened();
            f(&mut m);
            m
        };
        assert!(refused(&all, &other(|m| m.addressHash = text_hash("utest1attacker"))));
        assert!(refused(&all, &other(|m| m.tagKeyHash = tag_key_hash("uviewtest1attacker"))));
        assert!(refused(&all, &other(|m| m.expiryHeight += 1)));
        assert!(refused(&all, &other(|m| m.owner = Address::repeat_byte(0x99))));
        // Unusable signer sets.
        assert!(check_signed_by_all(&domain, &msg, &[], &[]).is_err());
        let dup = [set[0], set[0], set[2], set[3], set[4]];
        assert!(check_signed_by_all(&domain, &msg, &dup, &all).is_err());
    }

    #[test]
    fn sign_recover_and_order() {
        let domain = registry_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        let msg = DepositAttestation {
            vaultId: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            amount: 1_000_000,
            depositTag: B256::repeat_byte(3),
        };
        let signers: Vec<PrivateKeySigner> = (0..3).map(|_| PrivateKeySigner::random()).collect();
        let sigs: Vec<Bytes> = signers.iter().map(|s| sign_typed(s, &domain, &msg).unwrap()).collect();
        for (s, sig) in signers.iter().zip(&sigs) {
            assert_eq!(sig.len(), 65);
            assert!(sig[64] == 27 || sig[64] == 28, "v must be 27/28 for OpenZeppelin ECDSA");
            assert_eq!(recover_typed(&domain, &msg, sig).unwrap(), s.address());
        }
        let ordered = order_signatures(&domain, &msg, &sigs).unwrap();
        let addrs: Vec<Address> = ordered.iter().map(|s| recover_typed(&domain, &msg, s).unwrap()).collect();
        assert!(addrs.windows(2).all(|w| w[0] < w[1]));
        // A signature for another domain or message does not recover to the signer.
        let other = adapter_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        assert_ne!(recover_typed(&other, &msg, &sigs[0]).unwrap(), signers[0].address());
        // Duplicate signer is refused.
        assert!(order_signatures(&domain, &msg, &[sigs[0].clone(), sigs[0].clone()]).is_err());
    }

    /// Domain separators computed by the Solidity contracts (contracts/deployments dry run).
    #[test]
    fn domain_separators_match_solidity() {
        let registry: Address = "0xbdB942d882882bF795aA9c0Cd20ADe2115c73AAc".parse().unwrap();
        let adapter: Address = "0xC0a2A554dcFB5a1bd7e5E769A2Ad0c33BA0500fc".parse().unwrap();
        assert_eq!(
            registry_domain(BASE_SEPOLIA_CHAIN_ID, registry).separator(),
            "0x69d16596f5fe8b56a5a842fcca4461f0bef9d0a76fc625604317d9581a6b74c7".parse::<B256>().unwrap()
        );
        assert_eq!(
            adapter_domain(BASE_SEPOLIA_CHAIN_ID, adapter).separator(),
            "0x59e17d33c50be097bde376a1c88c46c405743fb47b43fea0eb1fac767095e5d4".parse::<B256>().unwrap()
        );
    }

    /// Golden vectors pinned in contracts/test/unit/ZecuredEIP712.t.sol (computed by the
    /// Solidity library, Foundry's EIP-712 implementation and `cast`).
    #[test]
    fn digests_match_solidity_golden_vectors() {
        let registry = Address::repeat_byte(0x11);
        let adapter = Address::repeat_byte(0x22);
        let vault = B256::repeat_byte(0x55);
        let rd = registry_domain(BASE_SEPOLIA_CHAIN_ID, registry);
        let ad = adapter_domain(BASE_SEPOLIA_CHAIN_ID, adapter);
        let b = |h: &str| h.parse::<B256>().unwrap();
        assert_eq!(rd.separator(), b("0x489271068185e1811817d2dfae78e2be898ee5fc9768bf01d870981f667ef9f8"));
        assert_eq!(ad.separator(), b("0xb62551e236af33b3b84da5fe38134bcea7c80b912f8b172bb4020080457a9260"));

        let att = DepositAttestation {
            vaultId: vault,
            account: Address::repeat_byte(0x33),
            amount: 1_000_000,
            depositTag: B256::repeat_byte(0x66),
        };
        assert_eq!(
            att.eip712_signing_hash(&rd),
            b("0xbe0251d9c909749194da8ebf137e42a33a60d6fba8672a022c4923558fecb90f")
        );

        let payout = PayoutConfirmation {
            vaultId: vault,
            claimIdsHash: claim_ids_hash(&[claim_id_of(vault, 0), claim_id_of(vault, 1)]),
        };
        assert_eq!(
            payout.eip712_signing_hash(&rd),
            b("0xf166a0191609bbccd2b7aa9e7527b1553a418f7626902cc10099034b838e4973")
        );

        let close = CloseRequest {
            vaultId: vault,
            account: Address::repeat_byte(0x33),
            executor: Address::repeat_byte(0x44),
            marginBps: 100,
            minUserAmount: 500_000,
            userCommitment: B256::repeat_byte(0x77),
            nonce: U256::from(42u8),
            deadline: 1_800_000_000,
        };
        assert_eq!(
            close.eip712_signing_hash(&ad),
            b("0x2207e7a8cd1b3966d87015d4b554f9271a760f2854c1c9d37ae0391b885ae8f7")
        );
    }

    /// The Registry rejects high-s signatures (OpenZeppelin ECDSA).
    #[test]
    fn signatures_are_low_s() {
        let half_n: U256 = "0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0".parse().unwrap();
        let domain = registry_domain(BASE_SEPOLIA_CHAIN_ID, Address::repeat_byte(0x11));
        let signer = PrivateKeySigner::random();
        for i in 0..64u8 {
            let msg = ViewKeyRequest {
                vaultId: B256::repeat_byte(i),
                nonce: u64::from(i),
                deadline: 1,
            };
            let sig = sign_typed(&signer, &domain, &msg).unwrap();
            let s_value = U256::from_be_slice(&sig[32..64]);
            assert!(s_value <= half_n, "high-s signature");
        }
    }

    #[test]
    fn claim_ids_hash_is_packed_keccak() {
        let ids = [B256::repeat_byte(1), B256::repeat_byte(2)];
        let mut packed = vec![1u8; 32];
        packed.extend([2u8; 32]);
        assert_eq!(claim_ids_hash(&ids), keccak256(packed));
    }
}
