//! Tier 3 watchtower (docs/TIER3.md § 6): anyone holding a vault's viewing key checks that every
//! spend of the vault's lineage was committed on Base, and that every committed change came back.
//!
//! For each note of the scan, its tag (the D24 deposit tag of its note commitment) tells whether
//! Base knows it as a lineage note. A lineage note spent by a transaction X must be X's committed
//! input: `spendRecord(payoutRef(X)).inputTag` is its tag, or X is an uncommitted spend (F1).
//! A committed payout with change must have delivered a note with the committed tag and value
//! (F2). Each finding comes with the `build-violation` command that builds its witness.
//!
//! A claimant checks its own claims (F3): for each committed payout X and each destination it
//! gave, `claimPayment(keccak256(salt || txid_X))` on Base names what X promised the claim and in
//! which output; the wallet's incoming viewing key shows what that output paid. A shortfall is
//! written to an evidence file for `build-violation misdirected`. With the vault's viewing key it
//! also checks that the commitment of X names the claim by that very reference (F4, review RT-1):
//! `claimRefOf(payoutRef(X), commitment)` must be `keccak256(salt || txid_X)`.

use std::path::Path;

use alloy::{
    primitives::{Address, B256},
    providers::{Provider, ProviderBuilder},
    rpc::types::Filter,
};
use anyhow::{Context, Result, anyhow};
use orchard::{
    keys::{FullViewingKey, IncomingViewingKey, Scope},
    note::ExtractedNoteCommitment,
};
use zcash_protocol::TxId;
use zecured_base::events::IZecuredRegistry;
use zecured_deposit_witness::violation::{ClaimEvidence, claim_output_value};
use zecured_policy::{
    derive::{deposit_tag, payout_ref},
    ledger::{claim_ref, commitment},
};
use zecured_zcash_ops::{lwd::LwdClient, scan::ReceivedNote};

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

/// One destination this wallet gave for a loan's claims, with the wallet's incoming viewing key
/// that owns its receiver (`None`: another wallet's address, which only that wallet can check).
pub struct OwnRecipient {
    salt: [u8; 32],
    receiver: [u8; 43],
    ivk: Option<IncomingViewingKey>,
}

/// A kept destination (hex salt and raw receiver) as seen by the wallet with viewing key `fvk`.
pub fn own_recipient(salt: &str, receiver: &str, fvk: &FullViewingKey) -> Result<OwnRecipient> {
    let salt: [u8; 32] = hex::decode(salt)?.try_into().map_err(|_| anyhow!("salt: 32 bytes"))?;
    let receiver: [u8; 43] = hex::decode(receiver)?.try_into().map_err(|_| anyhow!("receiver: 43 bytes"))?;
    let address = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&receiver))
        .context("not an Ironwood receiver")?;
    let ivk = [Scope::External, Scope::Internal]
        .map(|s| fvk.to_ivk(s))
        .into_iter()
        .find(|k| k.diversifier_index(&address).is_some());
    Ok(OwnRecipient { salt, receiver, ivk })
}

/// What a claimant found about one committed payment of its claims (F3, F4).
pub enum ClaimFinding {
    /// A commitment names the claim, but no Zcash transaction searched has its reference: the
    /// payout is not mined yet, or the reference is wrong (F4, which only the vault's borrower or
    /// an honest signer can prove: they know the vault's viewing key).
    Unmatched { claim_id: B256 },
    /// F4: the commitment of `payout` names the claim of this destination under a foreign
    /// reference, so that its claimant could never find the payout.
    WrongClaimRef { payout: TxId, evidence: ClaimEvidence },
    /// The claim's own output paid at least what the commitment promised.
    Paid { claim_id: B256, payout: TxId, value: u64 },
    /// The destination is another wallet's: only that wallet can check this payment.
    OtherWallet { claim_id: B256, payout: TxId },
    /// F3: the claim's output paid its receiver less than promised (`value < expected`).
    Misdirected {
        claim_id: B256,
        payout: TxId,
        expected: u64,
        value: u64,
        evidence: ClaimEvidence,
    },
}

