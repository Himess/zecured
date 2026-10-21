//! Zecured Swap, shared by the maker bot (`zecured-swap-maker`) and the CLI (`zecured-wallet swap`).
//!
//! USDC escrowed on Base (`ZecuredSwap`, contracts/src/ZecuredSwap.sol) is released by a
//! zero-knowledge proof of a shielded ZEC payment, made by whoever paid (tier2/payment-statement).
//! On Buy the taker locks USDC and the maker pays the ZEC; on Sell the maker locks USDC and the
//! taker pays. The payer pays one Ironwood note of at least the order's zatoshi, with the memo
//! `"ZSWP" || orderId`, to the receiver whose `keccak256(salt || receiver)` the order names, with
//! `ovk = None`; it keeps the note's opening ([`pay`]) to build the payment witness and prove it
//! ([`seal`]), then submits the proof and finalizes the order once the relay has moved on
//! ([`chain`]). The maker signs quotes under EIP-712 ([`quote`]).
//!
//! Privacy: the recipient's receiver and salt travel off-chain only; Base sees the commitment.
//! Nothing here logs a receiver, a salt, a viewing or spending key, or a Zcash txid.

use alloy::{
    primitives::{Address, B256, U256, keccak256},
    sol_types::SolValue,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use zcash_protocol::memo::MemoBytes;
use zecured_payment_statement::payment_memo;
pub use zecured_payment_statement::{MEMO_BINDING_LENGTH, MEMO_PREFIX};

pub mod abi;
pub mod chain;
pub mod pay;
pub mod quote;
pub mod seal;

/// `SwapQuote.side`: the taker buys ZEC (locks USDC; the maker pays the ZEC).
pub const SIDE_BUY: u8 = 0;
/// `SwapQuote.side`: the taker sells ZEC (the maker locks USDC; the taker pays the ZEC).
pub const SIDE_SELL: u8 = 1;
/// `VersionKind.Swap` of the verifier version registry.
pub const VERSION_SWAP: u8 = 2;
/// USDC has 6 decimals, ZEC 8 (zatoshi).
pub const USDC_PER_DOLLAR: u64 = 1_000_000;
pub const ZAT_PER_ZEC: u64 = 100_000_000;

/// Which way the ZEC goes, from the taker's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_u8(self) -> u8 {
        match self {
            Side::Buy => SIDE_BUY,
            Side::Sell => SIDE_SELL,
        }
    }

    pub fn from_u8(side: u8) -> Result<Self> {
        match side {
            SIDE_BUY => Ok(Side::Buy),
            SIDE_SELL => Ok(Side::Sell),
            other => bail!("unknown side {other}"),
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "buy" => Ok(Side::Buy),
            "sell" => Ok(Side::Sell),
            other => bail!("side must be buy or sell, not {other:?}"),
        }
    }
}

/// `OrderStatus` of ZecuredSwap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    None,
    Open,
    Settled,
    Refunded,
}

impl OrderStatus {
    pub fn from_u8(status: u8) -> Result<Self> {
        match status {
            0 => Ok(OrderStatus::None),
            1 => Ok(OrderStatus::Open),
            2 => Ok(OrderStatus::Settled),
            3 => Ok(OrderStatus::Refunded),
            other => bail!("unknown order status {other}"),
        }
    }
}

/// `keccak256(abi.encode(chainId, swap, maker, nonce))`: the id ZecuredSwap gives the order of
/// `maker`'s quote `nonce` (`orderIdOf`).
pub fn order_id(chain_id: u64, swap: Address, maker: Address, nonce: U256) -> B256 {
    keccak256((U256::from(chain_id), swap, maker, nonce).abi_encode())
}

/// `keccak256(salt32 || receiver43)`: how a recipient appears on Base.
pub fn receiver_commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> B256 {
    B256::from(zecured_payment_statement::receiver_commitment(salt, receiver))
}

/// The payment's memo: `"ZSWP" || orderId` (the rest zero, as `MemoBytes::from_bytes` pads it).
pub fn payment_memo_bytes(order_id: B256) -> MemoBytes {
    MemoBytes::from_bytes(&payment_memo(&order_id.0)[..MEMO_BINDING_LENGTH]).expect("36 bytes fit a memo")
}

/// 32 bytes from the operating system's CSPRNG (salts, nonces).
pub fn random_bytes32() -> [u8; 32] {
    use rand_core_06::{OsRng, RngCore};
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order id vector of tier2/payment-k4 and contracts/test/unit/ZecuredSwap.t.sol.
    #[test]
    fn the_order_id_vector() {
        let id = order_id(84_532, Address::repeat_byte(0x11), Address::repeat_byte(0x22), U256::from(7));
        assert_eq!(id, "0xd13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f".parse::<B256>().unwrap());
    }

    #[test]
    fn the_memo_is_the_statements() {
        let id = B256::repeat_byte(0xab);
        let memo = payment_memo_bytes(id);
        assert_eq!(&memo.as_array()[..4], b"ZSWP");
        assert_eq!(&memo.as_array()[4..36], id.as_slice());
        assert!(memo.as_array()[36..].iter().all(|b| *b == 0));
        assert_eq!(zecured_payment_statement::memo_order_id(memo.as_array()), Some(id.0));
    }

    #[test]
    fn the_receiver_commitment_vector() {
        assert_eq!(
            receiver_commitment(&[0x11; 32], &[0x22; 43]),
            "0xbae7b4976157459acb192cab961fec4003a1c8714e9cdee2a2bfa7f779ef6e10".parse::<B256>().unwrap()
        );
    }

    #[test]
    fn sides_and_statuses() {
        assert_eq!(Side::from_u8(0).unwrap(), Side::Buy);
        assert_eq!(Side::from_u8(1).unwrap(), Side::Sell);
        assert!(Side::from_u8(2).is_err());
        assert_eq!(Side::parse("sell").unwrap().as_u8(), SIDE_SELL);
        assert!(Side::parse("Buy").is_err());
        assert_eq!(OrderStatus::from_u8(3).unwrap(), OrderStatus::Refunded);
        assert!(OrderStatus::from_u8(4).is_err());
    }
}
