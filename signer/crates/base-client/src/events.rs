//! The contracts' ABIs (Registry, Intake, slasher, bond, verifier versions, Adapter), as of the v4
//! interface freeze, and ingestion of their events up to Base's `safe` head: the Registry's for
//! the signer nodes, every contract's for the coordinator.
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
    primitives::{Address, B256, U256},
    providers::Provider,
    rpc::types::{Filter, Log},
    sol,
    sol_types::SolEventInterface,
};
use anyhow::{Context, Result, anyhow};
use zecured_policy::ledger::{Claim, ClaimKind, ClaimState};

sol! {
    /// The Registry (contracts/src/interfaces/v3/IZecuredRegistry.sol, IZecuredSpendCommits.sol
    /// and IZecuredDisputes.sol, which the Registry serves through its fallback, with v4's changes:
    /// R-1's `confirmPayout` and `PayoutConfirmed`, section 3's `PayoutProven`, R-11's
    /// `RefundDormant`, R-16's `ChangeTagOverridden`), as far as the signer network, the CLI and
    /// the bots use it.
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
        event PayoutConfirmed(bytes32 indexed vaultId, bytes32 indexed payoutRef, bytes32[] claimIds);
        event PayoutProven(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 payoutRef);
        event ConfirmationExpired(bytes32 indexed vaultId, bytes32 indexed claimId);
        event VaultCompensated(bytes32 indexed vaultId, uint64 outstanding);
        event RefundDormant(bytes32 indexed vaultId, bytes32 indexed claimId);
        event ChangeTagOverridden(bytes32 indexed tag, bytes32 indexed oldVaultId);
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
        function confirmPayout(bytes32 vaultId, bytes32 payoutRef, bytes32[] claimIds, bytes[] sigs) external;
        function hashPayoutConfirmation(bytes32 vaultId, bytes32 payoutRef, bytes32[] claimIds) external view returns (bytes32);
        function payoutConfirmed(bytes32 payoutRef) external view returns (bool);
        function unprovenConfirmations(bytes32 vaultId) external view returns (uint256);
        function expireConfirmation(bytes32 claimId) external;
        function setReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline, bytes[] sigs) external;
        function hashReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline) external view returns (bytes32);
        function markRefundDormant(bytes32 claimId) external;
        function commitSpend(SpendCommit c, bytes[] sigs) external;
        function disputePayout(bytes32 claimId) external;
        function reopenDisputedClaim(bytes32 claimId) external;
        function lastCommitAt(bytes32 inputTag) external view returns (uint40);
        function inclusionWindow() external view returns (uint64);
        function lockedZatoshi() external view returns (uint256);
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
        function compensator() external view returns (address);
        function signerBond() external view returns (address);
        function signers() external view returns (address[5] memory);
    }
}

sol! {
    /// The Intake (contracts/src/interfaces/v3/IZecuredIntake.sol, with v4's pending refunds,
    /// R-12): deposit proofs, pending activations and refunds (K3), and the gates of minting. The
    /// image id a seal was made for is an argument, which the Intake checks against the verifier
    /// version registry (design notes T-07a).
    #[sol(rpc)]
    interface IZecuredIntake {
        /// `versionId` (v4, R-14): the deposit version whose seal proved it; once that version is
        /// no longer active the activation can only be dropped.
        #[derive(Debug, PartialEq, Eq)]
        struct PendingActivation {
            address account;
            uint64 amount;
            bytes32 depositTag;
            bytes32 anchor;
            uint256 readyHeight;
            uint256 readyTime;
            bytes32 versionId;
        }
        /// v4 (R-12): a proven deposit landing as a refund, waiting like an activation; keyed by
        /// its deposit tag. `reason` is the Intake's `RefundReason`.
        #[derive(Debug, PartialEq, Eq)]
        struct PendingRefund {
            bytes32 vaultId;
            address account;
            uint64 amount;
            uint8 reason;
            bytes32 anchor;
            uint256 readyHeight;
            uint256 readyTime;
            bytes32 versionId;
        }
        #[derive(Debug, PartialEq, Eq)]
        struct WorkTier {
            uint64 maxAmountZat;
            uint256 minWork;
        }

        event RefundPending(bytes32 indexed vaultId, bytes32 indexed depositTag, uint8 reason, uint256 readyHeight, uint256 readyTime);
        event PendingRefundDropped(bytes32 indexed vaultId, bytes32 indexed depositTag);

        function attestDepositWithProof(bytes journal, bytes seal, bytes32 imageId, bytes[] openedSigs) external;
        function finalizeActivation(bytes32 vaultId) external;
        function dropPendingActivation(bytes32 vaultId) external;
        function refundPending(bytes32 vaultId) external returns (bytes32 claimId);
        function finalizeRefund(bytes32 depositTag) external returns (bytes32 claimId);
        function dropPendingRefund(bytes32 depositTag) external;
        function pauseMinting(uint64 until, uint256 nonce, uint64 deadline, bytes[] sigs) external;
        function unpauseMinting(uint256 nonce, uint64 deadline, bytes[] sigs) external;
        function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory);
        function pendingRefund(bytes32 depositTag) external view returns (PendingRefund memory);
        function pendingRefundTotal() external view returns (uint64);
        function journalV2() external view returns (bool);
        function minRefundZat() external view returns (uint64);
        function minConfirmationWorkFor(uint64 amount) external view returns (uint256);
        function workTiers() external view returns (WorkTier[] memory);
        function mintingPaused() external view returns (bool);
        function mintBlock(uint64 amount) external view returns (uint8);
        function capRoom() external view returns (uint256);
        function maxTotalZat() external view returns (uint64);
        function pendingTotal() external view returns (uint64);
        function guardNonce() external view returns (uint256);
        function hashPauseMinting(uint64 until, uint256 nonce, uint64 deadline) external view returns (bytes32);
        function hashUnpauseMinting(uint256 nonce, uint64 deadline) external view returns (bytes32);
        function relay() external view returns (address);
        function signerBond() external view returns (address);
        function versions() external view returns (address);
    }
}

