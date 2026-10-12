//! Runs the Tier 2 deposit guest on a witness built by `build-witness`, or with `violation`
//! first, the Tier 3 violation guest on a witness built by `build-violation`.
//!
//!   zecured-tier2-host [violation] execute <witness.json>    # zkVM executor: cycles and journal
//!   zecured-tier2-host prove <witness.json> <receipt.bin>    # succinct (STARK) receipt, locally
//!   zecured-tier2-host groth16 <receipt.bin> <seal.json>     # Groth16 seal for Base (x86 + Docker)
//!   zecured-tier2-host mock-seal <witness.json> <seal.json>  # MOCK seal for the local fork only
//!   zecured-tier2-host image-id                              # the guest's image id
//!   zecured-tier2-host export-elf <deposit.bin>              # the embedded guest, to pin it
//!   zecured-tier2-host parity <witness.json>                 # K4: each change fails alike in the zkVM
//!
//! `seal.json` is `{ imageId, journal, seal }`, what `Registry.attestDepositWithProof` (or, for
//! a violation, `ZecuredSlasher.proveViolation`) takes. The violation guest is pinned the same
//! way, with `ZECURED_VIOLATION_ELF`.
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
use zecured_deposit_statement::{
    Witness, verify,
    violation::{ViolationWitness, verify_violation},
};
use zecured_tier2_methods::{DEPOSIT_ELF, DEPOSIT_ID, VIOLATION_ELF, VIOLATION_ID};

/// Which statement a command proves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Statement {
    Deposit,
    Violation,
}

/// The guest every command runs: the pinned file named by `ZECURED_DEPOSIT_ELF` (or
/// `ZECURED_VIOLATION_ELF`), or the one embedded in this binary.
struct Guest {
    elf: Vec<u8>,
    id: Digest,
}

impl Guest {
    fn load(statement: Statement) -> Result<Self> {
        let (var, elf, id) = match statement {
            Statement::Deposit => ("ZECURED_DEPOSIT_ELF", DEPOSIT_ELF, DEPOSIT_ID),
            Statement::Violation => ("ZECURED_VIOLATION_ELF", VIOLATION_ELF, VIOLATION_ID),
        };
        match std::env::var_os(var) {
            Some(path) => {
                let elf = std::fs::read(&path).with_context(|| format!("read {var}"))?;
                let id = compute_image_id(&elf).with_context(|| format!("{var} is not a guest program"))?;
                Ok(Self { elf, id })
            }
            None => Ok(Self {
                elf: elf.to_vec(),
                id: Digest::from(id),
            }),
        }
    }

    fn image_id(&self) -> [u8; 32] {
        self.id.as_bytes().try_into().expect("32 bytes")
    }
}

/// The witness at `path`, checked natively: its bincode frame (the guest's input) and the
/// journal the guest must commit.
fn load_witness(statement: Statement, path: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    let refused = |e: &dyn std::fmt::Debug| anyhow::anyhow!("statement refuses the witness natively: {e:?}");
    match statement {
        Statement::Deposit => {
            let witness: Witness = serde_json::from_slice(&bytes)?;
            let journal = verify(&witness).map_err(|e| refused(&e))?.abi_encode();
            Ok((bincode::serialize(&witness)?, journal))
        }
        Statement::Violation => {
            let witness: ViolationWitness = serde_json::from_slice(&bytes)?;
            let journal = verify_violation(&witness).map_err(|e| refused(&e))?.abi_encode();
            Ok((bincode::serialize(&witness)?, journal))
        }
    }
}

fn env_for(witness: &Witness, po2: Option<u32>) -> Result<ExecutorEnv<'static>> {
    env_for_frame(&bincode::serialize(witness)?, po2)
}

fn env_for_frame(frame: &[u8], po2: Option<u32>) -> Result<ExecutorEnv<'static>> {
    let mut builder = ExecutorEnv::builder();
    builder.write_frame(frame);
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

