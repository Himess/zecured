// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {SpendCommit} from "../libraries/ZecuredEIP712.sol";
import {ISignerBond} from "./ISignerBond.sol";

/// @notice A note of a vault's conforming lineage, as Base knows it: the activated deposit (its `depositTag`) or the
///         change of a committed payout (its `changeTag`). Keyed by the note's tag (docs/TIER3.md § 2).
struct LineageNote {
    bytes32 vaultId;
    uint64 value;
}

/// @notice One committed payout, keyed by its `payoutRef`.
/// @param recordHash EIP-712 struct hash of the SpendCommit: a node compares it with the record it signed before it
///        releases its shares (T3-4).
/// @param signerBitmap Bit i set: the signature of signer i (the Registry's sorted signer set) was used. These signers
///        answer for the record (F2, F3).
struct SpendRecord {
    bytes32 recordHash;
    bytes32 vaultId;
    bytes32 inputTag;
    bytes32 changeTag;
    uint64 changeValue;
    uint8 claimCount;
    uint8 signerBitmap;
}

/// @notice One claim payment named by a committed payout, keyed by its `claimRef` (F3).
/// @param expected Zatoshi the claimant must receive in one output: the claim amount, less the payout's ZIP 317 fee
///        for the fee-bearing claim (`claimIds[0]`).
struct ClaimPayment {
    bytes32 claimId;
    bytes32 payoutRef;
    uint64 expected;
}

/// @title IZecuredSpendCommits
/// @notice Tier 3 part of the Registry (Deploy 2; docs/TIER3.md § 4): every payout of a lineage note is committed on
///         Base, by at least 3 signers, before any signer releases a FROST share for it. Interface locked 2026-10-12;
///         implemented by ZecuredRegistry v2.
/// @dev Rules of `commitSpend(c, sigs)` (anyone may send it; nodes resubmit after a Base reorg, T3-4):
///      - at least 3 valid signatures of distinct signers over `hashSpendCommit(c)`, sorted by signer (I6);
///      - `c.payoutRef` was never committed;
///      - `lineageOf(c.inputTag).vaultId == c.vaultId`;
///      - 1 to MAX_CLAIMS_PER_PAYOUT claims, no duplicates, each of `c.vaultId`, `Pending`, and bound to no other
///        input: `claimInput(id)` is zero or `c.inputTag` (the first commitment binds it, claim-reuse fix);
///      - `c.claimRefs.length == c.claimIds.length`, every claim ref unused;
///      - conservation: `lineageOf(c.inputTag).value == sum of claim amounts + c.changeValue`;
///      - `c.changeTag == 0` iff `c.changeValue == 0`; a non-zero change tag is not yet a lineage note;
///      - the fee-bearing claim covers the fee: `amount(claimIds[0]) > fee`, where
///        `fee = 5,000 * max(2, claimCount + (changeValue > 0 ? 1 : 0))` zatoshi (ZIP 317 for one lineage input, as
///        `zecured_policy::ledger::plan` charges it).
///      Effects: records the SpendRecord and each ClaimPayment, binds each claim to `c.inputTag`, adds the change as a
///      lineage note and emits SpendCommitted. Payouts keep working after a slash (users must be able to exit).
///      Elsewhere in v2: a deposit note becomes a lineage note as soon as its activation is pending (a spend during the
///      K3 window is F1) and stops being one if `dropPendingActivation` drops it; `confirmPayout` requires every
///      confirmed claim to have been committed.
interface IZecuredSpendCommits {
    event SpendCommitted(
        bytes32 indexed vaultId,
        bytes32 indexed payoutRef,
        bytes32 inputTag,
        bytes32[] claimIds,
        bytes32[] claimRefs,
        bytes32 changeTag,
        uint64 changeValue,
        uint8 signerBitmap
    );

    error NotALineageNote(bytes32 vaultId, bytes32 inputTag);
    error ZeroPayoutRef();
    error PayoutAlreadyCommitted(bytes32 payoutRef);
    error InvalidClaimCount(uint256 claims, uint256 claimRefs);
    error DuplicateClaim(bytes32 claimId);
    error ClaimNotPayable(bytes32 claimId);
    error ClaimBoundToAnotherInput(bytes32 claimId, bytes32 inputTag);
    error ClaimRefAlreadyUsed(bytes32 claimRef);
    error ConservationBroken(uint64 lineageValue, uint256 claimsTotal, uint64 changeValue);
    error InvalidChange(bytes32 changeTag, uint64 changeValue);
    error FeeBearerIsDust(bytes32 claimId, uint64 amount, uint64 fee);
    error ClaimNotCommitted(bytes32 claimId);
    error MintingHalted();
    error SignersNotBonded();
    error ViolationCaseOpen(bytes32 vaultId);

    /// @notice Most claims one payout may pay (8; the signers' plans pay one claim or a close pair).
    function MAX_CLAIMS_PER_PAYOUT() external view returns (uint256);

    /// @notice Records a payout commitment (rules above).
    function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external;

    /// @notice The lineage note with this tag (zero vault id if none).
    function lineageOf(bytes32 tag) external view returns (LineageNote memory);

    /// @notice The committed payout with this reference (zero vault id if none).
    function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);

    /// @notice The committed claim payment with this claim reference (zero claim id if none).
    function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);

    /// @notice The lineage note a claim is bound to (zero until its first commitment).
    function claimInput(bytes32 claimId) external view returns (bytes32 inputTag);

    /// @notice EIP-712 digest of a SpendCommit under the Registry domain.
    function hashSpendCommit(SpendCommit calldata c) external view returns (bytes32);

    /// @notice Vaults whose ZEC the signers still hold for Base: pending or active, or closed with a claim not yet
    ///         confirmed paid. Bonds stay locked while it is not zero (S5).
    function openVaultCount() external view returns (uint256);

    /// @notice When `openVaultCount` last dropped to zero (0 if never).
    function lastVaultSettledAt() external view returns (uint64);

    /// @notice The bond contract: new proofs and activations need it `fullyBonded()` and not `halted()`; zero on
    ///         deployments without bonds (local fork, tests).
    function signerBond() external view returns (ISignerBond);
}
