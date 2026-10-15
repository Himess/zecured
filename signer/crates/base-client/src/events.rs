//! Registry ABI and ingestion of its events up to Base's `safe` head.
//!
//! Logs only say what an RPC provider claims happened; signer nodes check every event they
//! apply against the Registry's state (see [`crate::verify`]).

use std::{
    collections::HashMap,
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};

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
    // `SpendCommitted` has eight fields, and so does the constructor sol! generates for it.
    #[sol(rpc)]
    #[allow(clippy::too_many_arguments)]
    interface IZecuredRegistry {
        struct DepositAttestation {
            bytes32 vaultId;
            address account;
            uint64 amount;
            bytes32 depositTag;
        }

        // Tier 3 (docs/TIER3.md; contracts/src/interfaces/IZecuredSpendCommits.sol).
        #[derive(Debug, PartialEq, Eq)]
        struct SpendCommit {
            bytes32 vaultId;
            bytes32 inputTag;
            bytes32[] claimIds;
            bytes32[] claimRefs;
            uint8[] claimOutputs;
            bytes32 payoutRef;
            bytes32 changeTag;
            uint64 changeValue;
        }
        #[derive(Debug, PartialEq, Eq)]
        struct LineageNote {
            bytes32 vaultId;
            uint64 value;
        }
        #[derive(Debug, PartialEq, Eq)]
        struct SpendRecord {
            bytes32 recordHash;
            bytes32 vaultId;
            bytes32 inputTag;
            bytes32 changeTag;
            uint64 changeValue;
            uint8 claimCount;
            uint8 signerBitmap;
        }
        #[derive(Debug, PartialEq, Eq)]
        struct ClaimPayment {
            bytes32 claimId;
            bytes32 payoutRef;
            uint64 expected;
            uint8 outputIndex;
        }

        event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
        event ActivationPending(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag, uint256 readyHeight, uint256 readyTime);
        event PendingActivationDropped(bytes32 indexed vaultId);
        event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
        event LiquidationRelease(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment);
        event CloseRelease(bytes32 indexed vaultId, bytes32 userClaimId, bytes32 mmClaimId, uint64 userAmount, uint64 mmAmount, bytes32 userCommitment, bytes32 mmCommitment);
        event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);
        event VaultCancelled(bytes32 indexed vaultId);
        event SpendCommitted(bytes32 indexed vaultId, bytes32 indexed payoutRef, bytes32 inputTag, bytes32[] claimIds, bytes32[] claimRefs, bytes32 changeTag, uint64 changeValue, uint8 signerBitmap);

        function attestDeposit(DepositAttestation att, bytes[] sigs) external;
        function attestDepositWithProof(bytes journal, bytes seal, bytes[] openedSigs) external;
        function finalizeActivation(bytes32 vaultId) external;
        function dropPendingActivation(bytes32 vaultId) external;
        function pendingActivation(bytes32 vaultId) external view returns (address account, uint64 amount, bytes32 depositTag, bytes32 anchor, uint256 readyHeight, uint256 readyTime);
        function mintMode() external view returns (uint8);
        function depositSealSelector() external view returns (bytes4);
        function finalizeDepth() external view returns (uint64);
        function finalizeDelay() external view returns (uint64);
        function cancelVault(bytes32 vaultId, bytes[] sigs) external;
        function depositTagUsed(bytes32 depositTag) external view returns (bool);
        function relay() external view returns (address);
        function minConfirmationWork() external view returns (uint256);
        function depositImageId() external view returns (bytes32);
        function confirmPayout(bytes32 vaultId, bytes32[] claimIds, bytes[] sigs) external;
        function vaultOf(address account) external view returns (bytes32 vaultId);
        function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
        function claim(bytes32 claimId) external view returns (bytes32 vaultId, uint8 kind, uint64 amount, bytes32 commitment, uint8 status);
        function commitSpend(SpendCommit c, bytes[] sigs) external;
        function lineageOf(bytes32 tag) external view returns (LineageNote memory);
        function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);
        function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);
        function claimInput(bytes32 claimId) external view returns (bytes32);
        function inputClaimSet(bytes32 inputTag) external view returns (bytes32);
        function claimRefOf(bytes32 payoutRef, bytes32 commitment) external view returns (bytes32);
        function hashSpendCommit(SpendCommit c) external view returns (bytes32);
        function openVaultCount() external view returns (uint256);
        function signerBond() external view returns (address);
        function signers() external view returns (address[5] memory);
    }
}