sol! {
    /// The slasher (contracts/src/interfaces/v3/IZecuredSlasher.sol, with v4's batch of inclusion
    /// proofs, section 3), as far as the signer network uses it: the compromised and frozen flags
    /// (T-04), inclusion proofs of payouts (T-05 (a), proven delivery), and the executed cases whose
    /// journals prove a payout's inclusion too (R-2). `kind` is the slasher's `ViolationKind`
    /// (values below).
    #[sol(rpc)]
    interface IZecuredSlasher {
        /// The contract's `ViolationCase`; its `ref` field is named `reference` here (a Rust keyword
        /// otherwise; the ABI is positional).
        #[derive(Debug, PartialEq, Eq)]
        struct ViolationCase {
            uint8 kind;
            bytes32 anchor;
            address beneficiary;
            bytes32 reference;
            bytes32 noteTag;
            bytes32 vaultId;
            uint8 signerBitmap;
            uint64 executableAt;
            bool executed;
            uint64 value;
        }

        event ViolationProven(bytes32 indexed caseId, uint8 kind, bytes32 indexed vaultId, uint8 signerBitmap, address beneficiary, uint64 executableAt);
        event ViolationExecuted(bytes32 indexed caseId, uint256 slashed);
        event VaultCompromised(bytes32 indexed vaultId, bytes32 indexed caseId);

        function isCompromised(bytes32 vaultId) external view returns (bool);
        function vaultFrozen(bytes32 vaultId) external view returns (bool);
        function compromisedAt(bytes32 vaultId) external view returns (uint64);
        function frozenSince(bytes32 vaultId) external view returns (uint64);
        function caseOf(bytes32 caseId) external view returns (ViolationCase memory);
        function proveInclusion(bytes journal, bytes seal, bytes32 imageId, bytes32 claimId) external;
        function proveInclusionBatch(bytes journal, bytes seal, bytes32 imageId, bytes32[] claimIds) external returns (uint256 upheld);
        function proveInclusionByCase(bytes32 caseId, bytes32[] claimIds) external returns (uint256 upheld);
        function proveViolation(uint8 kind, bytes journal, bytes seal, bytes32 imageId) external returns (bytes32 caseId);
        /// v4.1 (W-2): an F1 case follows its note to the vault that owns its tag now.
        function followNote(bytes32 caseId) external returns (bytes32 newCaseId);
        /// v4.1 (W-5): an F3 or F4 case about the claim is open.
        function claimCaseOpen(bytes32 claimId) external view returns (bool);
        /// v4.1 (W-2): open F1 cases about the note with this tag.
        function openF1CasesOf(bytes32 noteTag) external view returns (uint256);
        function slashConfirmationWork() external view returns (uint256);
        function relay() external view returns (address);
        function versions() external view returns (address);
        function compensator() external view returns (address);
    }
}

