// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice The provable violations (docs/TIER3.md § 6). Unchanged from v2.
enum ViolationKind {
    None,
    Uncommitted,
    WithheldChange,
    Misdirected,
    WrongClaimRef
}

/// @notice Journal of a violation proof (unchanged from v2: eight words).
struct ViolationJournal {
    ViolationKind kind;
    bytes32 anchor;
    address beneficiary;
    bytes32 ref;
    bytes32 noteTag;
    uint64 value;
    bytes32 destCommitment;
    uint8 outputIndex;
}

/// @notice An open or executed violation case. v3 keeps the journal's `value` (F3: what the claimant received, from
///         which the compensator computes the shortfall).
struct ViolationCase {
    ViolationKind kind;
    bytes32 anchor;
    address beneficiary;
    bytes32 ref;
    bytes32 noteTag;
    bytes32 vaultId;
    uint8 signerBitmap;
    uint64 executableAt;
    bool executed;
    uint64 value;
}

/// @title IZecuredSlasher (v2)
/// @notice Tier 3: a zero-knowledge proof of theft opens a case; after the challenge window the case is checked again
///         and slashes the signers it names (docs/TIER3.md § 6). v2 adds the vault freeze and the compensation hand-off
///         (design notes T-04).
/// @dev Changes from v1:
///      - Freeze: while an F1 or F2 case is open on a vault (`theftCasesOf`), or once one was executed
///        (`isCompromised`), `vaultFrozen(vaultId)` is true and the Registry refuses the vault's exits. A dismissed
///        case unfreezes the vault (unless another is open or it is compromised). F3 and F4 freeze nothing: they are
///        about a claim, not the vault's collateral.
///      - Execution: `bond.slash(...)` (USDC to the compensator, halt), then for F1 and F2 the vault is marked
///        compromised, and `compensator.onCase(caseId, kind, vaultId, claimId, shortfallZat)` registers the loss:
///        F1, F2: the vault (claimId and shortfall zero); F3: the claim of `claimPayment(ref)` and `expected - value`;
///        F4: nothing to compensate (the call is still made, for the record).
///      - Disputes (design notes T-05 (a)): `proveInclusion` takes the same guest's `Included` journal (kind 5, which
///        is not a `ViolationKind`) and upholds a disputed payout confirmation in the Registry (`IZecuredDisputes`).
interface IZecuredSlasher {
    event ViolationProven(
        bytes32 indexed caseId,
        ViolationKind kind,
        bytes32 indexed vaultId,
        uint8 signerBitmap,
        address beneficiary,
        uint64 executableAt
    );
    event ViolationDismissed(bytes32 indexed caseId);
    event ViolationExecuted(bytes32 indexed caseId, uint256 slashed);
    event VaultCompromised(bytes32 indexed vaultId, bytes32 indexed caseId);
    /// @notice A disputed claim's payout `payoutRef` was proven to be in the block committed by `anchor`.
    event PayoutInclusionProven(bytes32 indexed claimId, bytes32 indexed payoutRef, bytes32 anchor);

    error ZeroAddress();
    error ZeroWindow();
    error InvalidConfig();
    error InvalidJournalLength(uint256 length);
    error KindMismatch(ViolationKind argument, ViolationKind journal);
    error UnknownKind();
    error InvalidBeneficiary(address beneficiary);
    error AnchorNotConfirmed(bytes32 anchor, uint256 work);
    error NoViolation(ViolationKind kind, bytes32 ref);
    error CaseAlreadyOpen(bytes32 caseId);
    error UnknownCase(bytes32 caseId);
    error ChallengeWindowOpen(bytes32 caseId, uint64 executableAt);
    error CaseAlreadyExecuted(bytes32 caseId);
    error NotAnInclusion();

    function JOURNAL_LENGTH() external view returns (uint256);
    /// @notice Anyone. `imageId` and the seal's router selector must be an active Violation version of `versions`
    ///         (T-07a).
    function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal, bytes32 imageId)
        external
        returns (bytes32 caseId);
    function executeSlash(bytes32 caseId) external;

    /// @notice Anyone (the signers' coordinator, in practice). Verifies a proof of the violation guest's `Included`
    ///         kind: the journal is `(5, anchor, 0, payoutRef, 0, 0, 0, 0)`, the seal is from an active Violation version
    ///         (`imageId`), and the anchor is on the relay's best chain under `slashConfirmationWork`, as for a slash.
    ///         Then upholds the disputed `claimId` in the Registry, which checks that `payoutRef` is a committed payout
    ///         of its input.
    function proveInclusion(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes32 claimId) external;

    function caseOf(bytes32 caseId) external view returns (ViolationCase memory);
    function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag) external pure returns (bytes32);
    function openCaseCount() external view returns (uint256);
    function openCasesOf(bytes32 vaultId) external view returns (uint256);
    /// @notice Open F1 and F2 cases (T-03's automatic pause counts these).
    function openTheftCaseCount() external view returns (uint256);
    /// @notice Open F1 and F2 cases of one vault.
    function theftCasesOf(bytes32 vaultId) external view returns (uint256);
    /// @notice An F1 or F2 case about the vault was executed: its lineage was taken.
    function isCompromised(bytes32 vaultId) external view returns (bool);
    /// @notice `theftCasesOf(vaultId) != 0 || isCompromised(vaultId)`: the Registry refuses the vault's exits.
    function vaultFrozen(bytes32 vaultId) external view returns (bool);

    function registry() external view returns (address);
    function signerBond() external view returns (address);
    function compensator() external view returns (address);
    function relay() external view returns (address);
    function verifier() external view returns (address);
    /// @notice The version registry (T-07a).
    function versions() external view returns (address);
    function slashConfirmationWork() external view returns (uint256);
    function challengeWindow() external view returns (uint64);
}
