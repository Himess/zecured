// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Events and errors of the dispute path.
interface IZecuredDisputesEvents {
    /// @notice A claimant disputes the confirmation of its claim; the signers have until `dueAt` to prove the payout.
    event PayoutDisputed(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 dueAt);
    /// @notice The committed payout `payoutRef`, which pays the disputed claim, is proven to be in a Zcash block.
    event PayoutUpheld(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 payoutRef);
    /// @notice Nobody proved the disputed claim's payout in time: the claim is payable again.
    event DisputedClaimReopened(bytes32 indexed vaultId, bytes32 indexed claimId);

    error NotDisputable(bytes32 claimId);
    error NotClaimant(bytes32 claimId, address caller);
    error DisputeWindowClosed(bytes32 claimId, uint64 closedAt);
    error DisputeVaultCompromised(bytes32 vaultId);
    error NotDisputed(bytes32 claimId);
    error InclusionWindowOpen(bytes32 claimId, uint64 dueAt);
    error PayoutNotOfClaim(bytes32 claimId, bytes32 payoutRef);
    error OnlySlasher(address caller);
}

/// @title IZecuredDisputes
/// @notice The dispute path (design notes T-05 (a)). Three signers can confirm a payout that never reached Zcash
///         (`confirmPayout` is their word, not a proof), which would leave its claimant with nothing to prove: no
///         transaction for F3, and a `Paid` claim the liveness penalty does not cover. So the claimant of a confirmed
///         claim may dispute the confirmation for `disputeWindow`. The claim is then unpaid again, and the signers have
///         `inclusionWindow` to prove, with the violation guest's `Included` kind (`ZecuredSlasher.proveInclusion`), that
///         a committed payout of the claim's input is in a Zcash block; every commitment of an input pays the same
///         claims (RT-4), so any of them pays this one. If nobody does, anyone reopens the claim: `Pending`, payable by a
///         new payout of its input, and from the bonds on its original liveness schedule (T-05), which a false
///         confirmation never restarts.
/// @dev Served by the Registry: its fallback runs `ZecuredSpendLogic`'s code on its storage (T-11), so call these on
///      the Registry's address. A dispute keeps its vault open (`openVaultCount`) and counts the claim among the
///      vault's unpaid claims and in `lockedZatoshi` until it is resolved. The deployment keeps
///      `disputeWindow + inclusionWindow <= challengeWindow + unbondingPeriod`, so a bond that started to leave after
///      the vault settled is still slashable when a reopened claim falls due.
interface IZecuredDisputes is IZecuredDisputesEvents {
    /// @notice How long after its confirmation a claim can be disputed.
    function disputeWindow() external view returns (uint64);

    /// @notice How long the signers have to prove a disputed payout.
    function inclusionWindow() external view returns (uint64);

    /// @notice The claim's claimant only (`claimDetails(claimId).claimant`; there is none for Fee and Retired claims).
    ///         Disputes a confirmed claim within `disputeWindow` of its confirmation: not one the bonds paid (its
    ///         claimant has its USDC), not one already upheld, not on a compromised vault (the compensator has settled
    ///         its claims).
    function disputePayout(bytes32 claimId) external;

    /// @notice The slasher only, after it verified an `Included` proof of `payoutRef`. Upholds the confirmation of a
    ///         disputed claim if `payoutRef` is a committed payout of the claim's input; the claim is `Paid` for good.
    function upholdPayout(bytes32 claimId, bytes32 payoutRef) external;

    /// @notice Anyone, once `inclusionWindow` has passed without a proof: the disputed claim is `Pending` again.
    function reopenDisputedClaim(bytes32 claimId) external;

    /// @return paidAt When the claim was confirmed paid (zero if it is not, or its confirmation was upheld).
    /// @return dueAt While it is disputed: the end of the inclusion window (zero otherwise).
    function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt);
}