/// RT-2: a claimant without the vault's viewing key (a liquidator, a market maker) finds each
/// payout of its claims on its own. Every `SpendCommitted` on Base that names a claim of one of
/// its destinations gives the claim's reference; the payout is the Zcash transaction X with
/// `keccak256(salt || txid_X)` equal to it (Zcash blocks `zcash_from..=zcash_to`); the committed
/// output of X is then decrypted with the claimant's own key, as in [`check_claims`].
pub async fn check_third_party(
    client: &mut LwdClient,
    own: &[OwnRecipient],
    registry_address: Address,
    rpc: &str,
    base_from: u64,
    zcash_from: u32,
    zcash_to: u32,
) -> Result<Vec<ClaimFinding>> {
    use alloy::sol_types::SolEvent;
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    let registry = IZecuredRegistry::new(registry_address, &provider);
    let head = provider.get_block_number().await.context("Base block number")?;
    let logs = zecured_base::events::get_logs_chunked(base_from, head, |a, b| {
        let filter = Filter::new()
            .address(registry_address)
            .event_signature(IZecuredRegistry::SpendCommitted::SIGNATURE_HASH)
            .from_block(a)
            .to_block(b);
        let provider = &provider;
        async move { provider.get_logs(&filter).await.map_err(anyhow::Error::from) }
    })
    .await?;
    // The claims of these destinations that a commitment names, with the reference it gives.
    let commitments: Vec<B256> = own.iter().map(|r| B256::from(commitment(&r.salt, &r.receiver))).collect();
    let mut wanted: Vec<(B256, B256, usize)> = vec![]; // (claim id, claim ref, destination)
    for log in logs.iter().filter(|l| !l.removed) {
        let ev = IZecuredRegistry::SpendCommitted::decode_log(&log.inner).context("SpendCommitted")?;
        for (id, claim_ref) in ev.claimIds.iter().zip(&ev.claimRefs) {
            let c = registry.claim(*id).call().await.context("Registry.claim")?;
            if let Some(d) = commitments.iter().position(|x| *x == c.commitment) {
                wanted.push((*id, *claim_ref, d));
            }
        }
    }
    // The payouts, among the Zcash transactions, by their reference.
    let mut found: Vec<Option<TxId>> = vec![None; wanted.len()];
    let mut from = zcash_from;
    while from <= zcash_to && found.iter().any(Option::is_none) {
        let to = from.saturating_add(999).min(zcash_to);
        for (_, txid) in zecured_zcash_ops::lwd::block_txids(client, from, to).await? {
            for (k, (_, committed_ref, d)) in wanted.iter().enumerate() {
                if found[k].is_none() && B256::from(claim_ref(&own[*d].salt, &txid)) == *committed_ref {
                    found[k] = Some(TxId::from_bytes(txid));
                }
            }
        }
        from = to.saturating_add(1);
    }
    let mut findings = vec![];
    for ((claim_id, committed_ref, d), x) in wanted.into_iter().zip(found) {
        let r = &own[d];
        let Some(x) = x else {
            findings.push(ClaimFinding::Unmatched { claim_id });
            continue;
        };
        let Some(ivk) = &r.ivk else {
            findings.push(ClaimFinding::OtherWallet { claim_id, payout: x });
            continue;
        };
        let p = registry.claimPayment(committed_ref).call().await.context("Registry.claimPayment")?;
        let address = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&r.receiver))
            .context("not an Ironwood receiver")?;
        let value = claim_output_value(client, x, p.outputIndex, ivk, &address).await?;
        findings.push(judge(claim_id, x, p.expected, value, evidence(x, p.outputIndex, r, Some(ivk))));
    }
    Ok(findings)
}

/// The committed payouts among the lineage findings: the transactions that can pay claims.
pub fn committed_payouts(findings: &[Finding]) -> Vec<TxId> {
    findings
        .iter()
        .filter_map(|f| match f {
            Finding::Committed { payout, .. } | Finding::WithheldChange { payout, .. } => Some(*payout),
            _ => None,
        })
        .collect()
}

