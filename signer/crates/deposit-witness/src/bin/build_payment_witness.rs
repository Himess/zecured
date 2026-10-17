//! Zecured Swap: builds the private input of a payment proof from public chain data plus what the
//! payer kept when it built the payment (the note's opening and the order's salt), then checks it
//! with the payment statement natively.
//!
//!   cargo run -p zecured-deposit-witness --bin build-payment-witness -- \
//!     --txid <txid> --opening opening.json --tip <height> \
//!     [--order <0x orderId>] [--commitment <0x receiver commitment>] \
//!     --headers data/tier2/headers_nu7.json [--update-headers] --out witness.json
//!
//! `opening.json` holds `action_index`, `recipient` (the 43-byte raw receiver, hex), `value`
//! (zatoshi), `rseed` and `salt` (hex): what the maker bot and `zecured-wallet swap sell` keep for
//! each payment. The witness holds the recipient's receiver: keep it with the payer and its prover
//! only. This is also how a real Testnet payment becomes a fixture for the payment statement.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use zcash_protocol::TxId;
use zecured_deposit_witness::{
    p2p,
    payment::{self, PaymentOpening},
};
use zecured_zcash_ops::lwd;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    txid: String,
    /// JSON with the payment note's opening and the order's salt.
    #[arg(long)]
    opening: PathBuf,
    /// Height of the header the proof is anchored to (after the payment's block).
    #[arg(long)]
    tip: u32,
    /// The order the payment must name (its memo), if known.
    #[arg(long)]
    order: Option<String>,
    /// The receiver commitment the payment must pay, if known.
    #[arg(long)]
    commitment: Option<String>,
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

fn hex32(s: &str, what: &str) -> Result<[u8; 32]> {
    hex::decode(s.trim().trim_start_matches("0x"))?.try_into().map_err(|_| anyhow!("{what}: 32 bytes"))
}

fn opening_from_json(path: &PathBuf) -> Result<PaymentOpening> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let text = |key: &str| v[key].as_str().with_context(|| format!("opening {key}"));
    Ok(PaymentOpening {
        action_index: u32::try_from(v["action_index"].as_u64().context("opening action_index")?)?,
        recipient: hex::decode(text("recipient")?.trim_start_matches("0x"))?,
        value: v["value"].as_u64().context("opening value")?,
        rseed: hex32(text("rseed")?, "rseed")?,
        salt: hex32(text("salt")?, "salt")?,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let opening = opening_from_json(&args.opening)?;
    if args.update_headers {
        let path = args.headers.clone();
        let height = tokio::task::spawn_blocking(move || p2p::extend_file(&path, &p2p::seed_peers())).await??;
        println!("headers up to {height}");
    }
    let order = args.order.as_deref().map(|s| hex32(s, "order")).transpose()?;
    let commitment = args.commitment.as_deref().map(|s| hex32(s, "commitment")).transpose()?;
    let mut client = lwd::connect(&args.lwd).await?;
    let txid = TxId::from_hex(&args.txid).context("txid")?;
    let b = payment::build(&mut client, &args.headers, &args.summaries, txid, &opening, order, commitment, args.tip)
        .await?;
    fs::write(&args.out, serde_json::to_vec(&b.witness)?)?;
    println!(
        "payment at {} (tx {} of {}), leaf {} of {}",
        b.height, b.tx_index, b.block_transactions, b.leaf_index, b.leaves
    );
    println!(
        "journal: branch 0x{:08x} anchor {} order {} zatoshi {}",
        b.journal.branch_id,
        hex::encode(b.journal.anchor),
        hex::encode(b.journal.order_id),
        b.journal.zatoshi
    );
    println!("witness written to {} (it holds the recipient's receiver: keep it private)", args.out.display());
    Ok(())
}
