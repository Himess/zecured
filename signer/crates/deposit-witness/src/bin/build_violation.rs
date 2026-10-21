//! Builds the private input of a Tier 3 violation proof (docs/TIER3.md § 6) and checks it with
//! the statement natively; prints the journal the slasher will check.
//!
//!   cargo run -p zecured-deposit-witness --bin build-violation -- uncommitted \
//!     --ufvk-file ufvk.txt --note-txid <txid> --spend-txid <txid> --tip <height> \
//!     --beneficiary 0x... --headers data/tier2/headers_nu7.json --out violation.json
//!
//!   ... -- misdirected --claim-file f3-<claim>.json --tip <height> --beneficiary 0x...
//!
//!   ... -- included --ufvk-file ufvk.txt --txid <payout txid> --tip <height>
//!
//! The viewing key comes from a file, never the command line (F3: the claim file that
//! `zecured-wallet watch --claims-of` writes). An F1, F2 or F3 witness contains it: keep the
//! witness on this machine. An F4 or Included witness holds only the vault's tag key K, derived
//! here.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use zcash_protocol::TxId;
use zecured_deposit_witness::{p2p, violation};
use zecured_zcash_ops::lwd;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    kind: KindArgs,
    /// Height of the header the proof is anchored to (after the transaction's block).
    #[arg(long, global = true)]
    tip: Option<u32>,
    /// Receives the bounty (an EOA; never a vault's smart account). Not used by `included`.
    #[arg(long, global = true)]
    beneficiary: Option<String>,
    /// JSON Lines headers from NU7 activation on.
    #[arg(long, global = true, default_value = "data/tier2/headers_nu7.json")]
    headers: PathBuf,
    /// First extend the header file to the Testnet tip over the P2P network.
    #[arg(long, global = true)]
    update_headers: bool,
    /// Cache of per-block summaries (built on first use; a few minutes).
    #[arg(long, global = true, default_value = "data/tier2/summaries.json")]
    summaries: PathBuf,
    #[arg(long, global = true)]
    out: Option<PathBuf>,
    #[arg(long, global = true, default_value = "https://testnet.zec.rocks:443")]
    lwd: String,
}

