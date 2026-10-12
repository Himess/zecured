//! Tier 3 watchtower (docs/TIER3.md § 6): anyone holding a vault's viewing key checks that every
//! spend of the vault's lineage was committed on Base, and that every committed change came back.
//!
//! For each note of the scan, its tag (the D24 deposit tag of its note commitment) tells whether
//! Base knows it as a lineage note. A lineage note spent by a transaction X must be X's committed
//! input: `spendRecord(payoutRef(X)).inputTag` is its tag, or X is an uncommitted spend (F1).
//! A committed payout with change must have delivered a note with the committed tag and value
//! (F2). Each finding comes with the `build-violation` command that builds its witness.

use alloy::{
    primitives::{Address, B256},
    providers::ProviderBuilder,
};
use anyhow::{Context, Result};
use orchard::note::ExtractedNoteCommitment;
use zcash_protocol::TxId;
use zecured_base::events::IZecuredRegistry;
use zecured_policy::derive::{deposit_tag, payout_ref};
use zecured_zcash_ops::scan::ReceivedNote;

/// What the watchtower found about one lineage note.
#[derive(Debug, PartialEq, Eq)]
pub enum Finding {
    /// Unspent lineage note: nothing to check yet.
    Unspent { tag: B256, value: u64 },
    /// Spent by its committed payout, and the change (if any) came back.
    Committed { tag: B256, payout: TxId },
    /// F1: spent by a transaction no commitment names it in.
    Uncommitted {
        note_txid: TxId,
        spend_txid: TxId,
        tag: B256,
    },
    /// F2: the committed change never arrived.
    WithheldChange {
        payout: TxId,
        change_tag: B256,
        change_value: u64,
    },
}

/// Checks the scanned notes of the vault with viewing key `ufvk` against the Registry.
pub async fn check_lineage(ufvk: &str, notes: &[ReceivedNote], registry: Address, rpc: &str) -> Result<Vec<Finding>> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    let registry = IZecuredRegistry::new(registry, &provider);
    let tags: Vec<B256> = notes.iter().map(|n| note_tag(ufvk, n)).collect();
    let mut findings = vec![];
    for (n, tag) in notes.iter().zip(&tags) {
        let lineage = registry.lineageOf(*tag).call().await.context("Registry.lineageOf")?;
        if lineage.vaultId == B256::ZERO {
            continue; // not backing zZEC: a non-conforming or foreign note
        }
        let Some(x) = n.spent_in else {
            findings.push(Finding::Unspent {
                tag: *tag,
                value: n.value(),
            });
            continue;
        };
        let record = registry
            .spendRecord(B256::from(payout_ref(ufvk, x.as_ref())))
            .call()
            .await
            .context("Registry.spendRecord")?;
        let received: Vec<(B256, u64)> = notes.iter().zip(&tags).map(|(m, t)| (*t, m.value())).collect();
        findings.push(spent(n.txid, x, *tag, &record, &received));
    }
    Ok(findings)
}

/// The finding about lineage note `tag` (created by `note_txid`) spent by `x`, given the Registry's
/// record of `payoutRef(x)` (all zero if none) and the (tag, value) of every note the vault received.
pub fn spent(
    note_txid: TxId,
    x: TxId,
    tag: B256,
    record: &IZecuredRegistry::SpendRecord,
    received: &[(B256, u64)],
) -> Finding {
    if record.inputTag != tag {
        return Finding::Uncommitted {
            note_txid,
            spend_txid: x,
            tag,
        };
    }
    if record.changeValue == 0 || received.contains(&(record.changeTag, record.changeValue)) {
        Finding::Committed { tag, payout: x }
    } else {
        Finding::WithheldChange {
            payout: x,
            change_tag: record.changeTag,
            change_value: record.changeValue,
        }
    }
}

/// The lineage tag of a scanned note (D24): keyed by the vault's viewing key.
fn note_tag(ufvk: &str, n: &ReceivedNote) -> B256 {
    B256::from(deposit_tag(ufvk, &ExtractedNoteCommitment::from(n.note.commitment()).to_bytes()))
}

pub fn report(findings: &[Finding]) {
    if findings.is_empty() {
        println!("tier 3: no lineage note of this vault is on Base");
        return;
    }
    for f in findings {
        match f {
            Finding::Unspent { tag, value } => println!("tier 3: lineage note {} ({value} zat) unspent", short(tag)),
            Finding::Committed { tag, payout } => {
                println!("tier 3: lineage note {} spent by its committed payout {payout}: ok", short(tag))
            }
            Finding::Uncommitted {
                note_txid,
                spend_txid,
                tag,
            } => {
                println!(
                    "tier 3: VIOLATION F1: lineage note {} spent by {spend_txid}, which no commitment names",
                    short(tag)
                );
                println!(
                    "  build the witness: build-violation uncommitted --ufvk-file <file> --note-txid {note_txid} --spend-txid {spend_txid} --tip <anchor> --beneficiary <your EOA>"
                );
            }
            Finding::WithheldChange {
                payout,
                change_tag,
                change_value,
            } => {
                println!(
                    "tier 3: VIOLATION F2: committed payout {payout} did not return its change ({change_value} zat)"
                );
                println!(
                    "  build the witness: build-violation withheld-change --ufvk-file <file> --txid {payout} --change-tag {change_tag} --change-value {change_value} --tip <anchor> --beneficiary <your EOA>"
                );
            }
        }
    }
}

fn short(tag: &B256) -> String {
    format!("0x{}", hex::encode(&tag[..4]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(input: u8, change_tag: u8, change_value: u64) -> IZecuredRegistry::SpendRecord {
        IZecuredRegistry::SpendRecord {
            recordHash: B256::ZERO,
            vaultId: B256::repeat_byte(1),
            inputTag: B256::repeat_byte(input),
            changeTag: B256::repeat_byte(change_tag),
            changeValue: change_value,
            claimCount: 1,
            signerBitmap: 0x15,
        }
    }

    #[test]
    fn a_spend_is_judged_by_its_record() {
        let (n, x, tag) = (TxId::from_bytes([1; 32]), TxId::from_bytes([2; 32]), B256::repeat_byte(9));
        let change = (B256::repeat_byte(7), 600_000);
        // Committed, change back; committed and emptied the vault.
        assert_eq!(spent(n, x, tag, &record(9, 7, 600_000), &[change]), Finding::Committed { tag, payout: x });
        assert_eq!(spent(n, x, tag, &record(9, 0, 0), &[]), Finding::Committed { tag, payout: x });
        // No record (all zero) or a record of another input: F1.
        let f1 = Finding::Uncommitted {
            note_txid: n,
            spend_txid: x,
            tag,
        };
        assert_eq!(spent(n, x, tag, &record(0, 0, 0), &[change]), f1);
        assert_eq!(spent(n, x, tag, &record(8, 7, 600_000), &[change]), f1);
        // The committed change never came, or came with another value: F2.
        let f2 = Finding::WithheldChange {
            payout: x,
            change_tag: B256::repeat_byte(7),
            change_value: 600_000,
        };
        assert_eq!(spent(n, x, tag, &record(9, 7, 600_000), &[]), f2);
        assert_eq!(spent(n, x, tag, &record(9, 7, 600_000), &[(B256::repeat_byte(7), 599_999)]), f2);
    }
}
