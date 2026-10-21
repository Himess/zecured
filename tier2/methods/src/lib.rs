//! The deposit guest (Tier 2), the violation guest (Tier 3) and Zecured Swap's payment guest,
//! embedded (ELF and image ID) by risc0-build, for one Zcash network: Testnet, or Mainnet with the
//! `mainnet` feature.

/// The network the embedded guests prove on (`test` or `main`, as `zecured_deposit_statement`'s
/// `Network::name`): their image ids are `deposit-<network>`, `violation-<network>` and
/// `payment-<network>`.
pub const GUEST_NETWORK: &str = if cfg!(feature = "mainnet") { "main" } else { "test" };

include!(concat!(env!("OUT_DIR"), "/methods.rs"));
