//! Following a broadcast payout until it is confirmed on Base, or provably dead (I-5, H-5).
//!
//! A payout is rebuilt only when the light wallet server **definitely** does not have it
//! mined (gRPC `NotFound`, mempool, or a side chain), its expiry height is safely behind the
//! tip, **and** none of the vault notes it spends is spent in our own scan. Transport errors
//! never clear it. A spent input with a transaction the server does not report means "do not
//! rebuild, investigate": rebuilding could only produce a transaction the signers refuse, and
//! dropping the record would lose the txid the signers need to confirm the real payout.

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow, ensure};
use tonic::Code;
use zcash_client_backend::proto::service::TxFilter;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Network},
};
use zecured_policy::classify;
use zecured_zcash_ops::lwd::LwdClient;

/// What the light wallet server says about a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxLookup {
    /// Mined in the server's main chain at this height.
    Mined(u32),
    /// Known, but in the mempool or only on a side chain.
    NotMined,
    /// The server definitely does not know the transaction.
    NotFound,
}

/// The payout's vault inputs, as seen by our own scan of the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inputs {
    /// Every input is an unspent vault note.
    Unspent,
    /// At least one input was spent by some mined transaction.
    Spent,
    /// The inputs are not known (record from an older version without them).
    Unknown,
}

/// What to do with an in-flight payout on this pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Mined with enough confirmations: confirm it on Base.
    Confirm,
    /// Nothing to do yet.
    Wait,
    /// Not mined, not expired: make sure it is in the mempool.
    Rebroadcast,
    /// Provably dead: drop the record so the claims are paid by a new transaction.
    Rebuild,
    /// Its inputs are spent by a transaction the server does not report: keep the record and
    /// alert; never rebuild.
    Investigate,
}

/// Blocks past the expiry height before a payout that is not found counts as dead: a short
/// reorg could still mine it below its expiry height, and the signers' tips may lag ours.
pub fn expiry_margin(confirmations: u32) -> u32 {
    confirmations.max(1)
}

/// The decision for one pass. `lookup` is `Err` when the server could not be asked or gave
/// an answer that does not say whether the transaction exists.
pub fn decide(lookup: &Result<TxLookup>, tip: u32, expiry_height: u32, confirmations: u32, inputs: Inputs) -> Action {
    match lookup {
        Ok(TxLookup::Mined(height)) => {
            if classify::confirmations(*height, tip) >= confirmations {
                Action::Confirm
            } else {
                Action::Wait
            }
        }
        Ok(TxLookup::NotFound | TxLookup::NotMined) => {
            if tip > expiry_height.saturating_add(expiry_margin(confirmations)) {
                match inputs {
                    Inputs::Spent => Action::Investigate,
                    Inputs::Unspent | Inputs::Unknown => Action::Rebuild,
                }
            } else if inputs == Inputs::Spent {
                // Mined (we see its nullifiers) but not indexed by GetTransaction yet.
                Action::Wait
            } else if tip <= expiry_height {
                Action::Rebroadcast
            } else {
                Action::Wait
            }
        }
        Err(_) => {
            if tip <= expiry_height && inputs != Inputs::Spent {
                Action::Rebroadcast
            } else {
                Action::Wait
            }
        }
    }
}

/// The height of a `GetTransaction` answer, if it says "mined in the main chain": 0 is the
/// mempool and `u64::MAX` a side chain (`RawTransaction.height`, lightwallet protocol).
pub fn mined_height(raw_height: u64) -> Option<u32> {
    (raw_height > 0).then(|| u32::try_from(raw_height).ok()).flatten()
}

/// True if a `GetTransaction` error status definitely means "no such transaction":
/// lightwalletd answers `NotFound`; Zebra-backed servers may relay the node's RPC error
/// ("No such mempool or main chain transaction") with another code.
pub fn is_not_found(code: Code, message: &str) -> bool {
    match code {
        Code::NotFound => true,
        // Transport and load problems never mean "not found".
        Code::Unavailable
        | Code::DeadlineExceeded
        | Code::Cancelled
        | Code::ResourceExhausted
        | Code::Unauthenticated
        | Code::PermissionDenied => false,
        _ => message.contains("No such mempool or main chain transaction"),
    }
}

/// Asks the light wallet server about `txid`.
pub async fn lookup(client: &mut LwdClient, network: &Network, txid: &TxId) -> Result<TxLookup> {
    let answer = client
        .get_transaction(TxFilter {
            hash: txid.as_ref().to_vec(),
            ..Default::default()
        })
        .await;
    let raw = match answer {
        Ok(r) => r.into_inner(),
        Err(status) if is_not_found(status.code(), status.message()) => return Ok(TxLookup::NotFound),
        // The message may carry server details; the code is enough.
        Err(status) => return Err(anyhow!("GetTransaction failed ({:?})", status.code())),
    };
    let Some(height) = mined_height(raw.height) else {
        return Ok(TxLookup::NotMined);
    };
    let tx = Transaction::read(&raw.data[..], BranchId::for_height(network, BlockHeight::from_u32(height)))
        .context("parse transaction")?;
    ensure!(tx.txid() == *txid, "server returned a different transaction");
    Ok(TxLookup::Mined(height))
}

/// Nullifiers of every Ironwood action of a serialized transaction (vault inputs and padding).
pub fn nullifiers_of(raw_hex: &str, network: &Network, height: u32) -> Result<Vec<B256>> {
    let raw = hex::decode(raw_hex)?;
    let tx = Transaction::read(&raw[..], BranchId::for_height(network, BlockHeight::from_u32(height)))?;
    Ok(tx
        .ironwood_bundle()
        .map(|b| b.actions().iter().map(|a| B256::from(a.nullifier().to_bytes())).collect())
        .unwrap_or_default())
}