/// Tier 3: changes of an F1 witness (`TEST-ONLY_vault9_f1_witness.json`), and the same
/// transaction as an F2 witness, give the same outcome natively and in the zkVM: the same journal,
/// or a panic with the statement's own error.
fn violation_parity(guest: &Guest, input: &str) -> Result<()> {
    use zecured_deposit_statement::violation::{Detail, Kind};
    let base: ViolationWitness = serde_json::from_slice(&std::fs::read(input)?)?;
    let Detail::Uncommitted { vault_ufvk, .. } = &base.detail else {
        bail!("an F1 (uncommitted) witness is expected");
    };
    let ufvk = vault_ufvk.clone();
    type Change = (&'static str, Box<dyn Fn(&mut ViolationWitness)>);
    let detail = |f: fn(&mut Detail)| -> Box<dyn Fn(&mut ViolationWitness)> { Box::new(move |w| f(&mut w.detail)) };
    let changes: Vec<Change> = vec![
        ("unchanged", Box::new(|_| {})),
        (
            "another note value",
            detail(|d| {
                if let Detail::Uncommitted { note_value, .. } = d {
                    *note_value += 1
                }
            }),
        ),
        (
            "another rseed",
            detail(|d| {
                if let Detail::Uncommitted { note_rseed, .. } = d {
                    note_rseed[0] ^= 1
                }
            }),
        ),
        (
            "another action",
            detail(|d| {
                if let Detail::Uncommitted { action_index, .. } = d {
                    *action_index ^= 1
                }
            }),
        ),
        (
            "no such action",
            detail(|d| {
                if let Detail::Uncommitted { action_index, .. } = d {
                    *action_index = 99
                }
            }),
        ),
        ("kind of another detail", Box::new(|w| w.kind = Kind::Misdirected)),
        ("txid path", Box::new(|w| w.txid_path[0][0] ^= 1)),
        ("tip auth data root", Box::new(|w| w.tip_auth_data_root[0] ^= 1)),
        ("history peak", Box::new(|w| w.peak_index ^= 1)),
        (
            "F2: a change that is not there",
            Box::new(move |w| {
                w.kind = Kind::WithheldChange;
                w.detail = Detail::WithheldChange {
                    vault_ufvk: ufvk.clone(),
                    change_tag: [7; 32],
                    change_value: 1,
                };
            }),
        ),
    ];
    let mut failures = 0;
    for (name, change) in changes {
        let mut w = base.clone();
        change(&mut w);
        let native = verify_violation(&w).map(|j| j.abi_encode());
        let executed = default_executor().execute(env_for_frame(&bincode::serialize(&w)?, None)?, &guest.elf);
        let same = match (&native, &executed) {
            (Ok(journal), Ok(session)) => session.journal.bytes == *journal,
            (Err(e), Err(x)) => format!("{x:#}").contains(&format!("not a violation: {e:?}")),
            _ => false,
        };
        println!(
            "{name:<34} native {:<38} zkVM {}",
            match &native {
                Ok(_) => "proves".to_owned(),
                Err(e) => format!("{e:?}"),
            },
            if same { "same" } else { "DIFFERS" }
        );
        failures += usize::from(!same);
    }
    ensure!(failures == 0, "{failures} changes differ between native and the zkVM");
    println!("every change has the same outcome natively and in the zkVM");
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    let statement = if args.get(1).map(String::as_str) == Some("violation") {
        args.remove(1);
        Statement::Violation
    } else {
        Statement::Deposit
    };
    let cmd = args
        .get(1)
        .context("command: execute | prove | groth16 | mock-seal | image-id | export-elf | parity")?;
    let guest = Guest::load(statement)?;
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
            let (frame, journal) = load_witness(statement, input)?;
            let started = std::time::Instant::now();
            let session = default_executor().execute(env_for_frame(&frame, None)?, &guest.elf)?;
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
            let (frame, journal) = load_witness(statement, input)?;
            let started = std::time::Instant::now();
            // Segments of 2^18 cycles keep memory around 3 GB on an 8 GB machine.
            let info = default_prover().prove_with_opts(
                env_for_frame(&frame, Some(18))?,
                &guest.elf,
                &ProverOpts::succinct(),
            )?;
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
            let (frame, journal) = load_witness(statement, input)?;
            let session = default_executor().execute(env_for_frame(&frame, None)?, &guest.elf)?;
            ensure!(session.journal.bytes == journal, "journal differs from the native computation");
            let journal_digest = Sha256::digest(&journal);
            let claim = Sha256::digest([&guest.image_id()[..], &journal_digest[..]].concat());
            let seal = [&[0xff, 0xff, 0xff, 0xff][..], &claim[..]].concat();
            write_seal(&guest, out, &journal, &seal)?;
            println!("MOCK seal for {} cycles written to {out}", session.cycles());
        }
        "parity" if statement == Statement::Violation => violation_parity(&guest, input)?,
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
