//! Research helper (Tier 2): does a light wallet server give every txid of a block, so a
//! deposit's txid can be proven against the header's merkleRoot? Prints the block's compact
//! transactions with all pool types requested, and the SHA-256d Merkle root over them.
//!
//!   cargo run -p zecured-zcash-ops --example block_txids -- <txid-hex-display-order>

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use zcash_client_backend::proto::service::{BlockId, BlockRange};
use zcash_protocol::TxId;
use zecured_zcash_ops::lwd;

fn sha256d(data: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(data)).into()
}

/// Bitcoin-style Merkle root (internal byte order; an odd last node is paired with itself).
fn merkle_root(mut level: Vec<[u8; 32]>) -> [u8; 32] {
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level.chunks(2).map(|p| sha256d(&[p[0], p[1]].concat())).collect();
    }
    level[0]
}

#[tokio::main]
async fn main() -> Result<()> {
    let txid_hex = std::env::args().nth(1).context("usage: block_txids <txid>")?;
    let txid = TxId::from_hex(&txid_hex).context("txid hex")?; // display order (reversed) hex
    let mut client = lwd::connect("https://testnet.zec.rocks:443").await?;
    let (raw, height) = lwd::raw_transaction(&mut client, &txid).await?;
    let height = u32::try_from(height)?;
    println!("tx {txid} at height {height}, {} bytes", raw.len());

    for (label, pools) in [("default", vec![]), ("all pools", vec![1, 2, 3, 4])] {
        let mut stream = client
            .get_block_range(BlockRange {
                start: Some(BlockId {
                    height: height.into(),
                    hash: vec![],
                }),
                end: Some(BlockId {
                    height: height.into(),
                    hash: vec![],
                }),
                pool_types: pools,
            })
            .await?
            .into_inner();
        let block = stream.message().await?.context("no block")?;
        let indices: Vec<u64> = block.vtx.iter().map(|t| t.index).collect();
        let txids: Vec<[u8; 32]> = block.vtx.iter().map(|t| t.txid.clone().try_into().unwrap()).collect();
        println!(
            "[{label}] block {} hash {} prev {} vtx {} indices {:?} header bytes {}",
            block.height,
            hex::encode(block.hash.iter().rev().copied().collect::<Vec<u8>>()),
            hex::encode(block.prev_hash.iter().rev().copied().collect::<Vec<u8>>()),
            block.vtx.len(),
            indices,
            block.header.len()
        );
        println!("[{label}] merkle root over these txids (internal order): {}", hex::encode(merkle_root(txids)));
    }
    Ok(())
}
