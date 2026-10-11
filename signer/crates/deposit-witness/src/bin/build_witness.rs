//! Builds the private input of a Tier 2 deposit proof from public chain data plus what the
//! depositor knows (the vault's address, viewing key and expiry; the deposit note is found by
//! trial decryption with the viewing key), then checks it with the statement natively.
//!
//!   cargo run -p zecured-deposit-witness --bin build-witness -- \
//!     --deposit-txid <txid> --vault vault.json --tip <height> \
//!     --headers data/tier2/headers_nu7.json --out witness.json
//!
//! `vault.json` holds `vault_id`, `owner`, `account`, `address`, `ufvk`, `expiry_height` and
//! `amount`, the values of the vault's `VaultOpened` (a coordinator vault record has them). The
//! witness contains the vault's viewing key: keep it local.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
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
use zecured_deposit_statement::{MmrStep, Witness, verify};
use zecured_deposit_witness::{
    chain,
    history::{self, BlockSummary},
    mmr::Mmr,
};
use zecured_zcash_ops::lwd;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    deposit_txid: String,
    /// JSON with the vault's `VaultOpened` values (`vault_id`, `owner`, `account`, `address`,
    /// `ufvk`, `expiry_height`, `amount`).
    #[arg(long)]
    vault: PathBuf,
    /// Height of the header the proof is anchored to (after the deposit's block).
    #[arg(long)]
    tip: u32,
    /// JSON Lines headers from NU7 activation to the tip.
    #[arg(long)]
    headers: PathBuf,
    /// Cache of per-block summaries (built on first use; 2-3 minutes).
    #[arg(long, default_value = "data/tier2/summaries.json")]
    summaries: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "https://testnet.zec.rocks:443")]
    lwd: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let vault: serde_json::Value = serde_json::from_slice(&fs::read(&args.vault)?)?;
    let bytes = |key: &str| -> Result<Vec<u8>> {
        let text = vault[key].as_str().with_context(|| format!("vault {key}"))?;
        Ok(hex::decode(text.trim_start_matches("0x"))?)
    };
    let vault_id: [u8; 32] = bytes("vault_id")?.try_into().map_err(|_| anyhow!("vault_id: 32 bytes"))?;
    let owner: [u8; 20] = bytes("owner")?.try_into().map_err(|_| anyhow!("owner: 20 bytes"))?;
    let account: [u8; 20] = bytes("account")?.try_into().map_err(|_| anyhow!("account: 20 bytes"))?;
    let address = vault["address"].as_str().context("vault address")?.to_owned();
    let ufvk = vault["ufvk"].as_str().context("vault ufvk")?.to_owned();
    let expiry_height = u32::try_from(vault["expiry_height"].as_u64().context("expiry_height")?)?;
    let amount = vault["amount"].as_u64().context("amount")?;

    let headers = chain::load_headers(&args.headers)?;
    let mut client = lwd::connect(&args.lwd).await?;
    let txid = TxId::from_hex(&args.deposit_txid).context("txid")?;
    let (raw_tx, height) = lwd::raw_transaction(&mut client, &txid).await?;
    let height = u32::try_from(height)?;
    ensure!(height >= history::NU7_ACTIVATION_HEIGHT && height < args.tip, "deposit at {height}, tip {}", args.tip);

    // Per-block summaries of the NU7 epoch up to the block before the tip.
    let last = args.tip - 1;
    let cached: Option<Vec<BlockSummary>> =
        fs::read(&args.summaries).ok().and_then(|b| serde_json::from_slice::<Vec<BlockSummary>>(&b).ok());
    let mut summaries = match cached {
        Some(s) if s.first().map(|b| b.height) == Some(history::NU7_ACTIVATION_HEIGHT) => s,
        _ => vec![],
    };
    summaries.retain(|s| s.height <= last);
    let have = summaries.last().map_or(history::NU7_ACTIVATION_HEIGHT - 1, |s| s.height);
    if have < last {
        // Extend the cache (the trees restart from the server's tree state at `have`).
        let more = chain::block_summaries(&mut client, &headers, have + 1, last).await?;
        summaries.extend(more);
        fs::write(&args.summaries, serde_json::to_vec(&summaries)?)?;
    }
    ensure!(summaries.len() == (last - history::NU7_ACTIVATION_HEIGHT + 1) as usize, "summary gap");

    let leaves: Vec<_> = summaries.iter().map(|s| history::leaf(history::NU7_BRANCH_ID, s)).collect();
    let mmr = Mmr::new(leaves.clone())?;
    let leaf_index = (height - history::NU7_ACTIVATION_HEIGHT) as usize;
    let (path, peak_index) = mmr.path(leaf_index)?;

    // The deposit's position in its block, and the tip block's auth data root.
    let block = chain::compact_blocks(&mut client, height, height).await?.remove(0);
    let txids: Vec<[u8; 32]> = block.vtx.iter().map(|t| *t.txid().as_ref()).collect();
    let tx_index = txids.iter().position(|t| t == txid.as_ref()).context("deposit not in its block")?;
    let txid_path = history::txid_merkle_path(&txids, tx_index)?;

    let tip_block = chain::compact_blocks(&mut client, args.tip, args.tip).await?.remove(0);
    let branch = BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(args.tip));
    let mut auth = vec![];
    for ctx in &tip_block.vtx {
        let (raw, _) = lwd::raw_transaction(&mut client, &ctx.txid()).await?;
        auth.push(Transaction::read(&raw[..], branch)?.auth_commitment().as_bytes().try_into()?);
    }
    let tip_auth_data_root = history::auth_data_root(&auth)?;

    // The deposit note, found with the vault's incoming viewing key.
    let fvk = UnifiedFullViewingKey::decode(&Network::TestNetwork, &ufvk)
        .map_err(|e| anyhow!("UFVK: {e}"))?
        .orchard()
        .cloned()
        .context("UFVK without an Orchard-protocol key")?;
    let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let tx = Transaction::read(&raw_tx[..], BranchId::for_height(&Network::TestNetwork, BlockHeight::from_u32(height)))?;
    let bundle = tx.ironwood_bundle().context("no Ironwood bundle")?;
    let (action_index, note, recipient) = bundle
        .actions()
        .iter()
        .enumerate()
        .find_map(|(i, a)| {
            try_note_decryption(&IronwoodDomain::for_action(a), &ivk, a)
                .filter(|(n, _, _)| n.value().inner() == amount)
                .map(|(n, r, _)| (i, n, r))
        })
        .context("no note of the vault's amount to the vault")?;

    let header = |h: u32| -> Result<Vec<u8>> {
        let raw = fs::read_to_string(&args.headers)?
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["height"].as_u64() == Some(u64::from(h)))
            .and_then(|v| v["raw"].as_str().map(str::to_owned))
            .with_context(|| format!("header {h}"))?;
        Ok(hex::decode(raw)?)
    };
    let witness = Witness {
        tx: raw_tx,
        action_index: u32::try_from(action_index)?,
        note_recipient: recipient.to_raw_address_bytes().to_vec(),
        note_value: note.value().inner(),
        note_rseed: *note.rseed().as_bytes(),
        tx_index: u32::try_from(tx_index)?,
        txid_path,
        block_header: header(height)?,
        leaf: V3::to_bytes(&leaves[leaf_index]),
        mmr_path: path.iter().map(|(n, left)| MmrStep { sibling: V3::to_bytes(n), sibling_is_left: *left }).collect(),
        peaks: mmr.peaks().iter().map(V3::to_bytes).collect(),
        peak_index: u32::try_from(peak_index)?,
        tip_header: header(args.tip)?,
        tip_auth_data_root,
        vault_id,
        owner,
        account,
        vault_address: address,
        vault_ufvk: ufvk,
        expiry_height,
    };
    let journal = verify(&witness).map_err(|e| anyhow!("the statement refuses the witness: {e:?}"))?;
    fs::write(&args.out, serde_json::to_vec(&witness)?)?;
    println!(
        "deposit at {height} (tx {tx_index} of {}), leaf {leaf_index} of {}, path {} steps, peak {peak_index} of {}",
        txids.len(),
        leaves.len(),
        path.len(),
        mmr.peaks().len()
    );
    println!(
        "journal: tip {} amount {} tag {} VaultOpened {}",
        hex::encode(journal.tip_hash),
        journal.amount,
        hex::encode(journal.tag),
        hex::encode(journal.vault_opened_hash)
    );
    println!("witness written to {} (keep it local: it contains the vault's viewing key)", args.out.display());
    Ok(())
}
