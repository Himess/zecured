//! MOCK, TEST ONLY: a stand-in for the zkVM host (`zecured-tier2-host`, tier2/host) where RISC
//! Zero does not run (Windows), for a deployment with `MockRiscZeroVerifier` on a local fork. It
//! answers the host's `image-id` and `mock-seal` commands and proves nothing: it runs no zkVM,
//! checks the witness with the native statement (the same function the guest runs) and seals the
//! journal the way the MOCK verifier expects, `0xffffffff || sha256(imageId || sha256(journal))`.
//! The image ids are the local deployment's: `ZECURED_MOCK_DEPOSIT_IMAGE_ID` and
//! `ZECURED_MOCK_VIOLATION_IMAGE_ID`.
//!
//!   mock-host [violation] image-id
//!   mock-host [violation] mock-seal <witness.json | -> <seal.json>   # `-`: the witness on stdin
//!   mock-host network
//!
//! The coordinator runs it like the host (`DEPOSIT_PROOF_HOST`, `DEPOSIT_PROOF_MOCK=true`).

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail, ensure};
use sha2::{Digest, Sha256};
use zecured_deposit_statement::{
    NETWORK, Witness, verify,
    violation::{ViolationWitness, verify_violation},
};

/// The image id the local deployment registered for the deposit or the violation guest.
fn image_id(violation: bool) -> Result<[u8; 32]> {
    let var = if violation {
        "ZECURED_MOCK_VIOLATION_IMAGE_ID"
    } else {
        "ZECURED_MOCK_DEPOSIT_IMAGE_ID"
    };
    let value = std::env::var(var).with_context(|| format!("MOCK host: set {var} to the deployment's image id"))?;
    let id: [u8; 32] = hex::decode(value.trim().trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("{var} is not 32 bytes"))?;
    ensure!(id != [0; 32], "{var} is zero");
    Ok(id)
}

/// The witness at `path`, or on standard input for `-` (never written to disk, H-12).
fn read_witness(path: &str) -> Result<Vec<u8>> {
    if path == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    } else {
        Ok(std::fs::read(path)?)
    }
}

/// The journal the guest would commit, from the native statement.
fn journal(violation: bool, witness: &[u8]) -> Result<Vec<u8>> {
    let refused = |e: &dyn std::fmt::Debug| anyhow!("statement refuses the witness natively: {e:?}");
    if violation {
        let w: ViolationWitness = serde_json::from_slice(witness)?;
        Ok(verify_violation(&w).map_err(|e| refused(&e))?.abi_encode())
    } else {
        let w: Witness = serde_json::from_slice(witness)?;
        Ok(verify(&w).map_err(|e| refused(&e))?.abi_encode())
    }
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    let violation = args.get(1).map(String::as_str) == Some("violation");
    if violation {
        args.remove(1);
    }
    match args.get(1).map(String::as_str) {
        Some("network") => println!("{}", NETWORK.name),
        Some("image-id") => println!("0x{}", hex::encode(image_id(violation)?)),
        Some("mock-seal") => {
            let input = args.get(2).context("witness path (or -)")?;
            let out = args.get(3).context("seal output path")?;
            let id = image_id(violation)?;
            let journal = journal(violation, &read_witness(input)?)?;
            let claim = Sha256::digest([&id[..], &Sha256::digest(&journal)[..]].concat());
            let seal = [&[0xff; 4][..], &claim[..]].concat();
            let json = serde_json::json!({
                "imageId": format!("0x{}", hex::encode(id)),
                "journal": format!("0x{}", hex::encode(&journal)),
                "seal": format!("0x{}", hex::encode(seal)),
            });
            std::fs::write(out, serde_json::to_vec_pretty(&json)?)?;
            println!("MOCK seal (native statement, no zkVM) written to {out}");
        }
        Some(other) => {
            bail!("MOCK host: `{other}` needs the zkVM host (tier2/host); here only image-id, mock-seal, network")
        }
        None => bail!("usage: mock-host [violation] image-id | mock-seal <witness.json|-> <seal.json> | network"),
    }
    Ok(())
}
