//! The deposit guest (Tier 2) and the violation guest (Tier 3), embedded (ELF and image ID) by
//! risc0-build, for one Zcash network: Testnet, or Mainnet with the `mainnet` feature.

/// The network the embedded guests prove on (`test` or `main`, as `zecured_deposit_statement`'s
/// `Network::name`): their image ids are `deposit-<network>` and `violation-<network>`.
pub const GUEST_NETWORK: &str = if cfg!(feature = "mainnet") { "main" } else { "test" };

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
