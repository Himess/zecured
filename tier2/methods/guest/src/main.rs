//! Compile and cost probe for the deposit statement (Tier 2): parses a real v6 transaction
//! and computes its txid, recomputes an Ironwood note commitment, hashes a history-tree node,
//! and reports the cycles each step takes.

use orchard::{
    Address,
    note::{ExtractedNoteCommitment, Note, NoteVersion, RandomSeed, Rho},
    value::NoteValue,
};
use risc0_zkvm::guest::env;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

fn main() {
    let raw_tx: Vec<u8> = env::read();
    let branch_id: u32 = env::read();
    let (recipient, value, rho, rseed): (Vec<u8>, u64, [u8; 32], [u8; 32]) = env::read();
    let recipient: [u8; 43] = recipient.try_into().expect("43-byte receiver");

    let c0 = env::cycle_count();
    let branch = BranchId::try_from(branch_id).expect("branch id");
    let tx = Transaction::read(&raw_tx[..], branch).expect("parse transaction");
    let txid = tx.txid();
    let c1 = env::cycle_count();

    let recipient = Address::from_raw_address_bytes(&recipient).expect("address");
    let rho = Rho::from_bytes(&rho).expect("rho");
    let rseed = RandomSeed::from_bytes(rseed, &rho).expect("rseed");
    let note = Note::from_parts(recipient, NoteValue::from_raw(value), rho, rseed, NoteVersion::V3).expect("note");
    let cmx = ExtractedNoteCommitment::from(note.commitment()).to_bytes();
    let c2 = env::cycle_count();
    let again = ExtractedNoteCommitment::from(note.commitment()).to_bytes();
    assert_eq!(again, cmx);
    let c2b = env::cycle_count();
    env::log(&format!("second NoteCommit: {} cycles", c2b - c2));

    let node = zcash_history::NodeDataV3::default();
    let node_hash = <zcash_history::V3 as zcash_history::Version>::hash(&node);
    let c3 = env::cycle_count();

    env::commit(&(*txid.as_ref(), cmx, node_hash, c1 - c0, c2 - c1, c3 - c2));
}
