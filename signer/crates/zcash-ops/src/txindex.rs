//! W-11: the txids of every Ironwood transaction since the network's Ironwood activation (NU6.3),
//! kept in a file and extended on each use.
//!
//! A commitment on Base may name a transaction mined long before it: `Included` proves only that
//! the named transaction X is in a block below the anchor, not that X spent the committed input nor
//! that it came after the commitment. A watcher that searched from a recent height would never find
//! such an X, and its claimant could neither prove F3 nor its borrower F2 or F4. So the watchers
//! search the whole Ironwood history, by claim reference (a claimant) or by payout reference (a
//! borrower, the coordinator). Only transactions with Ironwood actions can pay a claim, so only
//! theirs are kept. The last [`REORG_MARGIN`] blocks are read again on every update and never
//! written to the file (N-12: nor kept from a file that went further than the server's tip allows),
//! and a server's answer with a block outside the range asked is refused.
//!
//! The file holds public chain data only: `ZWIX`, a version byte, the network byte, the first
//! height, the last height kept and the count (u32, little-endian), then each transaction's height
//! (u32, little-endian) and txid (32 bytes, internal order).

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use zcash_protocol::consensus::{NetworkUpgrade, Parameters};

use crate::{lwd, network::ZcashNetwork};

/// Blocks below the tip that are read again on every update and never kept: a reorg there cannot
/// leave a stale txid in the file.
pub const REORG_MARGIN: u32 = 100;
/// Blocks asked of the light wallet server in one range.
const CHUNK: u32 = 10_000;
const MAGIC: &[u8; 4] = b"ZWIX";
const VERSION: u8 = 1;
const HEADER: usize = 4 + 1 + 1 + 4 + 4 + 4;
const ENTRY: usize = 4 + 32;

/// The first block of the network's Ironwood history: its NU6.3 activation.
pub fn ironwood_start(network: ZcashNetwork) -> Result<u32> {
    let h = network.params().activation_height(NetworkUpgrade::Nu6_3).context("no Ironwood activation height")?;
    Ok(u32::from(h))
}

/// The Ironwood transactions of the network's history, as far as it was read.
pub struct IronwoodIndex {
    /// Where it is kept (`None`: in memory only).
    path: Option<PathBuf>,
    network: ZcashNetwork,
    start: u32,
    /// The last height whose transactions are kept (`start - 1` before any).
    kept_to: u32,
    kept: Vec<(u32, [u8; 32])>,
    /// Read on the last update above `kept_to`: near the tip, never written.
    tail: Vec<(u32, [u8; 32])>,
    /// The last height read.
    read_to: u32,
}

impl IronwoodIndex {
    /// The index kept at `path` for `network`, or an empty one (a file of another network, or one
    /// that does not parse, is started again).
    pub fn open(path: &Path, network: ZcashNetwork) -> Result<Self> {
        let mut index = Self::in_memory(network, ironwood_start(network)? - 1, vec![])?;
        index.path = Some(path.to_owned());
        if let Ok(raw) = fs::read(path)
            && let Some((kept_to, kept)) = parse(&raw, network_byte(network), index.start)
        {
            index.kept_to = kept_to;
            index.read_to = kept_to;
            index.kept = kept;
        }
        Ok(index)
    }

    /// An index of `kept` (read up to `kept_to`) that is never written.
    pub fn in_memory(network: ZcashNetwork, kept_to: u32, kept: Vec<(u32, [u8; 32])>) -> Result<Self> {
        let start = ironwood_start(network)?;
        Ok(Self {
            path: None,
            network,
            start,
            kept_to: kept_to.max(start - 1),
            kept,
            tail: vec![],
            read_to: kept_to.max(start - 1),
        })
    }

    /// Reads the blocks above what is kept up to the server's `tip`, and keeps those at least
    /// [`REORG_MARGIN`] below it, saved after each range read (a first read of the whole history
    /// that is cut short resumes where it stopped).
    pub async fn update(&mut self, client: &mut lwd::LwdClient, tip: u32) -> Result<()> {
        self.cap(tip);
        let keep_to = tip.saturating_sub(REORG_MARGIN);
        self.tail.clear();
        let mut from = self.kept_to + 1;
        while from <= tip {
            let to = from.saturating_add(CHUNK - 1).min(tip);
            let txs = in_range(lwd::ironwood_txids(client, from, to).await?, from, to)?;
            let (keep, tail): (Vec<_>, Vec<_>) = txs.into_iter().partition(|(h, _)| *h <= keep_to);
            self.tail.extend(tail);
            if keep_to >= from {
                self.kept.extend(keep);
                self.kept_to = to.min(keep_to);
                self.save()?;
            }
            from = to + 1;
        }
        self.read_to = tip;
        Ok(())
    }

    /// N-12: what is kept stops [`REORG_MARGIN`] below the server's `tip`. A file that went further
    /// (kept against another server, or before a reorganisation) gives up its last blocks, which are
    /// read again.
    fn cap(&mut self, tip: u32) {
        let keep_to = tip.saturating_sub(REORG_MARGIN).max(self.start - 1);
        if self.kept_to > keep_to {
            self.kept.retain(|(h, _)| *h <= keep_to);
            self.kept_to = keep_to;
            self.tail.clear();
            self.read_to = keep_to;
        }
    }

