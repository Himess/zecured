//! Witness light wallet servers that must agree with the primary server before a deposit is
//! attested (security review C-1).
//!
//! Nothing in a compact block proves work or inclusion, so a single light wallet server can
//! fabricate a block that pays a vault, plus a matching full transaction. Until every node runs
//! its own full node, a node can require one or more witness servers, operated independently of
//! its primary server, to serve the same block hash at the deposit's height, a block that lists
//! the deposit transaction, the same transaction bytes, and a tip within a few blocks of the
//! primary's. The agreed transaction must contain the deposit note's commitment at the position
//! the node scanned it from. Any disagreement or witness failure refuses the attestation.

use anyhow::{Context, Result, ensure};
use zcash_protocol::{TxId, consensus::Parameters};

use crate::lwd::{self, LwdClient};

/// Blocks a witness's tip may differ from the primary server's.
pub const TIP_TOLERANCE: u32 = 3;

/// What one server says about a deposit transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepositView {
    /// The server's chain tip.
    pub tip: u32,
    /// Hash of the block at the deposit's height.
    pub block_hash: Vec<u8>,
    /// Whether that block lists the deposit transaction.
    pub in_block: bool,
    /// The deposit transaction's raw bytes.
    pub tx: Vec<u8>,
    /// Height of the block that mined it, as reported with the transaction.
    pub tx_height: u64,
}

/// Asks one server about the transaction `txid` mined at `height`.
pub async fn deposit_view(client: &mut LwdClient, txid: &TxId, height: u32) -> Result<DepositView> {
    let tip = u32::try_from(lwd::latest_block(client).await?.height).context("tip height")?;
    let block = lwd::compact_block(client, height).await?;
    let in_block = block.vtx.iter().any(|t| t.txid.as_slice() == txid.as_ref().as_slice());
    let (tx, tx_height) = lwd::raw_transaction(client, txid).await?;
    Ok(DepositView {
        tip,
        block_hash: block.hash,
        in_block,
        tx,
        tx_height,
    })
}

/// Checks a witness's view of the deposit mined at `height` against the primary server's.
/// Messages never name the transaction (it would link the vault to its deposit in logs).
pub fn check_agreement(primary: &DepositView, witness: &DepositView, height: u32) -> Result<()> {
    ensure!(primary.in_block, "the primary server's block {height} does not list the deposit");
    ensure!(primary.tx_height == u64::from(height), "the primary server places the deposit at another height");
    ensure!(primary.block_hash.len() == 32, "the primary server's block hash is malformed");
    ensure!(witness.block_hash == primary.block_hash, "witness has another block at height {height}");
    ensure!(witness.in_block, "witness block {height} does not list the deposit");
    ensure!(witness.tx == primary.tx, "witness serves other bytes for the deposit transaction");
    ensure!(witness.tx_height == primary.tx_height, "witness places the deposit at another height");
    ensure!(
        witness.tip.abs_diff(primary.tip) <= TIP_TOLERANCE,
        "witness tip {} is more than {TIP_TOLERANCE} blocks from the primary's {}",
        witness.tip,
        primary.tip
    );
    Ok(())
}

/// Checks that the agreed bytes are the transaction `txid` mined at `height` and that its
/// Ironwood action `action_index` creates the note commitment `cmx` the node scanned.
pub fn check_note_in_tx<P: Parameters>(
    params: &P,
    txid: &TxId,
    tx: &[u8],
    height: u32,
    action_index: u32,
    cmx: &[u8; 32],
) -> Result<()> {
    let (tx, _) = crate::tx::parse_mined(params, txid, tx, u64::from(height))?;
    let action = tx
        .ironwood_bundle()
        .and_then(|b| b.actions().get(usize::try_from(action_index).ok()?))
        .context("the deposit transaction has no such Ironwood action")?;
    ensure!(action.cmx().to_bytes() == *cmx, "the deposit transaction does not create the scanned note");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> DepositView {
        DepositView {
            tip: 1_000,
            block_hash: vec![7; 32],
            in_block: true,
            tx: vec![1, 2, 3],
            tx_height: 990,
        }
    }

    #[test]
    fn agreeing_witness_passes() {
        check_agreement(&view(), &view(), 990).unwrap();
        let mut close = view();
        close.tip = 1_000 - TIP_TOLERANCE;
        check_agreement(&view(), &close, 990).unwrap();
    }

    #[test]
    fn any_disagreement_is_refused() {
        let primary = view();
        let cases: [fn(&mut DepositView); 6] = [
            |w| w.block_hash[0] ^= 1,
            |w| w.in_block = false,
            |w| w.tx[0] ^= 1,
            |w| w.tx.push(0),
            |w| w.tx_height += 1,
            |w| w.tip += TIP_TOLERANCE + 1,
        ];
        for (i, tamper) in cases.iter().enumerate() {
            let mut witness = view();
            tamper(&mut witness);
            assert!(check_agreement(&primary, &witness, 990).is_err(), "case {i}");
        }
    }

    #[test]
    fn inconsistent_primary_is_refused() {
        let mut primary = view();
        primary.in_block = false;
        assert!(check_agreement(&primary, &primary.clone(), 990).is_err());
        let mut primary = view();
        primary.block_hash = vec![];
        assert!(check_agreement(&primary, &primary.clone(), 990).is_err());
        assert!(check_agreement(&view(), &view(), 991).is_err(), "deposit at another height");
    }
}
