//! Builds the private input of a Tier 2 deposit proof from public chain data plus what the
//! depositor knows (the vault's address, viewing key and expiry; the deposit note is found by
//! trial decryption with the viewing key), then checks it with the statement natively.
//!
//!   cargo run -p zecured-deposit-witness --bin build-witness -- \
//!     --deposit-txid <txid> --vault vault.json --tip <height> [--mode refund [--action <i>]] \
//!     --headers data/tier2/headers_nu7.json [--update-headers] [--network mainnet] --out witness.json
//!
//! `vault.json` holds `vault_id`, `owner`, `account`, `address`, `ufvk`, `expiry_height` and
//! `amount`, the values of the vault's `VaultOpened` (a coordinator vault record has them). The
//! witness holds the vault's tag key K, not its viewing key: whoever proves it can link the
//! vault's notes and payouts on Zcash, so hand it only to a prover you trust with that.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use zcash_protocol::TxId;
use zecured_deposit_witness::{
    ZcashNetwork, p2p,
    witness::{self, Mode, Vault},
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
    /// `activate`: the vault's deposit (D5, its amount, by its expiry height). `refund`: any
    /// Ironwood note to the vault's address, refunded through a claim (T-02).
    #[arg(long, default_value = "activate", value_parser = ["activate", "refund"])]
    mode: String,
    /// The action of the transaction that creates the note (default: the first that fits).
    #[arg(long)]
    action: Option<u32>,
    /// JSON Lines headers from the epoch's activation on (NU7 on Testnet).
    #[arg(long)]
    headers: PathBuf,
    /// First extend the header file to the network's tip over the P2P network.
    #[arg(long)]
    update_headers: bool,
    /// The Zcash network of the transaction (`testnet` or `mainnet`): its peers, encodings and
    /// epochs, and the statement the witness is checked with (prove it with that network's guest).
    #[arg(long, default_value = "testnet", value_parser = ["testnet", "mainnet"])]
    network: String,
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
    let network = ZcashNetwork::from_name(&args.network)?;
    let vault = vault_from_json(&args.vault)?;
    if args.update_headers {
        let path = args.headers.clone();
        let height =
            tokio::task::spawn_blocking(move || p2p::extend_file(network, &path, &p2p::seed_peers(network))).await??;
        println!("headers up to {height}");
    }
    let mut client = lwd::connect(&args.lwd).await?;
    lwd::check_network(&mut client, network).await?;
    let txid = TxId::from_hex(&args.deposit_txid).context("txid")?;
    let mode = if args.mode == "refund" {
        Mode::Refund
    } else {
        Mode::Activate
    };
    let b =
        witness::build(&mut client, network, &args.headers, &args.summaries, txid, &vault, mode, args.action, args.tip)
            .await?;
    fs::write(&args.out, serde_json::to_vec(&b.witness)?)?;
    println!(
        "deposit at {} (tx {} of {}), leaf {} of {}, path {} steps, peak {} of {}",
        b.deposit_height, b.tx_index, b.block_transactions, b.leaf_index, b.leaves, b.path_len, b.peak_index, b.peaks
    );
    println!(
        "journal: mode {} branch 0x{:08x} tip {} amount {} tag {} VaultOpened {}",
        b.journal.mode,
        b.journal.branch_id,
        hex::encode(b.journal.tip_hash),
        b.journal.amount,
        hex::encode(b.journal.tag),
        hex::encode(b.journal.vault_opened_hash)
    );
    println!(
        "witness written to {} (it holds the vault's tag key: it links the vault's notes)",
        args.out.display()
    );
    Ok(())
}
