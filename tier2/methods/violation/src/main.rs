//! The Tier 3 violation proof (docs/TIER3.md § 6): proves that the private witness satisfies the
//! violation statement (`zecured_deposit_statement::violation`) and publishes its public output,
//! ABI-encoded for `ZecuredSlasher.proveViolation`, as the journal.

use risc0_zkvm::guest::env;
use zecured_deposit_statement::violation::{ViolationWitness, verify_violation};

fn main() {
    // One bincode frame (length, then bytes), as the deposit guest.
    let mut len = 0u32;
    env::read_slice(core::slice::from_mut(&mut len));
    let mut frame = vec![0u8; len as usize];
    env::read_slice(&mut frame);
    let witness: ViolationWitness = bincode::deserialize(&frame).expect("witness encoding");
    match verify_violation(&witness) {
        Ok(journal) => env::commit_slice(&journal.abi_encode()),
        Err(e) => panic!("not a violation: {e:?}"),
    }
}