sol! {
    /// The Zcash header relay (contracts/src/relay/IZcashHeaderRelay.sol), as far as a deposit
    /// prover (an anchor header must be on its best chain under enough work) and a relayer need it.
    #[sol(rpc)]
    interface IZcashHeaderRelay {
        struct HeaderInfo {
            bool known;
            bytes32 prevHash;
            uint256 height;
            uint256 time;
            uint256 bits;
            uint256 chainWork;
            bytes32 merkleRoot;
            bytes32 blockCommitments;
        }

        function submitHeaders(bytes[] headers) external;
        function headerOf(bytes32 blockHash) external view returns (HeaderInfo memory);
        function isInBestChain(bytes32 blockHash) external view returns (bool);
        function bestChainTip() external view returns (bytes32 blockHash, uint256 height, uint256 chainWork);
        function bestBlockHashAt(uint256 height) external view returns (bytes32);
        function confirmationWork(bytes32 blockHash) external view returns (uint256);
    }
}

/// A Registry event, reduced to what the signer network needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryEvent {
    /// A deposit attestation or a finalized deposit proof activated the vault: zZEC minted into
    /// the account's position. `deposit_tag` names the note that backs it, the head of the
    /// vault's lineage (K2-a): it is never refundable.
    VaultActivated {
        account: Address,
        amount: u64,
        deposit_tag: B256,
    },
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

/// The latest block's number and timestamp (Unix seconds).
pub async fn latest_block<P: Provider>(provider: &P) -> Result<(u64, u64)> {
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .await
        .context("get latest block")?
        .context("node has no latest block")?;
    Ok((block.header.number, block.header.timestamp))
}

/// Review H-11: a block `secs` older than `latest` (number, timestamp), on `provider`; see
/// [`block_secs_deep_by`].
pub async fn block_secs_deep<P: Provider>(provider: &P, latest: (u64, u64), secs: u64) -> Result<u64> {
    block_secs_deep_by(latest, secs, |number| async move {
        Ok(provider
            .get_block_by_number(number.into())
            .await
            .context("get block")?
            .context("block not found")?
            .header
            .timestamp)
    })
    .await
}

/// Seconds by which [`block_secs_deep_by`] may answer an older block than the newest one that
/// qualifies (a coarser answer is safe, and saves lookups).
pub const SECS_DEEP_SLACK: u64 = 1;

/// Review H-11: a block whose timestamp is at least `secs` below the latest block's (`latest`:
/// its number and timestamp), at most [`SECS_DEEP_SLACK`] seconds older than the newest such
/// block; 0 if the chain is younger. Depths in time instead of blocks hold whatever Base's block
/// time is (2 s, then 200 ms with Denim, and both across the switch).
///
/// `timestamp_of(n)` gives block `n`'s timestamp. The search interpolates on timestamps from a
/// bracket found by stepping back from the latest block, so it takes a few lookups.
pub async fn block_secs_deep_by<F, Fut>(latest: (u64, u64), secs: u64, mut timestamp_of: F) -> Result<u64>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<u64>>,
{
    const MAX_LOOKUPS: u32 = 48;
    let (number, timestamp) = latest;
    let Some(target) = timestamp.checked_sub(secs) else {
        return Ok(0);
    };
    if secs == 0 {
        return Ok(number);
    }
    // `hi` is newer than the target; step back, doubling the step, until `lo` is old enough.
    let (mut hi, mut t_hi) = (number, timestamp);
    let mut step = secs.max(1);
    let mut lookups = 0;
    let (mut lo, mut t_lo) = loop {
        if hi == 0 {
            return Ok(0); // even the first block is newer than the target
        }
        let candidate = hi.saturating_sub(step);
        let t = timestamp_of(candidate).await?;
        lookups += 1;
        if t <= target {
            break (candidate, t);
        }
        (hi, t_hi) = (candidate, t);
        step = step.saturating_mul(2);
    };
    // Interpolate between the two (timestamps grow with block numbers), bisecting if that stalls.
    while hi - lo > 1 && t_hi.saturating_sub(t_lo) > SECS_DEEP_SLACK && lookups < MAX_LOOKUPS {
        let span = hi - lo;
        let guess = if t_hi > t_lo {
            let frac = (target - t_lo) as f64 / (t_hi - t_lo) as f64;
            lo + (span as f64 * frac) as u64
        } else {
            lo + span / 2
        };
        let mid = if lookups % 4 == 3 {
            lo + span / 2
        } else {
            guess.clamp(lo + 1, hi - 1)
        };
        let t = timestamp_of(mid).await?;
        lookups += 1;
        if t <= target {
            (lo, t_lo) = (mid, t);
        } else {
            (hi, t_hi) = (mid, t);
        }
    }
    Ok(lo)
}

