//! Membership paths in the ZIP 221 history tree.
//!
//! The tree is a Merkle mountain range: its leaves split into perfect binary trees ("peaks"),
//! largest (oldest) first, one per set bit of the leaf count, and the root is the left fold of
//! the peaks with the node-combining function (as `zcash_history::Tree` builds it). A path is a
//! leaf's siblings up to its peak; with all peaks it recomputes the root.

use anyhow::{Result, ensure};
use zcash_history::{NodeDataV3, V3, Version};

pub struct Mmr {
    leaves: Vec<NodeDataV3>,
}

/// One step of a path: the sibling node and whether it is the left child.
pub type PathStep = (NodeDataV3, bool);

impl Mmr {
    pub fn new(leaves: Vec<NodeDataV3>) -> Result<Self> {
        ensure!(!leaves.is_empty(), "empty history");
        Ok(Self { leaves })
    }

    /// The perfect subtrees, oldest first, as (first leaf, size).
    fn peak_ranges(&self) -> Vec<(usize, usize)> {
        let n = self.leaves.len();
        let mut out = vec![];
        let mut start = 0;
        for bit in (0..usize::BITS).rev() {
            let size = 1usize << bit;
            if n & size != 0 {
                out.push((start, size));
                start += size;
            }
        }
        out
    }

    fn subtree(&self, start: usize, size: usize) -> NodeDataV3 {
        if size == 1 {
            return self.leaves[start].clone();
        }
        let half = size / 2;
        V3::combine(&self.subtree(start, half), &self.subtree(start + half, half))
    }

    pub fn peaks(&self) -> Vec<NodeDataV3> {
        self.peak_ranges().into_iter().map(|(s, n)| self.subtree(s, n)).collect()
    }

    /// The root node: the left fold of the peaks.
    pub fn root(&self) -> NodeDataV3 {
        let mut peaks = self.peaks().into_iter();
        let first = peaks.next().expect("non-empty");
        peaks.fold(first, |acc, p| V3::combine(&acc, &p))
    }

    /// The path of leaf `index` up to its peak (bottom-up), and which peak that is.
    pub fn path(&self, index: usize) -> Result<(Vec<PathStep>, usize)> {
        ensure!(index < self.leaves.len(), "no leaf {index}");
        let (peak_index, (mut start, mut size)) = self
            .peak_ranges()
            .into_iter()
            .enumerate()
            .find(|(_, (s, n))| index >= *s && index < s + n)
            .expect("every leaf is under a peak");
        let mut top_down = vec![];
        while size > 1 {
            let half = size / 2;
            if index < start + half {
                top_down.push((self.subtree(start + half, half), false));
            } else {
                top_down.push((self.subtree(start, half), true));
                start += half;
            }
            size = half;
        }
        top_down.reverse();
        Ok((top_down, peak_index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{BlockSummary, history_root, leaf};

    fn leaves(n: u32) -> Vec<NodeDataV3> {
        (0..n)
            .map(|i| {
                leaf(
                    crate::history::NU7_BRANCH_ID,
                    &BlockSummary {
                        height: 100 + i,
                        hash: [i as u8; 32],
                        time: 1_000 + i,
                        bits: 0x1d00ffff,
                        sapling_root: [1; 32],
                        orchard_root: [2; 32],
                        ironwood_root: [3; 32],
                        sapling_tx: u64::from(i % 2),
                        orchard_tx: 0,
                        ironwood_tx: 1,
                    },
                )
            })
            .collect()
    }

    /// Our peaks and fold give the same root as zcash_history's incremental tree, for every
    /// size up to 40, and every path recomputes its peak.
    #[test]
    fn root_and_paths_match_zcash_history() {
        for n in 1..=40 {
            let ls = leaves(n);
            let mmr = Mmr::new(ls.clone()).unwrap();
            assert_eq!(V3::hash(&mmr.root()), history_root(&ls).unwrap(), "root of {n} leaves");
            let peaks = mmr.peaks();
            for i in 0..n as usize {
                let (path, peak) = mmr.path(i).unwrap();
                let mut acc = ls[i].clone();
                for (sibling, left) in &path {
                    acc = if *left { V3::combine(sibling, &acc) } else { V3::combine(&acc, sibling) };
                }
                assert_eq!(V3::to_bytes(&acc), V3::to_bytes(&peaks[peak]), "path of leaf {i} of {n}");
            }
        }
    }
}
