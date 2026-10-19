//! The ZIP 221 chain history tree (V3 node data, ZIP 258) and the ZIP 244 block commitments.
//!
//! The header of block n commits, through `hashBlockCommitments`, to a Merkle mountain range
//! with one leaf per earlier block of the current network-upgrade epoch (from the epoch's
//! activation height to n - 1). Each leaf records the block hash, time, difficulty, work, the
//! final Sapling, Orchard and Ironwood note commitment tree roots and per-pool transaction
//! counts. A deposit proof can therefore show "my deposit's block is in the history committed
//! by this recent header" without revealing which block it is.

use anyhow::{Context, Result, anyhow, ensure};
use primitive_types::U256;
use sha2::{Digest, Sha256};
use zcash_history::{Entry, NodeData, NodeDataV2, NodeDataV3, Tree, V3, Version};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zecured_zcash_ops::network::ZcashNetwork;

/// NU7 consensus branch ID (testnet activation height 4,465,026; docs/ref/FACTS.md).
pub const NU7_BRANCH_ID: u32 = 0x77190AD9;
/// First block of the NU7 epoch on testnet: the first leaf of the epoch's history tree.
pub const NU7_ACTIVATION_HEIGHT: u32 = 4_465_026;

/// A network-upgrade epoch's history tree: its first block (the tree's first leaf) and the
/// consensus branch id its leaves carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Epoch {
    pub activation: u32,
    pub branch_id: u32,
}

/// The epoch of block `height` of Zcash `network`, whose history tree that block's header commits
/// to (ZIP 221: the tree restarts at every network upgrade). Refused unless the statements prove
/// on it: Testnet's NU7 epoch; Mainnet's NU6.3 epoch, then its NU7 epoch. The heights are
/// zcash_protocol's (docs/ref/FACTS.md); Mainnet's NU7 has none yet, so every Mainnet block from
/// NU6.3 on counts as NU6.3's until a release with that height (docs/MAINNET.md § 5).
pub fn epoch(network: ZcashNetwork, height: u32) -> Result<Epoch> {
    let params = network.params();
    let branch = BranchId::for_height(&params, BlockHeight::from_u32(height));
    let branch_id = u32::from(branch);
    ensure!(
        crate::statement_network(network).allows(branch_id),
        "block {height} of Zcash {network} is in an epoch the statements do not prove on (branch id {branch_id:#010x})"
    );
    let (start, _) = branch.height_bounds(&params).context("the epoch has no activation height")?;
    Ok(Epoch {
        activation: u32::from(start),
        branch_id,
    })
}

/// Size of a serialized post-NU5 block header (140-byte prefix, compactSize, 1344-byte solution).
pub const HEADER_LENGTH: usize = 1487;

/// The fields of a block header the history tree and the proofs need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// SHA-256d of the serialized header, internal byte order.
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub block_commitments: [u8; 32],
    pub time: u32,
    pub bits: u32,
}

impl Header {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        ensure!(raw.len() == HEADER_LENGTH, "header is {} bytes, expected {HEADER_LENGTH}", raw.len());
        let field = |at: usize| -> [u8; 32] { raw[at..at + 32].try_into().expect("32 bytes") };
        let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("4 bytes"));
        Ok(Self {
            hash: sha256d(raw),
            prev_hash: field(4),
            merkle_root: field(36),
            block_commitments: field(68),
            time: word(100),
            bits: word(104),
        })
    }
}

/// What a history-tree leaf records about one block.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockSummary {
    pub height: u32,
    /// Internal byte order.
    pub hash: [u8; 32],
    pub time: u32,
    pub bits: u32,
    /// Final note commitment tree roots after the block (LEBS2OSP_256 of the root).
    pub sapling_root: [u8; 32],
    pub orchard_root: [u8; 32],
    pub ironwood_root: [u8; 32],
    /// Transactions with Sapling spends or outputs / Orchard actions / Ironwood actions.
    pub sapling_tx: u64,
    pub orchard_tx: u64,
    pub ironwood_tx: u64,
}

pub fn sha256d(data: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(data)).into()
}

/// The target encoded by a compact `nBits` value.
pub fn target_from_bits(bits: u32) -> U256 {
    let exponent = bits >> 24;
    let mantissa = U256::from(bits & 0x007f_ffff);
    if exponent <= 3 {
        mantissa >> (8 * (3 - exponent))
    } else {
        mantissa << (8 * (exponent - 3))
    }
}

/// The work of a block: floor(2^256 / (target + 1)), as zcashd's `GetBlockProof`.
pub fn block_work(bits: u32) -> U256 {
    let target = target_from_bits(bits);
    (!target / (target + U256::one())) + U256::one()
}

/// The ZIP 221 V3 leaf of a block.
pub fn leaf(branch_id: u32, b: &BlockSummary) -> NodeDataV3 {
    NodeDataV3 {
        v2: NodeDataV2 {
            v1: NodeData {
                consensus_branch_id: branch_id,
                subtree_commitment: b.hash,
                start_time: b.time,
                end_time: b.time,
                start_target: b.bits,
                end_target: b.bits,
                start_sapling_root: b.sapling_root,
                end_sapling_root: b.sapling_root,
                subtree_total_work: block_work(b.bits),
                start_height: u64::from(b.height),
                end_height: u64::from(b.height),
                sapling_tx: b.sapling_tx,
            },
            start_orchard_root: b.orchard_root,
            end_orchard_root: b.orchard_root,
            orchard_tx: b.orchard_tx,
        },
        start_ironwood_root: b.ironwood_root,
        end_ironwood_root: b.ironwood_root,
        ironwood_tx: b.ironwood_tx,
    }
}

