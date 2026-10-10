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

use crate::lwd::LwdClient;

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
    ivks: [(Scope, PreparedIncomingViewingKey); 2],
}

impl ScanState {
    /// Starts scanning at `birthday`, from the server's Ironwood tree state at `birthday - 1`.
    pub async fn from_birthday(
        client: &mut LwdClient,
        fvk: FullViewingKey,
        birthday: u32,
    ) -> Result<Self> {
        let prior = birthday.checked_sub(1).context("birthday must be > 0")?;
        let tree_state = client
            .get_tree_state(BlockId {
                height: prior.into(),
                hash: vec![],
            })
            .await
            .map_err(|s| anyhow!("GetTreeState({prior}): {s}"))?
            .into_inner();
        let tree = tree_state
            .ironwood_tree()
            .context("parse Ironwood tree state")?;
        let ivks = [Scope::External, Scope::Internal]
            .map(|scope| (scope, PreparedIncomingViewingKey::new(&fvk.to_ivk(scope))));
        Ok(Self {
            fvk,
            tree,
            scanned_to: prior,
            notes: vec![],
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
        while let Some(block) = stream
            .message()
            .await
            .map_err(|s| anyhow!("GetBlockRange stream: {s}"))?
        {
            self.scan_block(&block)?;
        }
        ensure!(
            self.scanned_to == to,
            "server returned blocks up to {} instead of {to}",
            self.scanned_to
        );
        Ok(())
    }

    fn scan_block(&mut self, block: &CompactBlock) -> Result<()> {
        let height = u32::try_from(block.height).context("block height")?;
        ensure!(
            height == self.scanned_to + 1,
            "expected block {}, got {height}",
            self.scanned_to + 1
        );
        for tx in &block.vtx {
            for (action_index, action) in tx.ironwood_actions.iter().enumerate() {
                let nf = action.nf().map_err(|e| anyhow!("action nullifier: {e:?}"))?;
                for note in self.notes.iter_mut().filter(|n| n.spent_at.is_none()) {
                    if note.nullifier == nf {
                        note.spent_at = Some(height);
                    }
                }

                let cmx = action.cmx().map_err(|e| anyhow!("action cmx: {e:?}"))?;
                let node = MerkleHashOrchard::from_cmx(&cmx);
                for note in &mut self.notes {
                    note.witness
                        .append(node)
                        .map_err(|_| anyhow!("note commitment tree is full"))?;
                }
                self.tree
                    .append(node)
                    .map_err(|_| anyhow!("note commitment tree is full"))?;

                let compact = CompactAction::try_from(action)
                    .map_err(|e| anyhow!("compact action: {e:?}"))?;
                let domain = IronwoodDomain::for_compact_action(&compact);
                for (scope, ivk) in &self.ivks {
                    if let Some((note, recipient)) =
                        try_compact_note_decryption(&domain, ivk, &compact)
                    {
                        let witness = IronwoodWitness::from_tree(self.tree.clone())
                            .context("witness of an empty tree")?;
                        self.notes.push(ReceivedNote {
                            position: NotePosition {
                                height,
                                tx_index: tx.index,
                                action_index: u32::try_from(action_index)?,
                            },
                            txid: tx.txid(),
                            nullifier: note.nullifier(&self.fvk),
                            note,
                            recipient,
                            scope: *scope,
                            witness,
                            spent_at: None,
                        });
                        break;
                    }
                }
            }
        }
        self.scanned_to = height;
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
        ensure!(
            server.root() == self.tree.root(),
            "Ironwood tree root mismatch at height {}",
            self.scanned_to
        );
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
