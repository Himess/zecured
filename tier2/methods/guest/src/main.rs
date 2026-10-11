//! The Tier 2 deposit proof (docs/TIER2.md): proves that the private witness satisfies the
//! deposit statement (`zecured-deposit-statement`) and publishes its public output, ABI-encoded
//! for the Registry, as the journal.

use risc0_zkvm::guest::env;
use zecured_deposit_statement::{Witness, verify};

fn main() {
    // One bincode frame (length, then bytes): two reads instead of one per serialized word.
    let mut len = 0u32;
    env::read_slice(core::slice::from_mut(&mut len));
    let mut frame = vec![0u8; len as usize];
    env::read_slice(&mut frame);
    let witness: Witness = bincode::deserialize(&frame).expect("witness encoding");
    match verify(&witness) {
        Ok(journal) => env::commit_slice(&journal.abi_encode()),
        Err(e) => panic!("not a valid deposit: {e:?}"),
    }
}
