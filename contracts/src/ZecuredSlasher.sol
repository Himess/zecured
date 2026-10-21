// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredSlasher, ViolationKind, ViolationJournal, ViolationCase} from "./interfaces/v3/IZecuredSlasher.sol";
import {IZecuredRegistry} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake} from "./interfaces/v3/IZecuredIntake.sol";
import {IZecuredSpendCommits, SpendRecord, ClaimPayment} from "./interfaces/v3/IZecuredSpendCommits.sol";
import {ISignerBond} from "./interfaces/v3/ISignerBond.sol";
import {IZecuredCompensator} from "./interfaces/v3/IZecuredCompensator.sol";
import {IZecuredDisputes} from "./interfaces/v3/IZecuredDisputes.sol";
import {IZecuredVersions, VersionKind} from "./interfaces/v3/IZecuredVersions.sol";
import {IRiscZeroVerifier} from "./interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "./relay/IZcashHeaderRelay.sol";

/// @title ZecuredSlasher (v2)
/// @notice Tier 3 (docs/TIER3.md § 6): a zero-knowledge proof that the signers took collateral (F1: a lineage note spent
///         without a commitment; F2: a committed payout that withheld the change; F3: a committed payout that did not
///         pay a claim; F4: a commitment that hides a payout from its claimant) opens a case; after the challenge window
///         the case is checked again and the signers it names are slashed through the SignerBond, which halts minting.
///         v2 (design notes T-04): an open F1/F2 case freezes its vault's exits, an executed one marks the vault
///         compromised, and the loss is handed to the compensator. The same guest's `Included` kind upholds a disputed
///         payout confirmation (T-05 (a)) and, since v4, proves any committed payout (proven delivery). No owner, no
///         upgrade path.
/// @dev The guest proves facts about one Zcash transaction X in a relay-anchored block; this contract compares them with
///      the Registry's records. Every journal field a kind does not use must be zero, so a violation has exactly one
///      journal shape and one case id.
contract ZecuredSlasher is IZecuredSlasher {
    /// @inheritdoc IZecuredSlasher
    uint256 public constant JOURNAL_LENGTH = 8 * 32;
    /// @dev An uncommitted spend was signed by three signers, and FROST does not tell which: all five answer for it.
    uint8 private constant ALL_SIGNERS = 0x1f;
    /// @dev The violation guest's `Included` kind (tier2/statement, `violation::Kind`).
    uint256 private constant INCLUDED = 5;
    /// @inheritdoc IZecuredSlasher
    /// @dev The burn accrues Morpho's interest (calling its IRM), reads the price, and withdraws, transfers and burns
    ///      zZEC: at most about 245,000 gas from cold storage in the R-19 tests on a Base Sepolia fork (which check it
    ///      stays below this), so this leaves about twice that.
    uint256 public constant BURN_GAS = 500_000;

    /// @inheritdoc IZecuredSlasher
    address public immutable registry;
    /// @inheritdoc IZecuredSlasher
    address public immutable signerBond;
    /// @inheritdoc IZecuredSlasher
    address public immutable compensator;
    /// @inheritdoc IZecuredSlasher
    address public immutable relay;
    /// @inheritdoc IZecuredSlasher
    address public immutable verifier;
    /// @inheritdoc IZecuredSlasher
    address public immutable versions;
    /// @inheritdoc IZecuredSlasher
    uint256 public immutable slashConfirmationWork;
    /// @inheritdoc IZecuredSlasher
    uint64 public immutable challengeWindow;

    mapping(bytes32 caseId => ViolationCase) private _cases;
    /// @inheritdoc IZecuredSlasher
    uint256 public openCaseCount;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => uint256) public openCasesOf;
    /// @inheritdoc IZecuredSlasher
    uint256 public openTheftCaseCount;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => uint256) public theftCasesOf;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => bool) public isCompromised;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => uint64) public compromisedAt;
    /// @inheritdoc IZecuredSlasher
    mapping(bytes32 vaultId => uint64) public frozenSince;

    /// @param registry_ The Registry (v3) whose records the violations are checked against.
    /// @param signerBond_ The SignerBond whose `slasher` is this contract and whose `registry` is `registry_`.
    /// @param compensator_ The ZecuredCompensator (its predicted address) that pays the victims; it checks this wiring
    ///        in its own constructor. Zero on deployments without compensation (tests).
    /// @param relay_ The Zcash header relay the Intake uses.
    /// @param verifier_ The RISC Zero verifier (router).
    /// @param versions_ ZecuredVersions (T-07a, its predicted address): seals are accepted only from an active
    ///        Violation version (router selector and guest image id, Y1).
    /// @param slashConfirmationWork_ Work above an anchor a slash needs (S4: at least 100 normal Testnet blocks).
    /// @param challengeWindow_ Time between a proof and its slash (24 hours; 2 minutes on the theft demo, T3-6).
    constructor(
        address registry_,
        address signerBond_,
        address compensator_,
        address relay_,
        address verifier_,
        address versions_,
        uint256 slashConfirmationWork_,
        uint64 challengeWindow_
    ) {
        if (
            registry_ == address(0) || signerBond_ == address(0) || relay_ == address(0) || verifier_ == address(0)
                || versions_ == address(0)
        ) revert ZeroAddress();
        if (challengeWindow_ == 0) revert ZeroWindow();
        if (slashConfirmationWork_ == 0) revert InvalidConfig();
        // The contracts must name each other (the bond and this contract are deployed at predicted addresses).
        if (
            IZecuredSpendCommits(registry_).signerBond() != signerBond_
                || IZecuredRegistry(registry_).slasher() != address(this)
                || IZecuredRegistry(registry_).compensator() != compensator_
                || ISignerBond(signerBond_).slasher() != address(this)
                || ISignerBond(signerBond_).registry() != registry_
                || ISignerBond(signerBond_).challengeWindow() != challengeWindow_
                || address(IZecuredIntake(IZecuredRegistry(registry_).intake()).relay()) != relay_
        ) revert InvalidConfig();
        registry = registry_;
        signerBond = signerBond_;
        compensator = compensator_;
        relay = relay_;
        verifier = verifier_;
        versions = versions_;
        slashConfirmationWork = slashConfirmationWork_;
        challengeWindow = challengeWindow_;
    }

    /// @inheritdoc IZecuredSlasher
    function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal, bytes32 imageId)
        external
        returns (bytes32 caseId)
    {
        if (journal.length != JOURNAL_LENGTH) revert InvalidJournalLength(journal.length);
        _checkVersion(seal, imageId);
        // abi.decode rejects an out-of-range kind and dirty address / uint64 words.
        ViolationJournal memory j = abi.decode(journal, (ViolationJournal));
        if (j.kind != kind) revert KindMismatch(kind, j.kind);
        if (j.beneficiary == address(0) || IZecuredRegistry(registry).vaultOf(j.beneficiary) != bytes32(0)) {
            revert InvalidBeneficiary(j.beneficiary); // never a vault's smart account
        }
        _checkAnchor(j.anchor);
        (bytes32 vaultId, uint8 signerBitmap) = _violation(j);
        caseId = caseIdOf(kind, j.ref, j.noteTag);
        if (_cases[caseId].kind != ViolationKind.None) revert CaseAlreadyOpen(caseId);
        IRiscZeroVerifier(verifier).verify(seal, imageId, sha256(journal));

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
            executed: false,
            value: j.value
        });
        openCaseCount += 1;
        openCasesOf[vaultId] += 1;
        if (_isTheft(kind)) {
            openTheftCaseCount += 1;
            // R-3: the vault's freeze begins with its first open theft case.
            if (theftCasesOf[vaultId] == 0 && !isCompromised[vaultId]) frozenSince[vaultId] = uint64(block.timestamp);
            theftCasesOf[vaultId] += 1;
        }
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
        bool theft = _isTheft(c.kind);
        if (theft) {
            openTheftCaseCount -= 1;
            theftCasesOf[c.vaultId] -= 1;
        }

        bool anchorHolds = IZcashHeaderRelay(relay).isInBestChain(c.anchor)
            && IZcashHeaderRelay(relay).confirmationWork(c.anchor) >= slashConfirmationWork;
        // F2, F3 and F4 rest on records that never change; F1 is dismissed if the commitment appeared meanwhile (an
        // honest resubmission after a Base reorg) or if the note stopped being a lineage note (its activation was
        // dropped).
        bool stillHolds = c.kind != ViolationKind.Uncommitted || _uncommitted(c.ref, c.noteTag);
        if (!anchorHolds || !stillHolds) {
            delete _cases[caseId];
            // R-3: the freeze ends with the vault's last open theft case, unless an executed one compromised it.
            if (theft && theftCasesOf[c.vaultId] == 0 && !isCompromised[c.vaultId]) delete frozenSince[c.vaultId];
            emit ViolationDismissed(caseId);
            return;
        }
        _cases[caseId].executed = true;
        bool compromises = theft && !isCompromised[c.vaultId];
        if (compromises) {
            isCompromised[c.vaultId] = true;
            compromisedAt[c.vaultId] = uint64(block.timestamp);
            emit VaultCompromised(c.vaultId, caseId);
        }
        uint256 slashed = ISignerBond(signerBond).slash(caseId, c.signerBitmap, c.beneficiary);
        emit ViolationExecuted(caseId, slashed);

        if (compensator == address(0)) return;
        bytes32 claimId;
        uint64 shortfall;
        if (c.kind == ViolationKind.Misdirected) {
            // F3: the claim the payout named, and what its claimant did not receive.
            ClaimPayment memory p = IZecuredSpendCommits(registry).claimPayment(c.ref);
            claimId = p.claimId;
            shortfall = p.expected - c.value; // value < expected was checked when the case opened
        }
        IZecuredCompensator(compensator).onCase(caseId, c.kind, c.vaultId, claimId, shortfall);
        // R-19: nothing more is borrowed against the vault's unbacked zZEC from now on. The burn may fail (an account
        // that revoked the Registry's authorization, a pending vault): the slash stands, and anyone retries it.
        if (compromises) {
            if (gasleft() < BURN_GAS + BURN_GAS / 63 + 10_000) revert InsufficientGasForBurn();
            try IZecuredCompensator(compensator).burnCollateral{gas: BURN_GAS}(c.vaultId) {} catch {}
        }
    }

    /// @inheritdoc IZecuredSlasher
    function proveInclusion(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes32 claimId) external {
        (bytes32 anchor, bytes32 payoutRef) = _verifyInclusion(journal, seal, imageId);
        emit PayoutInclusionProven(claimId, payoutRef, anchor);
        IZecuredDisputes(registry).upholdPayout(claimId, payoutRef);
    }

    /// @inheritdoc IZecuredSlasher
    function proveInclusionBatch(
        bytes calldata journal,
        bytes calldata seal,
        bytes32 imageId,
        bytes32[] calldata claimIds
    ) external returns (uint256 upheld) {
        (bytes32 anchor, bytes32 payoutRef) = _verifyInclusion(journal, seal, imageId);
        return _upholdEach(claimIds, payoutRef, anchor);
    }

    /// @inheritdoc IZecuredSlasher
    function proveInclusionByCase(bytes32 caseId, bytes32[] calldata claimIds) external returns (uint256 upheld) {
        ViolationCase storage c = _cases[caseId];
        ViolationKind kind = c.kind;
        if (!c.executed || (kind != ViolationKind.WithheldChange && kind != ViolationKind.WrongClaimRef)) {
            revert NotAnInclusionCase(caseId);
        }
        return _upholdEach(claimIds, c.ref, c.anchor);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredSlasher
    function vaultFrozen(bytes32 vaultId) external view returns (bool) {
        return theftCasesOf[vaultId] != 0 || isCompromised[vaultId];
    }

    /// @inheritdoc IZecuredSlasher
    function caseOf(bytes32 caseId) external view returns (ViolationCase memory) {
        return _cases[caseId];
    }

    /// @inheritdoc IZecuredSlasher
    function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag) public pure returns (bytes32) {
        return keccak256(abi.encode(kind, ref, noteTag));
    }

    // ------------------------------------------------------------------ internal

    /// @dev F1 and F2 take a vault's lineage; F3 and F4 are about one claim.
    function _isTheft(ViolationKind kind) internal pure returns (bool) {
        return kind == ViolationKind.Uncommitted || kind == ViolationKind.WithheldChange;
    }

    /// @dev Y1, T-07a: the seal's router selector and `imageId` are an active Violation version.
    function _checkVersion(bytes calldata seal, bytes32 imageId) internal view {
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        IZecuredVersions(versions).activeVersion(VersionKind.Violation, selector, imageId);
    }

    function _checkAnchor(bytes32 anchor) internal view {
        uint256 work = IZcashHeaderRelay(relay).confirmationWork(anchor);
        if (!IZcashHeaderRelay(relay).isInBestChain(anchor) || work < slashConfirmationWork) {
            revert AnchorNotConfirmed(anchor, work);
        }
    }

    /// @dev An `Included` proof: its journal's shape, an active Violation version, the anchor under the slash's work, the
    ///      seal. Returns the anchor and the proven payout's reference.
    function _verifyInclusion(bytes calldata journal, bytes calldata seal, bytes32 imageId)
        internal
        view
        returns (bytes32 anchor, bytes32 payoutRef)
    {
        (anchor, payoutRef) = _inclusion(journal);
        _checkVersion(seal, imageId);
        _checkAnchor(anchor);
        IRiscZeroVerifier(verifier).verify(seal, imageId, sha256(journal));
    }

    /// @dev Proves `payoutRef`'s payment of each claim; a claim the Registry refuses (already final, compensated, or not
    ///      of this payout) is skipped, so that nobody who proves one claim first makes the batch fail.
    function _upholdEach(bytes32[] calldata claimIds, bytes32 payoutRef, bytes32 anchor)
        internal
        returns (uint256 upheld)
    {
        for (uint256 i; i < claimIds.length; ++i) {
            try IZecuredDisputes(registry).upholdPayout(claimIds[i], payoutRef) {
                emit PayoutInclusionProven(claimIds[i], payoutRef, anchor);
                ++upheld;
            } catch {}
        }
    }

    /// @dev The anchor and X's reference of an `Included` journal, read word by word (kind 5 is not a `ViolationKind`):
    ///      kind 5, the anchor, a zero beneficiary, X's payoutRef, and four zero words (note tag, value, destination
    ///      commitment, output index).
    function _inclusion(bytes calldata journal) internal pure returns (bytes32 anchor, bytes32 payoutRef) {
        if (journal.length != JOURNAL_LENGTH) revert InvalidJournalLength(journal.length);
        anchor = bytes32(journal[32:64]);
        payoutRef = bytes32(journal[96:128]);
        if (uint256(bytes32(journal[:32])) != INCLUDED || bytes32(journal[64:96]) != bytes32(0) || payoutRef == 0) {
            revert NotAnInclusion();
        }
        for (uint256 offset = 128; offset < JOURNAL_LENGTH; offset += 32) {
            if (bytes32(journal[offset:offset + 32]) != bytes32(0)) revert NotAnInclusion();
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

    /// @dev F1's condition: `noteTag` is a lineage note and no commitment of its vault under `payoutRef` spends it. R-16:
    ///      a commitment of another vault, made while a false change record held the tag, never covers it.
    function _uncommitted(bytes32 payoutRef, bytes32 noteTag) internal view returns (bool) {
        IZecuredSpendCommits r = IZecuredSpendCommits(registry);
        bytes32 vaultId = r.lineageOf(noteTag).vaultId;
        if (vaultId == bytes32(0)) return false;
        SpendRecord memory s = r.spendRecord(payoutRef);
        return s.inputTag != noteTag || s.vaultId != vaultId;
    }
}
