//! The recipient commitment of an Ironwood receiver, for a destination that must be fixed before
//! any backend runs: the treasury's `FEE_COMMITMENT` of a deployment with the opening fee on. It
//! draws a fresh salt, writes the receiver and the salt to a file only its owner can read (to be
//! registered with the coordinator, `POST /recipients`, once it runs), and prints the commitment,
//! `keccak256(salt32 || receiver43)` (docs/INTERFACES.md § 1), and nothing else.
//!
//!   zecured-recipient --address <unified address> --out data/treasury-recipient.json

use std::{fs, io::Write, path::Path, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rand_core_06::{OsRng, RngCore};
use zcash_protocol::consensus::Network;
use zecured_policy::ledger::commitment;
use zecured_zcash_ops::keys::ironwood_receiver;

#[derive(Parser)]
#[command(about = "Draws a salt for an Ironwood receiver and prints its recipient commitment")]
struct Args {
    /// The receiver's unified address (it must have an Orchard-protocol receiver).
    #[arg(long)]
    address: String,
    /// Where the receiver and the salt are written (refused if it exists: a salt is drawn once).
    #[arg(long)]
    out: PathBuf,
    /// Zcash Mainnet addresses (default Testnet).
    #[arg(long)]
    mainnet: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.out.exists() {
        bail!("{} exists: a recipient's salt is drawn once", args.out.display());
    }
    let network = if args.mainnet {
        Network::MainNetwork
    } else {
        Network::TestNetwork
    };
    let receiver = ironwood_receiver(&network, &args.address)?.to_raw_address_bytes();
    let mut salt = [0u8; 32];
    OsRng.fill_bytes(&mut salt);
    let c = commitment(&salt, &receiver);
    let body = serde_json::json!({
        "receiver": args.address,
        "salt": format!("0x{}", hex::encode(salt)),
        "commitment": format!("0x{}", hex::encode(c)),
    });
    write_private(&args.out, &serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", args.out.display()))?;
    println!("0x{}", hex::encode(c));
    Ok(())
}

/// Creates `path` readable by its owner only (0600 on Unix), never overwriting it.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