/// The `eth_getLogs` block range asked for first: `ZECURED_LOG_RANGE`, or 2,000. Providers allow
/// less (Base Sepolia's public RPC: 200 blocks), so a refused range is halved (G1).
pub const DEFAULT_LOG_RANGE: u64 = 2_000;

/// The largest `eth_getLogs` range asked for (`ZECURED_LOG_RANGE`, or [`DEFAULT_LOG_RANGE`]); every
/// scanner (nodes, coordinator, liquidator) also steps through Base by it, so one setting scales
/// them all to a provider that allows more, as 200 ms blocks call for (review H-11).
pub fn log_range_ceiling() -> u64 {
    std::env::var("ZECURED_LOG_RANGE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|r: &u64| *r > 0)
        .unwrap_or(DEFAULT_LOG_RANGE)
}

/// The range this process last saw accepted (0: not learned yet).
static LEARNED_LOG_RANGE: AtomicU64 = AtomicU64::new(0);

/// Logs of `from..=to` through `get`, in requests the provider accepts. A range it refuses is
/// halved and retried (a refused single block is an error); after eight accepted requests in a
/// row the range doubles again, up to the first one, so a passing error does not slow every
/// later request. The accepted size carries over to the next calls of this process.
pub async fn get_logs_chunked<F, Fut>(from: u64, to: u64, mut get: F) -> Result<Vec<Log>>
where
    F: FnMut(u64, u64) -> Fut,
    Fut: Future<Output = Result<Vec<Log>>>,
{
    const GROW_AFTER: u32 = 8;
    let ceiling = log_range_ceiling();
    let mut range = match LEARNED_LOG_RANGE.load(Ordering::Relaxed) {
        0 => ceiling,
        learned => learned.min(ceiling),
    };
    let mut logs = vec![];
    let mut streak = 0;
    let mut start = from;
    while start <= to {
        let end = start.saturating_add(range - 1).min(to);
        match get(start, end).await {
            Ok(mut chunk) => {
                logs.append(&mut chunk);
                LEARNED_LOG_RANGE.store(range, Ordering::Relaxed);
                if end == to {
                    break;
                }
                start = end + 1;
                streak += 1;
                if streak >= GROW_AFTER && range < ceiling {
                    range = range.saturating_mul(2).min(ceiling);
                    streak = 0;
                }
            }
            Err(_) if range > 1 => {
                range /= 2;
                streak = 0;
            }
            Err(e) => return Err(e.context("eth_getLogs")),
        }
    }
    Ok(logs)
}

/// Registry events in blocks `from..=to`, in chain order.
pub async fn fetch<P: Provider>(provider: &P, registry: Address, from: u64, to: u64) -> Result<Vec<VaultEvent>> {
    if from > to {
        return Ok(vec![]);
    }
    let logs = get_logs_chunked(from, to, |a, b| async move {
        let filter = Filter::new().address(registry).from_block(a).to_block(b);
        provider.get_logs(&filter).await.map_err(anyhow::Error::from)
    })
    .await?;
    let logs = registry_logs(logs, registry);
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

/// `MintMode.ProofOnly` (K1).
pub const MINT_MODE_PROOF_ONLY: u8 = 1;

/// Decodes one Registry log. Unknown logs, the pending-activation logs (K3: nothing is minted and
/// nothing changes for the signers until `VaultActivated`) and Tier 3 commitments (each node reads
/// the record it signed straight from the Registry) are ignored.
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
                deposit_tag: e.depositTag,
            },
        ),
        E::ActivationPending(_) | E::PendingActivationDropped(_) | E::SpendCommitted(_) => return Ok(None),
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

    /// A chain whose block `n` has timestamp `timestamp(n)`; the search's lookups are counted.
    async fn secs_deep(chain: &dyn Fn(u64) -> u64, latest: u64, secs: u64) -> (u64, usize) {
        let lookups = std::sync::Mutex::new(0usize);
        let found = block_secs_deep_by((latest, chain(latest)), secs, |n| {
            *lookups.lock().unwrap() += 1;
            let t = chain(n);
            async move { Ok(t) }
        })
        .await
        .unwrap();
        (found, lookups.into_inner().unwrap())
    }

    /// H-11: the block a time depth reads at is old enough, close to the newest that is, and found
    /// in a few lookups: at 2 s blocks, at 200 ms blocks (several blocks per second, as Denim's
    /// seconds-resolution timestamps give), across the switch from one to the other, and on a chain
    /// younger than the depth.
    #[tokio::test]
    async fn a_time_depth_finds_a_block_that_old() {
        let two_secs = |n: u64| 1_700_000_000 + 2 * n;
        let fifth = |n: u64| 1_700_000_000 + n / 5;
        // Denim at block 1,000,000: 2 s blocks before, 200 ms after.
        let denim = |n: u64| {
            if n <= 1_000_000 {
                1_700_000_000 + 2 * n
            } else {
                1_702_000_000 + (n - 1_000_000) / 5
            }
        };
        let chains: [(&dyn Fn(u64) -> u64, u64); 4] = [
            (&two_secs, 2_000_000),
            (&fifth, 9_000_000),
            (&denim, 1_000_300),
            (&denim, 1_100_000),
        ];
        for (chain, latest) in chains {
            for secs in [1, 6, 120, 180, 600, 7_200] {
                let (b, lookups) = secs_deep(chain, latest, secs).await;
                let target = chain(latest) - secs;
                assert!(chain(b) <= target, "old enough: block {b} for {secs} s");
                assert!(chain(b) + SECS_DEEP_SLACK + 1 >= target, "close to the newest such block: {b} for {secs} s");
                assert!(lookups <= 30, "{lookups} lookups for {secs} s");
            }
        }
        // No depth: the latest block. A chain younger than the depth: block 0.
        assert_eq!(secs_deep(&two_secs, 500, 0).await.0, 500);
        assert_eq!(secs_deep(&two_secs, 50, 120).await.0, 0);
        let young = |n: u64| 1_000 + 2 * n;
        assert_eq!(secs_deep(&young, 10, 5_000).await.0, 0);
    }

    /// G1: a provider that refuses ranges over 200 blocks (Base Sepolia's public RPC) is still
    /// asked for every block, once and in order; a provider that refuses even one block fails.
    #[tokio::test]
    async fn log_ranges_shrink_to_what_the_provider_accepts() {
        let asked = std::sync::Mutex::new(vec![]);
        let logs = get_logs_chunked(1_000, 5_999, |a, b| {
            asked.lock().unwrap().push((a, b));
            async move {
                anyhow::ensure!(b - a < 200, "eth_getLogs is limited to a 200 range");
                Ok(vec![])
            }
        })
        .await
        .unwrap();
        assert!(logs.is_empty());
        let accepted: Vec<(u64, u64)> = asked.lock().unwrap().iter().copied().filter(|(a, b)| b - a < 200).collect();
        assert_eq!((accepted[0].0, accepted[accepted.len() - 1].1), (1_000, 5_999));
        assert!(accepted.windows(2).all(|w| w[0].1 + 1 == w[1].0), "contiguous, no block twice");
        assert!(LEARNED_LOG_RANGE.load(Ordering::Relaxed) <= 200);

        let down = get_logs_chunked(1, 10, |_, _| async { anyhow::bail!("connection refused") }).await;
        assert!(down.is_err());
    }

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
    fn vault_activated_carries_the_deposit_tag() {
        let ev = IZecuredRegistry::VaultActivated {
            vaultId: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            amount: 1_000_000,
            depositTag: B256::repeat_byte(3),
        };
        let got = decode(&rpc_log(ev.encode_log_data(), 5, 1), 50).unwrap().unwrap();
        assert_eq!(
            got.event,
            RegistryEvent::VaultActivated {
                account: Address::repeat_byte(2),
                amount: 1_000_000,
                deposit_tag: B256::repeat_byte(3),
            }
        );
    }

    #[test]
    fn pending_activation_logs_are_not_vault_events() {
        let pending = IZecuredRegistry::ActivationPending {
            vaultId: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            amount: 1_000_000,
            depositTag: B256::repeat_byte(3),
            readyHeight: U256::from(10),
            readyTime: U256::from(20),
        };
        assert!(decode(&rpc_log(pending.encode_log_data(), 5, 1), 50).unwrap().is_none());
        let dropped = IZecuredRegistry::PendingActivationDropped {
            vaultId: B256::repeat_byte(1),
        };
        assert!(decode(&rpc_log(dropped.encode_log_data(), 5, 2), 50).unwrap().is_none());
    }

    #[test]
    fn unknown_logs_are_ignored() {
        let data = LogData::new_unchecked(vec![B256::repeat_byte(9)], U256::from(1).to_be_bytes_vec().into());
        assert!(decode(&rpc_log(data, 1, 0), 0).unwrap().is_none());
    }
}
