//! Extends a JSON Lines header file (`{ height, hash, raw }`) to the tip of its Zcash network
//! (Testnet unless `mainnet` follows the file) over the P2P network (read-only; every header's
//! hash and linkage are checked).
//!
//!   cargo run -p zecured-deposit-witness --bin update-headers -- data/tier2/headers_nu7.json [mainnet]

use std::path::PathBuf;

use anyhow::{Context, Result};
use zecured_deposit_witness::{ZcashNetwork, p2p};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().context("usage: update-headers <headers.json> [testnet|mainnet]")?);
    let network = args.next().map_or(Ok(ZcashNetwork::Testnet), |n| ZcashNetwork::from_name(&n))?;
    let height = p2p::extend_file(network, &path, &p2p::seed_peers(network))?;
    println!("{height}");
    Ok(())
}
