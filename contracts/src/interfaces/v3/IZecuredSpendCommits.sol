// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {SpendCommit} from "../../libraries/ZecuredEIP712.sol";
import {IZecuredSignerErrors} from "./IZecuredSignerErrors.sol";

/// @notice A note of a vault's lineage, as Base knows it: a deposit (its tag, from a pending or activated deposit or a
///         refund) or the change of a committed payout (its `changeTag`). Keyed by the note's tag (docs/TIER3.md § 2).
///         v3: a vault can hold several unspent lineage notes at once (its main lineage and one per refunded deposit);
///         each is spent by its own committed payouts.
struct LineageNote {
    bytes32 vaultId;
    uint64 value;
}

/// @notice One committed payout, keyed by its `payoutRef` (unchanged from v2).
struct SpendRecord {
    bytes32 recordHash;
    bytes32 vaultId;
    bytes32 inputTag;
    bytes32 changeTag;
    uint64 changeValue;
    uint8 claimCount;
    uint8 signerBitmap;
}

/// @notice One claim payment named by a committed payout, keyed by its `claimRef` (unchanged from v2).
/// @param expected Zatoshi the claimant must receive: the claim's amount less its share of the payout's ZIP 317 fee.
struct ClaimPayment {
    bytes32 claimId;
    bytes32 payoutRef;
    uint64 expected;
    uint8 outputIndex;
}

/// @title IZecuredSpendCommits (v3)
/// @notice Tier 3 part of the Registry (docs/TIER3.md § 4): every payout of a lineage note is committed on Base, by at
///         least 3 signers, before any signer releases a FROST share for it.
/// @dev Rules of `commitSpend(c, sigs)`: those of v2, plus:
///      - every claim has a non-zero commitment (a Refund claim needs its destination, a bond-paid claim its
///        Reimburse destination: `ClaimDestinationUnset`);
///      - the vault is not compromised (`IZecuredRegistry.VaultCompromised`): its claims are settled by the
///        compensator;
///      - the vault is not `Pending` (`VaultPending`): a pending activation's note backs a mint that has not happened,
///        and its reservation can still be released (K3); a refund opened meanwhile waits for the vault to activate,
///        convert or drop;
///      - the fee: `fee = 5,000 * max(2, claimCount + (changeValue > 0 ? 1 : 0))` zatoshi as in v2. Each Fee claim
///        (T-06) at index >= 1 bears exactly 5,000 (its own output), and `claimIds[0]` bears the rest. So a Fee claim
///        never makes the first claimant pay for the fee's output. `claimIds[0]` must cover its share
///        (`FeeBearerIsDust`), as in v2.
///      Effects: those of v2, plus `lastCommitAt[c.inputTag] = block.timestamp` (T-05's in-flight protection).
/// @notice The events and errors of `commitSpend`, which `ZecuredSpendLogic` emits on the Registry's behalf.
interface IZecuredSpendCommitsEvents is IZecuredSignerErrors {
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
    error DuplicateClaimOutput(uint8 outputIndex);
    error DuplicateClaimCommitment(bytes32 commitment);
    error ClaimSetMismatch(bytes32 inputTag);
    error ClaimNotPayable(bytes32 claimId);
    error ClaimBoundToAnotherInput(bytes32 claimId, bytes32 inputTag);
    error ClaimRefAlreadyUsed(bytes32 claimRef);
    error ConservationBroken(uint64 lineageValue, uint256 claimsTotal, uint64 changeValue);
    error InvalidChange(bytes32 changeTag, uint64 changeValue);
    error FeeBearerIsDust(bytes32 claimId, uint64 amount, uint64 fee);
    error ClaimNotCommitted(bytes32 claimId);
    error ClaimDestinationUnset(bytes32 claimId);
    error VaultPending(bytes32 vaultId);
}

interface IZecuredSpendCommits is IZecuredSpendCommitsEvents {
    /// @notice Most claims one payout may pay (8).
    function MAX_CLAIMS_PER_PAYOUT() external view returns (uint256);

    /// @notice ZIP 317's marginal fee per logical action (5,000 zatoshi): what each Fee claim bears of a payout's fee.
    function ZIP317_MARGINAL_FEE() external view returns (uint64);

    function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external;

    function lineageOf(bytes32 tag) external view returns (LineageNote memory);
    function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);
    function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);
    function claimInput(bytes32 claimId) external view returns (bytes32 inputTag);
    function inputClaimSet(bytes32 inputTag) external view returns (bytes32);
    function claimRefOf(bytes32 payoutRef, bytes32 commitment) external view returns (bytes32);
    function hashSpendCommit(SpendCommit calldata c) external view returns (bytes32);

    /// @notice T-05: when the latest commitment spending `inputTag` was recorded (0 if none). A payout committed less
    ///         than `commitGrace` ago may still be mined, so the bonds do not pay its claims meanwhile.
    function lastCommitAt(bytes32 inputTag) external view returns (uint40);

    /// @notice Vaults whose ZEC the signers still hold for Base: pending or active, or closed or cancelled with a claim
    ///         still pending. Full bond withdrawals wait for zero (S5).
    function openVaultCount() external view returns (uint256);

    /// @notice When `openVaultCount` last dropped to zero (0 if never).
    function lastVaultSettledAt() external view returns (uint64);

    /// @notice The bond contract (zero on deployments without bonds).
    function signerBond() external view returns (address);
}