/// `ClaimStatus.Pending` and `ClaimStatus.Paid` in the Registry.
pub const CLAIM_PENDING: u8 = 1;
pub const CLAIM_PAID: u8 = 2;

/// R-1: the claims of a mined payout of `vault_id` (`claims`, in the payout's order) that three
/// signers confirm: those the Registry still has pending (`states`: its `(vaultId, status)` of each
/// claim), never a reopened one, which only a proof settles again. The others were confirmed
/// already (a confirmation whose receipt was lost, H-5), or a payout planned again after a false
/// confirmation (O-4) names them: the payout's proof makes every claim final (section 3).
pub fn confirmable(vault_id: &B256, claims: &[B256], states: &[(B256, u8)], reopened: &[[u8; 32]]) -> Vec<B256> {
    claims
        .iter()
        .zip(states)
        .filter(|(c, (v, s))| v == vault_id && *s == CLAIM_PENDING && !reopened.contains(&c.0))
        .map(|(c, _)| *c)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPIRY: u32 = 1_000;
    const CONFS: u32 = 3;

    fn act(lookup: Result<TxLookup>, tip: u32, inputs: Inputs) -> Action {
        decide(&lookup, tip, EXPIRY, CONFS, inputs)
    }

    #[test]
    fn mined_payouts_are_confirmed_once_deep_enough() {
        assert_eq!(act(Ok(TxLookup::Mined(990)), 991, Inputs::Spent), Action::Wait);
        assert_eq!(act(Ok(TxLookup::Mined(990)), 992, Inputs::Spent), Action::Confirm);
    }

    /// I-5: a transport error never clears the record, however late it is.
    #[test]
    fn transport_errors_never_rebuild() {
        for tip in [EXPIRY - 5, EXPIRY + 1, EXPIRY + 500] {
            for inputs in [Inputs::Unspent, Inputs::Spent, Inputs::Unknown] {
                let a = act(Err(anyhow!("GetTransaction failed (Unavailable)")), tip, inputs);
                assert!(matches!(a, Action::Wait | Action::Rebroadcast), "{tip} {inputs:?} {a:?}");
            }
        }
        assert_eq!(act(Err(anyhow!("x")), EXPIRY - 5, Inputs::Unspent), Action::Rebroadcast);
    }

    #[test]
    fn a_definitely_dead_payout_is_rebuilt_after_the_margin() {
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY, Inputs::Unspent), Action::Rebroadcast);
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY + CONFS, Inputs::Unspent), Action::Wait);
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY + CONFS + 1, Inputs::Unspent), Action::Rebuild);
        assert_eq!(act(Ok(TxLookup::NotMined), EXPIRY + CONFS + 1, Inputs::Unspent), Action::Rebuild);
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY + CONFS + 1, Inputs::Unknown), Action::Rebuild);
    }

    /// I-5: spent inputs with a transaction the server does not report: never rebuild.
    #[test]
    fn spent_inputs_with_an_unknown_transaction_are_investigated() {
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY + 50, Inputs::Spent), Action::Investigate);
        assert_eq!(act(Ok(TxLookup::NotMined), EXPIRY + 50, Inputs::Spent), Action::Investigate);
        assert_eq!(act(Ok(TxLookup::NotFound), EXPIRY - 1, Inputs::Spent), Action::Wait);
    }

    #[test]
    fn grpc_statuses_are_classified() {
        assert!(is_not_found(Code::NotFound, "GetTransaction: getrawtransaction failed"));
        assert!(is_not_found(Code::Internal, "RPC error: No such mempool or main chain transaction"));
        assert!(!is_not_found(Code::Internal, "connection reset"));
        assert!(!is_not_found(Code::Unavailable, "No such mempool or main chain transaction"));
        assert!(!is_not_found(Code::Unknown, "transport error"));
        assert!(!is_not_found(Code::DeadlineExceeded, ""));
    }

    #[test]
    fn raw_heights_follow_the_lightwallet_protocol() {
        assert_eq!(mined_height(0), None, "mempool");
        assert_eq!(mined_height(u64::MAX), None, "side chain");
        assert_eq!(mined_height(4_486_956), Some(4_486_956));
    }

    /// H-5 and R-1: a payout whose confirmation already went through is not confirmed again; a
    /// payout planned again after a false confirmation confirms only its pending claims; a reopened
    /// claim, another vault's or an unknown one is never confirmed.
    #[test]
    fn only_pending_claims_are_confirmed() {
        let v = B256::repeat_byte(1);
        let (a, b, c) = (B256::repeat_byte(0xa), B256::repeat_byte(0xb), B256::repeat_byte(0xc));
        let claims = [a, b, c];
        let all_paid = [(v, CLAIM_PAID); 3];
        assert!(confirmable(&v, &claims, &all_paid, &[]).is_empty(), "confirmed already");
        let pending = [(v, CLAIM_PENDING); 3];
        assert_eq!(confirmable(&v, &claims, &pending, &[]), vec![a, b, c]);
        let mixed = [(v, CLAIM_PAID), (v, CLAIM_PENDING), (v, CLAIM_PENDING)];
        assert_eq!(confirmable(&v, &claims, &mixed, &[]), vec![b, c], "in the payout's order");
        assert_eq!(confirmable(&v, &claims, &pending, &[b.0]), vec![a, c], "never a reopened claim");
        let strangers = [(B256::repeat_byte(2), CLAIM_PENDING), (B256::ZERO, 0), (v, 5)];
        assert!(confirmable(&v, &claims, &strangers, &[]).is_empty());
    }
}
