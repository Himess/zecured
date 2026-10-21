//! Proving a payment: the zkVM host (`zecured-tier2-host payment prove` then `groth16`) or, on a
//! local fork with `MockRiscZeroVerifier`, the MOCK seal (`payment mock-seal`, from the host or from
//! `mock-host` where RISC Zero does not run). The witness goes to the host on standard input (`-`):
//! it holds the recipient's receiver and is never written to disk here.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use alloy::primitives::{B256, Bytes};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use zecured_payment_statement::PaymentWitness;

/// What `proveSettlement` takes: `{ imageId, journal, seal }`, the host's seal file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealFile {
    pub image_id: B256,
    pub journal: Bytes,
    pub seal: Bytes,
}

impl SealFile {
    /// The seal's router selector (its first four bytes).
    pub fn selector(&self) -> Result<[u8; 4]> {
        self.seal.get(..4).and_then(|b| b.try_into().ok()).context("seal without a selector")
    }
}

/// RISC Zero's Groth16 verifier v3.0 selector behind the router: the first four bytes of a seal.
pub const GROTH16_SELECTOR: [u8; 4] = [0x73, 0xc4, 0x57, 0xba];
/// The MOCK verifier's selector (`MockRiscZeroVerifier`, local fork only).
pub const MOCK_SELECTOR: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

/// How to prove.
#[derive(Clone, Debug)]
pub struct Prover {
    /// `tier2/target/release/zecured-tier2-host`, or `mock-host` (MOCK, local fork only).
    pub host: PathBuf,
    /// MOCK seal for a deployment with `MockRiscZeroVerifier` (local fork only).
    pub mock: bool,
    /// Owner-only directory for the receipt and the seal.
    pub work: PathBuf,
}

fn run_host(host: &Path, args: &[&str], witness: Option<&[u8]>) -> Result<()> {
    let mut cmd = Command::new(host);
    cmd.arg("payment").args(args).stdout(Stdio::null());
    if witness.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().with_context(|| format!("run the zkVM host {}", host.display()))?;
    if let Some(w) = witness {
        child.stdin.take().context("host stdin")?.write_all(w)?;
    }
    let status = child.wait()?;
    ensure!(status.success(), "zkVM host `payment {}` failed", args.first().copied().unwrap_or(""));
    Ok(())
}

impl Prover {
    /// The selector of the seals it makes: Groth16, or MOCK.
    pub fn selector(&self) -> [u8; 4] {
        if self.mock { MOCK_SELECTOR } else { GROTH16_SELECTOR }
    }

    /// The image id of the payment guest it proves with (`<host> payment image-id`).
    pub fn image_id(&self) -> Result<B256> {
        let out = Command::new(&self.host)
            .args(["payment", "image-id"])
            .output()
            .with_context(|| format!("run the zkVM host {}", self.host.display()))?;
        ensure!(out.status.success(), "the zkVM host gave no payment image id");
        String::from_utf8(out.stdout)?.trim().parse().context("payment image id")
    }

    /// The seal of `witness` (its journal checked against the native statement's).
    pub fn seal(&self, witness: &PaymentWitness) -> Result<SealFile> {
        std::fs::create_dir_all(&self.work)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.work, std::fs::Permissions::from_mode(0o700))?;
        }
        let expected = zecured_payment_statement::verify(witness)
            .map_err(|e| anyhow::anyhow!("the payment statement refuses the witness: {e:?}"))?
            .abi_encode();
        let frame = serde_json::to_vec(witness)?;
        let seal_path = self.work.join("payment-seal.json");
        let seal_arg = seal_path.to_str().context("seal path")?;
        let sealed = if self.mock {
            run_host(&self.host, &["mock-seal", "-", seal_arg], Some(&frame))
        } else {
            let receipt = self.work.join("payment-receipt.bin");
            let receipt_arg = receipt.to_str().context("receipt path")?;
            let proved = run_host(&self.host, &["prove", "-", receipt_arg], Some(&frame))
                .and_then(|()| run_host(&self.host, &["groth16", receipt_arg, seal_arg], None));
            let _ = std::fs::remove_file(&receipt);
            proved
        };
        sealed?;
        let s: SealFile = serde_json::from_slice(&std::fs::read(&seal_path)?)?;
        let _ = std::fs::remove_file(&seal_path);
        ensure!(s.journal.as_ref() == expected.as_slice(), "the host's journal is not the native statement's");
        Ok(s)
    }
}
