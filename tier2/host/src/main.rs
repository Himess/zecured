//! Runs the Tier 2 deposit guest on a witness built by `build-witness`.
//!
//!   zecured-tier2-host execute <witness.json>          # zkVM executor: cycles and journal
//!   zecured-tier2-host prove <witness.json> <out.bin>  # local succinct proof (receipt)
//!
//! Proving happens on this machine: the witness (with the vault's viewing key) never leaves it.
//! Only the receipt, which reveals nothing but the journal, would go to a Groth16 wrapper.

use anyhow::{Context, Result, bail, ensure};
use risc0_zkvm::{ExecutorEnv, ProverOpts, default_executor, default_prover};
use zecured_deposit_statement::{Witness, verify};
use zecured_tier2_methods::{DEPOSIT_ELF, DEPOSIT_ID};

fn image_id_hex() -> String {
    hex::encode(DEPOSIT_ID.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (cmd, path) = (args.get(1).context("command")?, args.get(2).context("witness path")?);
    let witness: Witness = serde_json::from_slice(&std::fs::read(path)?)?;
    let expected = verify(&witness).map_err(|e| anyhow::anyhow!("statement refuses the witness natively: {e:?}"))?;
    let expected_journal = expected.abi_encode();
    println!("image id {}", image_id_hex());

    match cmd.as_str() {
        "execute" => {
            let frame = bincode::serialize(&witness)?;
            let env = ExecutorEnv::builder().write_frame(&frame).build()?;
            let started = std::time::Instant::now();
            let session = default_executor().execute(env, DEPOSIT_ELF)?;
            ensure!(session.journal.bytes == expected_journal, "journal differs from the native computation");
            println!(
                "executed in {:.1?}: {} cycles, {} segments; journal matches the native computation",
                started.elapsed(),
                session.cycles(),
                session.segments.len()
            );
        }
        "prove" => {
            let out = args.get(3).context("receipt output path")?;
            // Segments of 2^18 cycles keep memory around 3 GB on an 8 GB machine.
            let frame = bincode::serialize(&witness)?;
            let env = ExecutorEnv::builder().write_frame(&frame).segment_limit_po2(18).build()?;
            let started = std::time::Instant::now();
            let info = default_prover().prove_with_opts(env, DEPOSIT_ELF, &ProverOpts::succinct())?;
            info.receipt.verify(DEPOSIT_ID)?;
            ensure!(info.receipt.journal.bytes == expected_journal, "journal differs");
            std::fs::write(out, bincode::serialize(&info.receipt)?)?;
            println!(
                "succinct receipt for {} cycles ({} segments) in {:.1?}, verified, written to {out}",
                info.stats.total_cycles,
                info.stats.segments,
                started.elapsed()
            );
        }
        other => bail!("unknown command {other}"),
    }
    println!("journal {}", hex::encode(&expected_journal));
    Ok(())
}
