// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredSlasher, ViolationKind, ViolationJournal, ViolationCase} from "./interfaces/IZecuredSlasher.sol";
import {IZecuredRegistry} from "./interfaces/IZecuredRegistry.sol";
import {IZecuredSpendCommits, SpendRecord, ClaimPayment} from "./interfaces/IZecuredSpendCommits.sol";
import {ISignerBond} from "./interfaces/ISignerBond.sol";
import {IRiscZeroVerifier} from "./interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "./relay/IZcashHeaderRelay.sol";

/// @title ZecuredSlasher
/// @notice Tier 3 (docs/TIER3.md § 6): a zero-knowledge proof that the signers took collateral (F1: a lineage note spent
///         without a commitment; F2: a committed payout that withheld the change; F3: a committed payout that did not
///         pay a claim) opens a case; after the challenge window the case is checked again and the signers it names
///         are slashed through the SignerBond, which halts minting. No owner, no upgrade path.
/// @dev The guest proves facts about one Zcash transaction X in a relay-anchored block; this contract compares them with
///      the Registry's records. Every journal field a kind does not use must be zero, so a violation has exactly one
///      journal shape and one case id.
contract ZecuredSlasher is IZecuredSlasher {
    /// @inheritdoc IZecuredSlasher
    uint256 public constant JOURNAL_LENGTH = 8 * 32;
    /// @dev An uncommitted spend was signed by three signers, and FROST does not tell which: all five answer for it.
    uint8 private constant ALL_SIGNERS = 0x1f;

    /// @inheritdoc IZecuredSlasher
    address public immutable registry;
    /// @inheritdoc IZecuredSlasher
    address public immutable signerBond;
    /// @inheritdoc IZecuredSlasher
    address public immutable relay;
    /// @inheritdoc IZecuredSlasher
    address public immutable verifier;
    /// @inheritdoc IZecuredSlasher
    bytes32 public immutable violationImageId;
    /// @inheritdoc IZecuredSlasher
    bytes4 public immutable sealSelector;
    /// @inheritdoc IZecuredSlasher
    uint256 public immutable slashConfirmationWork;
    /// @inheritdoc IZecuredSlasher
    uint64 public immutable challengeWindow;

    mapping(bytes32 caseId => ViolationCase) private _cases;
    /// @inheritdoc IZecuredSlasher
    uint256 public openCaseCount;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => uint256) public openCasesOf;

    /// @param registry_ The Registry (v2) whose records the violations are checked against.
    /// @param signerBond_ The SignerBond whose `slasher` is this contract and whose `registry` is `registry_`.
    /// @param relay_ The Zcash header relay the Registry uses.
    /// @param verifier_ The RISC Zero verifier (router) and `sealSelector_` the verifier its seals must be for (Y1).
    /// @param violationImageId_ The violation guest's image id.
    /// @param slashConfirmationWork_ Work above an anchor a slash needs (S4: at least 100 normal Testnet blocks).
    /// @param challengeWindow_ Time between a proof and its slash (24 hours; 2 minutes on the theft demo, T3-6).
    constructor(
        address registry_,
        address signerBond_,
        address relay_,
        address verifier_,
        bytes32 violationImageId_,
        bytes4 sealSelector_,
        uint256 slashConfirmationWork_,
        uint64 challengeWindow_
    ) {
        if (registry_ == address(0) || signerBond_ == address(0) || relay_ == address(0) || verifier_ == address(0)) {
            revert ZeroAddress();
        }
        if (challengeWindow_ == 0) revert ZeroWindow();
        if (violationImageId_ == bytes32(0) || sealSelector_ == bytes4(0) || slashConfirmationWork_ == 0) {
            revert InvalidConfig();
        }
        // The three contracts must name each other (the bond and this contract are deployed at predicted addresses).
        if (
            address(IZecuredSpendCommits(registry_).signerBond()) != signerBond_
                || ISignerBond(signerBond_).slasher() != address(this)
                || ISignerBond(signerBond_).registry() != registry_
                || ISignerBond(signerBond_).challengeWindow() != challengeWindow_
                || address(IZecuredRegistry(registry_).relay()) != relay_
        ) revert InvalidConfig();
        registry = registry_;
        signerBond = signerBond_;
        relay = relay_;
        verifier = verifier_;
        violationImageId = violationImageId_;
        sealSelector = sealSelector_;
        slashConfirmationWork = slashConfirmationWork_;
        challengeWindow = challengeWindow_;
    }

    /// @inheritdoc IZecuredSlasher
    function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal)
        external
        returns (bytes32 caseId)
    {
        if (journal.length != JOURNAL_LENGTH) revert InvalidJournalLength(journal.length);
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        if (selector != sealSelector) revert InvalidSealSelector(selector);
        // abi.decode rejects an out-of-range kind and dirty address / uint64 words.
        ViolationJournal memory j = abi.decode(journal, (ViolationJournal));
        if (j.kind != kind) revert KindMismatch(kind, j.kind);
        if (j.beneficiary == address(0) || IZecuredRegistry(registry).vaultOf(j.beneficiary) != bytes32(0)) {
            revert InvalidBeneficiary(j.beneficiary); // a vault's smart account never receives ETH
        }
        _checkAnchor(j.anchor);
        (bytes32 vaultId, uint8 signerBitmap) = _violation(j);
        caseId = caseIdOf(kind, j.ref, j.noteTag);
        if (_cases[caseId].kind != ViolationKind.None) revert CaseAlreadyOpen(caseId);
        IRiscZeroVerifier(verifier).verify(seal, violationImageId, sha256(journal));

        uint64 executableAt = uint64(block.timestamp) + challengeWindow;
        _cases[caseId] = ViolationCase({
            kind: kind,
            anchor: j.anchor,
            beneficiary: j.beneficiary,
            ref: j.ref,
            noteTag: j.noteTag,
            vaultId: vaultId,
            signerBitmap: signerBitmap,
            executableAt: executableAt,
            executed: false
        });
        openCaseCount += 1;
        openCasesOf[vaultId] += 1;
        emit ViolationProven(caseId, kind, vaultId, signerBitmap, j.beneficiary, executableAt);
    }

    /// @inheritdoc IZecuredSlasher
    function executeSlash(bytes32 caseId) external {
        ViolationCase memory c = _cases[caseId];
        if (c.kind == ViolationKind.None) revert UnknownCase(caseId);
        if (c.executed) revert CaseAlreadyExecuted(caseId);
        if (block.timestamp < c.executableAt) revert ChallengeWindowOpen(caseId, c.executableAt);
        openCaseCount -= 1;
        openCasesOf[c.vaultId] -= 1;

        bool anchorHolds = IZcashHeaderRelay(relay).isInBestChain(c.anchor)
            && IZcashHeaderRelay(relay).confirmationWork(c.anchor) >= slashConfirmationWork;
        // F2 and F3 rest on records that never change; F1 is dismissed if the commitment appeared meanwhile (an honest
        // resubmission after a Base reorg) or if the note stopped being a lineage note (its activation was dropped).
        bool stillHolds = c.kind != ViolationKind.Uncommitted || _uncommitted(c.ref, c.noteTag);
        if (!anchorHolds || !stillHolds) {
            delete _cases[caseId];
            emit ViolationDismissed(caseId);
            return;
        }
        _cases[caseId].executed = true;
        uint256 slashed = ISignerBond(signerBond).slash(caseId, c.signerBitmap, c.beneficiary);
        emit ViolationExecuted(caseId, slashed);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredSlasher
    function caseOf(bytes32 caseId) external view returns (ViolationCase memory) {
        return _cases[caseId];
    }

    /// @inheritdoc IZecuredSlasher
    function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag) public pure returns (bytes32) {
        return keccak256(abi.encode(kind, ref, noteTag));
    }

    // ------------------------------------------------------------------ internal

    function _checkAnchor(bytes32 anchor) internal view {
        uint256 work = IZcashHeaderRelay(relay).confirmationWork(anchor);
        if (!IZcashHeaderRelay(relay).isInBestChain(anchor) || work < slashConfirmationWork) {
            revert AnchorNotConfirmed(anchor, work);
        }
    }

    /// @dev The kind's condition against the Registry's records; returns the vault and whose bonds answer for it.
    function _violation(ViolationJournal memory j) internal view returns (bytes32 vaultId, uint8 signerBitmap) {
        IZecuredSpendCommits r = IZecuredSpendCommits(registry);
        if (j.kind == ViolationKind.Uncommitted) {
            // X spends lineage note `noteTag`, and no commitment of X names it as its input.
            if (j.value == 0 && j.destCommitment == bytes32(0) && j.outputIndex == 0 && _uncommitted(j.ref, j.noteTag))
            {
                return (r.lineageOf(j.noteTag).vaultId, ALL_SIGNERS);
            }
        } else if (j.kind == ViolationKind.WithheldChange) {
            // X is committed with change (`noteTag`, `value`), and none of X's outputs is that note.
            SpendRecord memory s = r.spendRecord(j.ref);
            if (
                j.destCommitment == bytes32(0) && j.outputIndex == 0 && s.vaultId != bytes32(0) && s.changeValue != 0
                    && s.changeTag == j.noteTag && s.changeValue == j.value
            ) return (s.vaultId, s.signerBitmap);
        } else if (j.kind == ViolationKind.Misdirected) {
            // X is committed to pay the claim behind `ref`, whose recipient is `destCommitment`, at least `expected` in its
            // output `outputIndex`; that output pays the recipient `value` (E1).
            ClaimPayment memory p = r.claimPayment(j.ref);
            if (
                j.noteTag == bytes32(0) && p.claimId != bytes32(0) && j.outputIndex == p.outputIndex
                    && j.value < p.expected
            ) {
                (bytes32 claimVault,,, bytes32 commitment,) = IZecuredRegistry(registry).claim(p.claimId);
                if (commitment == j.destCommitment) return (claimVault, r.spendRecord(p.payoutRef).signerBitmap);
            }
        } else if (j.kind == ViolationKind.WrongClaimRef) {
            // X is committed (`ref`) with a claim of recipient `destCommitment` under a reference other than the one its
            // claimant computes, keccak256(salt || txid_X) = `noteTag`, so the claimant can never find X (RT-1).
            SpendRecord memory s = r.spendRecord(j.ref);
            bytes32 committed = r.claimRefOf(j.ref, j.destCommitment);
            if (
                j.value == 0 && j.outputIndex == 0 && j.noteTag != bytes32(0) && s.vaultId != bytes32(0)
                    && committed != bytes32(0) && committed != j.noteTag
            ) return (s.vaultId, s.signerBitmap);
        } else {
            revert UnknownKind();
        }
        revert NoViolation(j.kind, j.ref);
    }

    /// @dev F1's condition: `noteTag` is a lineage note and the commitment of `payoutRef` (if any) spends another note.
    function _uncommitted(bytes32 payoutRef, bytes32 noteTag) internal view returns (bool) {
        IZecuredSpendCommits r = IZecuredSpendCommits(registry);
        return r.lineageOf(noteTag).vaultId != bytes32(0) && r.spendRecord(payoutRef).inputTag != noteTag;
    }
}
