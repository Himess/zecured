// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Errors of the refund functions the spend logic serves.
interface IZecuredRefundsErrors {
    /// @notice R-11: `markRefundDormant` before `createdAt + refundDormancy`.
    error NotDormantYet(bytes32 claimId, uint64 at);
    /// @notice R-11: not a pending Refund claim without a destination, not bond-paid and not dormant yet.
    error NotDormantable(bytes32 claimId);
    /// @notice R-12: no reserved refund of this tag for this vault.
    error NoReservedRefund(bytes32 vaultId, bytes32 depositTag);
}

/// @title IZecuredRefunds
/// @notice The parts of the refund lifecycle (design notes T-02; review items R-11, R-12) that the Registry serves
///         through its fallback (`ZecuredSpendLogic`, T-11), so call them on the Registry's address. The Registry emits
///         their events (`IZecuredRegistry.RefundOpened`, `RefundDormant`).
/// @dev - R-12: a refunded deposit waits like an activation (K3). The Intake reserves it when its proof lands
///        (`IZecuredRegistry.reserveRefund`), then either opens its Refund claim once the relay has moved on with the
///        proof's anchor still on the best chain and its verifier version still active (`openReservedRefund`, from
///        `ZecuredIntake.finalizeRefund`), or releases it (`releaseRefund`, from `ZecuredIntake.dropPendingRefund`).
///        A refund never opens on a vault whose activation is pending (R-8): it waits until the vault activates,
///        converts or drops, so no claim ever exists on a pending vault and none accrues liveness there.
///      - R-11: a Refund claim whose account gave no destination for `refundDormancy` (48 hours on the judge
///        deployment, 7 days on mainnet) can be marked dormant by anyone: it stays pending, keeps its vault open and
///        can still be abandoned (`abandonRefund`) or compensated, but no longer counts in `lockedZatoshi` and the
///        coverage cap until its account gives a destination. So an unclaimed refund no longer fills the cap for the
///        `refundAbandonPeriod` (30 days on the judge deployment, 2 years on mainnet).
interface IZecuredRefunds is IZecuredRefundsErrors {
    /// @notice R-12, the Intake only: opens the Refund claim of a reserved refund for the account it was reserved for,
    ///         without a destination (`RefundOpened`). A `None` vault becomes `Cancelled` with that account; an existing
    ///         vault must have it. Refused on a pending vault (`VaultPending`, R-8) and on a compromised one
    ///         (`VaultCompromised`).
    function openReservedRefund(bytes32 vaultId, bytes32 depositTag) external returns (bytes32 claimId);

    /// @notice R-12, the Intake only: frees a reserved refund whose proof was dropped: tag unused, lineage note removed.
    function releaseRefund(bytes32 vaultId, bytes32 depositTag) external;

    /// @notice As `IZecuredRegistry.setRefundDestination` (the Registry delegates it here): the vault's account gives
    ///         its Refund claim's destination; a dormant claim counts in `lockedZatoshi` again.
    function setRefundDestination(bytes32 claimId, bytes32 destCommitment) external;

    /// @notice As `IZecuredRegistry.abandonRefund` (the Registry delegates it here).
    function abandonRefund(bytes32 claimId) external;

    /// @notice R-11, anyone: a pending Refund claim without a destination, not bond-paid, `refundDormancy` after it was
    ///         created, leaves `lockedZatoshi` until its account gives a destination (`RefundDormant`).
    function markRefundDormant(bytes32 claimId) external;

    /// @notice R-11: how long a Refund claim without a destination counts in `lockedZatoshi`.
    function refundDormancy() external view returns (uint64);

    /// @notice R-11: whether the claim is dormant.
    function isRefundDormant(bytes32 claimId) external view returns (bool);

    /// @notice R-12: the account a reserved refund of `depositTag` will open for (zero if none is reserved).
    function reservedRefundOf(bytes32 depositTag) external view returns (address account);
}
