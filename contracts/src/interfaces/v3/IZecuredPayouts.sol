// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice Events and errors of the payout confirmations and the compensator's hooks (v4), which `ZecuredSpendLogic`
///         emits and raises on the Registry's behalf. The errors that `IZecuredRegistry` also declares keep its
///         signatures, hence its selectors.
interface IZecuredPayoutsEvents {
    /// @notice Three signers confirmed that the committed payout `payoutRef` paid `claimIds` (R-1). Each claim's
    ///         claimant may dispute this for `disputeWindow`; a proof of the payout's inclusion makes it final.
    event PayoutConfirmed(bytes32 indexed vaultId, bytes32 indexed payoutRef, bytes32[] claimIds);
    /// @notice R-3: a claim confirmed by signatures was not disputed within its window and its vault was not frozen
    ///         meanwhile: the confirmation is final.
    event ConfirmationExpired(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice T-04: the compensator pays this claim instead (a compromised vault's claim, R-3, R-21), or an executed F3
    ///         case proved its payout short and the compensator owes the shortfall (R-7). As in `IZecuredRegistry`.
    event ClaimCompensated(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice R-20: the compensator settled this compromised vault while it stays active (its debt only partly covered
    ///         by the slashed bonds, or its account no longer authorizing the Registry): it no longer counts as open, and
    ///         its `outstanding` no longer counts as locked.
    event VaultCompensated(bytes32 indexed vaultId, uint64 outstanding);

    // As in IZecuredRegistry.
    error NoClaims();
    error UnknownClaim(bytes32 claimId);
    error ClaimVaultMismatch(bytes32 claimId, bytes32 vaultId);
    error ClaimAlreadyPaid(bytes32 claimId);
    error VaultFrozen(bytes32 vaultId);
    error OnlyCompensator(address caller);
    error NotCompensable(bytes32 claimId);

    /// @notice `payoutRef` is not a committed payout of `vaultId`.
    error PayoutNotOfVault(bytes32 vaultId, bytes32 payoutRef);
    /// @notice R-1: the payout's confirmation was already recorded.
    error PayoutAlreadyConfirmed(bytes32 payoutRef);
    /// @notice R-1: a claim reopened after a dispute is paid only by a proof of its payout.
    error ClaimReopened(bytes32 claimId);
    /// @notice The claim is not a confirmation by signatures still open to a dispute or a proof (Paid, `paidAt != 0`).
    error NotUnproven(bytes32 claimId);
    /// @notice R-3: the claim's dispute window has not ended yet.
    error DisputeWindowOpen(bytes32 claimId, uint64 closesAt);
    /// @notice R-3: the claim was confirmed within `disputeWindow` before its vault froze: only a proof or the
    ///         compensator settles it.
    error ConfirmationSuspect(bytes32 claimId);
    /// @notice R-20: not an active compromised vault the compensator has yet to mark.
    error NotCompensableVault(bytes32 vaultId);
}

/// @title IZecuredPayouts (v4)
/// @notice The payout confirmations and the compensator's hooks, served at the Registry's address: its fallback runs
///         `ZecuredSpendLogic`'s code on its storage (T-11), so call these on the Registry, like `IZecuredDisputes`.
/// @dev The life of a claim's payment (docs/TIER3.md; the v4 work order's "proven delivery"):
///      - `commitSpend` binds the claim to the lineage note its payout spends;
///      - `confirmPayout` (three signers, fast) marks it `Paid` with `paidAt != 0`: disputable for `disputeWindow`;
///      - a proof of the payout's inclusion (`ZecuredSlasher.proveInclusion`, `proveInclusionBatch`) makes it final
///        (`paidAt == 0`) from any state before the compensator took it over: committed `Pending` (also on a frozen or
///        compromised vault), confirmed, disputed, or reopened;
///      - a confirmation whose dispute window ended is final too (`expireConfirmation`), unless its vault froze within
///        that window (R-3): the signers' word given shortly before a proven theft counts for nothing, so such a claim
///        is either proven or compensated (`settleCompensatedClaim`).
///      `unprovenConfirmations(vaultId)` counts the vault's confirmations still open (Paid, `paidAt != 0`); the
///      compensator tallies a compromised vault only once it is zero.
interface IZecuredPayouts is IZecuredPayoutsEvents {
    /// @notice Anyone may relay; needs at least three signer signatures over
    ///         `PayoutConfirmation(vaultId, payoutRef, claimIdsHash)` (Registry domain). R-1, R-3:
    ///         - `payoutRef` is a committed payout of `vaultId`, whose confirmation was not recorded before (each payout
    ///           is confirmed at most once: confirm every claim of a payout in one call);
    ///         - the vault is neither frozen nor compromised (`ZecuredSlasher.vaultFrozen`);
    ///         - every claim is of the vault, `Pending`, bound to the payout's input (`spendRecord(payoutRef).inputTag`),
    ///           and was never reopened after a dispute (a reopened claim is closed by a proof only).
    ///         Each claim becomes `Paid` with `paidAt = now`; its claimant may dispute it for `disputeWindow`.
    function confirmPayout(bytes32 vaultId, bytes32 payoutRef, bytes32[] calldata claimIds, bytes[] calldata sigs)
        external;

    /// @notice The EIP-712 digest three signers sign for `confirmPayout`.
    function hashPayoutConfirmation(bytes32 vaultId, bytes32 payoutRef, bytes32[] calldata claimIds)
        external
        view
        returns (bytes32);

    /// @notice Whether the confirmation of `payoutRef` was recorded.
    function payoutConfirmed(bytes32 payoutRef) external view returns (bool);

    /// @notice R-3: the vault's claims confirmed by signatures and not yet proven, disputed, expired or compensated.
    function unprovenConfirmations(bytes32 vaultId) external view returns (uint256);

    /// @notice Anyone, once the claim's dispute window has ended: a confirmation nobody disputed is final. Refused for a
    ///         claim confirmed within `disputeWindow` before its vault froze (`ConfirmationSuspect`, R-3).
    function expireConfirmation(bytes32 claimId) external;

    /// @notice The compensator only, for a claim of a compromised vault it pays instead (T-04): a pending claim, a
    ///         confirmation by signatures made within `disputeWindow` before the vault froze (R-3), or an abandoned
    ///         refund (R-21). The claim becomes `Compensated`.
    function settleCompensatedClaim(bytes32 claimId) external;

    /// @notice The compensator only, when an F3 case is executed (R-7): its claim's committed payout landed short, so the
    ///         claim leaves the books (`Compensated`) whether it was pending, disputed or confirmed by signatures; the
    ///         compensator owes the shortfall. A final or settled claim is left as it is.
    function closeMisdirectedClaim(bytes32 claimId) external;

    /// @notice The compensator only (R-20): it settled this compromised vault, which stays active. The vault leaves
    ///         `openVaultCount` and its outstanding leaves `lockedZatoshi`.
    function markVaultCompensated(bytes32 vaultId) external;

    /// @notice R-20: the compensator settled the vault while it stayed active (`markVaultCompensated`).
    function vaultCompensated(bytes32 vaultId) external view returns (bool);
}