/// `hashChainHistoryRoot` of the history tree over `leaves` (oldest first): the BLAKE2b-256
/// digest of the serialized root node (ZIP 221).
pub fn history_root(leaves: &[NodeDataV3]) -> Result<[u8; 32]> {
    let (first, rest) = leaves.split_first().ok_or_else(|| anyhow!("empty history"))?;
    let mut tree = Tree::<V3>::new(1, vec![(0, Entry::new_leaf(first.clone()))], vec![]);
    for l in rest {
        tree.append_leaf(l.clone()).map_err(|e| anyhow!("append history leaf: {e:?}"))?;
    }
    let root = tree.root_node().map_err(|e| anyhow!("history root: {e:?}"))?;
    Ok(V3::hash(root.data()))
}

fn blake2b_256(personal: &[u8; 16], parts: &[&[u8]]) -> [u8; 32] {
    let mut state = blake2b_simd::Params::new().hash_length(32).personal(personal).to_state();
    for p in parts {
        state.update(p);
    }
    state.finalize().as_bytes().try_into().expect("32 bytes")
}

/// `hashAuthDataRoot` (ZIP 244): a binary Merkle tree over the transactions' authorizing data
/// commitments, in block order, padded with zero leaves to a power of two.
pub fn auth_data_root(auth_commitments: &[[u8; 32]]) -> Result<[u8; 32]> {
    ensure!(!auth_commitments.is_empty(), "a block has at least its coinbase");
    let mut level = auth_commitments.to_vec();
    level.resize(level.len().next_power_of_two(), [0u8; 32]);
    while level.len() > 1 {
        level = level.chunks(2).map(|p| blake2b_256(b"ZcashAuthDatHash", &[&p[0], &p[1]])).collect();
    }
    Ok(level[0])
}

/// `hashBlockCommitments` (ZIP 244): BLAKE2b-256("ZcashBlockCommit", history root ‖ auth data
/// root ‖ [0; 32]).
pub fn block_commitments(history_root: &[u8; 32], auth_data_root: &[u8; 32]) -> [u8; 32] {
    blake2b_256(b"ZcashBlockCommit", &[history_root, auth_data_root, &[0u8; 32]])
}

/// The block's txid Merkle root (`hashMerkleRoot`): SHA-256d pairs, the last node of an odd
/// level paired with itself; txids in internal byte order.
pub fn txid_merkle_root(txids: &[[u8; 32]]) -> Result<[u8; 32]> {
    ensure!(!txids.is_empty(), "a block has at least its coinbase");
    let mut level = txids.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        level = level.chunks(2).map(|p| sha256d(&[p[0], p[1]].concat())).collect();
    }
    Ok(level[0])
}

/// The SHA-256d Merkle path (siblings, bottom-up) of the transaction at `index` in a block.
pub fn txid_merkle_path(txids: &[[u8; 32]], index: usize) -> Result<Vec<[u8; 32]>> {
    ensure!(index < txids.len(), "no transaction {index}");
    let mut level = txids.to_vec();
    let mut i = index;
    let mut path = vec![];
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        path.push(level[i ^ 1]);
        level = level.chunks(2).map(|p| sha256d(&[p[0], p[1]].concat())).collect();
        i /= 2;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each network's epochs as zcash_protocol's heights give them (docs/ref/FACTS.md): Testnet
    /// proves from NU7 (4,465,026) only; Mainnet from NU6.3 (3,428,143), and NU7 has no Mainnet
    /// height yet.
    #[test]
    fn the_epoch_is_the_networks() {
        let nu7 = Epoch {
            activation: NU7_ACTIVATION_HEIGHT,
            branch_id: NU7_BRANCH_ID,
        };
        assert_eq!(epoch(ZcashNetwork::Testnet, NU7_ACTIVATION_HEIGHT).unwrap(), nu7);
        assert_eq!(epoch(ZcashNetwork::Testnet, 4_500_000).unwrap(), nu7);
        // Testnet's NU6.3 epoch (4,134,000 ..): not one the Testnet statements prove on.
        assert!(epoch(ZcashNetwork::Testnet, NU7_ACTIVATION_HEIGHT - 1).is_err());
        let nu6_3 = Epoch {
            activation: 3_428_143,
            branch_id: zecured_deposit_statement::NU6_3_BRANCH_ID,
        };
        assert_eq!(epoch(ZcashNetwork::Mainnet, 3_428_143).unwrap(), nu6_3);
        assert_eq!(epoch(ZcashNetwork::Mainnet, 3_499_045).unwrap(), nu6_3);
        // Mainnet's NU6.2 epoch: no Ironwood pool, nothing to prove.
        assert!(epoch(ZcashNetwork::Mainnet, 3_428_142).is_err());
    }

    #[test]
    fn work_matches_the_compact_target() {
        // Testnet's PoWLimit (nBits 0x2007ffff) has work 32 (contracts/src/relay README).
        assert_eq!(block_work(0x2007ffff), U256::from(32u8));
        // A tiny target: 0x03000001 is target 1, so work = 2^256 / 2.
        assert_eq!(block_work(0x0300_0001), U256::one() << 255);
    }

    #[test]
    fn auth_root_pads_to_a_power_of_two() {
        let a = [1u8; 32];
        assert_eq!(auth_data_root(&[a]).unwrap(), a, "one transaction: the root is its commitment");
        let three = auth_data_root(&[a, [2u8; 32], [3u8; 32]]).unwrap();
        let left = blake2b_256(b"ZcashAuthDatHash", &[&a, &[2u8; 32]]);
        let right = blake2b_256(b"ZcashAuthDatHash", &[&[3u8; 32], &[0u8; 32]]);
        assert_eq!(three, blake2b_256(b"ZcashAuthDatHash", &[&left, &right]));
    }
}
