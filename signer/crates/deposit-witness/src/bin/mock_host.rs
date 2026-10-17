//! MOCK, TEST ONLY: a stand-in for the zkVM host (`zecured-tier2-host`, tier2/host) where RISC
//! Zero does not run (Windows), for a deployment with `MockRiscZeroVerifier` on a local fork. It
//! answers the host's `image-id` and `mock-seal` commands and proves nothing: it runs no zkVM,
//! checks the witness with the native statement (the same function the guest runs) and seals the
//! journal the way the MOCK verifier expects, `0xffffffff || sha256(imageId || sha256(journal))`.
//! The image ids are the local deployment's: `ZECURED_MOCK_DEPOSIT_IMAGE_ID`,
//! `ZECURED_MOCK_VIOLATION_IMAGE_ID` and (Zecured Swap's payment guest)
//! `ZECURED_MOCK_PAYMENT_IMAGE_ID`.
//!
//!   mock-host [violation|payment] image-id
//!   mock-host [violation|payment] mock-seal <witness.json | -> <seal.json>   # `-`: the witness on stdin
//!   mock-host network
//!
//! The coordinator runs it like the host (`DEPOSIT_PROOF_HOST`, `DEPOSIT_PROOF_MOCK=true`); the
//! swap maker bot and `zecured-wallet swap sell --mock` run its `payment` commands.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail, ensure};
use sha2::{Digest, Sha256};
use zecured_deposit_statement::{
    NETWORK, Witness, verify,
    violation::{ViolationWitness, verify_violation},
};
use zecured_payment_statement::{PaymentWitness, verify as verify_payment};

/// Which statement a command is about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Statement {
    Deposit,
    Violation,
    Payment,
}

/// The image id the local deployment registered for the guest of `statement`.
fn image_id(statement: Statement) -> Result<[u8; 32]> {
    let var = match statement {
        Statement::Deposit => "ZECURED_MOCK_DEPOSIT_IMAGE_ID",
        Statement::Violation => "ZECURED_MOCK_VIOLATION_IMAGE_ID",
        Statement::Payment => "ZECURED_MOCK_PAYMENT_IMAGE_ID",
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
fn journal(statement: Statement, witness: &[u8]) -> Result<Vec<u8>> {
    let refused = |e: &dyn std::fmt::Debug| anyhow!("statement refuses the witness natively: {e:?}");
    match statement {
        Statement::Deposit => {
            let w: Witness = serde_json::from_slice(witness)?;
            Ok(verify(&w).map_err(|e| refused(&e))?.abi_encode())
        }
        Statement::Violation => {
            let w: ViolationWitness = serde_json::from_slice(witness)?;
            Ok(verify_violation(&w).map_err(|e| refused(&e))?.abi_encode())
        }
        Statement::Payment => {
            let w: PaymentWitness = serde_json::from_slice(witness)?;
            Ok(verify_payment(&w).map_err(|e| refused(&e))?.abi_encode())
        }
    }
}

/// The MOCK verifier's seal of `journal` for `image_id`: `0xffffffff || sha256(imageId || sha256(journal))`.
fn mock_seal(image_id: &[u8; 32], journal: &[u8]) -> Vec<u8> {
    let claim = Sha256::digest([&image_id[..], &Sha256::digest(journal)[..]].concat());
    [&[0xff; 4][..], &claim[..]].concat()
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    let statement = match args.get(1).map(String::as_str) {
        Some("violation") => Statement::Violation,
        Some("payment") => Statement::Payment,
        _ => Statement::Deposit,
    };
    if statement != Statement::Deposit {
        args.remove(1);
    }
    match args.get(1).map(String::as_str) {
        Some("network") => println!("{}", NETWORK.name),
        Some("image-id") => println!("0x{}", hex::encode(image_id(statement)?)),
        Some("mock-seal") => {
            let input = args.get(2).context("witness path (or -)")?;
            let out = args.get(3).context("seal output path")?;
            let id = image_id(statement)?;
            let journal = journal(statement, &read_witness(input)?)?;
            let json = serde_json::json!({
                "imageId": format!("0x{}", hex::encode(id)),
                "journal": format!("0x{}", hex::encode(&journal)),
                "seal": format!("0x{}", hex::encode(mock_seal(&id, &journal))),
            });
            std::fs::write(out, serde_json::to_vec_pretty(&json)?)?;
            println!("MOCK seal (native statement, no zkVM) written to {out}");
        }
        Some(other) => {
            bail!("MOCK host: `{other}` needs the zkVM host (tier2/host); here only image-id, mock-seal, network")
        }
        None => {
            bail!("usage: mock-host [violation|payment] image-id | mock-seal <witness.json|-> <seal.json> | network")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MOCK seal is what `MockRiscZeroVerifier.mockSeal` computes (contracts/src/mocks): the
    /// marker, then SHA-256 of the image id and the journal's SHA-256.
    #[test]
    fn the_mock_seal_is_the_mock_verifiers() {
        let seal = mock_seal(&[0x11; 32], b"journal");
        assert_eq!(&seal[..4], &[0xff; 4]);
        let mut preimage = vec![0x11; 32];
        preimage.extend_from_slice(&Sha256::digest(b"journal"));
        assert_eq!(&seal[4..], &Sha256::digest(&preimage)[..]);
        assert_eq!(seal.len(), 36);
    }

    /// The payment fixture (TEST-ONLY, synthetic) has the journal the payment statement computes.
    #[test]
    fn the_payment_journal_is_the_statements() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tier2/payment-statement/tests/fixtures/TEST-ONLY_swap_payment_witness.json"
        );
        let bytes = std::fs::read(fixture).unwrap();
        let journal = journal(Statement::Payment, &bytes).unwrap();
        assert_eq!(journal.len(), 192);
        assert_eq!(journal[31], 1, "kind");
        // The deposit statement refuses a payment witness (another struct).
        assert!(super::journal(Statement::Deposit, &bytes).is_err());
    }
}
