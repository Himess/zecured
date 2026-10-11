//! Runs the Tier 2 deposit guest on a witness built by `build-witness`.
//!
//!   zecured-tier2-host execute <witness.json>                # zkVM executor: cycles and journal
//!   zecured-tier2-host prove <witness.json> <receipt.bin>    # succinct (STARK) receipt, locally
//!   zecured-tier2-host groth16 <receipt.bin> <seal.json>     # Groth16 seal for Base (x86 + Docker)
//!   zecured-tier2-host mock-seal <witness.json> <seal.json>  # MOCK seal for the local fork only
//!   zecured-tier2-host image-id                              # the guest's image id
//!
//! `seal.json` is `{ imageId, journal, seal }`, what `Registry.attestDepositWithProof` takes.
//! The witness contains the vault's viewing key and stays on the machine that builds and proves
//! it; the Groth16 step needs only the succinct receipt, which RISC Zero documents as a
//! zero-knowledge proof.

use anyhow::{Context, Result, bail, ensure};
use risc0_zkvm::{ExecutorEnv, ProverOpts, Receipt, default_executor, default_prover, sha::Digest};
use sha2::{Digest as _, Sha256};
use zecured_deposit_statement::{Witness, verify};
use zecured_tier2_methods::{DEPOSIT_ELF, DEPOSIT_ID};

fn image_id() -> [u8; 32] {
    Digest::from(DEPOSIT_ID).as_bytes().try_into().expect("32 bytes")
}

/// The witness at `path`, checked natively, and the journal the guest must commit.
fn load_witness(path: &str) -> Result<(Witness, Vec<u8>)> {
    let witness: Witness = serde_json::from_slice(&std::fs::read(path)?)?;
    let journal =
        verify(&witness).map_err(|e| anyhow::anyhow!("statement refuses the witness natively: {e:?}"))?.abi_encode();
    Ok((witness, journal))
}

fn env_for(witness: &Witness, po2: Option<u32>) -> Result<ExecutorEnv<'static>> {
    let frame = bincode::serialize(witness)?;
    let mut builder = ExecutorEnv::builder();
    builder.write_frame(&frame);
    if let Some(po2) = po2 {
        builder.segment_limit_po2(po2);
    }
    builder.build()
}

fn write_seal(out: &str, journal: &[u8], seal: &[u8]) -> Result<()> {
    let json = serde_json::json!({
        "imageId": format!("0x{}", hex::encode(image_id())),
        "journal": format!("0x{}", hex::encode(journal)),
        "seal": format!("0x{}", hex::encode(seal)),
    });
    std::fs::write(out, serde_json::to_vec_pretty(&json)?)?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).context("command: execute | prove | groth16 | mock-seal | image-id")?;
    if cmd == "image-id" {
        println!("0x{}", hex::encode(image_id()));
        return Ok(());
    }
    let input = args.get(2).context("input path")?;
    println!("image id 0x{}", hex::encode(image_id()));

    match cmd.as_str() {
        "execute" => {
            let (witness, journal) = load_witness(input)?;
            let started = std::time::Instant::now();
            let session = default_executor().execute(env_for(&witness, None)?, DEPOSIT_ELF)?;
            ensure!(session.journal.bytes == journal, "journal differs from the native computation");
            println!(
                "executed in {:.1?}: {} cycles, {} segments; journal matches the native computation",
                started.elapsed(),
                session.cycles(),
                session.segments.len()
            );
            println!("journal 0x{}", hex::encode(&journal));
        }
        "prove" => {
            let out = args.get(3).context("receipt output path")?;
            let (witness, journal) = load_witness(input)?;
            let started = std::time::Instant::now();
            // Segments of 2^18 cycles keep memory around 3 GB on an 8 GB machine.
            let info = default_prover().prove_with_opts(env_for(&witness, Some(18))?, DEPOSIT_ELF, &ProverOpts::succinct())?;
            info.receipt.verify(DEPOSIT_ID)?;
            ensure!(info.receipt.journal.bytes == journal, "journal differs");
            std::fs::write(out, bincode::serialize(&info.receipt)?)?;
            println!(
                "succinct receipt for {} cycles ({} segments) in {:.1?}, verified, written to {out}",
                info.stats.total_cycles,
                info.stats.segments,
                started.elapsed()
            );
        }
        "groth16" => {
            let out = args.get(3).context("seal output path")?;
            let receipt: Receipt = bincode::deserialize(&std::fs::read(input)?)?;
            receipt.verify(DEPOSIT_ID).context("the receipt is not a proof of this guest")?;
            let started = std::time::Instant::now();
            let wrapped = default_prover().compress(&ProverOpts::groth16(), &receipt)?;
            wrapped.verify(DEPOSIT_ID)?;
            let g = wrapped.inner.groth16()?;
            // The router dispatches on the first four bytes of the verifier parameters digest.
            let seal = [&g.verifier_parameters.as_bytes()[..4], &g.seal[..]].concat();
            write_seal(out, &wrapped.journal.bytes, &seal)?;
            println!("Groth16 seal in {:.1?}, verified, written to {out}", started.elapsed());
        }
        "mock-seal" => {
            // MOCK: proves nothing. For a Registry deployed with MockRiscZeroVerifier (local fork).
            let out = args.get(3).context("seal output path")?;
            let (witness, journal) = load_witness(input)?;
            let session = default_executor().execute(env_for(&witness, None)?, DEPOSIT_ELF)?;
            ensure!(session.journal.bytes == journal, "journal differs from the native computation");
            let journal_digest = Sha256::digest(&journal);
            let claim = Sha256::digest([&image_id()[..], &journal_digest[..]].concat());
            let seal = [&[0xff, 0xff, 0xff, 0xff][..], &claim[..]].concat();
            write_seal(out, &journal, &seal)?;
            println!("MOCK seal for {} cycles written to {out}", session.cycles());
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
