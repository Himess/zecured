//! Tier 2 (docs/TIER2.md): Zcash chain data for trust-minimized deposit proofs.
//!
//! A deposit proof shows that a deposit transaction is in a block committed to by a recent
//! header on the Base relay, without revealing which block: since NU5 every header's
//! `hashBlockCommitments` commits to the ZIP 221 history tree of the earlier blocks of its
//! network-upgrade epoch, and since NU6.3 that tree also records the Ironwood pool (ZIP 258).
//! This crate rebuilds that tree from public data and checks it against real headers.

pub mod chain;
pub mod history;
pub mod mmr;