    /// Every transaction read, oldest first: its height and txid.
    pub fn txs(&self) -> impl Iterator<Item = &(u32, [u8; 32])> {
        self.kept.iter().chain(&self.tail)
    }

    /// The heights searched: from the Ironwood activation to the last block read.
    pub fn range(&self) -> (u32, u32) {
        (self.start, self.read_to)
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut out = Vec::with_capacity(HEADER + self.kept.len() * ENTRY);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(network_byte(self.network));
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.kept_to.to_le_bytes());
        out.extend_from_slice(&u32::try_from(self.kept.len())?.to_le_bytes());
        for (h, txid) in &self.kept {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(txid);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, out).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
    }
}

/// N-12: the light wallet server's answer for the blocks `from..=to`, refused if it holds a
/// transaction of any other block.
fn in_range(txs: Vec<(u32, [u8; 32])>, from: u32, to: u32) -> Result<Vec<(u32, [u8; 32])>> {
    anyhow::ensure!(
        txs.iter().all(|(h, _)| (from..=to).contains(h)),
        "the light wallet server answered with a block outside the range asked"
    );
    Ok(txs)
}

fn network_byte(network: ZcashNetwork) -> u8 {
    match network {
        ZcashNetwork::Testnet => 0,
        ZcashNetwork::Mainnet => 1,
    }
}

/// The kept height and transactions of a file of this network and start, if it is one.
fn parse(raw: &[u8], network: u8, start: u32) -> Option<(u32, Vec<(u32, [u8; 32])>)> {
    if raw.len() < HEADER || &raw[..4] != MAGIC || raw[4] != VERSION || raw[5] != network {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
    let (first, kept_to, count) = (word(6), word(10), usize::try_from(word(14)).ok()?);
    if first != start || kept_to < start - 1 || raw.len() != HEADER + count.checked_mul(ENTRY)? {
        return None;
    }
    let kept = raw[HEADER..]
        .chunks_exact(ENTRY)
        .map(|e| (u32::from_le_bytes([e[0], e[1], e[2], e[3]]), e[4..].try_into().unwrap_or([0; 32])))
        .collect();
    Some((kept_to, kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let mut r = [0u8; 8];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut r);
        std::env::temp_dir().join(format!("zecured-txindex-{tag}-{}.bin", hex::encode(r)))
    }

    /// W-11: the history starts at NU6.3, and what is kept is read back as it was; a file of the
    /// other network, or a damaged one, is started again.
    #[test]
    fn w11_the_index_is_kept_and_read_back() {
        assert_eq!(ironwood_start(ZcashNetwork::Testnet).unwrap(), 4_134_000);
        assert_eq!(ironwood_start(ZcashNetwork::Mainnet).unwrap(), 3_428_143);
        let path = temp("roundtrip");
        let txs = vec![(4_134_005, [1; 32]), (4_200_000, [2; 32])];
        let mut index = IronwoodIndex::in_memory(ZcashNetwork::Testnet, 4_300_000, txs.clone()).unwrap();
        index.path = Some(path.clone());
        index.save().unwrap();
        let back = IronwoodIndex::open(&path, ZcashNetwork::Testnet).unwrap();
        assert_eq!(back.txs().copied().collect::<Vec<_>>(), txs);
        assert_eq!(back.range(), (4_134_000, 4_300_000));
        let other = IronwoodIndex::open(&path, ZcashNetwork::Mainnet).unwrap();
        assert_eq!(other.txs().count(), 0, "another network's file is not used");
        fs::write(&path, b"ZWIX garbage").unwrap();
        assert_eq!(IronwoodIndex::open(&path, ZcashNetwork::Testnet).unwrap().txs().count(), 0);
        let _ = fs::remove_file(&path);
    }

    /// N-12: a file kept further than the server's tip allows is cut back to [`REORG_MARGIN`] below
    /// it (its last blocks are read again); one behind the tip is kept as it is. An answer with a block
    /// outside the range asked is refused.
    #[test]
    fn n12_the_index_never_keeps_within_the_margin_of_the_tip() {
        let txs = vec![(4_134_005, [1; 32]), (4_199_950, [2; 32]), (4_199_990, [3; 32])];
        let mut index = IronwoodIndex::in_memory(ZcashNetwork::Testnet, 4_200_000, txs.clone()).unwrap();
        index.cap(4_200_050);
        assert_eq!(index.kept_to, 4_199_950);
        assert_eq!(index.txs().copied().collect::<Vec<_>>(), txs[..2], "the block 40 below the tip is read again");
        assert_eq!(index.range().1, 4_199_950);
        let mut behind = IronwoodIndex::in_memory(ZcashNetwork::Testnet, 4_200_000, txs.clone()).unwrap();
        behind.cap(4_300_000);
        assert_eq!((behind.kept_to, behind.txs().count()), (4_200_000, 3), "behind the tip: kept");
        let tx = |h: u32| (h, [9; 32]);
        assert!(in_range(vec![tx(10), tx(20)], 10, 20).is_ok());
        assert!(in_range(vec![tx(9)], 10, 20).is_err());
        assert!(in_range(vec![tx(21)], 10, 20).is_err());
    }
}