/// Checks every committed payment of the claims behind `own` among `payouts` of the vault with
/// viewing key `ufvk`.
pub async fn check_claims(
    client: &mut LwdClient,
    ufvk: &str,
    own: &[OwnRecipient],
    payouts: &[TxId],
    registry: Address,
    rpc: &str,
) -> Result<Vec<ClaimFinding>> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    let registry = IZecuredRegistry::new(registry, &provider);
    let mut findings = vec![];
    for x in payouts {
        let payout = B256::from(payout_ref(ufvk, x.as_ref()));
        for r in own {
            let claim_ref = B256::from(claim_ref(&r.salt, x.as_ref()));
            let commitment = B256::from(commitment(&r.salt, &r.receiver));
            let named = registry.claimRefOf(payout, commitment).call().await.context("Registry.claimRefOf")?;
            if named != B256::ZERO && named != claim_ref {
                findings.push(ClaimFinding::WrongClaimRef {
                    payout: *x,
                    evidence: evidence(*x, 0, r, None),
                });
                continue;
            }
            let p = registry.claimPayment(claim_ref).call().await.context("Registry.claimPayment")?;
            if p.claimId == B256::ZERO {
                continue; // X pays no claim of this destination
            }
            let Some(ivk) = &r.ivk else {
                findings.push(ClaimFinding::OtherWallet {
                    claim_id: p.claimId,
                    payout: *x,
                });
                continue;
            };
            let address = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&r.receiver))
                .context("not an Ironwood receiver")?;
            let value = claim_output_value(client, *x, p.outputIndex, ivk, &address).await?;
            findings.push(judge(p.claimId, *x, p.expected, value, evidence(*x, p.outputIndex, r, Some(ivk))));
        }
    }
    Ok(findings)
}

/// The evidence file of a finding about payout `x` and destination `r` (the wallet's key that owns
/// the receiver, if any; F4 does not need it).
fn evidence(x: TxId, output_index: u8, r: &OwnRecipient, ivk: Option<&IncomingViewingKey>) -> ClaimEvidence {
    ClaimEvidence {
        txid: x.to_string(),
        output_index,
        salt: hex::encode(r.salt),
        receiver: hex::encode(r.receiver),
        ivk: ivk.map(|k| hex::encode(k.to_bytes())).unwrap_or_default(),
    }
}

/// A committed payment is honest when the claim's output paid at least what was promised.
pub fn judge(claim_id: B256, payout: TxId, expected: u64, value: u64, evidence: ClaimEvidence) -> ClaimFinding {
    if value >= expected {
        ClaimFinding::Paid {
            claim_id,
            payout,
            value,
        }
    } else {
        ClaimFinding::Misdirected {
            claim_id,
            payout,
            expected,
            value,
            evidence,
        }
    }
}