/// The slasher's `ViolationKind`: F1, a lineage note spent without a commitment.
pub const VIOLATION_UNCOMMITTED: u8 = 1;
/// The slasher's `ViolationKind`: F2, a withheld change (the payout committed, its change missing).
pub const VIOLATION_WITHHELD_CHANGE: u8 = 2;
/// The slasher's `ViolationKind`: F4, a payout whose claim reference is wrong.
pub const VIOLATION_WRONG_CLAIM_REF: u8 = 4;

sol! {
    /// The compensator (contracts/src/interfaces/v3/IZecuredCompensator.sol), as far as the
    /// coordinator follows it: R-19's burn of a compromised vault's collateral down to the LLTV,
    /// which anyone may run again when the one inside `executeSlash` failed.
    #[sol(rpc)]
    interface IZecuredCompensator {
        event CollateralBurned(bytes32 indexed vaultId, uint64 retired);

        function burnCollateral(bytes32 vaultId) external returns (uint64 retired);
    }
}

sol! {
    /// SignerBond v2 (contracts/src/interfaces/v3/ISignerBond.sol, with v4's liveness receipts,
    /// R-5): the liveness clock (T-05), the halt and the bonds.
    #[sol(rpc)]
    interface ISignerBond {
        event LivenessArmed(bytes32 indexed claimId, bytes32 commitment, uint64 payableAt);

        function armLiveness(bytes32 claimId, uint64 receivedAt, bytes signature) external;
        function hashDestinationReceived(bytes32 claimId, bytes32 commitment, uint64 receivedAt) external view returns (bytes32);
        function livenessDue(bytes32 claimId) external view returns (uint64 dueAt, uint256 amount);
        /// v4.1 (W-4): the due time from the arming alone (no price, freeze or claim case): what the
        /// planners defer by (R-10).
        function livenessDeadline(bytes32 claimId) external view returns (uint64);
        function livenessArming(bytes32 claimId) external view returns (bytes32 commitment, uint64 payableAt);
        function domainSeparator() external view returns (bytes32);
        function livenessPeriod() external view returns (uint64);
        function commitGrace() external view returns (uint64);
        function halted() external view returns (bool);
        function fullyBonded() external view returns (bool);
        function effectiveBond() external view returns (uint256);
    }
}

sol! {
    /// The verifier version registry (design notes T-07a, with v4's `Swap` kind): the guest image
    /// ids, with their RISC Zero router selector and (deposits) consensus branch ids, that Base
    /// accepts proofs of.
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

        function scheduleVersion(uint8 kind, bytes4 selector, bytes32 imageId, uint32[4] branches, uint256 nonce, uint64 deadline, bytes[] sigs) external returns (bytes32 versionId);
        function removeVersion(bytes32 versionId, uint256 nonce, uint64 deadline, bytes[] sigs) external;
        function versionIds(uint8 kind) external view returns (bytes32[] memory);
        function version(bytes32 versionId) external view returns (Version memory);
        function isActive(bytes32 versionId) external view returns (bool);
        /// v4 (R-22): active, and the router still verifies its selector's seals.
        function isWorking(bytes32 versionId) external view returns (bool);
        function branchAllowed(bytes32 versionId, uint32 branchId) external view returns (bool);
        function nonce() external view returns (uint256);
        function hashScheduleVersion(uint8 kind, bytes4 selector, bytes32 imageId, uint32[4] branches, uint256 nonce, uint64 deadline) external view returns (bytes32);
        function hashRemoveVersion(bytes32 versionId, uint256 nonce, uint64 deadline) external view returns (bytes32);
    }
}

sol! {
    /// The SettlementAdapter's events the coordinator follows (v4 pre-liquidation, section 4).
    interface ISettlementAdapterEvents {
        event PreLiquidated(bytes32 indexed vaultId, address indexed account, address indexed liquidator, uint256 repaidAssets, uint64 seized);
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
        function networkId() external view returns (bytes32);
    }
}

/// `VersionKind.Deposit`, `VersionKind.Violation` and (v4) `VersionKind.Swap` of the verifier
/// version registry (T-07a).
pub const VERSION_DEPOSIT: u8 = 0;
pub const VERSION_VIOLATION: u8 = 1;
pub const VERSION_SWAP: u8 = 2;

