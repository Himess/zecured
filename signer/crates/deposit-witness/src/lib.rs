//! Tier 2 (docs/TIER2.md): Zcash chain data for trust-minimized deposit proofs.
//!
//! A deposit proof shows that a deposit transaction is in a block committed to by a recent
//! header on the Base relay, without revealing which block: since NU5 every header's
//! `hashBlockCommitments` commits to the ZIP 221 history tree of the earlier blocks of its
//! network-upgrade epoch, and since NU6.3 that tree also records the Ironwood pool (ZIP 258).
//! This crate rebuilds that tree from public data and checks it against real headers, and builds
//! the witnesses of deposit, violation and (Zecured Swap) payment proofs.

pub mod chain;
pub mod history;
pub mod mmr;
pub mod p2p;
pub mod payment;
pub mod violation;
pub mod witness;

pub use zecured_zcash_ops::network::ZcashNetwork;

/// The statements' parameters for Zcash `network`. Their guests are built per network (the
/// statements' `NETWORK`); a builder checks a witness natively with the parameters of the network
/// it reads, which must be the network of the guest that proves it.
pub fn statement_network(network: ZcashNetwork) -> &'static zecured_deposit_statement::Network {
    match network {
        ZcashNetwork::Testnet => &zecured_deposit_statement::TESTNET,
        ZcashNetwork::Mainnet => &zecured_deposit_statement::MAINNET,
    }
}