#[derive(Subcommand)]
enum KindArgs {
    /// F1: a lineage note of the vault spent by a transaction nobody committed for it.
    Uncommitted {
        #[arg(long)]
        ufvk_file: PathBuf,
        /// The transaction that created the note.
        #[arg(long)]
        note_txid: String,
        /// The transaction that spent it.
        #[arg(long)]
        spend_txid: String,
    },
    /// F2: a committed payout that does not return its committed change.
    WithheldChange {
        #[arg(long)]
        ufvk_file: PathBuf,
        #[arg(long)]
        txid: String,
        #[arg(long)]
        change_tag: String,
        #[arg(long)]
        change_value: u64,
    },
    /// F4: a committed payout that names a claim under a reference other than
    /// keccak256(salt || txid), hiding the payout from the claimant.
    WrongClaimRef {
        #[arg(long)]
        ufvk_file: PathBuf,
        #[arg(long)]
        txid: String,
        /// The claim's destination (salt and raw receiver), as `zecured-wallet watch --claims-of`
        /// writes it.
        #[arg(long)]
        claim_file: PathBuf,
    },
    /// F3: a committed payout whose output for a claim did not pay the claim's receiver in full.
    Misdirected {
        /// The claimant's evidence (payout, output index, salt, receiver, incoming viewing key),
        /// as written by `zecured-wallet watch --claims-of`.
        #[arg(long)]
        claim_file: PathBuf,
    },
    /// Not a violation (T-05): a payout of the vault is in a block under the anchor. The journal
    /// names it by its payoutRef; the v2 slasher refuses this kind.
    Included {
        #[arg(long)]
        ufvk_file: PathBuf,
        #[arg(long)]
        txid: String,
    },
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    hex::decode(s.trim_start_matches("0x"))?.try_into().map_err(|_| anyhow!("expected 32 bytes"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let tip = args.tip.context("--tip")?;
    let beneficiary: [u8; 20] = match (&args.kind, args.beneficiary.as_deref()) {
        (KindArgs::Included { .. }, _) => [0; 20],
        (_, b) => hex::decode(b.context("--beneficiary")?.trim_start_matches("0x"))?
            .try_into()
            .map_err(|_| anyhow!("beneficiary: 20 bytes"))?,
    };
    if args.update_headers {
        let path = args.headers.clone();
        let height = tokio::task::spawn_blocking(move || p2p::extend_file(&path, &p2p::seed_peers())).await??;
        println!("headers up to {height}");
    }
    let mut client = lwd::connect(&args.lwd).await?;
    let read_ufvk = |p: &PathBuf| -> Result<String> { Ok(fs::read_to_string(p)?.trim().to_owned()) };
    let (witness, journal) = match &args.kind {
        KindArgs::Uncommitted {
            ufvk_file,
            note_txid,
            spend_txid,
        } => {
            violation::uncommitted(
                &mut client,
                &args.headers,
                &args.summaries,
                &read_ufvk(ufvk_file)?,
                TxId::from_hex(note_txid).context("note txid")?,
                TxId::from_hex(spend_txid).context("spend txid")?,
                tip,
                beneficiary,
            )
            .await?
        }
        KindArgs::WithheldChange {
            ufvk_file,
            txid,
            change_tag,
            change_value,
        } => {
            violation::withheld_change(
                &mut client,
                &args.headers,
                &args.summaries,
                &read_ufvk(ufvk_file)?,
                TxId::from_hex(txid).context("txid")?,
                hex32(change_tag)?,
                *change_value,
                tip,
                beneficiary,
            )
            .await?
        }
        KindArgs::WrongClaimRef {
            ufvk_file,
            txid,
            claim_file,
        } => {
            let e: violation::ClaimEvidence = serde_json::from_slice(
                &fs::read(claim_file).with_context(|| format!("read {}", claim_file.display()))?,
            )?;
            violation::wrong_claim_ref(
                &mut client,
                &args.headers,
                &args.summaries,
                &read_ufvk(ufvk_file)?,
                TxId::from_hex(txid).context("txid")?,
                hex32(&e.salt)?,
                hex::decode(&e.receiver)?.try_into().map_err(|_| anyhow!("receiver: 43 bytes"))?,
                tip,
                beneficiary,
            )
            .await?
        }
        KindArgs::Misdirected { claim_file } => {
            let e: violation::ClaimEvidence = serde_json::from_slice(
                &fs::read(claim_file).with_context(|| format!("read {}", claim_file.display()))?,
            )?;
            violation::misdirected(
                &mut client,
                &args.headers,
                &args.summaries,
                TxId::from_hex(&e.txid).context("txid")?,
                hex32(&e.salt)?,
                hex::decode(&e.receiver)?.try_into().map_err(|_| anyhow!("receiver: 43 bytes"))?,
                hex::decode(&e.ivk)?.try_into().map_err(|_| anyhow!("ivk: 64 bytes"))?,
                e.output_index,
                tip,
                beneficiary,
            )
            .await?
        }
        KindArgs::Included { ufvk_file, txid } => {
            violation::included(
                &mut client,
                &args.headers,
                &args.summaries,
                zecured_deposit_statement::tag_key(&read_ufvk(ufvk_file)?),
                TxId::from_hex(txid).context("txid")?,
                tip,
            )
            .await?
        }
    };
    if let Some(out) = &args.out {
        fs::write(out, serde_json::to_vec(&witness)?)?;
    }
    println!("kind {:?}", journal.kind);
    println!("anchor 0x{}", hex::encode(journal.anchor));
    println!("reference 0x{}", hex::encode(journal.reference));
    println!("note tag 0x{}", hex::encode(journal.note_tag));
    println!("value {}", journal.value);
    println!("output index {}", journal.output_index);
    println!("journal 0x{}", hex::encode(journal.abi_encode()));
    Ok(())
}