/// Prints a claim finding; `evidence` is where a misdirected payment's evidence file was written.
pub fn report_claim(f: &ClaimFinding, evidence: Option<&Path>) {
    match f {
        ClaimFinding::Paid {
            claim_id,
            payout,
            value,
        } => println!("tier 3: claim {} paid {value} zat by its committed output in {payout}: ok", short(claim_id)),
        ClaimFinding::Unmatched { claim_id } => println!(
            "tier 3: claim {} is committed, but no Zcash transaction searched has its reference: not mined yet, or a wrong reference (F4, provable by the vault's borrower or an honest signer)",
            short(claim_id)
        ),
        ClaimFinding::WrongClaimRef { payout, .. } => {
            println!(
                "tier 3: VIOLATION F4: the commitment of payout {payout} names one of this loan's claims under a reference its claimant cannot find"
            );
            if let Some(path) = evidence {
                println!(
                    "  build the witness: build-violation wrong-claim-ref --ufvk-file <vault UFVK file> --txid {payout} --claim-file {} --tip <anchor> --beneficiary <your EOA>",
                    path.display()
                );
            }
        }
        ClaimFinding::OtherWallet { claim_id, payout } => println!(
            "tier 3: claim {} is committed to {payout}, paid to another wallet's address: check it there",
            short(claim_id)
        ),
        ClaimFinding::Misdirected {
            claim_id,
            payout,
            expected,
            value,
            evidence: e,
        } => {
            println!(
                "tier 3: VIOLATION F3: committed payout {payout} paid claim {} {value} zat in its output {}, {expected} promised",
                short(claim_id),
                e.output_index
            );
            if let Some(path) = evidence {
                println!(
                    "  build the witness: build-violation misdirected --claim-file {} --tip <anchor> --beneficiary <your EOA>",
                    path.display()
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

    fn fvk(seed: u8) -> FullViewingKey {
        let sk = (seed..=255)
            .map(|i| [i; 32])
            .find_map(|b| Option::<orchard::keys::SpendingKey>::from(orchard::keys::SpendingKey::from_bytes(b)))
            .unwrap();
        FullViewingKey::from(&sk)
    }

    /// F3 runs only on committed payouts: an uncommitted spend has no claim payment to check.
    #[test]
    fn claims_are_checked_in_committed_payouts_only() {
        let (a, b, c) = (TxId::from_bytes([1; 32]), TxId::from_bytes([2; 32]), TxId::from_bytes([3; 32]));
        let tag = B256::repeat_byte(9);
        let findings = [
            Finding::Committed { tag, payout: a },
            Finding::Uncommitted {
                note_txid: a,
                spend_txid: b,
                tag,
            },
            Finding::WithheldChange {
                payout: c,
                change_tag: tag,
                change_value: 1,
            },
            Finding::Unspent { tag, value: 1 },
        ];
        assert_eq!(committed_payouts(&findings), vec![a, c]);
    }

    /// The wallet checks a destination with the viewing key that owns its receiver, at any
    /// diversifier and in either scope; another wallet's destination it cannot check.
    #[test]
    fn a_destination_is_checked_with_the_key_that_owns_it() {
        let (mine, theirs) = (fvk(1), fvk(100));
        let salt = hex::encode([5u8; 32]);
        for address in [
            mine.address_at(0u32, Scope::External),
            mine.address_at(7u32, Scope::External),
            mine.address_at(0u32, Scope::Internal),
        ] {
            let r = own_recipient(&salt, &hex::encode(address.to_raw_address_bytes()), &mine).unwrap();
            assert!(r.ivk.is_some_and(|k| k.diversifier_index(&address).is_some()));
        }
        let other = theirs.address_at(0u32, Scope::External);
        assert!(own_recipient(&salt, &hex::encode(other.to_raw_address_bytes()), &mine).unwrap().ivk.is_none());
        assert!(own_recipient(&salt, "00", &mine).is_err());
    }

    /// A shortfall of even one zatoshi in the claim's own output is F3, with its evidence.
    #[test]
    fn a_payment_short_of_the_promise_is_misdirected() {
        let (claim, x) = (B256::repeat_byte(4), TxId::from_bytes([2; 32]));
        let evidence = || ClaimEvidence {
            txid: x.to_string(),
            output_index: 1,
            salt: String::new(),
            receiver: String::new(),
            ivk: String::new(),
        };
        assert!(matches!(judge(claim, x, 990_000, 990_000, evidence()), ClaimFinding::Paid { value: 990_000, .. }));
        assert!(matches!(judge(claim, x, 990_000, 1_000_000, evidence()), ClaimFinding::Paid { .. }));
        assert!(matches!(judge(claim, x, 990_000, 0, evidence()), ClaimFinding::Misdirected { value: 0, .. }));
        let ClaimFinding::Misdirected {
            expected,
            value,
            evidence,
            ..
        } = judge(claim, x, 990_000, 989_999, evidence())
        else {
            panic!("a shortfall is F3")
        };
        assert_eq!((expected, value, evidence.output_index), (990_000, 989_999, 1));
        assert_eq!(TxId::from_hex(&evidence.txid), Some(x), "build-violation reads the txid back");
    }
}
