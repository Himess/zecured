// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Events and errors of the dispute path.
interface IZecuredDisputesEvents {
    /// @notice A claimant disputes the confirmation of its claim; the signers have until `dueAt` to prove the payout.
    event PayoutDisputed(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 dueAt);
    /// @notice The committed payout `payoutRef`, which pays the disputed (or reopened) claim, is proven to be in a Zcash
    ///         block.
    event PayoutUpheld(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 payoutRef);
    /// @notice Nobody proved the disputed claim's payout in time: the claim is payable again.
    event DisputedClaimReopened(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice v4 (proven delivery): the committed payout `payoutRef` of a claim nobody disputed (pending, or confirmed
    ///         by signatures) is proven to be in a Zcash block: the claim is paid for good.
    event PayoutProven(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 payoutRef);

    error NotDisputable(bytes32 claimId);
    error NotClaimant(bytes32 claimId, address caller);
    error DisputeWindowClosed(bytes32 claimId, uint64 closedAt);
    error DisputeVaultCompromised(bytes32 vaultId);
    error NotDisputed(bytes32 claimId);
    error InclusionWindowOpen(bytes32 claimId, uint64 dueAt);
    error PayoutNotOfClaim(bytes32 claimId, bytes32 payoutRef);
    error OnlySlasher(address caller);
    /// @notice The claim is already paid for good (proven, or its confirmation final): nothing left to prove.
    error AlreadyFinal(bytes32 claimId);
    /// @notice The claim is not paid by a payout any more (unknown, written off, or compensated).
    error NotUpholdable(bytes32 claimId);
}

/// @title IZecuredDisputes
/// @notice The dispute path (design notes T-05 (a)) and, since v4, every proof of a payout. Three signers can confirm a
///         payout that never reached Zcash (`confirmPayout` is their word, not a proof), which would leave its claimant
///         with nothing to prove: no transaction for F3, and a `Paid` claim the liveness penalty does not cover. So the
///         claimant of a confirmed claim may dispute the confirmation for `disputeWindow`. The claim is then unpaid
///         again, and the signers have `inclusionWindow` to prove, with the violation guest's `Included` kind
///         (`ZecuredSlasher.proveInclusion`), that a committed payout of the claim's input is in a Zcash block; every
///         commitment of an input pays the same claims (RT-4), so any of them pays this one. If nobody does, anyone
///         reopens the claim: `Pending`, payable by a new payout of its input, which names the same claims, confirmed
///         ones included (D57), and from the bonds on its original liveness schedule (T-05), which a false confirmation
///         never restarts. A proof that comes later still upholds the reopened claim, so that a payout which did land
///         never keeps its vault open for good.
///         v4 (proven delivery): a proof makes any claim's payment final, not only a disputed one's: a committed
///         `Pending` claim (also on a frozen or compromised vault, until the compensator takes it over), a claim
///         confirmed by signatures (its dispute window then closes), a disputed claim, a reopened claim. A final claim
///         is never disputed, confirmed or proven again.
/// @dev Served by the Registry: its fallback runs `ZecuredSpendLogic`'s code on its storage (T-11), so call these on
///      the Registry's address. A dispute keeps its vault open (`openVaultCount`) and counts the claim among the
///      vault's unpaid claims and in `lockedZatoshi` until it is resolved. The deployment keeps
///      `disputeWindow + inclusionWindow <= challengeWindow + unbondingPeriod`, so a bond that started to leave after
///      the vault settled is still slashable when a reopened claim falls due.
interface IZecuredDisputes is IZecuredDisputesEvents {
    /// @notice How long after its confirmation a claim can be disputed.
    function disputeWindow() external view returns (uint64);

    /// @notice How long the signers have to prove a disputed payout; also how long after a vault was compromised the
    ///         compensator waits before it compensates a committed claim whose payout may have landed (R-2).
    function inclusionWindow() external view returns (uint64);

    /// @notice The claim's claimant only (`claimDetails(claimId).claimant`; there is none for Fee and Retired claims).
    ///         Disputes a confirmed claim within `disputeWindow` of its confirmation: not one the bonds paid (its
    ///         claimant has its USDC), not one already final (proven), not on a compromised vault (the compensator has
    ///         settled its claims).
    function disputePayout(bytes32 claimId) external;

    /// @notice The slasher only, after it verified an `Included` proof of `payoutRef`, which must be a committed payout
    ///         of the claim's input, recorded for the claim's vault. Accepts every proof transition: a committed
    ///         `Pending` claim (also on a frozen or compromised vault, before the compensator tallied it) and a claim
    ///         confirmed by signatures (`PayoutProven`), a disputed or a reopened claim (`PayoutUpheld`; D57: also one the
    ///         bonds paid meanwhile). Afterwards the claim is `Paid` for good: `paidAt == 0`, `disputeDueAt == 0`. A
    ///         final claim reverts `AlreadyFinal`; a written-off or compensated one `NotUpholdable`.
    function upholdPayout(bytes32 claimId, bytes32 payoutRef) external;

    /// @notice Anyone, once `inclusionWindow` has passed without a proof: the disputed claim is `Pending` again (and
    ///         stays upholdable by a later proof).
    function reopenDisputedClaim(bytes32 claimId) external;

    /// @return paidAt When the claim was confirmed paid (zero if it is not, or its payment is final).
    /// @return dueAt While it is disputed: the end of the inclusion window (zero otherwise).
    function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt);
}
