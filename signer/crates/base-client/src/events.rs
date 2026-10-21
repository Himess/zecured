//! The v3 contracts' ABIs (Registry, Intake, slasher, bond, verifier versions) and ingestion of
//! the Registry's events up to Base's `safe` head.
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
    /// The Registry v3 (contracts/src/interfaces/v3/IZecuredRegistry.sol, IZecuredSpendCommits.sol
    /// and IZecuredDisputes.sol, which the Registry serves through its fallback), as far as the
    /// signer network, the CLI and the bots use it.
    // `SpendCommitted` has eight fields, and so does the constructor sol! generates for it.
    #[sol(rpc)]
    #[allow(clippy::too_many_arguments)]
    interface IZecuredRegistry {
        // Tier 3 (docs/TIER3.md; contracts/src/interfaces/v3/IZecuredSpendCommits.sol).
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
        // `kind` and `status` are the Registry's `ClaimKind` and `ClaimStatus` enums (values in
        // `crate::verify`).
        #[derive(Debug, PartialEq, Eq)]
        struct ClaimDetails {
            bytes32 vaultId;
            uint8 kind;
            uint8 status;
            uint64 amount;
            bytes32 commitment;
            uint40 createdAt;
            bool bondPaid;
            address claimant;
        }
        #[derive(Debug, PartialEq, Eq)]
        struct VaultDetails {
            address account;
            uint64 outstanding;
            uint8 status;
            uint64 claimCount;
            uint64 unpaidClaims;
        }

        event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
        event VaultReserved(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
        event VaultReservationReleased(bytes32 indexed vaultId);
        event RefundOpened(bytes32 indexed vaultId, bytes32 indexed claimId, address account, uint64 amount, bytes32 depositTag);
        event RefundDestinationSet(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 commitment);
        event RefundAbandoned(bytes32 indexed vaultId, bytes32 indexed claimId);
        event FeeCharged(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 fee);
        event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
        event LiquidationRelease(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment);
        event CloseRelease(bytes32 indexed vaultId, bytes32 userClaimId, bytes32 mmClaimId, uint64 userAmount, uint64 mmAmount, bytes32 userCommitment, bytes32 mmCommitment);
        event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);
        event DustWrittenOff(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount);
        event ClaimBondPaid(bytes32 indexed vaultId, bytes32 indexed claimId);
        event ReimburseDestinationSet(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 commitment);
        event VaultRetired(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 retired);
        event ClaimCompensated(bytes32 indexed vaultId, bytes32 indexed claimId);
        event SpendCommitted(bytes32 indexed vaultId, bytes32 indexed payoutRef, bytes32 inputTag, bytes32[] claimIds, bytes32[] claimRefs, bytes32 changeTag, uint64 changeValue, uint8 signerBitmap);
        event PayoutDisputed(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 dueAt);
        event PayoutUpheld(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 payoutRef);
        event DisputedClaimReopened(bytes32 indexed vaultId, bytes32 indexed claimId);

        function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);
        function setRefundDestination(bytes32 claimId, bytes32 destCommitment) external;
        function confirmPayout(bytes32 vaultId, bytes32[] claimIds, bytes[] sigs) external;
        function commitSpend(SpendCommit c, bytes[] sigs) external;
        function disputePayout(bytes32 claimId) external;
        function reopenDisputedClaim(bytes32 claimId) external;
        function vaultOf(address account) external view returns (bytes32 vaultId);
        function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, uint8 status);
        function vaultDetails(bytes32 vaultId) external view returns (VaultDetails memory);
        function claim(bytes32 claimId) external view returns (bytes32 vaultId, uint8 kind, uint64 amount, bytes32 commitment, uint8 status);
        function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory);
        function claimIdOf(bytes32 vaultId, uint64 sequence) external pure returns (bytes32);
        function depositTagUsed(bytes32 depositTag) external view returns (bool);
        function feeCommitment() external view returns (bytes32);
        function lineageOf(bytes32 tag) external view returns (LineageNote memory);
        function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);
        function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);
        function claimInput(bytes32 claimId) external view returns (bytes32);
        function inputClaimSet(bytes32 inputTag) external view returns (bytes32);
        function claimRefOf(bytes32 payoutRef, bytes32 commitment) external view returns (bytes32);
        function hashSpendCommit(SpendCommit c) external view returns (bytes32);
        function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt);
        function disputeWindow() external view returns (uint64);
        function openVaultCount() external view returns (uint256);
        function intake() external view returns (address);
        function slasher() external view returns (address);
        function signerBond() external view returns (address);
        function signers() external view returns (address[5] memory);
    }
}