/// `MintBlock.CapExceeded` of the Intake (`mintBlock(amount)`): the coverage cap has no room; from
/// v4 a proof that would activate a new vault reverts instead of landing as a refund (R-11.1).
pub const MINT_BLOCK_CAP_EXCEEDED: u8 = 4;

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
    /// `PayoutUpheld`: a committed payout of its input (`payout_ref`) was proven mined: paid, and
    /// final (v4: `paidAt` cleared).
    Upheld { payout_ref: B256 },
    /// `DisputedClaimReopened`: nobody proved its payout in time: pending again, still bound to its
    /// input.
    Reopened,
    /// v4 `PayoutProven` (proven delivery): a committed payout of its input (`payout_ref`) was
    /// proven mined, outside a dispute: paid and final (`paidAt == 0`), never disputable again.
    Proven { payout_ref: B256 },
    /// v4 `ConfirmationExpired` (R-3): nobody disputed the claim's confirmation within its window:
    /// paid and final (`paidAt == 0`), like a proven claim, though no payout was proven.
    Expired,
    /// v4 `RefundDormant` (R-11): a Refund claim without a destination for `refundDormancy` left the
    /// coverage cap until its account gives one. Nothing changes in the books.
    Dormant,
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
    /// Claims were confirmed as paid on Base by three signers' word, for the payout `payout_ref`
    /// (R-1); disputable for `disputeWindow` until proven.
    PayoutConfirmed { payout_ref: B256, claims: Vec<B256> },
    /// R-16: a deposit took the tag `tag` that a commitment of this vault (the event's vault, the
    /// old owner of the tag) had registered as a change tag: another vault's deposit, or a refund to
    /// this same vault (R-12). An honest commitment's change can never carry a deposit's tag: the
    /// commitment's signers registered a false record.
    ChangeTagOverridden { tag: B256 },
    /// Tier 3: a payout of `input_tag` was committed (`payout_ref`, for `claims`). The coordinator
    /// follows it to prove the payout's delivery (section 3); signer nodes read the records they
    /// signed straight from the Registry.
    SpendCommitted {
        payout_ref: B256,
        input_tag: B256,
        claims: Vec<B256>,
        /// W-1: each claim's reference as committed (`keccak256(salt || txid)` if honest).
        claim_refs: Vec<B256>,
    },
    /// The Intake (R-12): a refund proof is pending until the relay has moved on.
    RefundPending {
        deposit_tag: B256,
        reason: u8,
        ready_height: u64,
        ready_time: u64,
    },
    /// The Intake (R-12): a pending refund was dropped (its anchor left the best chain, or its
    /// version was removed); the deposit can be proven again.
    PendingRefundDropped { deposit_tag: B256 },
    /// The bond (R-5): a claim's liveness clock was armed with a signer's receipt. The event names
    /// no vault: [`VaultEvent::vault_id`] is zero, the claim says which.
    LivenessArmed {
        claim_id: B256,
        commitment: B256,
        payable_at: u64,
    },
    /// The Adapter (section 4): a pre-liquidation seized `seized` zatoshi for `repaid_assets` of the
    /// debt; the Registry's `LiquidationRelease` creates the claim.
    PreLiquidated { repaid_assets: U256, seized: u64 },
    /// The slasher: case `case_id` was executed. `ViolationExecuted` names neither the case's vault
    /// nor its kind: as decoded, [`VaultEvent::vault_id`] is zero, `kind` 0 and `payout_ref` zero, and
    /// the coordinator reads the case (`caseOf`) to fill them in. An executed F2 or F4 case proves
    /// its committed payout `payout_ref` is in a block (R-2: `proveInclusionByCase`).
    CaseExecuted { case_id: B256, kind: u8, payout_ref: B256 },
    /// The slasher (v4.1, W-2): case `case_id` of kind `kind` was opened on the event's vault (proven,
    /// or moved there with its note). The coordinator keeps its F1 cases, to move them with their
    /// note when a deposit takes the note's tag over (`followNote`).
    CaseProven { case_id: B256, kind: u8 },
    /// The slasher: an executed F1 or F2 case compromised the vault (T-04): the compensator settles
    /// its claims, and burns its collateral down to the LLTV (R-19).
    VaultCompromised { case_id: B256 },
    /// The compensator (R-19): `retired` zatoshi of the compromised vault's collateral were burned.
    CollateralBurned { retired: u64 },
    /// The Registry (R-20): the compensator settled the compromised vault while it stays active; it
    /// no longer counts as open or locked.
    VaultCompensated { outstanding: u64 },
}

