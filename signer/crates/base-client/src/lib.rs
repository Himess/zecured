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
