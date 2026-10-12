//! The private input of a deposit proof, built from public chain data plus what any holder of
//! the vault's viewing key knows (the vault's `VaultOpened` values; the deposit note is found by
//! trial decryption), and checked with the statement natively before anything is proven.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, anyhow, ensure};
use orchard::{
    keys::{PreparedIncomingViewingKey, Scope},
    note_encryption::IronwoodDomain,
};
use zcash_history::{V3, Version};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Network},
};
use zecured_deposit_statement::{Journal, MmrStep, Witness, verify};
use zecured_zcash_ops::lwd::{self, LwdClient};

use crate::{
    chain,
    history::{self, BlockSummary},
    mmr::Mmr,
    p2p::HeaderLine,
};

/// A vault as its five signers described it in `VaultOpened`.
#[derive(Clone, Debug)]
pub struct Vault {
    pub vault_id: [u8; 32],
    pub owner: [u8; 20],
    pub account: [u8; 20],
    /// Unified address and unified full viewing key, exactly as signed.
    pub address: String,
    pub ufvk: String,
    pub expiry_height: u32,
    pub amount: u64,
}

/// A witness, its journal and where the deposit sits.
pub struct Built {
    pub witness: Witness,
    pub journal: Journal,
    pub deposit_height: u32,
    pub tx_index: usize,
    pub block_transactions: usize,
    pub leaf_index: usize,
    pub leaves: usize,
    pub path_len: usize,
    pub peak_index: usize,
    pub peaks: usize,
}

/// Raw headers by height from a JSON Lines header file.
fn raw_headers(path: &Path, heights: &[u32]) -> Result<BTreeMap<u32, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines().filter(|l| !l.trim().is_empty()) {
        let h: HeaderLine = serde_json::from_str(line)?;
        if heights.contains(&h.height) {
            out.insert(h.height, hex::decode(&h.raw)?);
        }
    }
    for h in heights {
        ensure!(out.contains_key(h), "header {h} is not in {}", path.display());
    }
    Ok(out)
}

/// The per-block summaries of the NU7 epoch up to `last`, from the cache at `cache` (extended
/// and rewritten as needed: the first build takes a few minutes, later ones seconds).
async fn summaries(
    client: &mut LwdClient,
    headers: &BTreeMap<u32, history::Header>,
    cache: &Path,
    last: u32,
) -> Result<Vec<BlockSummary>> {
    let cached: Option<Vec<BlockSummary>> =
        fs::read(cache).ok().and_then(|b| serde_json::from_slice::<Vec<BlockSummary>>(&b).ok());
    let mut out = match cached {
        Some(s) if s.first().map(|b| b.height) == Some(history::NU7_ACTIVATION_HEIGHT) => s,
        _ => vec![],
    };
    out.retain(|s| s.height <= last);
    let have = out.last().map_or(history::NU7_ACTIVATION_HEIGHT - 1, |s| s.height);
    if have < last {
        // The commitment trees restart from the server's tree state at `have`.
        out.extend(chain::block_summaries(client, headers, have + 1, last).await?);
        fs::write(cache, serde_json::to_vec(&out)?)?;
    }
    ensure!(out.len() == (last - history::NU7_ACTIVATION_HEIGHT + 1) as usize, "summary gap");
    Ok(out)
}

/// A mined transaction and where it is: its block B and B's place in the NU7-epoch history that
/// header `tip` commits to (the inclusion part of every Tier 2 and Tier 3 witness).
pub struct Located {
    pub raw_tx: Vec<u8>,
    pub height: u32,
    pub tx_index: usize,
    pub block_transactions: usize,
    pub txid_path: Vec<[u8; 32]>,
    pub block_header: Vec<u8>,
    pub leaf: Vec<u8>,
    pub leaf_index: usize,
    pub leaves: usize,
    pub mmr_path: Vec<MmrStep>,
    pub peaks: Vec<Vec<u8>>,
    pub peak_index: usize,
    pub tip_header: Vec<u8>,
    pub tip_auth_data_root: [u8; 32],
}

impl Located {
    /// The transaction, parsed.
    pub fn transaction(&self) -> Result<Transaction> {
        let branch = BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(self.height));
        Ok(Transaction::read(&self.raw_tx[..], branch)?)
    }
}

