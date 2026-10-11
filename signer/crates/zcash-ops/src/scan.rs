//! Light-client scanning of the Ironwood pool for one vault (or one test account).
//!
//! Notes are recorded with their exact chain position (block height, transaction index in
//! the block, action index in the transaction's Ironwood bundle). Policy P1 orders a
//! vault's notes by exactly that key, so every signer classifies them identically.
//!
//! Witnesses are kept with the legacy incremental API: a vault holds a handful of notes
//! and is scanned from its (recent) birthday, so this stays cheap and dependency-light.

use anyhow::{Context, Result, anyhow, ensure};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Address, Note,
    keys::{FullViewingKey, PreparedIncomingViewingKey, Scope},
    note::Nullifier,
    note_encryption::{CompactAction, IronwoodDomain},
    tree::{MerkleHashOrchard, MerklePath},
};
use zcash_client_backend::proto::{
    compact_formats::CompactBlock,
    service::{BlockId, BlockRange},
};
use zcash_note_encryption::try_compact_note_decryption;
use zcash_protocol::TxId;

use crate::lwd::{self, LwdClient};

/// Depth of the Orchard-protocol note commitment tree (shared shape with Ironwood).
pub const TREE_DEPTH: u8 = orchard::NOTE_COMMITMENT_TREE_DEPTH as u8;

/// Ironwood note commitment tree (legacy representation).
pub type IronwoodTree = CommitmentTree<MerkleHashOrchard, TREE_DEPTH>;
/// Witness for one note in the Ironwood tree.
pub type IronwoodWitness = IncrementalWitness<MerkleHashOrchard, TREE_DEPTH>;

/// Where a note sits in the chain. Ordering is (height, tx_index, action_index), which is
/// the deterministic order used by policy P1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NotePosition {
    pub height: u32,
    pub tx_index: u64,
    pub action_index: u32,
}

/// A note received by the scanned viewing key.
#[derive(Clone)]
pub struct ReceivedNote {
    pub position: NotePosition,
    pub txid: TxId,
    pub note: Note,
    pub recipient: Address,
    pub scope: Scope,
    pub nullifier: Nullifier,
    pub witness: IronwoodWitness,
    /// Height of the block that spent this note, if seen.
    pub spent_at: Option<u32>,
    /// Id of the transaction that spent this note, if seen.
    pub spent_in: Option<TxId>,
}

impl ReceivedNote {
    /// Value in zatoshis.
    pub fn value(&self) -> u64 {
        self.note.value().inner()
    }

    /// Merkle path from the note to the current anchor.
    pub fn merkle_path(&self) -> Result<MerklePath> {
        let path = self.witness.path().context("note witness has no path")?;
        Ok(MerklePath::from(path))
    }
}

/// Scanning state: the Ironwood tree after `scanned_to`, and every note found so far.
pub struct ScanState {
    pub fvk: FullViewingKey,
    pub tree: IronwoodTree,
    /// Last fully scanned block height.
    pub scanned_to: u32,
    pub notes: Vec<ReceivedNote>,
    /// Hash of block `scanned_to`, to detect reorgs (empty before the first block).
    last_hash: Vec<u8>,
    ivks: [(Scope, PreparedIncomingViewingKey); 2],
}

impl ScanState {
    /// Starts scanning at `birthday`, from the server's Ironwood tree state at `birthday - 1`.
    pub async fn from_birthday(client: &mut LwdClient, fvk: FullViewingKey, birthday: u32) -> Result<Self> {
        let prior = birthday.checked_sub(1).context("birthday must be > 0")?;
        let tree_state = client
            .get_tree_state(BlockId {
                height: prior.into(),
                hash: vec![],
            })
            .await
            .map_err(|s| anyhow!("GetTreeState({prior}): {s}"))?
            .into_inner();
        let tree = tree_state.ironwood_tree().context("parse Ironwood tree state")?;
        let ivks = [Scope::External, Scope::Internal]
            .map(|scope| (scope, PreparedIncomingViewingKey::new(&fvk.to_ivk(scope))));
        Ok(Self {
            fvk,
            tree,
            scanned_to: prior,
            notes: vec![],
            last_hash: vec![],
            ivks,
        })
    }