impl RegistryEvent {
    /// Whether signer nodes apply it: the reservation events, the Tier 3 commitments and the other
    /// contracts' events matter to the coordinator only.
    pub fn for_signers(&self) -> bool {
        !matches!(
            self,
            RegistryEvent::Reserved { .. }
                | RegistryEvent::ReservationReleased
                | RegistryEvent::SpendCommitted { .. }
                | RegistryEvent::RefundPending { .. }
                | RegistryEvent::PendingRefundDropped { .. }
                | RegistryEvent::LivenessArmed { .. }
                | RegistryEvent::PreLiquidated { .. }
                | RegistryEvent::CaseExecuted { .. }
                | RegistryEvent::CaseProven { .. }
                | RegistryEvent::VaultCompromised { .. }
                | RegistryEvent::CollateralBurned { .. }
                | RegistryEvent::VaultCompensated { .. }
        )
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

/// The contracts whose events the coordinator follows (a zero address: none on this deployment).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contracts {
    pub registry: Address,
    pub intake: Address,
    pub bond: Address,
    pub adapter: Address,
    pub slasher: Address,
    pub compensator: Address,
}

impl Contracts {
    fn addresses(&self) -> Vec<Address> {
        [
            self.registry,
            self.intake,
            self.bond,
            self.adapter,
            self.slasher,
            self.compensator,
        ]
        .into_iter()
        .filter(|a| *a != Address::ZERO)
        .collect()
    }
}

/// The Registry's events in blocks `from..=to` that signer nodes apply ([`RegistryEvent::for_signers`]),
/// in chain order. A Fee claim's commitment, which its event does not carry, is the Registry's
/// `feeCommitment` (the same for every vault, T-06).
pub async fn fetch<P: Provider>(provider: &P, registry: Address, from: u64, to: u64) -> Result<Vec<VaultEvent>> {
    let contracts = Contracts {
        registry,
        ..Contracts::default()
    };
    fetch_events(provider, &contracts, from, to, true).await
}

/// Every event of the deployment's contracts in blocks `from..=to`, in chain order: the Registry's
/// (those signer nodes ignore included), the Intake's pending refunds, the bond's liveness arming
/// and the Adapter's pre-liquidations (the coordinator's view).
pub async fn fetch_deployment<P: Provider>(
    provider: &P,
    contracts: &Contracts,
    from: u64,
    to: u64,
) -> Result<Vec<VaultEvent>> {
    fetch_events(provider, contracts, from, to, false).await
}

async fn fetch_events<P: Provider>(
    provider: &P,
    contracts: &Contracts,
    from: u64,
    to: u64,
    signers_only: bool,
) -> Result<Vec<VaultEvent>> {
    if from > to {
        return Ok(vec![]);
    }
    let addresses = contracts.addresses();
    let logs = get_logs_chunked(from, to, |a, b| {
        let addresses = addresses.clone();
        async move {
            let filter = Filter::new().address(addresses).from_block(a).to_block(b);
            provider.get_logs(&filter).await.map_err(anyhow::Error::from)
        }
    })
    .await?;
    let registry = contracts.registry;
    let mut timestamps: HashMap<u64, u64> = HashMap::new();
    let mut fee_commitment: Option<B256> = None;
    let mut out = Vec::with_capacity(logs.len());
    for log in logs.into_iter().filter(|l| !l.removed) {
        // Logs nobody here uses (and, for the nodes, those they do not apply) cost no block lookup.
        let Some(mut ev) = decode_from(&log, 0, contracts)? else {
            continue;
        };
        if signers_only && !ev.event.for_signers() {
            continue;
        }
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

/// Decodes one log of a deployment contract by its address (a log of any other address is
/// ignored); `created_at` is its block's timestamp.
pub fn decode_from(log: &Log, created_at: u64, contracts: &Contracts) -> Result<Option<VaultEvent>> {
    let address = log.inner.address;
    if address == Address::ZERO {
        return Ok(None);
    }
    if address == contracts.registry {
        decode(log, created_at)
    } else if address == contracts.intake {
        decode_intake(log, created_at)
    } else if address == contracts.bond {
        decode_bond(log, created_at)
    } else if address == contracts.adapter {
        decode_adapter(log, created_at)
    } else if address == contracts.slasher {
        decode_slasher(log, created_at)
    } else if address == contracts.compensator {
        decode_compensator(log, created_at)
    } else {
        Ok(None)
    }
}

/// The block number and log index of a log.
fn position(log: &Log) -> Result<(u64, u64)> {
    Ok((log.block_number.context("log without block number")?, log.log_index.context("log without index")?))
}

fn vault_event(log: &Log, created_at: u64, vault_id: B256, event: RegistryEvent) -> Result<Option<VaultEvent>> {
    let (block, log_index) = position(log)?;
    Ok(Some(VaultEvent {
        vault_id,
        block,
        log_index,
        block_hash: log.block_hash,
        timestamp: created_at,
        event,
    }))
}

/// Decodes one Intake log (R-12's pending refunds); other Intake events are ignored.
pub fn decode_intake(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use IZecuredIntake::IZecuredIntakeEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let (vault_id, event) = match decoded.data {
        E::RefundPending(e) => (
            e.vaultId,
            RegistryEvent::RefundPending {
                deposit_tag: e.depositTag,
                reason: e.reason,
                ready_height: u64::try_from(e.readyHeight).unwrap_or(u64::MAX),
                ready_time: u64::try_from(e.readyTime).unwrap_or(u64::MAX),
            },
        ),
        E::PendingRefundDropped(e) => (
            e.vaultId,
            RegistryEvent::PendingRefundDropped {
                deposit_tag: e.depositTag,
            },
        ),
    };
    vault_event(log, created_at, vault_id, event)
}

/// Decodes one bond log (R-5's `LivenessArmed`, which names a claim, no vault).
pub fn decode_bond(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use ISignerBond::ISignerBondEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let E::LivenessArmed(e) = decoded.data;
    let event = RegistryEvent::LivenessArmed {
        claim_id: e.claimId,
        commitment: e.commitment,
        payable_at: e.payableAt,
    };
    vault_event(log, created_at, B256::ZERO, event)
}

/// Decodes one Adapter log (section 4's `PreLiquidated`).
pub fn decode_adapter(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use ISettlementAdapterEvents::ISettlementAdapterEventsEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let E::PreLiquidated(e) = decoded.data;
    let event = RegistryEvent::PreLiquidated {
        repaid_assets: e.repaidAssets,
        seized: e.seized,
    };
    vault_event(log, created_at, e.vaultId, event)
}

/// Decodes one slasher log: a case opened on a vault (W-2), an executed case (vault, kind and payout
/// left for the coordinator to read, see [`RegistryEvent::CaseExecuted`]) or a compromised vault;
/// other slasher events are ignored.
pub fn decode_slasher(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use IZecuredSlasher::IZecuredSlasherEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let (vault_id, event) = match decoded.data {
        E::ViolationExecuted(e) => (
            B256::ZERO,
            RegistryEvent::CaseExecuted {
                case_id: e.caseId,
                kind: 0,
                payout_ref: B256::ZERO,
            },
        ),
        E::VaultCompromised(e) => (e.vaultId, RegistryEvent::VaultCompromised { case_id: e.caseId }),
        E::ViolationProven(e) => (
            e.vaultId,
            RegistryEvent::CaseProven {
                case_id: e.caseId,
                kind: e.kind,
            },
        ),
    };
    vault_event(log, created_at, vault_id, event)
}

/// Decodes one compensator log (R-19's `CollateralBurned`); other compensator events are ignored.
pub fn decode_compensator(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use IZecuredCompensator::IZecuredCompensatorEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let E::CollateralBurned(e) = decoded.data;
    vault_event(log, created_at, e.vaultId, RegistryEvent::CollateralBurned { retired: e.retired })
}

/// Decodes one Registry log; `created_at` is its block's timestamp. Unknown logs are ignored. A Fee
/// claim's commitment is left zero ([`fetch`] fills it in).
pub fn decode(log: &Log, created_at: u64) -> Result<Option<VaultEvent>> {
    use IZecuredRegistry::IZecuredRegistryEvents as E;
    let Ok(decoded) = E::decode_log(&log.inner) else {
        return Ok(None);
    };
    let (block, log_index) = position(log)?;
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
        proven: false,
        confirmed_at: 0,
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
        E::PayoutProven(e) => (
            e.vaultId,
            changed(
                e.claimId,
                ClaimChange::Proven {
                    payout_ref: e.payoutRef,
                },
            ),
        ),
        E::ConfirmationExpired(e) => (e.vaultId, changed(e.claimId, ClaimChange::Expired)),
        E::VaultCompensated(e) => (
            e.vaultId,
            RegistryEvent::VaultCompensated {
                outstanding: e.outstanding,
            },
        ),
        E::RefundDormant(e) => (e.vaultId, changed(e.claimId, ClaimChange::Dormant)),
        E::PayoutConfirmed(e) => (
            e.vaultId,
            RegistryEvent::PayoutConfirmed {
                payout_ref: e.payoutRef,
                claims: e.claimIds,
            },
        ),
        E::ChangeTagOverridden(e) => (e.oldVaultId, RegistryEvent::ChangeTagOverridden { tag: e.tag }),
        E::SpendCommitted(e) => (
            e.vaultId,
            RegistryEvent::SpendCommitted {
                payout_ref: e.payoutRef,
                input_tag: e.inputTag,
                claims: e.claimIds,
                claim_refs: e.claimRefs,
            },
        ),
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
            payoutRef: B256::repeat_byte(3),
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
            (
                IZecuredRegistry::PayoutProven {
                    vaultId: vault,
                    claimId: id,
                    payoutRef: commitment,
                }
                .encode_log_data(),
                ClaimChange::Proven { payout_ref: commitment },
            ),
            (
                IZecuredRegistry::RefundDormant {
                    vaultId: vault,
                    claimId: id,
                }
                .encode_log_data(),
                ClaimChange::Dormant,
            ),
        ];
        for (data, change) in cases {
            let got = decode(&rpc_log(data, 12, 3), 100).unwrap().unwrap();
            assert_eq!(got.vault_id, vault);
            assert_eq!(got.event, RegistryEvent::ClaimChanged { claim_id: id, change });
            assert!(got.event.for_signers());
        }
    }

    /// v4's Registry events: R-1's confirmation names its payout, R-16's override names the old
    /// owner of the tag as the event's vault, and Tier 3 commitments are decoded for the coordinator.
    #[test]
    fn v4_registry_events_are_decoded() {
        let (vault, payout) = (B256::repeat_byte(6), B256::repeat_byte(0xaa));
        let ids = vec![B256::repeat_byte(7), B256::repeat_byte(8)];
        let got = decode(
            &rpc_log(
                IZecuredRegistry::PayoutConfirmed {
                    vaultId: vault,
                    payoutRef: payout,
                    claimIds: ids.clone(),
                }
                .encode_log_data(),
                12,
                3,
            ),
            100,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            got.event,
            RegistryEvent::PayoutConfirmed {
                payout_ref: payout,
                claims: ids.clone()
            }
        );
        assert!(got.event.for_signers());

        let old = B256::repeat_byte(9);
        let got = decode(
            &rpc_log(
                IZecuredRegistry::ChangeTagOverridden {
                    tag: B256::repeat_byte(0x7a),
                    oldVaultId: old,
                }
                .encode_log_data(),
                12,
                4,
            ),
            100,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got.vault_id, old);
        assert_eq!(
            got.event,
            RegistryEvent::ChangeTagOverridden {
                tag: B256::repeat_byte(0x7a)
            }
        );
        assert!(got.event.for_signers());

        let committed = IZecuredRegistry::SpendCommitted {
            vaultId: vault,
            payoutRef: payout,
            inputTag: B256::repeat_byte(0x66),
            claimIds: ids.clone(),
            claimRefs: vec![B256::repeat_byte(1), B256::repeat_byte(2)],
            changeTag: B256::repeat_byte(0xbb),
            changeValue: 1_000,
            signerBitmap: 7,
        };
        let got = decode(&rpc_log(committed.encode_log_data(), 12, 5), 100).unwrap().unwrap();
        assert_eq!(
            got.event,
            RegistryEvent::SpendCommitted {
                payout_ref: payout,
                input_tag: B256::repeat_byte(0x66),
                claims: ids,
                claim_refs: vec![B256::repeat_byte(1), B256::repeat_byte(2)],
            }
        );
        assert!(!got.event.for_signers(), "the coordinator's only");
    }

    /// The other contracts' events, by the address that emitted them: the Intake's pending refunds
    /// (R-12), the bond's arming (R-5, which names no vault) and the Adapter's pre-liquidations.
    #[test]
    fn deployment_events_are_decoded_by_their_contract() {
        let contracts = Contracts {
            registry: Address::repeat_byte(0xaa),
            intake: Address::repeat_byte(0xa1),
            bond: Address::repeat_byte(0xa2),
            adapter: Address::repeat_byte(0xa3),
            slasher: Address::repeat_byte(0xa4),
            compensator: Address::repeat_byte(0xa5),
        };
        let at = |data: LogData, address: Address| {
            let mut log = rpc_log(data, 20, 1);
            log.inner.address = address;
            decode_from(&log, 300, &contracts).unwrap()
        };
        let (vault, tag) = (B256::repeat_byte(6), B256::repeat_byte(0x7a));
        let pending = IZecuredIntake::RefundPending {
            vaultId: vault,
            depositTag: tag,
            reason: 5,
            readyHeight: U256::from(4_500_000),
            readyTime: U256::from(1_800_000_000),
        };
        let got = at(pending.encode_log_data(), contracts.intake).unwrap();
        assert_eq!(got.vault_id, vault);
        assert_eq!(
            got.event,
            RegistryEvent::RefundPending {
                deposit_tag: tag,
                reason: 5,
                ready_height: 4_500_000,
                ready_time: 1_800_000_000
            }
        );
        assert!(!got.event.for_signers());
        let dropped = IZecuredIntake::PendingRefundDropped {
            vaultId: vault,
            depositTag: tag,
        };
        assert_eq!(
            at(dropped.encode_log_data(), contracts.intake).unwrap().event,
            RegistryEvent::PendingRefundDropped { deposit_tag: tag }
        );
        let armed = ISignerBond::LivenessArmed {
            claimId: B256::repeat_byte(7),
            commitment: B256::repeat_byte(8),
            payableAt: 1_700_000_000,
        };
        let got = at(armed.encode_log_data(), contracts.bond).unwrap();
        assert_eq!(got.vault_id, B256::ZERO, "the event names a claim, not a vault");
        assert_eq!(
            got.event,
            RegistryEvent::LivenessArmed {
                claim_id: B256::repeat_byte(7),
                commitment: B256::repeat_byte(8),
                payable_at: 1_700_000_000
            }
        );
        let pre = ISettlementAdapterEvents::PreLiquidated {
            vaultId: vault,
            account: Address::repeat_byte(1),
            liquidator: Address::repeat_byte(2),
            repaidAssets: U256::from(1_500_000),
            seized: 123_456,
        };
        let got = at(pre.encode_log_data(), contracts.adapter).unwrap();
        assert_eq!(
            (got.vault_id, got.event),
            (
                vault,
                RegistryEvent::PreLiquidated {
                    repaid_assets: U256::from(1_500_000),
                    seized: 123_456
                }
            )
        );
        // The same log from an address that is not the deployment's: ignored.
        assert!(at(pre.encode_log_data(), Address::repeat_byte(0xee)).is_none());
        assert!(at(armed.encode_log_data(), contracts.intake).is_none(), "a bond event from the Intake");
        // The slasher: an executed case names no vault (the coordinator reads the case), a
        // compromise does; the compensator's burn and the Registry's R-20 settlement.
        let case = B256::repeat_byte(0xca);
        let executed = IZecuredSlasher::ViolationExecuted {
            caseId: case,
            slashed: U256::from(1),
        };
        let got = at(executed.encode_log_data(), contracts.slasher).unwrap();
        assert_eq!(
            (got.vault_id, got.event.clone()),
            (
                B256::ZERO,
                RegistryEvent::CaseExecuted {
                    case_id: case,
                    kind: 0,
                    payout_ref: B256::ZERO
                }
            )
        );
        assert!(!got.event.for_signers());
        let compromised = IZecuredSlasher::VaultCompromised {
            vaultId: vault,
            caseId: case,
        };
        let got = at(compromised.encode_log_data(), contracts.slasher).unwrap();
        assert_eq!((got.vault_id, got.event), (vault, RegistryEvent::VaultCompromised { case_id: case }));
        let burned = IZecuredCompensator::CollateralBurned {
            vaultId: vault,
            retired: 400_000,
        };
        let got = at(burned.encode_log_data(), contracts.compensator).unwrap();
        assert_eq!((got.vault_id, got.event), (vault, RegistryEvent::CollateralBurned { retired: 400_000 }));
        let compensated = IZecuredRegistry::VaultCompensated {
            vaultId: vault,
            outstanding: 600_000,
        };
        let got = at(compensated.encode_log_data(), contracts.registry).unwrap();
        assert_eq!(
            (got.vault_id, got.event.clone()),
            (vault, RegistryEvent::VaultCompensated { outstanding: 600_000 })
        );
        assert!(!got.event.for_signers(), "the coordinator's only");
        assert!(at(burned.encode_log_data(), contracts.slasher).is_none(), "a compensator event from the slasher");
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
