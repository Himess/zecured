// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice The three provable violations (docs/TIER3.md § 6; review T3-1 to T3-3). One RISC Zero guest, one image id.
enum ViolationKind {
    None,
    /// F1: a lineage note was spent by a transaction X that has no commitment for that note.
    Uncommitted,
    /// F2: a committed payout X did not return the committed change note to the vault.
    WithheldChange,
    /// F3: a committed payout X did not pay a claim it names to the claim's recipient.
    Misdirected
}

/// @notice Journal of a violation proof: `abi.encode(ViolationJournal)`, committed by the guest; the seal proves
///         `sha256(journal)` under the violation image id. Every kind first shows that X, whose txid the guest
///         computes from its bytes, is in a block committed by `anchor` (Tier 2 inclusion code).
/// @param kind Equals the `kind` argument of `proveViolation`.
/// @param anchor Relay header whose history commitment includes X's block.
/// @param beneficiary Receives the bounty; bound inside the proof, so a copied seal cannot redirect it.
/// @param ref F1, F2: `payoutRef(txid_X)` under the vault's tag key; F3: `claimRef = keccak256(salt || txid_X)`.
/// @param noteTag F1: tag of the lineage note X spends; F2: the committed change tag; F3: zero.
/// @param value F2: the committed change value; F3: the largest value X pays to the claimant's IVK (zero if none);
///        F1: zero.
/// @param destCommitment F3: `keccak256(salt || receiver)`, the claim's recipient commitment; zero otherwise.
struct ViolationJournal {
    ViolationKind kind;
    bytes32 anchor;
    address beneficiary;
    bytes32 ref;
    bytes32 noteTag;
    uint64 value;
    bytes32 destCommitment;
}

/// @notice An open or executed violation case.
/// @param signerBitmap Whose bonds the case slashes: all five (0x1f) for F1, the commitment's signers for F2 and F3.
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
}

/// @title IZecuredSlasher
/// @notice Tier 3: turns a zero-knowledge proof of theft into a slash of the signers' bonds (docs/TIER3.md § 6).
///         Interface locked 2026-10-12.
/// @dev `proveViolation(kind, journal, seal)` (anyone): the seal has the pinned selector and verifies against
///      `violationImageId`; `journal.kind == kind`; the beneficiary is not zero and is no vault's account (smart
///      accounts never receive ETH); the anchor is on the relay's best chain with at least `slashConfirmationWork`;
///      the kind's condition holds against the Registry:
///      - F1: `lineageOf(noteTag)` exists and `spendRecord(ref).inputTag != noteTag`; slashes all five;
///      - F2: `spendRecord(ref)` exists with `changeValue > 0`, `changeTag == noteTag` and `changeValue == value`;
///        slashes its `signerBitmap`;
///      - F3: `claimPayment(ref)` exists, its claim's commitment is `destCommitment` and `value < expected`; slashes
///        the `signerBitmap` of its payout.
///      It opens case `caseIdOf(kind, ref, noteTag)` (one per violation, whoever proves it first), executable after
///      `challengeWindow`. `executeSlash(caseId)` (anyone) checks the anchor and the condition again: an F1 case is
///      dismissed (deleted) if the commitment appeared meanwhile, an honest resubmission after a Base reorg (a lying
///      late commitment is F2's to catch), and any case is dismissed if its anchor left the best chain; otherwise it
///      calls `ISignerBond.slash`, which halts minting.
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

    error KindMismatch(ViolationKind argument, ViolationKind journal);
    error UnknownKind();
    error InvalidSealSelector(bytes4 selector);
    error InvalidBeneficiary(address beneficiary);
    error AnchorNotConfirmed(bytes32 anchor, uint256 work);
    error NoViolation(ViolationKind kind, bytes32 ref);
    error CaseAlreadyOpen(bytes32 caseId);
    error UnknownCase(bytes32 caseId);
    error ChallengeWindowOpen(bytes32 caseId, uint64 executableAt);
    error CaseAlreadyExecuted(bytes32 caseId);

    function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal)
        external
        returns (bytes32 caseId);
    function executeSlash(bytes32 caseId) external;

    function caseOf(bytes32 caseId) external view returns (ViolationCase memory);
    function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag) external pure returns (bytes32);
    /// @notice Cases proven but neither executed nor dismissed (bonds stay locked meanwhile).
    function openCaseCount() external view returns (uint256);

    function registry() external view returns (address);
    function signerBond() external view returns (address);
    function relay() external view returns (address);
    function verifier() external view returns (address);
    function violationImageId() external view returns (bytes32);
    function sealSelector() external view returns (bytes4);
    /// @notice Work above the anchor a slash needs (at least 100 normal Testnet blocks, S4).
    function slashConfirmationWork() external view returns (uint256);
    /// @notice 24 hours on the judge deployment, 2 minutes on the separate theft demo (T3-6).
    function challengeWindow() external view returns (uint64);
}
