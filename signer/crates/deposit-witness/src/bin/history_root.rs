//! Recomputes a Zcash header's `hashBlockCommitments` from public chain data: the ZIP 221 history
//! tree (V3) over the tip's epoch (NU7 on Testnet, NU6.3 on Mainnet until its NU7) up to the block
//! before the tip, and the tip's ZIP 244 auth data root. A match means the history tree a deposit
//! proof walks is the one the header (and therefore its proof of work) commits to.
//!
//!   cargo run -p zecured-deposit-witness --bin history-root -- \
//!     --tip 4488543 --headers data/tier2/headers_4465026_4488543.json [--network mainnet --lwd <url>]

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zecured_deposit_witness::{ZcashNetwork, chain, history};
use zecured_zcash_ops::lwd;

#[derive(Parser)]
struct Args {
    /// Height of the header whose commitments are recomputed.
    #[arg(long)]
    tip: u32,
    /// JSON Lines headers covering the epoch's activation .. tip.
    #[arg(long)]
    headers: PathBuf,
    #[arg(long, default_value = "https://testnet.zec.rocks:443")]
    lwd: String,
    /// The Zcash network (`testnet` or `mainnet`).
    #[arg(long, default_value = "testnet", value_parser = ["testnet", "mainnet"])]
    network: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let network = ZcashNetwork::from_name(&args.network)?;
    let epoch = history::epoch(network, args.tip)?;
    let headers = chain::load_headers(&args.headers)?;
    let tip_header = *headers.get(&args.tip).context("tip header missing")?;
    let mut client = lwd::connect(&args.lwd).await?;
    lwd::check_network(&mut client, network).await?;

    let started = std::time::Instant::now();
    let summaries = chain::block_summaries(&mut client, &headers, epoch.activation, args.tip - 1).await?;
    let leaves: Vec<_> = summaries.iter().map(|s| history::leaf(epoch.branch_id, s)).collect();
    let history_root = history::history_root(&leaves)?;
    println!(
        "history tree: {} leaves ({}..={}, branch 0x{:08x}), built in {:.1?}",
        leaves.len(),
        epoch.activation,
        args.tip - 1,
        epoch.branch_id,
        started.elapsed()
    );

    // The tip's transactions: txid Merkle root and auth data root.
    let tip_block = chain::compact_blocks(&mut client, args.tip, args.tip).await?.remove(0);
    let branch = BranchId::for_height(&network.params(), BlockHeight::from_u32(args.tip));
    let mut txids = vec![];
    let mut auth = vec![];
    for ctx in &tip_block.vtx {
        let txid = ctx.txid();
        let (raw, _) = lwd::raw_transaction(&mut client, &txid).await?;
        let tx = Transaction::read(&raw[..], branch).context("parse tip transaction")?;
        ensure!(tx.txid() == txid, "server returned another transaction for {txid}");
        txids.push(*txid.as_ref());
        auth.push(tx.auth_commitment().as_bytes().try_into()?);
    }
    let merkle_root = history::txid_merkle_root(&txids)?;
    ensure!(merkle_root == tip_header.merkle_root, "txid Merkle root does not match the tip header");
    let auth_root = history::auth_data_root(&auth)?;
    let commitments = history::block_commitments(&history_root, &auth_root);

    println!("tip {} ({} transactions)", args.tip, txids.len());
    println!("  hashChainHistoryRoot  {}", hex::encode(history_root));
    println!("  hashAuthDataRoot      {}", hex::encode(auth_root));
    println!("  computed commitments  {}", hex::encode(commitments));
    println!("  header commitments    {}", hex::encode(tip_header.block_commitments));
    ensure!(commitments == tip_header.block_commitments, "MISMATCH");
    println!("MATCH: the header commits to the rebuilt history tree");
    Ok(())
}
