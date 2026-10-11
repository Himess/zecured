//! Builds the private input of a Tier 2 deposit proof from public chain data plus what the
//! depositor knows (the vault's address, viewing key and expiry; the deposit note is found by
//! trial decryption with the viewing key), then checks it with the statement natively.
//!
//!   cargo run -p zecured-deposit-witness --bin build-witness -- \
//!     --deposit-txid <txid> --vault vault.json --tip <height> \
//!     --headers data/tier2/headers_nu7.json [--update-headers] --out witness.json
//!
//! `vault.json` holds `vault_id`, `owner`, `account`, `address`, `ufvk`, `expiry_height` and
//! `amount`, the values of the vault's `VaultOpened` (a coordinator vault record has them). The
//! witness contains the vault's viewing key: keep it local.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use zcash_protocol::TxId;
use zecured_deposit_witness::{
    p2p,
    witness::{self, Vault},
};
use zecured_zcash_ops::lwd;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    deposit_txid: String,
    /// JSON with the vault's `VaultOpened` values (`vault_id`, `owner`, `account`, `address`,
    /// `ufvk`, `expiry_height`, `amount`).
    #[arg(long)]
    vault: PathBuf,
    /// Height of the header the proof is anchored to (after the deposit's block).
    #[arg(long)]
    tip: u32,
    /// JSON Lines headers from NU7 activation on.
    #[arg(long)]
    headers: PathBuf,
    /// First extend the header file to the Testnet tip over the P2P network.
    #[arg(long)]
    update_headers: bool,
    /// Cache of per-block summaries (built on first use; a few minutes).
    #[arg(long, default_value = "data/tier2/summaries.json")]
    summaries: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "https://testnet.zec.rocks:443")]
    lwd: String,
}

fn vault_from_json(path: &PathBuf) -> Result<Vault> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let bytes = |key: &str| -> Result<Vec<u8>> {
        let text = v[key].as_str().with_context(|| format!("vault {key}"))?;
        Ok(hex::decode(text.trim_start_matches("0x"))?)
    };
    Ok(Vault {
        vault_id: bytes("vault_id")?.try_into().map_err(|_| anyhow!("vault_id: 32 bytes"))?,
        owner: bytes("owner")?.try_into().map_err(|_| anyhow!("owner: 20 bytes"))?,
        account: bytes("account")?.try_into().map_err(|_| anyhow!("account: 20 bytes"))?,
        address: v["address"].as_str().context("vault address")?.to_owned(),
        ufvk: v["ufvk"].as_str().context("vault ufvk")?.to_owned(),
        expiry_height: u32::try_from(v["expiry_height"].as_u64().context("expiry_height")?)?,
        amount: v["amount"].as_u64().context("amount")?,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let vault = vault_from_json(&args.vault)?;
    if args.update_headers {
        let path = args.headers.clone();
        let height = tokio::task::spawn_blocking(move || p2p::extend_file(&path, &p2p::seed_peers())).await??;
        println!("headers up to {height}");
    }
    let mut client = lwd::connect(&args.lwd).await?;
    let txid = TxId::from_hex(&args.deposit_txid).context("txid")?;
    let b = witness::build(&mut client, &args.headers, &args.summaries, txid, &vault, args.tip).await?;
    fs::write(&args.out, serde_json::to_vec(&b.witness)?)?;
    println!(
        "deposit at {} (tx {} of {}), leaf {} of {}, path {} steps, peak {} of {}",
        b.deposit_height, b.tx_index, b.block_transactions, b.leaf_index, b.leaves, b.path_len, b.peak_index, b.peaks
    );
    println!(
        "journal: tip {} amount {} tag {} VaultOpened {}",
        hex::encode(b.journal.tip_hash),
        b.journal.amount,
        hex::encode(b.journal.tag),
        hex::encode(b.journal.vault_opened_hash)
    );
    println!("witness written to {} (keep it local: it contains the vault's viewing key)", args.out.display());
    Ok(())
}
