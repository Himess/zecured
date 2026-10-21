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
///        F4: since v4.1 the claim its commitment hid and that claim's `expected` (W-1; v2 to v4 registered nothing).
///      - Disputes (design notes T-05 (a)): `proveInclusion` takes the same guest's `Included` journal (kind 5, which
///        is not a `ViolationKind`) and upholds a disputed payout confirmation in the Registry (`IZecuredDisputes`).
///      Changes in v4 (proven delivery, R-2, R-3, R-19):
///      - an inclusion proof makes any committed claim's payment final, not only a disputed one's; `proveInclusionBatch`
///        verifies one proof for every claim of a payout and skips the claims someone else proved first;
///        `proveInclusionByCase` uses an executed F2 or F4 case, whose journal proved its payout in a block, the same
///        way;
///      - `frozenSince` records when a vault's current freeze began (its first open F1/F2 case), `compromisedAt` when an
///        executed case compromised it: the compensator counts a confirmation by signatures made within the dispute
///        window before the freeze as unpaid unless proven (R-3), and waits `inclusionWindow` after the compromise for
///        the proofs of committed claims (R-2);
///      - executing a case that compromises a vault asks the compensator to burn the vault's collateral down to the LLTV
///        at once (R-19), with `BURN_GAS` of gas (the call itself may fail, e.g. for an account that revoked the
///        Registry's Morpho authorization; anyone retries with `ZecuredCompensator.burnCollateral`);
///      - F1's condition reads the commitment of the note's own vault (R-16: a change record a deposit overrode never
///        covers that deposit's spend).
///      Changes in v4.1 (W-1, W-2, W-5):
///      - W-1: an F4 case records, when proven, the claim its commitment hid (`claimPayment(claimRefOf(ref,
///        destCommitment))`) and that claim's expected amount; its execution hands both to the compensator, which
///        compensates the claim like an F3 shortfall (`onCase(..., claimId, expected)`), also once a proof made it
///        final.
///      - W-2: an F1 case's id names its vault (`caseIdOf(kind, ref, noteTag, vaultId)`; the other kinds' ids do not),
///        and the case follows its note: when a proven deposit took the note's tag over from another vault's change
///        record, `executeSlash` (or anyone, with `followNote`, also for an executed case) opens the same case for the
///        tag's new owner (same anchor and beneficiary, a new challenge window, that vault frozen) and drops the old one
///        if it is still open. `openF1CasesOf` and `noteSpendProven` tell the Intake that a deposit's note is under an
///        F1 case or was proven spent: it never activates.
///      - W-5: an open F3 or F4 case counts against its claim (`claimCaseOpen`): the bonds pay no liveness for the
///        claim meanwhile. F3 checks the recipient against the commitment the payout recorded
///        (`claimRefOf(payoutRef, destCommitment) == ref`), which a liveness payment does not clear.
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
    /// @notice v4.1 (W-2): F1 case `caseId` followed its note to `vaultId`, the tag's owner now, as case `newCaseId`
    ///         (whose `ViolationProven` is emitted with it).
    event ViolationMoved(bytes32 indexed caseId, bytes32 indexed newCaseId, bytes32 indexed vaultId);

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
    /// @notice v4: not an executed F2 or F4 case (`proveInclusionByCase`).
    error NotAnInclusionCase(bytes32 caseId);
    /// @notice v4 (R-19): `executeSlash` of a case that compromises a vault needs at least `BURN_GAS` (and the 1/64
    ///         Ethereum keeps back) to hand to the compensator's burn, so that nobody skips the burn by starving it.
    error InsufficientGasForBurn();
    /// @notice v4.1 (W-2): not an F1 case whose note's tag another vault owns now (`followNote`).
    error NotMovable(bytes32 caseId);

    function JOURNAL_LENGTH() external view returns (uint256);
    /// @notice Gas `executeSlash` hands the compensator to burn a vault's collateral when it compromises it (R-19).
    function BURN_GAS() external view returns (uint256);
    /// @notice Anyone. `imageId` and the seal's router selector must be an active Violation version of `versions`
    ///         (T-07a).
    function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal, bytes32 imageId)
        external
        returns (bytes32 caseId);
    function executeSlash(bytes32 caseId) external;

    /// @notice Anyone (the signers' coordinator, in practice). Verifies a proof of the violation guest's `Included`
    ///         kind: the journal is `(5, anchor, 0, payoutRef, 0, 0, 0, 0)`, the seal is from an active Violation version
    ///         (`imageId`), and the anchor is on the relay's best chain under `slashConfirmationWork`, as for a slash.
    ///         Then proves `claimId`'s payment in the Registry (`IZecuredDisputes.upholdPayout`), which checks that
    ///         `payoutRef` is a committed payout of its input; reverts with the Registry's reason.
    function proveInclusion(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes32 claimId) external;

    /// @notice v4: as `proveInclusion`, one verification for every claim of the payout. A claim already final or no
    ///         longer upholdable (proven by someone else first, compensated) is skipped, never a revert.
    /// @return upheld How many of `claimIds` the proof upheld.
    function proveInclusionBatch(
        bytes calldata journal,
        bytes calldata seal,
        bytes32 imageId,
        bytes32[] calldata claimIds
    ) external returns (uint256 upheld);

    /// @notice v4 (R-2): an executed F2 or F4 case proved that its committed payout `ref` is in a block (its anchor
    ///         rechecked under `slashConfirmationWork` at execution), so it proves the payment of that payout's claims
    ///         like an `Included` proof; skips as `proveInclusionBatch`. F3's reference is a claim's, not the payout's,
    ///         and F1's spend was never committed: neither counts.
    function proveInclusionByCase(bytes32 caseId, bytes32[] calldata claimIds) external returns (uint256 upheld);

    /// @notice R-2: when an executed F1/F2 case compromised the vault (zero if never).
    function compromisedAt(bytes32 vaultId) external view returns (uint64);

    /// @notice R-3: when the vault's current freeze began (its first open F1/F2 case; zero while not frozen). Kept once
    ///         the vault is compromised.
    function frozenSince(bytes32 vaultId) external view returns (uint64);

    /// @notice v4.1 (W-2), anyone: F1 case `caseId` (open or executed) about a note whose tag is now another, non-zero
    ///         vault's (a proven deposit took it over from a change record) is opened for that vault, from the case's
    ///         own fields and with a new challenge window, unless that vault's case is open already or its commitment
    ///         now covers the spend; the old case is dropped if still open. Reverts `NotMovable` for any other case and
    ///         `AnchorNotConfirmed` once the case's anchor left the best chain.
    /// @return newCaseId The new owner's case (zero if none was opened).
    function followNote(bytes32 caseId) external returns (bytes32 newCaseId);

    function caseOf(bytes32 caseId) external view returns (ViolationCase memory);
    /// @notice A case's id: `keccak256(abi.encode(kind, ref, noteTag, vaultId))` for F1 (v4.1, W-2: its vault is part
    ///         of it), `keccak256(abi.encode(kind, ref, noteTag))` for the other kinds (`vaultId` ignored).
    function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag, bytes32 vaultId) external pure returns (bytes32);
    /// @notice v4.1 (W-5): an F3 or F4 case about the claim is open.
    function claimCaseOpen(bytes32 claimId) external view returns (bool);
    /// @notice v4.1 (W-2): open F1 cases about the note with this tag.
    function openF1CasesOf(bytes32 noteTag) external view returns (uint256);
    /// @notice v4.1 (W-2): an F1 case about the note with this tag was executed: the note was taken without a
    ///         commitment.
    function noteSpendProven(bytes32 noteTag) external view returns (bool);
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
