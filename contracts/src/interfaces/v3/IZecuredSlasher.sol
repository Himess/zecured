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
    /// @notice v4: not an executed F2 or F4 case (`proveInclusionByCase`).
    error NotAnInclusionCase(bytes32 caseId);
    /// @notice v4 (R-19): `executeSlash` of a case that compromises a vault needs at least `BURN_GAS` (and the 1/64
    ///         Ethereum keeps back) to hand to the compensator's burn, so that nobody skips the burn by starving it.
    error InsufficientGasForBurn();

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