/// Locates transaction `txid` under header `tip` (after its block; the relay must accept that
/// header and enough work on top of it).
pub async fn locate(
    client: &mut LwdClient,
    headers_path: &Path,
    summaries_path: &Path,
    txid: TxId,
    tip: u32,
) -> Result<Located> {
    let headers = chain::load_headers(headers_path)?;
    let (raw_tx, height) = lwd::raw_transaction(client, &txid).await?;
    let height = u32::try_from(height)?;
    ensure!(height >= history::NU7_ACTIVATION_HEIGHT && height < tip, "transaction at {height}, anchor {tip}");

    let summaries = summaries(client, &headers, summaries_path, tip - 1).await?;
    let leaves: Vec<_> = summaries.iter().map(|s| history::leaf(history::NU7_BRANCH_ID, s)).collect();
    let mmr = Mmr::new(leaves.clone())?;
    let leaf_index = (height - history::NU7_ACTIVATION_HEIGHT) as usize;
    let (path, peak_index) = mmr.path(leaf_index)?;

    // The transaction's position in its block, and the anchor block's auth data root.
    let block = chain::compact_blocks(client, height, height).await?.remove(0);
    let txids: Vec<[u8; 32]> = block.vtx.iter().map(|t| *t.txid().as_ref()).collect();
    let tx_index = txids.iter().position(|t| t == txid.as_ref()).context("transaction not in its block")?;
    let txid_path = history::txid_merkle_path(&txids, tx_index)?;
    let tip_block = chain::compact_blocks(client, tip, tip).await?.remove(0);
    let branch = BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(tip));
    let mut auth = vec![];
    for ctx in &tip_block.vtx {
        let (raw, _) = lwd::raw_transaction(client, &ctx.txid()).await?;
        auth.push(Transaction::read(&raw[..], branch)?.auth_commitment().as_bytes().try_into()?);
    }
    let tip_auth_data_root = history::auth_data_root(&auth)?;

    let mut raw = raw_headers(headers_path, &[height, tip])?;
    Ok(Located {
        raw_tx,
        height,
        tx_index,
        block_transactions: txids.len(),
        txid_path,
        block_header: raw.remove(&height).expect("checked"),
        leaf: V3::to_bytes(&leaves[leaf_index]),
        leaf_index,
        leaves: leaves.len(),
        mmr_path: path
            .iter()
            .map(|(n, left)| MmrStep {
                sibling: V3::to_bytes(n),
                sibling_is_left: *left,
            })
            .collect(),
        peaks: mmr.peaks().iter().map(V3::to_bytes).collect(),
        peak_index,
        tip_header: raw.remove(&tip).expect("checked"),
        tip_auth_data_root,
    })
}

/// The Orchard-protocol full viewing key of a Testnet UFVK.
pub fn ufvk_fvk(ufvk: &str) -> Result<orchard::keys::FullViewingKey> {
    UnifiedFullViewingKey::decode(&Network::TestNetwork, ufvk)
        .map_err(|e| anyhow!("UFVK: {e}"))?
        .orchard()
        .cloned()
        .context("UFVK without an Orchard-protocol key")
}

/// Builds the witness of deposit `txid` into `vault`, anchored at header `tip` (after the
/// deposit's block; the relay must accept that header and enough work on top of it), and
/// checks it with the statement.
pub async fn build(
    client: &mut LwdClient,
    headers_path: &Path,
    summaries_path: &Path,
    txid: TxId,
    vault: &Vault,
    tip: u32,
) -> Result<Built> {
    let at = locate(client, headers_path, summaries_path, txid, tip).await?;

    // The deposit note, found with the vault's incoming viewing key.
    let fvk = ufvk_fvk(&vault.ufvk)?;
    let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let tx = at.transaction()?;
    let bundle = tx.ironwood_bundle().context("no Ironwood bundle")?;
    let (action_index, note, recipient) = bundle
        .actions()
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            try_note_decryption(&IronwoodDomain::for_action(a), &ivk, a)
                .filter(|(n, _, _)| n.value().inner() == vault.amount)
                .map(|(n, r, _)| (i, n, r))
        })
        .context("no note of the vault's amount to the vault")?;

    let witness = Witness {
        tx: at.raw_tx.clone(),
        action_index: u32::try_from(action_index)?,
        note_recipient: recipient.to_raw_address_bytes().to_vec(),
        note_value: note.value().inner(),
        note_rseed: *note.rseed().as_bytes(),
        tx_index: u32::try_from(at.tx_index)?,
        txid_path: at.txid_path.clone(),
        block_header: at.block_header.clone(),
        leaf: at.leaf.clone(),
        mmr_path: at.mmr_path.clone(),
        peaks: at.peaks.clone(),
        peak_index: u32::try_from(at.peak_index)?,
        tip_header: at.tip_header.clone(),
        tip_auth_data_root: at.tip_auth_data_root,
        vault_id: vault.vault_id,
        owner: vault.owner,
        account: vault.account,
        vault_address: vault.address.clone(),
        vault_ufvk: vault.ufvk.clone(),
        expiry_height: vault.expiry_height,
    };
    let journal = verify(&witness).map_err(|e| anyhow!("the statement refuses the witness: {e:?}"))?;
    Ok(Built {
        witness,
        journal,
        deposit_height: at.height,
        tx_index: at.tx_index,
        block_transactions: at.block_transactions,
        leaf_index: at.leaf_index,
        leaves: at.leaves,
        path_len: at.mmr_path.len(),
        peak_index: at.peak_index,
        peaks: at.peaks.len(),
    })
}
