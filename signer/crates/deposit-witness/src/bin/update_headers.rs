//! Extends a JSON Lines header file (`{ height, hash, raw }`) to the Zcash Testnet tip over the
//! P2P network (read-only; every header's hash and linkage are checked).
//!
//!   cargo run -p zecured-deposit-witness --bin update-headers -- data/tier2/headers_nu7.json

use std::path::PathBuf;

use anyhow::{Context, Result};
use zecured_deposit_witness::p2p;

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).context("usage: update-headers <headers.json>")?);
    let height = p2p::extend_file(&path, &p2p::seed_peers())?;
    println!("{height}");
    Ok(())
}
