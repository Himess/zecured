//! Tier 3 violation witnesses (docs/TIER3.md § 6), built from public chain data plus what the
//! prover holds: the vault's viewing key (F1, F2), the claimant's salt, receiver and incoming
//! viewing key (F3), or only the vault's tag key K (F4, Included; derived here from the viewing key
//! when the builder has it). Each is checked with the statement natively before anything is
//! proven. An F1, F2 or F3 witness contains a viewing key: keep it on the prover's machine.

use std::path::Path;

use anyhow::{Context, Result, bail};
use orchard::{
    Address,
    keys::{IncomingViewingKey, PreparedIncomingViewingKey, Scope},
    note_encryption::IronwoodDomain,
};
use serde::{Deserialize, Serialize};
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId},
};
use zecured_deposit_statement::{
    tag_key,
    violation::{Detail, Kind, ViolationJournal, ViolationWitness, paid_to, verify_violation_on},
};
use zecured_zcash_ops::{
    lwd::{self, LwdClient},
    network::ZcashNetwork,
};

use crate::{
    statement_network,
    witness::{Located, locate, ufvk_fvk},
};

/// A witness, with the Zcash network its transaction was read from.
fn witness(kind: Kind, beneficiary: [u8; 20], at: Located, detail: Detail) -> Result<(ZcashNetwork, ViolationWitness)> {
    let branch_id = u32::from(at.transaction()?.consensus_branch_id());
    let network = at.network;
    let w = ViolationWitness {
        kind,
        beneficiary,
        branch_id,
        tx: at.raw_tx,
        tx_index: u32::try_from(at.tx_index)?,
        txid_path: at.txid_path,
        block_header: at.block_header,
        leaf: at.leaf,
        mmr_path: at.mmr_path,
        peaks: at.peaks,
        peak_index: u32::try_from(at.peak_index)?,
        tip_header: at.tip_header,
        tip_auth_data_root: at.tip_auth_data_root,
        detail,
    };
    Ok((network, w))
}

/// Checks a witness with the statement on the network it was read from.
fn checked((network, w): (ZcashNetwork, ViolationWitness)) -> Result<(ViolationWitness, ViolationJournal)> {
    let journal = verify_violation_on(statement_network(network), &w)
        .map_err(|e| anyhow::anyhow!("the statement refuses the witness: {e:?}"))?;
    Ok((w, journal))
}

/// F1: transaction `spend_txid` spends a note of the vault (viewing key `ufvk`) that transaction
/// `note_txid` created. The note is found by trial decryption with both of the vault's incoming
/// viewing keys, and its nullifier among the spend's actions.
#[allow(clippy::too_many_arguments)]
pub async fn uncommitted(
    client: &mut LwdClient,
    network: ZcashNetwork,
    headers_path: &Path,
    summaries_path: &Path,
    ufvk: &str,
    note_txid: TxId,
    spend_txid: TxId,
    tip: u32,
    beneficiary: [u8; 20],
) -> Result<(ViolationWitness, ViolationJournal)> {
    let fvk = ufvk_fvk(network, ufvk)?;
    let (raw, height) = lwd::raw_transaction(client, &note_txid).await?;
    let branch = BranchId::for_height(&network.params(), BlockHeight::from_u32(u32::try_from(height)?));
    let note_tx = Transaction::read(&raw[..], branch)?;
    let at = locate(client, network, headers_path, summaries_path, spend_txid, tip).await?;
    let spent: Vec<[u8; 32]> = at
        .transaction()?
        .ironwood_bundle()
        .context("the spend has no Ironwood bundle")?
        .actions()
        .iter()
        .map(|a| a.nullifier().to_bytes())
        .collect();
    let ivks = [Scope::External, Scope::Internal].map(|s| PreparedIncomingViewingKey::new(&fvk.to_ivk(s)));
    for action in note_tx.ironwood_bundle().context("the note transaction has no Ironwood bundle")?.actions() {
        for ivk in &ivks {
            let Some((note, recipient, _)) = try_note_decryption(&IronwoodDomain::for_action(action), ivk, action)
            else {
                continue;
            };
            let Some(index) = spent.iter().position(|nf| *nf == note.nullifier(&fvk).to_bytes()) else {
                continue;
            };
            let detail = Detail::Uncommitted {
                vault_ufvk: ufvk.to_owned(),
                action_index: u32::try_from(index)?,
                note_recipient: recipient.to_raw_address_bytes().to_vec(),
                note_value: note.value().inner(),
                note_rho: note.rho().to_bytes(),
                note_rseed: *note.rseed().as_bytes(),
            };
            return checked(witness(Kind::Uncommitted, beneficiary, at, detail)?);
        }
    }
    bail!("the spend transaction spends no note of the vault that the note transaction created")
}