sol! {
    /// The Intake (contracts/src/interfaces/v3/IZecuredIntake.sol): deposit proofs and pending
    /// activations (K3). The image id a seal was made for is an argument, which the Intake checks
    /// against the verifier version registry (design notes T-07a).
    #[sol(rpc)]
    interface IZecuredIntake {
        #[derive(Debug, PartialEq, Eq)]
        struct PendingActivation {
            address account;
            uint64 amount;
            bytes32 depositTag;
            bytes32 anchor;
            uint256 readyHeight;
            uint256 readyTime;
        }

        function attestDepositWithProof(bytes journal, bytes seal, bytes32 imageId, bytes[] openedSigs) external;
        function finalizeActivation(bytes32 vaultId) external;
        function dropPendingActivation(bytes32 vaultId) external;
        function refundPending(bytes32 vaultId) external returns (bytes32 claimId);
        function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory);
        function journalV2() external view returns (bool);
        function minRefundZat() external view returns (uint64);
        function minConfirmationWorkFor(uint64 amount) external view returns (uint256);
        function mintingPaused() external view returns (bool);
        function relay() external view returns (address);
        function signerBond() external view returns (address);
        function versions() external view returns (address);
    }
}

sol! {
    /// The slasher (contracts/src/interfaces/v3/IZecuredSlasher.sol), as far as the signer network
    /// uses it: the compromised flag (T-04) and inclusion proofs of disputed payouts (T-05 (a)).
    #[sol(rpc)]
    interface IZecuredSlasher {
        function isCompromised(bytes32 vaultId) external view returns (bool);
        function proveInclusion(bytes journal, bytes seal, bytes32 imageId, bytes32 claimId) external;
        function slashConfirmationWork() external view returns (uint256);
        function relay() external view returns (address);
        function versions() external view returns (address);
    }
}

sol! {
    /// SignerBond v2 (contracts/src/interfaces/v3/ISignerBond.sol): the liveness period (T-05) and
    /// the halt.
    #[sol(rpc)]
    interface ISignerBond {
        function livenessPeriod() external view returns (uint64);
        function halted() external view returns (bool);
    }
}

sol! {
    /// The verifier version registry (design notes T-07a): the guest image ids, with their RISC Zero
    /// router selector and (deposits) consensus branch ids, that Base accepts proofs of.
    #[sol(rpc)]
    interface IZecuredVersions {
        #[derive(Debug, PartialEq, Eq)]
        struct Version {
            uint8 kind;
            bytes4 selector;
            bytes32 imageId;
            uint32[4] branches;
            uint64 activeAt;
            bool removed;
        }

        function versionIds(uint8 kind) external view returns (bytes32[] memory);
        function version(bytes32 versionId) external view returns (Version memory);
        function isActive(bytes32 versionId) external view returns (bool);
        function branchAllowed(bytes32 versionId, uint32 branchId) external view returns (bool);
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

/// `VersionKind.Deposit` and `VersionKind.Violation` of the verifier version registry (T-07a).
pub const VERSION_DEPOSIT: u8 = 0;
pub const VERSION_VIOLATION: u8 = 1;

/// The verifier version registry's id of `(kind, selector, imageId)`:
/// `keccak256(abi.encode(kind, selector, imageId))` (`IZecuredVersions.versionIdOf`).
pub fn version_id(kind: u8, selector: [u8; 4], image_id: B256) -> B256 {
    let mut encoded = [0u8; 96];
    encoded[31] = kind;
    encoded[32..36].copy_from_slice(&selector);
    encoded[64..].copy_from_slice(image_id.as_slice());
    alloy::primitives::keccak256(encoded)
}

/// What changed about an existing claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimChange {
    /// `RefundDestinationSet` (T-02): the vault's account gave the refund's destination; an
    /// abandoned refund is pending again, and the claim is payable from now on.
    RefundDestination(B256),
    /// `ReimburseDestinationSet` (T-05): three signers pointed a bond-paid claim at the
    /// destination that reimburses the bonds.
    ReimburseDestination(B256),
    /// `ClaimBondPaid` (T-05): the bonds paid the claimant; its destination is cleared.
    BondPaid,
    /// `RefundAbandoned` or `DustWrittenOff`: written off, its value stays in the vault.
    WrittenOff,
    /// `ClaimCompensated` (T-04): settled in USDC by the compensator.
    Compensated,
    /// `PayoutDisputed` (T-05 (a)): its claimant disputes the confirmation; unpaid on Base until a
    /// committed payout of its input is proven mined, or until the claim is reopened after `due_at`.
    Disputed { due_at: u64 },
    /// `PayoutUpheld`: a committed payout of its input (`payout_ref`) was proven mined: paid.
    Upheld { payout_ref: B256 },
    /// `DisputedClaimReopened`: nobody proved its payout in time: pending again, still bound to its
    /// input.
    Reopened,
}

/// A Registry event, reduced to what the signer network needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryEvent {
    /// K3: a deposit proof reserved the vault; nothing is minted until `VaultActivated`. Signer
    /// nodes do not apply it (nothing is paid from a pending vault).
    Reserved {
        account: Address,
        amount: u64,
        deposit_tag: B256,
    },
    /// K3: the reservation was released (its anchor left the relay's best chain).
    ReservationReleased,
    /// A finalized deposit proof activated the vault: zZEC minted into the account's position.
    /// `deposit_tag` names the note that backs it, the head of the vault's main lineage (K2-a).
    VaultActivated {
        account: Address,
        amount: u64,
        deposit_tag: B256,
    },
    /// New claims: zZEC was burned (one claim per recipient), a deposit landed as a refund (T-02),
    /// the opening fee was charged (T-06), or the compensator retired a compromised vault's
    /// outstanding (T-04, bookkeeping).
    Claims(Vec<Claim>),
    /// An existing claim changed.
    ClaimChanged { claim_id: B256, change: ClaimChange },
    /// Claims were confirmed as paid on Base.
    PayoutConfirmed(Vec<B256>),
}

