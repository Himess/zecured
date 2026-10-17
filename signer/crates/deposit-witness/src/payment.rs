//! Zecured Swap: the private input of a payment proof (tier2/payment-statement), built from public
//! chain data plus what only the payer knows, and checked with the statement natively before
//! anything is proven.
//!
//! The payer (the maker on Buy, the taker on Sell) built the payment itself: one Ironwood note with
//! the memo `"ZSWP" || orderId`, encrypted with `ovk = None`, so nobody (the payer included) can
//! recover it from the chain afterwards. The payer therefore keeps the note's opening (recipient,
//! value, rseed, the action that creates it) when it builds the transaction ([`PaymentOpening`]),
//! with the salt of the order's receiver commitment, and the witness is the transaction located in
//! the chain ([`crate::witness::locate`]) plus that opening. The witness holds the recipient's raw
//! receiver: it stays with the payer and its prover, never in a log.

use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use zcash_protocol::TxId;
use zecured_payment_statement::{PaymentJournal, PaymentWitness, memo_order_id, receiver_commitment, verify};
use zecured_zcash_ops::lwd::LwdClient;

use crate::witness::{Located, locate};

/// What the payer keeps about its payment: the note it created and the order's salt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentOpening {
    /// The Ironwood action (index in the bundle) that creates the payment note.
    pub action_index: u32,
    /// The note's recipient: the 43-byte raw Orchard-protocol receiver.
    pub recipient: Vec<u8>,
    /// The note's value, in zatoshi.
    pub value: u64,
    /// The note's rseed.
    pub rseed: [u8; 32],
    /// The salt of the order's receiver commitment `keccak256(salt || recipient)`.
    pub salt: [u8; 32],
}

impl PaymentOpening {
    /// The receiver commitment the opening pays (what the order on Base must name).
    pub fn receiver_commitment(&self) -> Result<[u8; 32]> {
        let recipient: [u8; 43] =
            self.recipient.clone().try_into().map_err(|_| anyhow!("the recipient is not 43 bytes"))?;
        Ok(receiver_commitment(&self.salt, &recipient))
    }
}

/// A payment witness, its journal and where the payment sits.
pub struct BuiltPayment {
    pub witness: PaymentWitness,
    pub journal: PaymentJournal,
    pub height: u32,
    pub tx_index: usize,
    pub block_transactions: usize,
    pub leaf_index: usize,
    pub leaves: usize,
}

/// The witness of a payment whose transaction sits as `at` and whose note `opening` opens. Checked
/// with the statement; the order's id and receiver commitment, when given, must be the journal's.
pub fn from_located(
    at: &Located,
    opening: &PaymentOpening,
    order_id: Option<[u8; 32]>,
    commitment: Option<[u8; 32]>,
) -> Result<(PaymentWitness, PaymentJournal)> {
    let tx = at.transaction()?;
    let witness = PaymentWitness {
        branch_id: u32::from(tx.consensus_branch_id()),
        tx: at.raw_tx.clone(),
        action_index: opening.action_index,
        note_recipient: opening.recipient.clone(),
        note_value: opening.value,
        note_rseed: opening.rseed,
        salt: opening.salt,
        tx_index: u32::try_from(at.tx_index)?,
        txid_path: at.txid_path.clone(),
        block_header: at.block_header.clone(),
        leaf: at.leaf.clone(),
        mmr_path: at.mmr_path.clone(),
        peaks: at.peaks.clone(),
        peak_index: u32::try_from(at.peak_index)?,
        tip_header: at.tip_header.clone(),
        tip_auth_data_root: at.tip_auth_data_root,
    };
    let journal = verify(&witness).map_err(|e| anyhow!("the payment statement refuses the witness: {e:?}"))?;
    if let Some(order_id) = order_id {
        ensure!(journal.order_id == order_id, "the payment's memo names another order");
    }
    if let Some(commitment) = commitment {
        ensure!(journal.receiver_commitment == commitment, "the payment pays another receiver commitment");
    }
    Ok((witness, journal))
}

/// Builds the witness of payment transaction `txid`, anchored at header `tip` (after the payment's
/// block; the relay must accept that header and the order's tier of work on top of it), for the
/// order `order_id` paying `commitment` when given, and checks it with the statement.
#[allow(clippy::too_many_arguments)]
pub async fn build(
    client: &mut LwdClient,
    headers_path: &Path,
    summaries_path: &Path,
    txid: TxId,
    opening: &PaymentOpening,
    order_id: Option<[u8; 32]>,
    commitment: Option<[u8; 32]>,
    tip: u32,
) -> Result<BuiltPayment> {
    let at = locate(client, headers_path, summaries_path, txid, tip).await.context("locate the payment")?;
    let (witness, journal) = from_located(&at, opening, order_id, commitment)?;
    Ok(BuiltPayment {
        witness,
        journal,
        height: at.height,
        tx_index: at.tx_index,
        block_transactions: at.block_transactions,
        leaf_index: at.leaf_index,
        leaves: at.leaves,
    })
}

/// The order a 512-byte memo binds a payment to, if any (re-exported for the payers).
pub fn memo_order(memo: &[u8; 512]) -> Option<[u8; 32]> {
    memo_order_id(memo)
}

#[cfg(test)]
mod tests {
    use zecured_payment_statement::{PaymentWitness, payment_memo};

    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tier2/payment-statement/tests/fixtures/TEST-ONLY_swap_payment_witness.json"
    );

    /// The synthetic fixture (TEST-ONLY) as a located payment: the builder gives back the same
    /// witness and journal, and refuses another order or another commitment.
    #[test]
    fn the_builder_rebuilds_the_fixture() {
        let w: PaymentWitness = serde_json::from_slice(&std::fs::read(FIXTURE).unwrap()).unwrap();
        let at = Located {
            raw_tx: w.tx.clone(),
            height: 4_488_543,
            tx_index: w.tx_index as usize,
            block_transactions: 2,
            txid_path: w.txid_path.clone(),
            block_header: w.block_header.clone(),
            leaf: w.leaf.clone(),
            leaf_index: 0,
            leaves: 1,
            mmr_path: w.mmr_path.clone(),
            peaks: w.peaks.clone(),
            peak_index: w.peak_index as usize,
            tip_header: w.tip_header.clone(),
            tip_auth_data_root: w.tip_auth_data_root,
        };
        let opening = PaymentOpening {
            action_index: w.action_index,
            recipient: w.note_recipient.clone(),
            value: w.note_value,
            rseed: w.note_rseed,
            salt: w.salt,
        };
        let expected = verify(&w).unwrap();
        let (built, journal) =
            from_located(&at, &opening, Some(expected.order_id), Some(opening.receiver_commitment().unwrap())).unwrap();
        assert_eq!(journal, expected);
        assert_eq!(serde_json::to_value(&built).unwrap(), serde_json::to_value(&w).unwrap());
        assert!(from_located(&at, &opening, Some([0; 32]), None).is_err(), "another order");
        assert!(from_located(&at, &opening, None, Some([0; 32])).is_err(), "another commitment");
        let mut wrong = opening.clone();
        wrong.rseed[0] ^= 1;
        assert!(from_located(&at, &wrong, None, None).is_err(), "another opening");
        assert_eq!(memo_order(&payment_memo(&expected.order_id)), Some(expected.order_id));
    }
}
