//! Runs the deposit probe guest in the zkVM executor (no proof) and compares its outputs with
//! the same computations done natively.
//!
//!   cargo run -p zecured-tier2-host --release -- ../data/tier2/tx_61a4fdc0.json

use anyhow::{Context, Result, ensure};
use orchard::{
    keys::{FullViewingKey, Scope, SpendingKey},
    note::{ExtractedNoteCommitment, Note, NoteVersion, RandomSeed, Rho},
    value::NoteValue,
};
use risc0_zkvm::{ExecutorEnv, default_executor};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
use zecured_tier2_methods::{DEPOSIT_ELF, DEPOSIT_ID};

const NU7_BRANCH_ID: u32 = 0x77190AD9;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).context("usage: zecured-tier2-host <tx.json>")?;
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let raw = hex::decode(json["raw"].as_str().context("raw")?)?;

    let tx = Transaction::read(&raw[..], BranchId::try_from(NU7_BRANCH_ID).map_err(anyhow::Error::msg)?)?;
    let sk = (0u8..=255).find_map(|i| Option::<SpendingKey>::from(SpendingKey::from_bytes([i; 32]))).unwrap();
    let recipient = FullViewingKey::from(&sk).address_at(0u32, Scope::External);
    let (value, rho, rseed) = (1_000_000u64, [1u8; 32], [2u8; 32]);
    let rho_v = Option::<Rho>::from(Rho::from_bytes(&rho)).context("rho")?;
    let note = Option::<Note>::from(Note::from_parts(
        recipient,
        NoteValue::from_raw(value),
        rho_v,
        Option::<RandomSeed>::from(RandomSeed::from_bytes(rseed, &rho_v)).context("rseed")?,
        NoteVersion::V3,
    ))
    .context("note")?;
    let cmx = ExtractedNoteCommitment::from(note.commitment()).to_bytes();

    let env = ExecutorEnv::builder()
        .write(&raw)?
        .write(&NU7_BRANCH_ID)?
        .write(&(recipient.to_raw_address_bytes().to_vec(), value, rho, rseed))?
        .build()?;
    let started = std::time::Instant::now();
    let session = default_executor().execute(env, DEPOSIT_ELF)?;
    let (g_txid, g_cmx, _node_hash, c_tx, c_note, c_hist): ([u8; 32], [u8; 32], [u8; 32], u64, u64, u64) =
        session.journal.decode()?;
    ensure!(g_txid == *tx.txid().as_ref(), "txid differs");
    ensure!(g_cmx == cmx, "note commitment differs");
    println!("image id {}", hex::encode(bytemuck_id(DEPOSIT_ID)));
    println!("executed in {:.1?}: {} cycles, {} segments", started.elapsed(), session.cycles(), session.segments.len());
    println!("  parse + txid of a {}-byte v6 transaction: {c_tx} cycles", raw.len());
    println!("  Ironwood note commitment (NoteCommit, Sinsemilla): {c_note} cycles");
    println!("  one history-tree node hash (BLAKE2b): {c_hist} cycles");
    println!("outputs match the native computation");
    Ok(())
}

fn bytemuck_id(id: [u32; 8]) -> Vec<u8> {
    id.iter().flat_map(|w| w.to_le_bytes()).collect()
}
