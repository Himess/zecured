//! Public chain data for the history tree: per-block summaries rebuilt from a light wallet
//! server's compact blocks (all pool types) and full headers fetched over the Zcash P2P network
//! (`contracts/test/relay/tools/zcash_p2p_headers.py`).

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, anyhow, ensure};
use incrementalmerkletree::{Hashable, frontier::CommitmentTree};
use orchard::tree::MerkleHashOrchard;
use serde::Deserialize;
use zcash_client_backend::proto::{
    compact_formats::CompactBlock,
    service::{BlockId, BlockRange},
};
use zecured_zcash_ops::lwd::LwdClient;

use crate::history::{BlockSummary, Header};

/// Pool types to request: with all four, a compact block lists every transaction of the block
/// (with only the shielded ones, transparent-only transactions such as most coinbases are
/// left out and txid Merkle roots cannot be rebuilt).
pub const ALL_POOLS: [i32; 4] = [1, 2, 3, 4];

#[derive(Deserialize)]
struct HeaderLine {
    height: u32,
    raw: String,
}

/// Headers by height, from a JSON Lines file of `{ height, hash, raw }`.
pub fn load_headers(path: &Path) -> Result<BTreeMap<u32, Header>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let l: HeaderLine = serde_json::from_str(line).context("header line")?;
        let header = Header::parse(&hex::decode(&l.raw)?)?;
        if let Some((prev_height, prev)) = out.last_key_value() {
            let prev: &Header = prev;
            ensure!(
                *prev_height + 1 == l.height && header.prev_hash == prev.hash,
                "headers not linked at {}",
                l.height
            );
        }
        out.insert(l.height, header);
    }
    Ok(out)
}

/// The commitment trees of one pool, advanced block by block.
struct Trees {
    sapling: CommitmentTree<sapling::Node, { sapling::NOTE_COMMITMENT_TREE_DEPTH }>,
    orchard: CommitmentTree<MerkleHashOrchard, 32>,
    ironwood: CommitmentTree<MerkleHashOrchard, 32>,
}

fn root_bytes<H: Hashable + Clone, const D: u8>(
    tree: &CommitmentTree<H, D>,
    to_bytes: impl Fn(H) -> [u8; 32],
) -> [u8; 32] {
    to_bytes(tree.root())
}

/// Streams compact blocks `from..=to` with every pool type.
pub async fn compact_blocks(client: &mut LwdClient, from: u32, to: u32) -> Result<Vec<CompactBlock>> {
    let mut stream = client
        .get_block_range(BlockRange {
            start: Some(BlockId {
                height: from.into(),
                hash: vec![],
            }),
            end: Some(BlockId {
                height: to.into(),
                hash: vec![],
            }),
            pool_types: ALL_POOLS.to_vec(),
        })
        .await
        .map_err(|s| anyhow!("GetBlockRange: {s}"))?
        .into_inner();
    let mut blocks = Vec::with_capacity((to - from + 1) as usize);
    while let Some(b) = stream.message().await.map_err(|s| anyhow!("GetBlockRange stream: {s}"))? {
        blocks.push(b);
    }
    ensure!(
        blocks.len() == (to - from + 1) as usize,
        "server returned {} blocks for {from}..={to}",
        blocks.len()
    );
    Ok(blocks)
}

/// Per-block summaries for heights `from..=to`, rebuilding the three note commitment trees
/// from the tree state at `from - 1`. Checks every block against its header and the final trees
/// against the server's tree state at `to`.
pub async fn block_summaries(
    client: &mut LwdClient,
    headers: &BTreeMap<u32, Header>,
    from: u32,
    to: u32,
) -> Result<Vec<BlockSummary>> {
    let state = client
        .get_tree_state(BlockId {
            height: (from - 1).into(),
            hash: vec![],
        })
        .await
        .map_err(|s| anyhow!("GetTreeState: {s}"))?
        .into_inner();
    let mut trees = Trees {
        sapling: state.sapling_tree().context("Sapling tree state")?,
        orchard: state.orchard_tree().context("Orchard tree state")?,
        ironwood: state.ironwood_tree().context("Ironwood tree state")?,
    };

    let mut out = Vec::with_capacity((to - from + 1) as usize);
    // In chunks: one stream per chunk keeps memory and server load modest.
    let mut start = from;
    while start <= to {
        let end = (start + 999).min(to);
        for block in compact_blocks(client, start, end).await? {
            out.push(summarize(&block, headers, &mut trees)?);
        }
        start = end + 1;
    }

    // The rebuilt trees must equal the server's at `to`.
    let check = client
        .get_tree_state(BlockId {
            height: to.into(),
            hash: vec![],
        })
        .await
        .map_err(|s| anyhow!("GetTreeState: {s}"))?
        .into_inner();
    let last = out.last().context("no blocks")?;
    ensure!(last.sapling_root == check.sapling_tree()?.root().to_bytes(), "Sapling root mismatch at {to}");
    ensure!(last.orchard_root == check.orchard_tree()?.root().to_bytes(), "Orchard root mismatch at {to}");
    ensure!(last.ironwood_root == check.ironwood_tree()?.root().to_bytes(), "Ironwood root mismatch at {to}");
    Ok(out)
}

fn summarize(block: &CompactBlock, headers: &BTreeMap<u32, Header>, trees: &mut Trees) -> Result<BlockSummary> {
    let height = u32::try_from(block.height)?;
    let header = headers.get(&height).with_context(|| format!("no header for {height}"))?;
    ensure!(block.hash == header.hash, "compact block {height} does not match its header");
    ensure!(
        block.vtx.iter().enumerate().all(|(i, t)| t.index == i as u64),
        "block {height}: the server left transactions out"
    );

    let (mut sapling_tx, mut orchard_tx, mut ironwood_tx) = (0u64, 0u64, 0u64);
    for tx in &block.vtx {
        if !tx.spends.is_empty() || !tx.outputs.is_empty() {
            sapling_tx += 1;
        }
        if !tx.actions.is_empty() {
            orchard_tx += 1;
        }
        if !tx.ironwood_actions.is_empty() {
            ironwood_tx += 1;
        }
        for o in &tx.outputs {
            let cmu = o.cmu().map_err(|_| anyhow!("block {height}: bad Sapling cmu"))?;
            trees.sapling.append(sapling::Node::from_cmu(&cmu)).map_err(|_| anyhow!("Sapling tree full"))?;
        }
        for a in &tx.actions {
            let cmx = a.cmx().map_err(|e| anyhow!("block {height}: bad Orchard cmx: {e:?}"))?;
            trees.orchard.append(MerkleHashOrchard::from_cmx(&cmx)).map_err(|_| anyhow!("Orchard tree full"))?;
        }
        for a in &tx.ironwood_actions {
            let cmx = a.cmx().map_err(|e| anyhow!("block {height}: bad Ironwood cmx: {e:?}"))?;
            trees
                .ironwood
                .append(MerkleHashOrchard::from_cmx(&cmx))
                .map_err(|_| anyhow!("Ironwood tree full"))?;
        }
    }
    Ok(BlockSummary {
        height,
        hash: header.hash,
        time: header.time,
        bits: header.bits,
        sapling_root: root_bytes(&trees.sapling, |n| n.to_bytes()),
        orchard_root: root_bytes(&trees.orchard, |n| n.to_bytes()),
        ironwood_root: root_bytes(&trees.ironwood, |n| n.to_bytes()),
        sapling_tx,
        orchard_tx,
        ironwood_tx,
    })
}