    /// Scans every block after `scanned_to` up to and including `to`.
    pub async fn scan_to(&mut self, client: &mut LwdClient, to: u32) -> Result<()> {
        if to <= self.scanned_to {
            return Ok(());
        }
        let mut stream = client
            .get_block_range(BlockRange {
                start: Some(BlockId {
                    height: (self.scanned_to + 1).into(),
                    hash: vec![],
                }),
                end: Some(BlockId {
                    height: to.into(),
                    hash: vec![],
                }),
                // Empty = the legacy default: all shielded pools, including Ironwood.
                pool_types: vec![],
            })
            .await
            .map_err(|s| anyhow!("GetBlockRange: {s}"))?
            .into_inner();
        // The request timeout only covers the start of the stream; bound every message too (M-7).
        while let Some(block) = tokio::time::timeout(lwd::STREAM_MESSAGE_TIMEOUT, stream.message())
            .await
            .map_err(|_| anyhow!("GetBlockRange stream stalled"))?
            .map_err(|s| anyhow!("GetBlockRange stream: {s}"))?
        {
            self.scan_block(&block)?;
        }
        ensure!(self.scanned_to == to, "server returned blocks up to {} instead of {to}", self.scanned_to);
        Ok(())
    }

    fn scan_block(&mut self, block: &CompactBlock) -> Result<()> {
        let height = u32::try_from(block.height).context("block height")?;
        let expected = self.scanned_to.checked_add(1).context("block height overflow")?;
        ensure!(height == expected, "expected block {expected}, got {height}");
        // A reorg replaced a block we already scanned: the caller must rescan.
        ensure!(
            self.last_hash.is_empty() || block.prev_hash == self.last_hash,
            "chain reorganized below height {height}"
        );
        // Validate what the server sent before changing any state (`CompactTx::txid` panics on
        // a malformed id).
        ensure!(block.vtx.iter().all(|tx| tx.txid.len() == 32), "malformed transaction id in block {height}");
        for tx in &block.vtx {
            let txid = tx.txid();
            for (action_index, action) in tx.ironwood_actions.iter().enumerate() {
                let nf = action.nf().map_err(|e| anyhow!("action nullifier: {e:?}"))?;
                for note in self.notes.iter_mut().filter(|n| n.spent_at.is_none()) {
                    if note.nullifier == nf {
                        note.spent_at = Some(height);
                        note.spent_in = Some(txid);
                    }
                }

                let cmx = action.cmx().map_err(|e| anyhow!("action cmx: {e:?}"))?;
                let node = MerkleHashOrchard::from_cmx(&cmx);
                for note in &mut self.notes {
                    note.witness.append(node).map_err(|_| anyhow!("note commitment tree is full"))?;
                }
                self.tree.append(node).map_err(|_| anyhow!("note commitment tree is full"))?;

                let compact = CompactAction::try_from(action).map_err(|e| anyhow!("compact action: {e:?}"))?;
                let domain = IronwoodDomain::for_compact_action(&compact);
                for (scope, ivk) in &self.ivks {
                    if let Some((note, recipient)) = try_compact_note_decryption(&domain, ivk, &compact) {
                        let witness =
                            IronwoodWitness::from_tree(self.tree.clone()).context("witness of an empty tree")?;
                        self.notes.push(ReceivedNote {
                            position: NotePosition {
                                height,
                                tx_index: tx.index,
                                action_index: u32::try_from(action_index)?,
                            },
                            txid,
                            nullifier: note.nullifier(&self.fvk),
                            note,
                            recipient,
                            scope: *scope,
                            witness,
                            spent_at: None,
                            spent_in: None,
                        });
                        break;
                    }
                }
            }
        }
        self.scanned_to = height;
        self.last_hash = block.hash.clone();
        Ok(())
    }

