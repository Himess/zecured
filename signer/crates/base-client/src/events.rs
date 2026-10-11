//! Registry ABI and ingestion of its events up to Base's `safe` head.
//!
//! Logs only say what an RPC provider claims happened; signer nodes check every event they
//! apply against the Registry's state (see [`crate::verify`]).

use std::collections::HashMap;

use alloy::{
    eips::BlockNumberOrTag,
    primitives::{Address, B256},
    providers::Provider,
    rpc::types::{Filter, Log},
    sol,
    sol_types::SolEventInterface,
};
use anyhow::{Context, Result, anyhow};
use zecured_policy::ledger::{Claim, ClaimKind, ClaimState};

sol! {
    #[sol(rpc)]
    interface IZecuredRegistry {
        struct DepositAttestation {
            bytes32 vaultId;
            address account;
            uint64 amount;
            bytes32 depositTag;
        }

        event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount);
        event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
        event LiquidationRelease(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment);
        event CloseRelease(bytes32 indexed vaultId, bytes32 userClaimId, bytes32 mmClaimId, uint64 userAmount, uint64 mmAmount, bytes32 userCommitment, bytes32 mmCommitment);
        event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);
        event VaultCancelled(bytes32 indexed vaultId);

        function attestDeposit(DepositAttestation att, bytes[] sigs) external;
        function attestDepositWithProof(bytes journal, bytes seal, bytes[] openedSigs) external;
        function cancelVault(bytes32 vaultId, bytes[] sigs) external;
        function depositTagUsed(bytes32 depositTag) external view returns (bool);
        function confirmPayout(bytes32 vaultId, bytes32[] claimIds, bytes[] sigs) external;
        function vaultOf(address account) external view returns (bytes32 vaultId);
        function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
        function claim(bytes32 claimId) external view returns (bytes32 vaultId, uint8 kind, uint64 amount, bytes32 commitment, uint8 status);
    }
}

/// A Registry event, reduced to what the signer network needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryEvent {
    /// A deposit attestation was accepted: zZEC minted into the account's position.
    VaultActivated { account: Address, amount: u64 },
    /// zZEC was burned: these claims must be paid on Zcash (one per recipient).
    Claims(Vec<Claim>),
    /// Claims were confirmed as paid on Base.
    PayoutConfirmed(Vec<B256>),
    /// The signers cancelled a vault that was never activated (D23): it can never be activated,
    /// and its conforming deposit becomes refundable.
    VaultCancelled,
}

/// A decoded event with its vault and position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultEvent {
    pub vault_id: B256,
    pub block: u64,
    pub log_index: u64,
    /// Hash of the block, as reported with the log.
    pub block_hash: Option<B256>,
    /// Timestamp of the block.
    pub timestamp: u64,
    pub event: RegistryEvent,
}

/// The latest block number Base reports as `safe`.
pub async fn safe_head<P: Provider>(provider: &P) -> Result<u64> {
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Safe)
        .await
        .context("get safe block")?
        .context("node has no safe block")?;
    Ok(block.header.number)
}

/// Registry events in blocks `from..=to`, in chain order.
pub async fn fetch<P: Provider>(provider: &P, registry: Address, from: u64, to: u64) -> Result<Vec<VaultEvent>> {
    if from > to {
        return Ok(vec![]);
    }
    let filter = Filter::new().address(registry).from_block(from).to_block(to);
    let logs = registry_logs(provider.get_logs(&filter).await.context("eth_getLogs")?, registry);
    let mut timestamps: HashMap<u64, u64> = HashMap::new();
    let mut out = Vec::with_capacity(logs.len());
    for log in logs {
        let block = log.block_number.context("log without block number")?;
        let created_at = match log.block_timestamp {
            Some(t) => t,
            None => match timestamps.get(&block) {
                Some(t) => *t,
                None => {
                    let t = provider
                        .get_block_by_number(block.into())
                        .await?
                        .context("block of a log not found")?
                        .header
                        .timestamp;
                    timestamps.insert(block, t);
                    t
                }
            },
        };
        if let Some(ev) = decode(&log, created_at)? {
            out.push(ev);
        }
    }
    out.sort_by_key(|e| (e.block, e.log_index));
    Ok(out)
}

/// Keeps only the Registry's own logs, never one the provider reports as removed by a reorg,
/// whatever the provider returned for the filter (C-2).
pub fn registry_logs(logs: Vec<Log>, registry: Address) -> Vec<Log> {
    logs.into_iter().filter(|l| l.inner.address == registry && !l.removed).collect()
}

