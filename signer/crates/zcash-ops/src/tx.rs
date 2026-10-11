//! Full-transaction inspection for policy P1 (Ironwood → Ironwood deposits only).
//!
//! Compact blocks may omit transparent data, so the structure of a deposit transaction is
//! always checked on the full transaction fetched by txid.

use anyhow::{Context, Result, anyhow};
use zcash_client_backend::proto::service::TxFilter;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Parameters},
};

use crate::lwd::LwdClient;

/// Which value pools a transaction touches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxShape {
    pub transparent_inputs: usize,
    pub transparent_outputs: usize,
    pub sapling_spends: usize,
    pub sapling_outputs: usize,
    pub orchard_actions: usize,
    pub ironwood_actions: usize,
}

impl TxShape {
    /// The shape of `tx`.
    pub fn of(tx: &Transaction) -> Self {
        let (transparent_inputs, transparent_outputs) =
            tx.transparent_bundle().map_or((0, 0), |b| (b.vin.len(), b.vout.len()));
        let (sapling_spends, sapling_outputs) =
            tx.sapling_bundle().map_or((0, 0), |b| (b.shielded_spends().len(), b.shielded_outputs().len()));
        Self {
            transparent_inputs,
            transparent_outputs,
            sapling_spends,
            sapling_outputs,
            orchard_actions: tx.orchard_bundle().map_or(0, |b| b.actions().len()),
            ironwood_actions: tx.ironwood_bundle().map_or(0, |b| b.actions().len()),
        }
    }

    /// True iff the transaction moves value only inside the Ironwood pool: no transparent
    /// inputs or outputs, no Sapling spends or outputs, no Orchard-pool actions. Such a
    /// transaction crosses no turnstile, so it reveals no amount.
    pub fn is_ironwood_only(&self) -> bool {
        self.transparent_inputs == 0
            && self.transparent_outputs == 0
            && self.sapling_spends == 0
            && self.sapling_outputs == 0
            && self.orchard_actions == 0
            && self.ironwood_actions > 0
    }
}

/// Fetches and parses a mined transaction. Returns it with its block height.
pub async fn fetch<P: Parameters>(
    client: &mut LwdClient,
    params: &P,
    txid: &TxId,
) -> Result<(Transaction, BlockHeight)> {
    let raw = client
        .get_transaction(TxFilter {
            hash: txid.as_ref().to_vec(),
            ..Default::default()
        })
        .await
        .map_err(|s| anyhow!("GetTransaction: {s}"))?
        .into_inner();
    let height = BlockHeight::from_u32(u32::try_from(raw.height).context("height")?);
    anyhow::ensure!(u32::from(height) > 0, "transaction {txid} is not mined yet");
    let tx = Transaction::read(&raw.data[..], BranchId::for_height(params, height)).context("parse transaction")?;
    anyhow::ensure!(tx.txid() == *txid, "server returned a different transaction");
    Ok((tx, height))
}