/// F2: committed payout `txid` does not return the committed change (`change_tag`,
/// `change_value`) to the vault (viewing key `ufvk`).
#[allow(clippy::too_many_arguments)]
pub async fn withheld_change(
    client: &mut LwdClient,
    network: ZcashNetwork,
    headers_path: &Path,
    summaries_path: &Path,
    ufvk: &str,
    txid: TxId,
    change_tag: [u8; 32],
    change_value: u64,
    tip: u32,
    beneficiary: [u8; 20],
) -> Result<(ViolationWitness, ViolationJournal)> {
    let at = locate(client, network, headers_path, summaries_path, txid, tip).await?;
    let detail = Detail::WithheldChange {
        vault_ufvk: ufvk.to_owned(),
        change_tag,
        change_value,
    };
    checked(witness(Kind::WithheldChange, beneficiary, at, detail)?)
}

/// F3: committed payout `txid` names a claim whose recipient is `(salt, receiver)`, paid by its
/// action `output_index` (`claimPayment(claimRef).outputIndex` on Base); `ivk` (raw, 64 bytes) is an
/// incoming viewing key of the claimant's wallet.
#[allow(clippy::too_many_arguments)]
pub async fn misdirected(
    client: &mut LwdClient,
    network: ZcashNetwork,
    headers_path: &Path,
    summaries_path: &Path,
    txid: TxId,
    salt: [u8; 32],
    receiver: [u8; 43],
    ivk: [u8; 64],
    output_index: u8,
    tip: u32,
    beneficiary: [u8; 20],
) -> Result<(ViolationWitness, ViolationJournal)> {
    let at = locate(client, network, headers_path, summaries_path, txid, tip).await?;
    let detail = Detail::Misdirected {
        salt,
        receiver: receiver.to_vec(),
        ivk: ivk.to_vec(),
        output_index: u32::from(output_index),
    };
    checked(witness(Kind::Misdirected, beneficiary, at, detail)?)
}

/// F4: committed payout `txid` of the vault (viewing key `ufvk`) names the claim of recipient
/// `(salt, receiver)` under a reference other than `keccak256(salt || txid)` (review RT-1). The
/// borrower proves it for its own claims; the signers, who hold every salt, for any claim. The
/// witness carries the vault's tag key K, not the viewing key.
#[allow(clippy::too_many_arguments)]
pub async fn wrong_claim_ref(
    client: &mut LwdClient,
    network: ZcashNetwork,
    headers_path: &Path,
    summaries_path: &Path,
    ufvk: &str,
    txid: TxId,
    salt: [u8; 32],
    receiver: [u8; 43],
    tip: u32,
    beneficiary: [u8; 20],
) -> Result<(ViolationWitness, ViolationJournal)> {
    let at = locate(client, network, headers_path, summaries_path, txid, tip).await?;
    let detail = Detail::WrongClaimRef {
        tag_key: tag_key(ufvk),
        salt,
        receiver: receiver.to_vec(),
    };
    checked(witness(Kind::WrongClaimRef, beneficiary, at, detail)?)
}

/// Included (design notes T-05): payout `txid` of the vault (tag key `K`) is in a block under
/// header `tip`. Not a violation: the journal names the payout by its `payoutRef` for a contract
/// that confirms payouts by proof; the v2 slasher refuses it. The witness carries K only.
pub async fn included(
    client: &mut LwdClient,
    network: ZcashNetwork,
    headers_path: &Path,
    summaries_path: &Path,
    tag_key: [u8; 32],
    txid: TxId,
    tip: u32,
) -> Result<(ViolationWitness, ViolationJournal)> {
    let at = locate(client, network, headers_path, summaries_path, txid, tip).await?;
    checked(witness(Kind::Included, [0; 20], at, Detail::Included { tag_key })?)
}

/// What a claimant keeps to prove F3: written by `zecured-wallet watch --claims-of` when a
/// committed payment did not arrive in full, read by `build-violation misdirected`. It holds the
/// claim's salt and the wallet's incoming viewing key: keep it on the claimant's machine.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimEvidence {
    /// The committed payout (txid as displayed).
    pub txid: String,
    /// The claim's output in it (`claimPayment(claimRef).outputIndex` on Base).
    pub output_index: u8,
    /// The claim's salt (hex, 32 bytes).
    pub salt: String,
    /// The claim's raw Ironwood receiver (hex, 43 bytes).
    pub receiver: String,
    /// The claimant wallet's raw incoming viewing key that owns the receiver (hex, 64 bytes).
    pub ivk: String,
}

/// F3, before proving: what action `output_index` of transaction `txid` pays `receiver` exactly,
/// as the holder of `ivk` sees it, by the statement's own rule ([`paid_to`]).
pub async fn claim_output_value(
    client: &mut LwdClient,
    network: ZcashNetwork,
    txid: TxId,
    output_index: u8,
    ivk: &IncomingViewingKey,
    receiver: &Address,
) -> Result<u64> {
    let (raw, height) = lwd::raw_transaction(client, &txid).await?;
    let branch = BranchId::for_height(&network.params(), BlockHeight::from_u32(u32::try_from(height)?));
    let tx = Transaction::read(&raw[..], branch)?;
    let bundle = tx.ironwood_bundle().context("the payout has no Ironwood bundle")?;
    let prepared = PreparedIncomingViewingKey::new(ivk);
    Ok(bundle.actions().get(usize::from(output_index)).map_or(0, |a| paid_to(a, &prepared, receiver)))
}