    /// The anchor (Ironwood tree root) at `scanned_to`, valid for spends built now.
    pub fn anchor(&self) -> orchard::Anchor {
        orchard::Anchor::from(self.tree.root())
    }

    /// Checks our tree against the server's Ironwood tree state at `scanned_to`.
    pub async fn verify_against_server(&self, client: &mut LwdClient) -> Result<()> {
        let server = client
            .get_tree_state(BlockId {
                height: self.scanned_to.into(),
                hash: vec![],
            })
            .await
            .map_err(|s| anyhow!("GetTreeState: {s}"))?
            .into_inner()
            .ironwood_tree()
            .context("parse server Ironwood tree")?;
        ensure!(server.root() == self.tree.root(), "Ironwood tree root mismatch at height {}", self.scanned_to);
        Ok(())
    }

    /// Unspent notes in P1 order.
    pub fn unspent(&self) -> Vec<&ReceivedNote> {
        let mut v: Vec<_> = self.notes.iter().filter(|n| n.spent_at.is_none()).collect();
        v.sort_by_key(|n| n.position);
        v
    }

    /// Sum of unspent note values, in zatoshis.
    pub fn balance(&self) -> u64 {
        self.unspent().iter().map(|n| n.value()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard::keys::SpendingKey;

    fn state_at(height: u32) -> ScanState {
        let sk = (0u8..=255)
            .map(|i| [i; 32])
            .find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes(b)))
            .unwrap();
        let fvk = FullViewingKey::from(&sk);
        let ivks = [Scope::External, Scope::Internal]
            .map(|scope| (scope, PreparedIncomingViewingKey::new(&fvk.to_ivk(scope))));
        ScanState {
            fvk,
            tree: IronwoodTree::empty(),
            scanned_to: height,
            notes: vec![],
            last_hash: vec![],
            ivks,
        }
    }

    fn block(height: u32, hash: u8, prev: u8) -> CompactBlock {
        CompactBlock {
            height: u64::from(height),
            hash: vec![hash; 32],
            prev_hash: vec![prev; 32],
            ..Default::default()
        }
    }

    #[test]
    fn scanning_follows_the_chain_and_refuses_a_reorg() {
        let mut s = state_at(99);
        s.scan_block(&block(100, 1, 0)).unwrap(); // the first block has no known parent yet
        s.scan_block(&block(101, 2, 1)).unwrap();

        // Block 102 does not build on the block 101 we scanned: the chain was reorganized.
        let err = s.scan_block(&block(102, 3, 9)).unwrap_err();
        assert!(err.to_string().contains("reorganized"), "{err}");
        assert_eq!(s.scanned_to, 101, "nothing applied from the foreign block");

        // Heights must be consecutive, and the right child is still accepted.
        assert!(s.scan_block(&block(103, 4, 2)).is_err());
        s.scan_block(&block(102, 3, 2)).unwrap();
        assert_eq!(s.scanned_to, 102);
    }

    #[test]
    fn malformed_server_data_is_refused_without_panicking() {
        use zcash_client_backend::proto::compact_formats::CompactTx;
        let mut s = state_at(99);
        let mut b = block(100, 1, 0);
        b.vtx = vec![
            CompactTx {
                txid: vec![5; 32],
                ..Default::default()
            },
            CompactTx {
                txid: vec![5; 31],
                ..Default::default()
            },
        ];
        assert!(s.scan_block(&b).is_err());
        assert_eq!(s.scanned_to, 99, "nothing applied");
        // A server claiming the highest representable height cannot overflow the scanner.
        let mut s = state_at(u32::MAX);
        assert!(s.scan_block(&block(u32::MAX, 1, 0)).is_err());
    }
}