impl RegistryEvent {
    /// Whether signer nodes apply it: the reservation events matter to the coordinator only.
    pub fn for_signers(&self) -> bool {
        !matches!(self, RegistryEvent::Reserved { .. } | RegistryEvent::ReservationReleased)
    }
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

impl VaultEvent {
    /// Sets the block's timestamp, which is also when the event's new claims were created.
    pub fn set_timestamp(&mut self, timestamp: u64) {
        self.timestamp = timestamp;
        if let RegistryEvent::Claims(claims) = &mut self.event {
            for c in claims {
                c.created_at = timestamp;
            }
        }
    }
}

// The latest block number Base reports as `safe`.
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

//// Registry events in blocks `from..=to`, in chain order. A Fee claim's commitment, which its
/// event does not carry, is the Registry's `feeCommitment` (the same for every vault, T-06).
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
    let mut fee_commitment: Option<B256> = None;
    let mut out = Vec::with_capacity(logs.len());
    for log in logs {
        // Logs the signer network does not use (Tier 3 commitments) cost no block lookup.
        let Some(mut ev) = decode(&log, 0)? else {
            continue;
        };
        let block = ev.block;
        let timestamp = match log.block_timestamp {
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
        ev.set_timestamp(timestamp);
        if let RegistryEvent::Claims(claims) = &mut ev.event {
            for c in claims.iter_mut().filter(|c| c.kind == ClaimKind::Fee) {
                let commitment = match fee_commitment {
                    Some(f) => f,
                    None => {
                        let f = IZecuredRegistry::new(registry, provider)
                            .feeCommitment()
                            .call()
                            .await
                            .context("Registry.feeCommitment")?;
                        *fee_commitment.insert(f)
                    }
                };
                c.commitment = commitment.0;
            }
        }
        out.push(ev);
    }
    out.sort_by_key(|e| (e.block, e.log_index));
    Ok(out)
}

/// Keeps only the Registry's own logs, never one the provider reports as removed by a reorg,
/// whatever the provider returned for the filter (C-2).
pub fn registry_logs(logs: Vec<Log>, registry: Address) -> Vec<Log> {
    logs.into_iter().filter(|l| l.inner.address == registry && !l.removed).collect()
}

/// Decodes one Registry log; `created_at` is its block's timestamp. Unknown logs and Tier 3
/// commitments (each node reads the record it signed straight from the Registry) are ignored. A
/// Fee claim's commitment is left zero ([`fetch`] fills it in).
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
        refund_tag: None,
        bond_paid: false,
    };
    let changed = |claim_id: B256, change: ClaimChange| RegistryEvent::ClaimChanged { claim_id, change };
    let (vault_id, event) = match decoded.data {
        E::VaultActivated(e) => (
            e.vaultId,
            RegistryEvent::VaultActivated {
                account: e.account,
                amount: e.amount,
                deposit_tag: e.depositTag,
            },
        ),
        E::VaultReserved(e) => (
            e.vaultId,
            RegistryEvent::Reserved {
                account: e.account,
                amount: e.amount,
                deposit_tag: e.depositTag,
            },
        ),
        E::VaultReservationReleased(e) => (e.vaultId, RegistryEvent::ReservationReleased),
        E::RefundOpened(e) => (
            e.vaultId,
            RegistryEvent::Claims(vec![Claim {
                refund_tag: Some(e.depositTag.0),
                ..claim(e.claimId, ClaimKind::Refund, e.amount, B256::ZERO, None)
            }]),
        ),
        E::FeeCharged(e) => {
            (e.vaultId, RegistryEvent::Claims(vec![claim(e.claimId, ClaimKind::Fee, e.fee, B256::ZERO, None)]))
        }
        E::VaultRetired(e) => (
            e.vaultId,
            RegistryEvent::Claims(vec![Claim {
                state: ClaimState::Compensated,
                ..claim(e.claimId, ClaimKind::Retired, e.retired, B256::ZERO, None)
            }]),
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
        E::RefundDestinationSet(e) => (e.vaultId, changed(e.claimId, ClaimChange::RefundDestination(e.commitment))),
        E::ReimburseDestinationSet(e) => {
            (e.vaultId, changed(e.claimId, ClaimChange::ReimburseDestination(e.commitment)))
        }
        E::ClaimBondPaid(e) => (e.vaultId, changed(e.claimId, ClaimChange::BondPaid)),
        E::RefundAbandoned(e) => (e.vaultId, changed(e.claimId, ClaimChange::WrittenOff)),
        E::DustWrittenOff(e) => (e.vaultId, changed(e.claimId, ClaimChange::WrittenOff)),
        E::ClaimCompensated(e) => (e.vaultId, changed(e.claimId, ClaimChange::Compensated)),
        E::PayoutDisputed(e) => (e.vaultId, changed(e.claimId, ClaimChange::Disputed { due_at: e.dueAt })),
        E::PayoutUpheld(e) => (
            e.vaultId,
            changed(
                e.claimId,
                ClaimChange::Upheld {
                    payout_ref: e.payoutRef,
                },
            ),
        ),
        E::DisputedClaimReopened(e) => (e.vaultId, changed(e.claimId, ClaimChange::Reopened)),
        E::PayoutConfirmed(e) => (e.vaultId, RegistryEvent::PayoutConfirmed(e.claimIds)),
        E::SpendCommitted(_) => return Ok(None),
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

    /// The Registry v3's refund, fee and retirement events become claims: a refund knows the note
    /// that pays it and has no destination yet, a fee's commitment is filled in by `fetch`, a
    /// retirement is bookkeeping (settled in USDC).
    #[test]
    fn v3_claim_events_are_decoded() {
        let decode_at = |data: LogData| decode(&rpc_log(data, 12, 3), 100).unwrap().unwrap();
        let refund = decode_at(
            IZecuredRegistry::RefundOpened {
                vaultId: B256::repeat_byte(6),
                claimId: B256::repeat_byte(7),
                account: Address::repeat_byte(8),
                amount: 250_000,
                depositTag: B256::repeat_byte(9),
            }
            .encode_log_data(),
        );
        assert_eq!(refund.vault_id, B256::repeat_byte(6));
        let RegistryEvent::Claims(claims) = refund.event else {
            panic!()
        };
        assert_eq!((claims[0].kind, claims[0].amount), (ClaimKind::Refund, 250_000));
        assert_eq!(claims[0].refund_tag, Some([9; 32]));
        assert_eq!(claims[0].commitment, [0; 32], "no destination yet");
        assert_eq!((claims[0].order, claims[0].created_at), ((12, 3), 100));

        let fee = decode_at(
            IZecuredRegistry::FeeCharged {
                vaultId: B256::repeat_byte(6),
                claimId: B256::repeat_byte(7),
                fee: 25_000,
            }
            .encode_log_data(),
        );
        let RegistryEvent::Claims(claims) = fee.event else {
            panic!()
        };
        assert_eq!((claims[0].kind, claims[0].amount, claims[0].state), (ClaimKind::Fee, 25_000, ClaimState::Unpaid));

        let retired = decode_at(
            IZecuredRegistry::VaultRetired {
                vaultId: B256::repeat_byte(6),
                claimId: B256::repeat_byte(7),
                retired: 400_000,
            }
            .encode_log_data(),
        );
        let RegistryEvent::Claims(claims) = retired.event else {
            panic!()
        };
        assert_eq!((claims[0].kind, claims[0].state), (ClaimKind::Retired, ClaimState::Compensated));
    }

    /// Every change of an existing claim, the dispute path's included, is decoded.
    #[test]
    fn claim_changes_are_decoded() {
        let (vault, id) = (B256::repeat_byte(6), B256::repeat_byte(7));
        let commitment = B256::repeat_byte(8);
        let cases: Vec<(LogData, ClaimChange)> = vec![
            (
                IZecuredRegistry::RefundDestinationSet {
                    vaultId: vault,
                    claimId: id,
                    commitment,
                }
                .encode_log_data(),
                ClaimChange::RefundDestination(commitment),
            ),
            (
                IZecuredRegistry::ReimburseDestinationSet {
                    vaultId: vault,
                    claimId: id,
                    commitment,
                }
                .encode_log_data(),
                ClaimChange::ReimburseDestination(commitment),
            ),
            (
                IZecuredRegistry::ClaimBondPaid {
                    vaultId: vault,
                    claimId: id,
                }
                .encode_log_data(),
                ClaimChange::BondPaid,
            ),
            (
                IZecuredRegistry::RefundAbandoned {
                    vaultId: vault,
                    claimId: id,
                }
                .encode_log_data(),
                ClaimChange::WrittenOff,
            ),
            (
                IZecuredRegistry::DustWrittenOff {
                    vaultId: vault,
                    claimId: id,
                    amount: 9_000,
                }
                .encode_log_data(),
                ClaimChange::WrittenOff,
            ),
            (
                IZecuredRegistry::ClaimCompensated {
                    vaultId: vault,
                    claimId: id,
                }
                .encode_log_data(),
                ClaimChange::Compensated,
            ),
            (
                IZecuredRegistry::PayoutDisputed {
                    vaultId: vault,
                    claimId: id,
                    dueAt: 5_000,
                }
                .encode_log_data(),
                ClaimChange::Disputed { due_at: 5_000 },
            ),
            (
                IZecuredRegistry::PayoutUpheld {
                    vaultId: vault,
                    claimId: id,
                    payoutRef: commitment,
                }
                .encode_log_data(),
                ClaimChange::Upheld { payout_ref: commitment },
            ),
            (
                IZecuredRegistry::DisputedClaimReopened {
                    vaultId: vault,
                    claimId: id,
                }
                .encode_log_data(),
                ClaimChange::Reopened,
            ),
        ];
        for (data, change) in cases {
            let got = decode(&rpc_log(data, 12, 3), 100).unwrap().unwrap();
            assert_eq!(got.vault_id, vault);
            assert_eq!(got.event, RegistryEvent::ClaimChanged { claim_id: id, change });
        }
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

    /// K3: the reservation events are decoded for the coordinator; signer nodes do not apply them.
    #[test]
    fn reservations_are_decoded_but_not_for_signers() {
        let reserved = IZecuredRegistry::VaultReserved {
            vaultId: B256::repeat_byte(1),
            account: Address::repeat_byte(2),
            amount: 1_000_000,
            depositTag: B256::repeat_byte(3),
        };
        let got = decode(&rpc_log(reserved.encode_log_data(), 5, 1), 50).unwrap().unwrap();
        assert_eq!(
            got.event,
            RegistryEvent::Reserved {
                account: Address::repeat_byte(2),
                amount: 1_000_000,
                deposit_tag: B256::repeat_byte(3),
            }
        );
        assert!(!got.event.for_signers());
        let released = IZecuredRegistry::VaultReservationReleased {
            vaultId: B256::repeat_byte(1),
        };
        let got = decode(&rpc_log(released.encode_log_data(), 5, 2), 50).unwrap().unwrap();
        assert_eq!(got.event, RegistryEvent::ReservationReleased);
        assert!(!got.event.for_signers());
    }

    /// The verifier version registry's ids, as `versionIdOf` computes them (`abi.encode` puts a
    /// `bytes4` at the start of its word).
    #[test]
    fn version_ids_are_keccak_of_the_abi_encoded_triple() {
        use alloy::{
            primitives::{FixedBytes, U256},
            sol_types::SolValue,
        };
        let image = B256::repeat_byte(0x42);
        let selector = [0x73, 0xc4, 0x57, 0xba];
        // A uint8 enum takes a whole word, as a uint256 does.
        let encoded = (U256::from(VERSION_DEPOSIT), FixedBytes::<4>::from(selector), image).abi_encode();
        assert_eq!(version_id(VERSION_DEPOSIT, selector, image), alloy::primitives::keccak256(encoded));
        assert_ne!(version_id(VERSION_VIOLATION, selector, image), version_id(VERSION_DEPOSIT, selector, image));
    }

    #[test]
    fn unknown_logs_are_ignored() {
        let data = LogData::new_unchecked(vec![B256::repeat_byte(9)], U256::from(1).to_be_bytes_vec().into());
        assert!(decode(&rpc_log(data, 1, 0), 0).unwrap().is_none());
    }
}