/// Decodes one Registry log. Unknown logs are ignored.
pub fn decode(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use IZecuredRegistry::IZecuredRegistryEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let block = log.block_number.context("log without block number")?;
    let log_index = log.log_index.context("log without index")?;
    let order = (block, log_index);
    let claim = |id: B256, kind, amount: u64, commitment: B256, partner: Option<B256>| Claim {
        id: id.0,
        kind,
        amount,
        commitment: commitment.0,
        order,
        created_at,
        partner: partner.map(|p| p.0),
        state: ClaimState::Unpaid,
    };
    let (vault_id, event) = match decoded.data {
        E::VaultActivated(e) => (
            e.vaultId,
            RegistryEvent::VaultActivated {
                account: e.account,
                amount: e.amount,
            },
        ),
        E::ReleaseRequested(e) => (
            e.vaultId,
            RegistryEvent::Claims(vec![claim(e.claimId, ClaimKind::Release, e.amount, e.destCommitment, None)]),
        ),
        E::LiquidationRelease(e) => (
            e.vaultId,
            RegistryEvent::Claims(vec![claim(
                e.claimId,
                ClaimKind::Liquidation,
                e.amount,
                e.recipientCommitment,
                None,
            )]),
        ),
        E::CloseRelease(e) => (
            e.vaultId,
            RegistryEvent::Claims(vec![
                claim(e.userClaimId, ClaimKind::CloseUser, e.userAmount, e.userCommitment, Some(e.mmClaimId)),
                claim(e.mmClaimId, ClaimKind::CloseMarketMaker, e.mmAmount, e.mmCommitment, Some(e.userClaimId)),
            ]),
        ),
        E::PayoutConfirmed(e) => (e.vaultId, RegistryEvent::PayoutConfirmed(e.claimIds)),
        E::VaultCancelled(e) => (e.vaultId, RegistryEvent::VaultCancelled),
    };
    Ok(Some(VaultEvent {
        vault_id,
        block,
        log_index,
        block_hash: log.block_hash,
        timestamp: created_at,
        event,
    }))
}

/// Parses `0x`-prefixed hex into a 32-byte id.
pub fn parse_b256(s: &str) -> Result<B256> {
    s.parse::<B256>().map_err(|e| anyhow!("invalid bytes32 {s}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{
        primitives::{LogData, U256},
        sol_types::SolEvent,
    };

    fn rpc_log(data: LogData, block: u64, index: u64) -> Log {
        Log {
            inner: alloy::primitives::Log {
                address: Address::repeat_byte(0xaa),
                data,
            },
            block_number: Some(block),
            log_index: Some(index),
            ..Default::default()
        }
    }

    #[test]
    fn close_release_becomes_a_linked_claim_pair() {
        let ev = IZecuredRegistry::CloseRelease {
            vaultId: B256::repeat_byte(1),
            userClaimId: B256::repeat_byte(2),
            mmClaimId: B256::repeat_byte(3),
            userAmount: 1_200_000,
            mmAmount: 3_800_000,
            userCommitment: B256::repeat_byte(4),
            mmCommitment: B256::repeat_byte(5),
        };
        let got = decode(&rpc_log(ev.encode_log_data(), 10, 7), 1_000).unwrap().unwrap();
        assert_eq!(got.vault_id, B256::repeat_byte(1));
        let RegistryEvent::Claims(claims) = got.event else {
            panic!()
        };
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].kind, ClaimKind::CloseUser);
        assert_eq!(claims[0].partner, Some([3; 32]));
        assert_eq!(claims[1].kind, ClaimKind::CloseMarketMaker);
        assert_eq!(claims[1].amount, 3_800_000);
        assert_eq!(claims[1].order, (10, 7));
        assert_eq!(claims[1].created_at, 1_000);
    }

    #[test]
    fn only_live_registry_logs_are_kept() {
        let ev = IZecuredRegistry::PayoutConfirmed {
            vaultId: B256::repeat_byte(1),
            claimIds: vec![B256::repeat_byte(2)],
        };
        let registry = Address::repeat_byte(0xaa);
        let good = rpc_log(ev.encode_log_data(), 10, 0);
        let mut other_contract = good.clone();
        other_contract.inner.address = Address::repeat_byte(0xbb);
        let mut removed = good.clone();
        removed.removed = true;
        let kept = registry_logs(vec![other_contract, good.clone(), removed], registry);
        assert_eq!(kept, vec![good]);
    }

    #[test]
    fn vault_cancelled_is_decoded() {
        let ev = IZecuredRegistry::VaultCancelled {
            vaultId: B256::repeat_byte(6),
        };
        let got = decode(&rpc_log(ev.encode_log_data(), 12, 3), 100).unwrap().unwrap();
        assert_eq!(got.vault_id, B256::repeat_byte(6));
        assert_eq!(got.event, RegistryEvent::VaultCancelled);
        assert_eq!((got.block, got.log_index), (12, 3));
    }

    #[test]
    fn unknown_logs_are_ignored() {
        let data = LogData::new_unchecked(vec![B256::repeat_byte(9)], U256::from(1).to_be_bytes_vec().into());
        assert!(decode(&rpc_log(data, 1, 0), 0).unwrap().is_none());
    }
}
