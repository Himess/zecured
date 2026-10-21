//! Zecured signer policy: what a signer node is willing to sign (policies P0-P6 in docs/ARCHITECTURE.md).
//!
//! Pure functions only; every node evaluates them on its own view of both chains.

pub mod classify;
pub mod derive;
pub mod ledger;
pub mod treasury;
pub mod verify;
