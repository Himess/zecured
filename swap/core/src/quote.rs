//! The maker's EIP-712 quote (domain "ZecuredSwap" "1", the deployed ZecuredSwap as verifying
//! contract): signing and checking, as ZecuredSwap's `open` checks it for an EOA maker.

use alloy::{
    primitives::{Address, B256, Bytes},
    signers::{Signature, SignerSync, local::PrivateKeySigner},
    sol_types::{Eip712Domain, SolStruct, eip712_domain},
};
use anyhow::{Context, Result, anyhow, ensure};

use crate::abi::SwapQuote;

/// The frozen type string (docs/private/V4_FREEZE.md § 5; IZecuredSwap.sol).
pub const SWAP_QUOTE_TYPE: &str = "SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,uint32 payWindow,uint64 quoteDeadline,uint256 nonce)";

/// ZecuredSwap's EIP-712 domain.
pub fn swap_domain(chain_id: u64, swap: Address) -> Eip712Domain {
    eip712_domain! {
        name: "ZecuredSwap",
        version: "1",
        chain_id: chain_id,
        verifying_contract: swap,
    }
}

/// The digest a maker signs (`ZecuredSwap.hashSwapQuote`).
pub fn quote_digest(domain: &Eip712Domain, q: &SwapQuote) -> B256 {
    q.eip712_signing_hash(domain)
}

/// The maker's 65-byte signature of `q` (r || s || v).
pub fn sign_quote(signer: &PrivateKeySigner, domain: &Eip712Domain, q: &SwapQuote) -> Result<Bytes> {
    let sig = signer.sign_hash_sync(&quote_digest(domain, q)).map_err(|e| anyhow!("sign the quote: {e}"))?;
    Ok(Bytes::from(sig.as_bytes().to_vec()))
}

/// Checks that the EOA `q.maker` signed `q` (a contract maker is checked by ZecuredSwap through
/// ERC-1271 only).
pub fn check_quote_signature(domain: &Eip712Domain, q: &SwapQuote, signature: &[u8]) -> Result<()> {
    ensure!(signature.len() == 65, "a quote signature is 65 bytes");
    let sig = Signature::try_from(signature).context("malformed quote signature")?;
    let signer = sig
        .recover_address_from_prehash(&quote_digest(domain, q))
        .context("cannot recover the quote's signer")?;
    ensure!(signer == q.maker, "the quote is not signed by its maker");
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{U256, keccak256};

    use super::*;
    use crate::SIDE_SELL;

    fn quote(maker: Address) -> SwapQuote {
        SwapQuote {
            maker,
            taker: Address::repeat_byte(0x33),
            side: SIDE_SELL,
            zatoshi: 50_000_000,
            usdc: U256::from(20_000_000u64),
            makerReceiverCommitment: B256::repeat_byte(0x44),
            payWindow: 21_600,
            quoteDeadline: 1_760_000_120,
            nonce: U256::from(7),
        }
    }

    /// The type string is the frozen one, byte for byte (alloy derives it from the struct).
    #[test]
    fn the_type_is_the_frozen_one() {
        let q = quote(Address::ZERO);
        assert_eq!(SwapQuote::eip712_encode_type(), SWAP_QUOTE_TYPE);
        assert_eq!(q.eip712_type_hash(), keccak256(SWAP_QUOTE_TYPE));
    }

    /// `hashStruct` is `keccak256(typehash || the nine fields as ABI words)`, as ZecuredSwap's
    /// `_hashQuote` computes it.
    #[test]
    fn the_struct_hash_is_the_contracts() {
        let q = quote(Address::repeat_byte(0x22));
        let mut words = keccak256(SWAP_QUOTE_TYPE).to_vec();
        let word = |v: &[u8]| {
            let mut w = [0u8; 32];
            w[32 - v.len()..].copy_from_slice(v);
            w
        };
        words.extend_from_slice(&word(q.maker.as_slice()));
        words.extend_from_slice(&word(q.taker.as_slice()));
        words.extend_from_slice(&word(&[q.side]));
        words.extend_from_slice(&word(&q.zatoshi.to_be_bytes()));
        words.extend_from_slice(&q.usdc.to_be_bytes::<32>());
        words.extend_from_slice(q.makerReceiverCommitment.as_slice());
        words.extend_from_slice(&word(&q.payWindow.to_be_bytes()));
        words.extend_from_slice(&word(&q.quoteDeadline.to_be_bytes()));
        words.extend_from_slice(&q.nonce.to_be_bytes::<32>());
        assert_eq!(q.eip712_hash_struct(), keccak256(words));
    }

    /// The digest vector of contracts/test/unit/ZecuredSwap.t.sol (`test_eip712_digestVector`,
    /// computed by the contract's `hashSwapQuote`): alloy's EIP-712 encoder gives the same.
    #[test]
    fn the_digest_vector() {
        let q = SwapQuote {
            maker: Address::repeat_byte(0x22),
            taker: Address::repeat_byte(0x33),
            side: SIDE_SELL,
            zatoshi: 50_000_000,
            usdc: U256::from(19_800_000u64),
            makerReceiverCommitment: B256::repeat_byte(0x44),
            payWindow: 21_600,
            quoteDeadline: 1_760_000_120,
            nonce: U256::from(7),
        };
        let domain = swap_domain(84_532, Address::repeat_byte(0x11));
        assert_eq!(
            quote_digest(&domain, &q),
            "0xd3e594a519b8f06b57c3151ddf712e48edafdccf3d65f5079b0bdbbff4c89c00".parse::<B256>().unwrap()
        );
    }

    #[test]
    fn signatures_are_checked_against_the_maker() {
        let signer = PrivateKeySigner::random();
        let domain = swap_domain(84_532, Address::repeat_byte(0x11));
        let q = quote(signer.address());
        let sig = sign_quote(&signer, &domain, &q).unwrap();
        check_quote_signature(&domain, &q, &sig).unwrap();
        // Another field, another deployment, another chain, another signer: refused.
        let mut other = q.clone();
        other.usdc += U256::from(1);
        assert!(check_quote_signature(&domain, &other, &sig).is_err());
        assert!(check_quote_signature(&swap_domain(84_532, Address::repeat_byte(0x12)), &q, &sig).is_err());
        assert!(check_quote_signature(&swap_domain(8_453, Address::repeat_byte(0x11)), &q, &sig).is_err());
        let stranger = PrivateKeySigner::random();
        assert!(check_quote_signature(&domain, &q, &sign_quote(&stranger, &domain, &q).unwrap()).is_err());
        assert!(check_quote_signature(&domain, &q, &sig[..64]).is_err());
    }
}
