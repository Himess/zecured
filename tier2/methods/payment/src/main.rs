//! The Zecured Swap payment proof: proves that the private witness satisfies the payment statement
//! (`zecured-payment-statement`) and publishes its public output, ABI-encoded for
//! `ZecuredSwap.proveSettlement`, as the journal.

use risc0_zkvm::guest::env;
use zecured_payment_statement::{PaymentWitness, verify};

fn main() {
    // One bincode frame (length, then bytes), as the deposit guest.
    let mut len = 0u32;
    env::read_slice(core::slice::from_mut(&mut len));
    let mut frame = vec![0u8; len as usize];
    env::read_slice(&mut frame);
    let witness: PaymentWitness = bincode::deserialize(&frame).expect("witness encoding");
    match verify(&witness) {
        Ok(journal) => env::commit_slice(&journal.abi_encode()),
        Err(e) => panic!("not a valid payment: {e:?}"),
    }
}
