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

/// Signs `message` under `domain`; returns the 65-byte `r || s || v` (v = 27/28) form that
/// OpenZeppelin's `ECDSA.recover` accepts.
pub fn sign_typed<T: SolStruct>(signer: &PrivateKeySigner, domain: &Eip712Domain, message: &T) -> Result<Bytes> {
    let digest = message.eip712_signing_hash(domain);
    let sig = signer
        .sign_hash_sync(&digest)
        .map_err(|e| anyhow!("sign EIP-712 digest: {e}"))?;
    Ok(Bytes::from(sig.as_bytes().to_vec()))
}

/// Recovers the EOA that signed `message` under `domain`.
pub fn recover_typed<T: SolStruct>(domain: &Eip712Domain, message: &T, signature: &[u8]) -> Result<Address> {
    ensure!(signature.len() == 65, "signature must be 65 bytes");
    let sig = Signature::try_from(signature).context("malformed signature")?;
    let digest = message.eip712_signing_hash(domain);
    sig.recover_address_from_prehash(&digest)
        .context("cannot recover signer")
}

/// Orders signatures by strictly increasing signer address, as the Registry requires.
/// Fails on duplicate signers.
pub fn order_signatures<T: SolStruct>(
    domain: &Eip712Domain,
    message: &T,
    signatures: &[Bytes],
) -> Result<Vec<Bytes>> {
    let mut with_signer = signatures
        .iter()
        .map(|s| Ok((recover_typed(domain, message, s)?, s.clone())))
        .collect::<Result<Vec<_>>>()?;
    with_signer.sort_by_key(|(a, _)| *a);
    ensure!(
        with_signer.windows(2).all(|w| w[0].0 < w[1].0),
        "duplicate signer"
    );
    Ok(with_signer.into_iter().map(|(_, s)| s).collect())
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

    #[test]
    fn claim_ids_hash_is_packed_keccak() {
        let ids = [B256::repeat_byte(1), B256::repeat_byte(2)];
        let mut packed = vec![1u8; 32];
        packed.extend([2u8; 32]);
        assert_eq!(claim_ids_hash(&ids), keccak256(packed));
    }
}
