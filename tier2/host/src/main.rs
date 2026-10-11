//! Runs the Tier 2 deposit guest on a witness built by `build-witness`.
//!
//!   zecured-tier2-host execute <witness.json>                # zkVM executor: cycles and journal
//!   zecured-tier2-host prove <witness.json> <receipt.bin>    # succinct (STARK) receipt, locally
//!   zecured-tier2-host groth16 <receipt.bin> <seal.json>     # Groth16 seal for Base (x86 + Docker)
//!   zecured-tier2-host mock-seal <witness.json> <seal.json>  # MOCK seal for the local fork only
//!   zecured-tier2-host image-id                              # the guest's image id
//!   zecured-tier2-host export-elf <deposit.bin>              # the embedded guest, to pin it
//!   zecured-tier2-host parity <witness.json>                 # K4: each change fails alike in the zkVM
//!
//! `seal.json` is `{ imageId, journal, seal }`, what `Registry.attestDepositWithProof` takes.
//!
//! The guest's image id depends on where it was built (panic messages carry the source paths
//! of its dependencies), and the Registry pins one image id for good. So the guest is built
//! once, exported with `export-elf`, and every prover runs that same file: with
//! `ZECURED_DEPOSIT_ELF=<deposit.bin>` set, every command uses it instead of the guest
//! embedded in this binary.
//! The witness contains the vault's viewing key and stays on the machine that builds and proves
//! it; the Groth16 step needs only the succinct receipt, which RISC Zero documents as a
//! zero-knowledge proof.

use anyhow::{Context, Result, bail, ensure};
use risc0_zkvm::{ExecutorEnv, ProverOpts, Receipt, compute_image_id, default_executor, default_prover, sha::Digest};
use sha2::{Digest as _, Sha256};
use zecured_deposit_statement::{Witness, verify};
use zecured_tier2_methods::{DEPOSIT_ELF, DEPOSIT_ID};

/// The deposit guest every command runs: the pinned file named by `ZECURED_DEPOSIT_ELF`, or the
/// one embedded in this binary.
struct Guest {
    elf: Vec<u8>,
    id: Digest,
}

impl Guest {
    fn load() -> Result<Self> {
        match std::env::var_os("ZECURED_DEPOSIT_ELF") {
            Some(path) => {
                let elf = std::fs::read(&path).context("read ZECURED_DEPOSIT_ELF")?;
                let id = compute_image_id(&elf).context("ZECURED_DEPOSIT_ELF is not a guest program")?;
                Ok(Self { elf, id })
            }
            None => Ok(Self {
                elf: DEPOSIT_ELF.to_vec(),
                id: Digest::from(DEPOSIT_ID),
            }),
        }
    }

    fn image_id(&self) -> [u8; 32] {
        self.id.as_bytes().try_into().expect("32 bytes")
    }
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

fn write_seal(guest: &Guest, out: &str, journal: &[u8], seal: &[u8]) -> Result<()> {
    let json = serde_json::json!({
        "imageId": format!("0x{}", hex::encode(guest.image_id())),
        "journal": format!("0x{}", hex::encode(journal)),
        "seal": format!("0x{}", hex::encode(seal)),
    });
    std::fs::write(out, serde_json::to_vec_pretty(&json)?)?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).context("command: execute | prove | groth16 | mock-seal | image-id | export-elf | parity")?;
    let guest = Guest::load()?;
    if cmd == "image-id" {
        println!("0x{}", hex::encode(guest.image_id()));
        return Ok(());
    }
    let input = args.get(2).context("input path")?;
    if cmd == "export-elf" {
        std::fs::write(input, &guest.elf)?;
        println!("guest 0x{} written to {input}", hex::encode(guest.image_id()));
        return Ok(());
    }
    println!("image id 0x{}", hex::encode(guest.image_id()));

    match cmd.as_str() {
        "execute" => {
            let (witness, journal) = load_witness(input)?;
            let started = std::time::Instant::now();
            let session = default_executor().execute(env_for(&witness, None)?, &guest.elf)?;
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
            let info = default_prover().prove_with_opts(env_for(&witness, Some(18))?, &guest.elf, &ProverOpts::succinct())?;
            info.receipt.verify(guest.id)?;
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
            receipt.verify(guest.id).context("the receipt is not a proof of this guest")?;
            let started = std::time::Instant::now();
            let wrapped = default_prover().compress(&ProverOpts::groth16(), &receipt)?;
            wrapped.verify(guest.id)?;
            let g = wrapped.inner.groth16()?;
            // The router dispatches on the first four bytes of the verifier parameters digest.
            let seal = [&g.verifier_parameters.as_bytes()[..4], &g.seal[..]].concat();
            write_seal(&guest, out, &wrapped.journal.bytes, &seal)?;
            println!("Groth16 seal in {:.1?}, verified, written to {out}", started.elapsed());
        }
        "mock-seal" => {
            // MOCK: proves nothing. For a Registry deployed with MockRiscZeroVerifier (local fork).
            let out = args.get(3).context("seal output path")?;
            let (witness, journal) = load_witness(input)?;
            let session = default_executor().execute(env_for(&witness, None)?, &guest.elf)?;
            ensure!(session.journal.bytes == journal, "journal differs from the native computation");
            let journal_digest = Sha256::digest(&journal);
            let claim = Sha256::digest([&guest.image_id()[..], &journal_digest[..]].concat());
            let seal = [&[0xff, 0xff, 0xff, 0xff][..], &claim[..]].concat();
            write_seal(&guest, out, &journal, &seal)?;
            println!("MOCK seal for {} cycles written to {out}", session.cycles());
        }
        "parity" => {
            // K4: every change of the catalog fails natively and in the executor with the same
            // error (the guest panics with the statement's own error).
            let base: Witness = serde_json::from_slice(&std::fs::read(input)?)?;
            let started = std::time::Instant::now();
            let session = default_executor().execute(env_for(&base, None)?, &guest.elf)?;
            println!("unchanged witness: proves, {} cycles in {:.1?}", session.cycles(), started.elapsed());
            let mut failures = 0;
            for m in zecured_deposit_k4::mutations() {
                let mut w = base.clone();
                (m.apply)(&mut w);
                let native = verify(&w).err();
                let expected = format!("not a valid deposit: {:?}", m.expected);
                let started = std::time::Instant::now();
                let executed = default_executor().execute(env_for(&w, None)?, &guest.elf);
                let elapsed = started.elapsed();
                let same_native = native.as_ref() == Some(&m.expected);
                let same_zkvm = matches!(&executed, Err(e) if format!("{e:#}").contains(&expected));
                println!(
                    "{:<46} {:<24} native {} zkVM {} ({elapsed:.1?})",
                    m.name,
                    format!("{:?}", m.expected),
                    if same_native { "ok" } else { "DIFFERS" },
                    if same_zkvm { "ok" } else { "DIFFERS" },
                );
                if !(same_native && same_zkvm) {
                    failures += 1;
                }
            }
            ensure!(failures == 0, "{failures} changes did not fail alike natively and in the zkVM");
            println!("every change fails with the same error natively and in the zkVM");
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}
