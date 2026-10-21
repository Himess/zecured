// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IRiscZeroVerifier} from "./interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "./relay/IZcashHeaderRelay.sol";
import {
    IZecuredIntake,
    MintBlock,
    RefundReason,
    DepositJournal,
    PendingActivation,
    PendingRefund,
    WorkTier,
    IntakeConfig
} from "./interfaces/v3/IZecuredIntake.sol";
import {IZecuredRegistry, VaultStatus} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredRefunds} from "./interfaces/v3/IZecuredRefunds.sol";
import {ISignerBond} from "./interfaces/v3/ISignerBond.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {IZecuredCapPrice} from "./interfaces/v3/IZecuredPriceFeed.sol";
import {IZecuredVersions, Version, VersionKind} from "./interfaces/v3/IZecuredVersions.sol";
import {ZecuredEIP712} from "./libraries/ZecuredEIP712.sol";

/// @title ZecuredIntake
/// @notice Everything before a vault is active (docs/TIER2.md; design notes T-01, T-02, T-03, T-07a, T-08, T-09):
///         deposit proofs (from an active verifier version), pending activations and pending refunds (K3), and the
///         gates of minting (the halt, the bonds, the coverage cap, the pause). Minting needs no signer: a proof lands
///         without one; the signers' only say is the five `VaultOpened` signatures given when the vault opened. Every
///         proven deposit lands, as a pending activation or as a pending refund: from then on its note is a lineage
///         note, so taking it without a commitment is provable (F1).
/// @dev v4 (review items R-11 to R-14, R-17, R-18): a refund waits like an activation before its claim opens, and
///      opens only once its vault is not pending (R-8, R-12); a full coverage cap makes a new vault's proof revert
///      rather than cancel the vault (R-11); a pending activation's account can take its deposit back
///      `pendingRefundDelay` after it was ready, whatever gate holds it (R-13); both kinds of pending proof remember
///      their deposit version, which must still be active when they finalize or convert, and anyone may drop them once
///      it is not (R-14); nothing pending is dropped while a violation case is open on its vault (R-17).
///      v4.1: a pending activation mints only with a working version (W-13: active and its verifier not stopped), and
///      never while an F1 case about its note is open or once one was executed (W-2: the note may be another vault's
///      false change record's theft, and a stolen note backs no zZEC); such an item, or any item of a compromised
///      vault (W-3), is dropped only once its proof's anchor left the best chain, and a compromised vault's items are
///      converted or opened as refunds without their version (the claims are the compensator's).
///      The Registry trusts this contract for its deposit hooks and re-checks its own invariants in each (I4, I5,
///      unique lineage tags). No owner, no upgrade path. Signer-attested minting (Tier 1) does not exist here: tests
///      use a harness that calls the hooks directly.
contract ZecuredIntake is IZecuredIntake, EIP712 {
    uint256 private constant SIGNER_COUNT = 5;
    uint256 private constant THRESHOLD = 3;
    uint256 private constant BPS = 10_000;
    /// @notice Length of a v1 deposit journal (six ABI words).
    uint256 public constant JOURNAL_V1_LENGTH = 6 * 32;
    /// @notice Length of a v2 deposit journal (eight ABI words).
    uint256 public constant JOURNAL_V2_LENGTH = 8 * 32;
    /// @notice Most work tiers.
    uint256 public constant MAX_TIERS = 4;
    uint8 private constant MODE_ACTIVATE = 0;
    uint8 private constant MODE_REFUND = 1;
    /// @notice W-18: the longest pause the constructor accepts (every deployment uses exactly this).
    uint64 public constant MAX_PAUSE = 30 days;

    IZecuredRegistry internal immutable _registry;
    /// @inheritdoc IZecuredIntake
    address public immutable signerBond;
    /// @inheritdoc IZecuredIntake
    address public immutable slasher;
    /// @inheritdoc IZecuredIntake
    IRiscZeroVerifier public immutable verifier;
    /// @inheritdoc IZecuredIntake
    address public immutable versions;
    /// @inheritdoc IZecuredIntake
    bool public immutable journalV2;
    /// @inheritdoc IZecuredIntake
    IZcashHeaderRelay public immutable relay;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable finalizeDepth;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable finalizeDelay;
    /// @inheritdoc IZecuredIntake
    uint16 public immutable coverageBps;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable maxTotalZat;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable capPriceMaxAge;
    /// @inheritdoc IZecuredIntake
    address public immutable capOracle;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable maxPause;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable pendingRefundDelay;
    /// @inheritdoc IZecuredIntake
    bool public immutable autoPauseOnCase;
    /// @inheritdoc IZecuredIntake
    uint64 public immutable minRefundZat;

    // The Registry's signer set, strictly increasing (read once at construction).
    address private immutable _signer0;
    address private immutable _signer1;
    address private immutable _signer2;
    address private immutable _signer3;
    address private immutable _signer4;

    WorkTier[] private _tiers;
    mapping(bytes32 vaultId => PendingActivation) private _pending;
    /// @inheritdoc IZecuredIntake
    uint64 public pendingTotal;
    /// @inheritdoc IZecuredIntake
    uint64 public pausedUntil;
    /// @inheritdoc IZecuredIntake
    uint64 public pausedSince;
    /// @inheritdoc IZecuredIntake
    uint256 public guardNonce;
    mapping(bytes32 depositTag => PendingRefund) private _pendingRefunds;
    /// @inheritdoc IZecuredIntake
    uint64 public pendingRefundTotal;

    /// @param c See `IntakeConfig`. The Registry must already be deployed and name this contract its intake.
    constructor(IntakeConfig memory c) EIP712(ZecuredEIP712.INTAKE_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION) {
        if (
            c.registry == address(0) || address(c.verifier) == address(0) || c.versions == address(0)
                || address(c.relay) == address(0)
        ) revert ZeroAddress();
        IZecuredRegistry registry_ = IZecuredRegistry(c.registry);
        if (registry_.intake() != address(this)) revert InvalidConfig();
        // R-18: a pending proof always waits for the relay and for time (K3), and a converted one for its delay.
        if (
            c.finalizeDepth == 0 || c.finalizeDelay == 0 || c.pendingRefundDelay == 0 || c.maxPause == 0
                || c.maxPause > MAX_PAUSE || c.coverageBps > BPS || c.minRefundZat <= registry_.DUST_LIMIT()
                || c.workTiers.length == 0 || c.workTiers.length > MAX_TIERS
        ) revert InvalidConfig();
        // The coverage cap needs bonds and a price; without bonds (tests) it is off.
        if (
            c.coverageBps != 0
                && (c.signerBond == address(0)
                    || c.capOracle == address(0)
                    || c.capPriceMaxAge == 0
                    || c.maxTotalZat == 0)
        ) revert InvalidConfig();
        // Tiers ascend in amount and, R-18, never ask less work for a larger amount.
        uint64 previousAmount;
        uint256 previousWork;
        for (uint256 i; i < c.workTiers.length; ++i) {
            WorkTier memory t = c.workTiers[i];
            if (t.maxAmountZat <= previousAmount || t.minWork == 0 || t.minWork < previousWork) {
                revert InvalidConfig();
            }
            previousAmount = t.maxAmountZat;
            previousWork = t.minWork;
            _tiers.push(t);
        }

        _registry = registry_;
        signerBond = c.signerBond;
        slasher = c.slasher;
        verifier = c.verifier;
        versions = c.versions;
        journalV2 = c.journalV2;
        relay = c.relay;
        finalizeDepth = c.finalizeDepth;
        finalizeDelay = c.finalizeDelay;
        coverageBps = c.coverageBps;
        maxTotalZat = c.maxTotalZat;
        capPriceMaxAge = c.capPriceMaxAge;
        capOracle = c.capOracle;
        maxPause = c.maxPause;
        pendingRefundDelay = c.pendingRefundDelay;
        autoPauseOnCase = c.autoPauseOnCase;
        minRefundZat = c.minRefundZat;
        address[5] memory s = registry_.signers();
        _signer0 = s[0];
        _signer1 = s[1];
        _signer2 = s[2];
        _signer3 = s[3];
        _signer4 = s[4];
    }

    // ------------------------------------------------------------------ deposits

    /// @inheritdoc IZecuredIntake
    /// @dev What the proof itself establishes is in tier2/statement. This contract adds: the seal is from an active
    ///      Deposit version, its router selector and guest image id (Y1, T-07a), and a v2 journal names a branch id that
    ///      version allows; the anchor header is on the relay's best chain with enough work on top for the amount (so
    ///      is the deposit's block, which precedes it); all five signers described the vault in the `VaultOpened`
    ///      whose struct hash the proof computed (which binds the note, and with VaultOpened v2 its tag key, to this
    ///      vault and account without anything about Zcash reaching Base); the tag is unused. Then the deposit lands,
    ///      pending until the relay moves on (K3), with the version that proved it (R-14).
    function attestDepositWithProof(
        bytes calldata journal,
        bytes calldata seal,
        bytes32 imageId,
        bytes[] calldata openedSigs
    ) external {
        DepositJournal memory j = _decodeJournal(journal);
        bytes32 versionId = _checkVersionOf(j, seal, imageId);
        _checkAnchor(j.tipHash, minConfirmationWorkFor(j.amount));
        // All five: the DKG needs every signer, so all five always sign, and one honest signer suffices to refuse a
        // vault key it did not help create.
        _verifySignerSignatures(
            MessageHashUtils.toTypedDataHash(_registry.domainSeparator(), j.vaultOpenedHash), openedSigs, SIGNER_COUNT
        );
        verifier.verify(seal, imageId, sha256(journal));
        if (_registry.depositTagUsed(j.depositTag)) revert DepositTagUsed(j.depositTag);

        (bool refund, RefundReason reason) = _refundReason(j);
        if (refund) _pendRefund(j, reason, versionId);
        else _pend(j, versionId);
    }

    /// @inheritdoc IZecuredIntake
    function finalizeActivation(bytes32 vaultId) external {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        if (!_isReady(p.readyHeight, p.readyTime)) revert ActivationNotReady(vaultId, p.readyHeight, p.readyTime);
        // Still on the best chain, and confirmation work only grows while it stays there.
        _checkAnchor(p.anchor, minConfirmationWorkFor(p.amount));
        // R-14: a version removed meanwhile (a broken guest) proves nothing any more; W-13: nor one whose verifier the
        // router stopped.
        if (!IZecuredVersions(versions).isWorking(p.versionId)) revert VersionNotWorking(p.versionId);
        // E2: not while a violation case about this vault (its deposit spent during the wait) is open.
        if (_caseOpen(vaultId)) revert ViolationCaseOpen(vaultId);
        // W-2: nor while an F1 case about its note is open on any vault, or once one was executed.
        if (_noteContested(p.depositTag)) revert NoteSpendCase(p.depositTag);
        if (_halted()) revert MintingHalted();
        if (signerBond != address(0) && !ISignerBond(signerBond).fullyBonded()) revert SignersNotBonded();
        if (mintingPaused()) revert MintingIsPaused();
        delete _pending[vaultId];
        pendingTotal -= p.amount;
        _registry.activate(vaultId, p.account, p.amount, p.depositTag);
    }

    /// @inheritdoc IZecuredIntake
    function dropPendingActivation(bytes32 vaultId) external {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        // R-17: a case may be about this very note (spent while pending): dropping it would make the note no lineage
        // note and dismiss the case.
        if (_caseOpen(vaultId)) revert ViolationCaseOpen(vaultId);
        if (relay.isInBestChain(p.anchor) && (_versionActive(p.versionId) || _heldToItsProof(vaultId, p.depositTag))) {
            revert AnchorStillOnBestChain(vaultId);
        }
        delete _pending[vaultId];
        pendingTotal -= p.amount;
        // Nothing was minted: the note may be proven again against the chain that won, or with a working version.
        _registry.releaseDeposit(vaultId, p.account, p.depositTag);
        emit PendingActivationDropped(vaultId);
    }

    /// @inheritdoc IZecuredIntake
    function refundPending(bytes32 vaultId) external returns (bytes32 claimId) {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        // R-12: K3 holds in both branches: a deposit a reorganisation may still remove is dropped, not refunded.
        if (!_isReady(p.readyHeight, p.readyTime)) revert ActivationNotReady(vaultId, p.readyHeight, p.readyTime);
        _checkAnchor(p.anchor, minConfirmationWorkFor(p.amount));
        // W-3: a compromised vault's deposit is refunded without its version: the claim is the compensator's.
        if (!_compromised(vaultId)) _checkVersionActive(p.versionId);
        if (!_halted()) {
            // R-13: only the vault's account, once the activation has waited `pendingRefundDelay` past its ready time,
            // whatever holds it (a pause, a missing bond, an open case): no gate can keep a deposit for good.
            if (msg.sender != p.account) revert NotPendingAccount(vaultId, msg.sender);
            if (block.timestamp < p.readyTime + pendingRefundDelay) revert NotRefundableYet(vaultId);
        }
        delete _pending[vaultId];
        pendingTotal -= p.amount;
        claimId = _registry.convertToRefund(vaultId, p.account, p.amount, p.depositTag);
        emit DepositRefunded(vaultId, claimId, RefundReason.PendingConverted);
    }

    /// @inheritdoc IZecuredIntake
    function finalizeRefund(bytes32 depositTag) external returns (bytes32 claimId) {
        PendingRefund memory p = _pendingRefunds[depositTag];
        if (p.account == address(0)) revert NoPendingRefund(depositTag);
        if (!_isReady(p.readyHeight, p.readyTime)) revert RefundNotReady(depositTag, p.readyHeight, p.readyTime);
        _checkAnchor(p.anchor, minConfirmationWorkFor(p.amount));
        // W-3: as `refundPending`.
        if (!_compromised(p.vaultId)) _checkVersionActive(p.versionId);
        delete _pendingRefunds[depositTag];
        pendingRefundTotal -= p.amount;
        // The Registry refuses a pending vault (R-8: the refund waits for it) and a compromised one.
        claimId = IZecuredRefunds(address(_registry)).openReservedRefund(p.vaultId, depositTag);
        emit DepositRefunded(p.vaultId, claimId, p.reason);
    }

    /// @inheritdoc IZecuredIntake
    function dropPendingRefund(bytes32 depositTag) external {
        PendingRefund memory p = _pendingRefunds[depositTag];
        if (p.account == address(0)) revert NoPendingRefund(depositTag);
        if (_caseOpen(p.vaultId)) revert ViolationCaseOpen(p.vaultId); // as R-17
        if (relay.isInBestChain(p.anchor) && (_versionActive(p.versionId) || _heldToItsProof(p.vaultId, depositTag))) {
            revert RefundStillValid(depositTag);
        }
        delete _pendingRefunds[depositTag];
        pendingRefundTotal -= p.amount;
        IZecuredRefunds(address(_registry)).releaseRefund(p.vaultId, depositTag);
        emit PendingRefundDropped(p.vaultId, depositTag);
    }

    // ------------------------------------------------------------------ T-03: pause

    /// @inheritdoc IZecuredIntake
    function pauseMinting(uint64 until, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external {
        _checkGuardMessage(nonce, deadline);
        uint64 maxUntil = uint64(block.timestamp) + maxPause;
        if (until <= block.timestamp || until > maxUntil) revert PauseTooLong(until, maxUntil);
        uint8 bitmap = _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashPauseMinting(until, nonce, deadline)), sigs, THRESHOLD
        );
        guardNonce = nonce + 1;
        if (block.timestamp >= pausedUntil) pausedSince = uint64(block.timestamp);
        pausedUntil = until;
        emit MintingPaused(until, bitmap, nonce);
    }

    /// @inheritdoc IZecuredIntake
    function unpauseMinting(uint256 nonce, uint64 deadline, bytes[] calldata sigs) external {
        _checkGuardMessage(nonce, deadline);
        uint8 bitmap = _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashUnpauseMinting(nonce, deadline)), sigs, THRESHOLD
        );
        guardNonce = nonce + 1;
        pausedUntil = 0;
        emit MintingUnpaused(bitmap, nonce);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredIntake
    function mintingPaused() public view returns (bool) {
        if (block.timestamp < pausedUntil) return true;
        return autoPauseOnCase && slasher != address(0) && IZecuredSlasher(slasher).openTheftCaseCount() != 0;
    }

    /// @inheritdoc IZecuredIntake
    function mintBlock(uint64 amount) external view returns (MintBlock) {
        if (_halted()) return MintBlock.Halted;
        if (!_capRoom(amount)) return MintBlock.CapExceeded;
        if (signerBond != address(0) && !ISignerBond(signerBond).fullyBonded()) return MintBlock.NotBonded;
        if (mintingPaused()) return MintBlock.Paused;
        return MintBlock.None;
    }

    /// @inheritdoc IZecuredIntake
    function capRoom() external view returns (uint256 zatoshi) {
        // Without the coverage cap (tests) nothing is capped, as in `_capRoom`.
        if (coverageBps == 0) return type(uint256).max;
        uint256 locked = _locked();
        uint256 byTotal = locked >= maxTotalZat ? 0 : maxTotalZat - locked;
        (uint256 price, uint256 updatedAt) = IZecuredCapPrice(capOracle).priceHigh();
        if (price == 0 || block.timestamp > updatedAt + capPriceMaxAge) return 0;
        uint256 coveredZat =
            Math.mulDiv(ISignerBond(signerBond).effectiveBond() * coverageBps, ORACLE_PRICE_SCALE, BPS * price);
        uint256 byCoverage = coveredZat > locked ? coveredZat - locked : 0;
        return Math.min(byTotal, byCoverage);
    }

    /// @inheritdoc IZecuredIntake
    function coverageHoldsWith(uint256 effectiveBond) external view returns (bool) {
        if (coverageBps == 0) return true;
        uint256 locked = _locked();
        (uint256 price, uint256 updatedAt) = IZecuredCapPrice(capOracle).priceHigh();
        // A stale price never frees a bond.
        if (block.timestamp > updatedAt + capPriceMaxAge) return false;
        uint256 valueUsd = Math.mulDiv(locked, price, ORACLE_PRICE_SCALE, Math.Rounding.Ceil);
        return valueUsd * BPS <= effectiveBond * coverageBps;
    }

    /// @inheritdoc IZecuredIntake
    function minConfirmationWorkFor(uint64 amount) public view returns (uint256) {
        uint256 n = _tiers.length;
        for (uint256 i; i < n; ++i) {
            WorkTier storage t = _tiers[i];
            if (amount <= t.maxAmountZat) return t.minWork;
        }
        revert AmountAboveLargestTier(amount);
    }

    /// @notice The work tiers, ascending.
    function workTiers() external view returns (WorkTier[] memory) {
        return _tiers;
    }

    /// @inheritdoc IZecuredIntake
    function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory) {
        return _pending[vaultId];
    }

    /// @inheritdoc IZecuredIntake
    function pendingRefund(bytes32 depositTag) external view returns (PendingRefund memory) {
        return _pendingRefunds[depositTag];
    }

    /// @inheritdoc IZecuredIntake
    function registry() external view returns (address) {
        return address(_registry);
    }

    /// @inheritdoc IZecuredIntake
    function hashPauseMinting(uint64 until, uint256 nonce, uint64 deadline) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashPauseMinting(until, nonce, deadline));
    }

    /// @inheritdoc IZecuredIntake
    function hashUnpauseMinting(uint256 nonce, uint64 deadline) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashUnpauseMinting(nonce, deadline));
    }

    /// @inheritdoc IZecuredIntake
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ internal

    /// @dev v1: abi.encode(tipHash, vaultId, account, amount, depositTag, vaultOpenedHash), Activate mode. v2: the same
    ///      words preceded by mode and branch id (checked against the seal's version by the caller). abi.decode rejects
    ///      dirty address and integer words.
    function _decodeJournal(bytes calldata journal) internal view returns (DepositJournal memory j) {
        if (journalV2) {
            if (journal.length != JOURNAL_V2_LENGTH) revert InvalidJournalLength(journal.length);
            j = abi.decode(journal, (DepositJournal));
            if (j.mode > MODE_REFUND) revert InvalidMode(j.mode);
        } else {
            if (journal.length != JOURNAL_V1_LENGTH) revert InvalidJournalLength(journal.length);
            (j.tipHash, j.vaultId, j.account, j.amount, j.depositTag, j.vaultOpenedHash) =
                abi.decode(journal, (bytes32, bytes32, address, uint64, bytes32, bytes32));
        }
    }

    /// @dev T-07a: the seal's router selector and `imageId` are an active Deposit version, and a v2 journal names one of
    ///      its branch ids (D54: the version lists them). Returns the version's id (R-14).
    function _checkVersionOf(DepositJournal memory j, bytes calldata seal, bytes32 imageId)
        internal
        view
        returns (bytes32)
    {
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        Version memory version = IZecuredVersions(versions).activeVersion(VersionKind.Deposit, selector, imageId);
        if (journalV2 && !_branchAllowed(version.branches, j.branchId)) revert BranchNotAllowed(j.branchId);
        return IZecuredVersions(versions).versionIdOf(VersionKind.Deposit, selector, imageId);
    }

    /// @dev A non-zero branch id among the version's.
    function _branchAllowed(uint32[4] memory branches, uint32 b) internal pure returns (bool) {
        return b != 0 && (b == branches[0] || b == branches[1] || b == branches[2] || b == branches[3]);
    }

    /// @dev The header committing to a proof's deposit must be on the relay's best chain with `minWork` on top of it
    ///      (then so is the deposit's block, which precedes it).
    function _checkAnchor(bytes32 tipHash, uint256 minWork) internal view {
        uint256 work = relay.confirmationWork(tipHash);
        if (!relay.isInBestChain(tipHash) || work < minWork) revert AnchorNotConfirmed(tipHash, work);
    }

    /// @dev K3: the relay's best chain reached `readyHeight` and `readyTime` passed (height alone could be fed by an
    ///      attacker, time alone could pass with no feeder running).
    function _isReady(uint256 readyHeight, uint256 readyTime) internal view returns (bool) {
        (, uint256 height,) = relay.bestChainTip();
        return height >= readyHeight && block.timestamp >= readyTime;
    }

    /// @dev When a proof landing now is ready (K3).
    function _readyAt() internal view returns (uint256 readyHeight, uint256 readyTime) {
        (, uint256 height,) = relay.bestChainTip();
        readyHeight = height + finalizeDepth;
        readyTime = block.timestamp + finalizeDelay;
    }

    function _versionActive(bytes32 versionId) internal view returns (bool) {
        return IZecuredVersions(versions).isActive(versionId);
    }

    /// @dev R-14.
    function _checkVersionActive(bytes32 versionId) internal view {
        if (!_versionActive(versionId)) revert VersionNotActive(versionId);
    }

    /// @dev An open violation case about the vault (E2, R-17).
    function _caseOpen(bytes32 vaultId) internal view returns (bool) {
        return slasher != address(0) && IZecuredSlasher(slasher).openCasesOf(vaultId) != 0;
    }

    /// @dev W-2: an F1 case about the note `depositTag` is open (on whichever vault the tag belonged to then), or one
    ///      was executed.
    function _noteContested(bytes32 depositTag) internal view returns (bool) {
        if (slasher == address(0)) return false;
        IZecuredSlasher s = IZecuredSlasher(slasher);
        return s.openF1CasesOf(depositTag) != 0 || s.noteSpendProven(depositTag);
    }

    /// @dev W-3: the slasher compromised the vault (an executed F1 or F2 case).
    function _compromised(bytes32 vaultId) internal view returns (bool) {
        return slasher != address(0) && IZecuredSlasher(slasher).isCompromised(vaultId);
    }

    /// @dev W-2, W-3: a pending item that only its proof's anchor leaving the best chain drops (not a removed version):
    ///      one whose note an F1 case is about, or one of a compromised vault. Dropping either would free the note and
    ///      leave the theft without its victim.
    function _heldToItsProof(bytes32 vaultId, bytes32 depositTag) internal view returns (bool) {
        return _noteContested(depositTag) || _compromised(vaultId);
    }

    /// @dev K3: reserves the vault, its tag and its account until `finalizeActivation`, `dropPendingActivation` or
    ///      `refundPending`.
    function _pend(DepositJournal memory j, bytes32 versionId) internal {
        _registry.reserveDeposit(j.vaultId, j.account, j.amount, j.depositTag);
        pendingTotal += j.amount;
        (uint256 readyHeight, uint256 readyTime) = _readyAt();
        _pending[j.vaultId] = PendingActivation({
            account: j.account,
            amount: j.amount,
            depositTag: j.depositTag,
            anchor: j.tipHash,
            readyHeight: readyHeight,
            readyTime: readyTime,
            versionId: versionId
        });
        emit ActivationPending(j.vaultId, j.account, j.amount, j.depositTag, readyHeight, readyTime);
    }

    /// @dev T-02, R-12: the deposit will land as a Refund claim of the vault's account; until `finalizeRefund` (or
    ///      `dropPendingRefund`) its tag and lineage note are reserved.
    function _pendRefund(DepositJournal memory j, RefundReason reason, bytes32 versionId) internal {
        if (j.amount < minRefundZat) revert RefundTooSmall(j.amount, minRefundZat);
        _registry.reserveRefund(j.vaultId, j.account, j.amount, j.depositTag);
        pendingRefundTotal += j.amount;
        (uint256 readyHeight, uint256 readyTime) = _readyAt();
        _pendingRefunds[j.depositTag] = PendingRefund({
            vaultId: j.vaultId,
            account: j.account,
            amount: j.amount,
            reason: reason,
            anchor: j.tipHash,
            readyHeight: readyHeight,
            readyTime: readyTime,
            versionId: versionId
        });
        emit RefundPending(j.vaultId, j.depositTag, uint8(reason), readyHeight, readyTime);
    }

    /// @dev T-02: whether a proven deposit lands as a refund, and why. Decided without reading the price whenever the
    ///      deposit cannot activate the vault anyway; the cap counts pending activations and refunds, so a paused or
    ///      underbonded network still checks it (I18), and its price must be fresh (`CapPriceStale` otherwise). R-11: a
    ///      new vault the cap has no room for is no longer refunded (that cancelled the vault for good): the proof
    ///      reverts and is sent again later, or the note is proven in Refund mode.
    function _refundReason(DepositJournal memory j) internal view returns (bool, RefundReason) {
        if (j.mode == MODE_REFUND) return (true, RefundReason.ModeRefund);
        (,, VaultStatus status) = _registry.vault(j.vaultId);
        if (status == VaultStatus.Pending) return (true, RefundReason.PendingExists);
        if (status != VaultStatus.None) return (true, RefundReason.VaultNotNew);
        if (_registry.vaultOf(j.account) != bytes32(0)) return (true, RefundReason.AccountHasVault);
        if (_halted()) return (true, RefundReason.Halted);
        if (!_capRoom(j.amount)) revert CapExceeded(j.amount);
        return (false, RefundReason.ModeRefund);
    }

    function _halted() internal view returns (bool) {
        return signerBond != address(0) && ISignerBond(signerBond).halted();
    }

    /// @dev T-01: what the vaults hold for Base (the zZEC supply and every pending claim but dormant refunds) plus what
    ///      is pending here.
    function _locked() internal view returns (uint256) {
        return _registry.lockedZatoshi() + pendingTotal + pendingRefundTotal;
    }

    /// @dev T-01: locked and pending zatoshi plus `amount` stay within the absolute cap and, valued at the high price,
    ///      within `coverageBps` of the effective bond. A stale price reverts: the proof is sent again later.
    function _capRoom(uint64 amount) internal view returns (bool) {
        if (coverageBps == 0) return true;
        uint256 locked = _locked() + amount;
        if (locked > maxTotalZat) return false;
        (uint256 price, uint256 updatedAt) = IZecuredCapPrice(capOracle).priceHigh();
        if (block.timestamp > updatedAt + capPriceMaxAge) revert CapPriceStale(updatedAt);
        uint256 valueUsd = Math.mulDiv(locked, price, ORACLE_PRICE_SCALE, Math.Rounding.Ceil);
        return valueUsd * BPS <= ISignerBond(signerBond).effectiveBond() * coverageBps;
    }

    function _checkGuardMessage(uint256 nonce, uint64 deadline) internal view {
        if (nonce != guardNonce) revert BadGuardNonce(guardNonce);
        if (block.timestamp > deadline) revert SignatureExpired(deadline);
    }

    /// @dev I6: at least `required` signatures; each a valid, non-malleable ECDSA signature of a signer, recovered
    ///      signers strictly increasing. Returns who signed: bit i for signer i.
    function _verifySignerSignatures(bytes32 digest, bytes[] calldata sigs, uint256 required)
        internal
        view
        returns (uint8 signerBitmap)
    {
        if (sigs.length < required) revert InsufficientSignatures(sigs.length);
        address previous = address(0);
        for (uint256 i; i < sigs.length; ++i) {
            address recovered = ECDSA.recoverCalldata(digest, sigs[i]);
            uint256 index = _signerIndex(recovered);
            if (index == SIGNER_COUNT) revert UnknownSigner(recovered);
            if (recovered <= previous) revert SignaturesNotSorted(i);
            previous = recovered;
            // forge-lint: disable-next-line(incorrect-shift,unsafe-typecast)
            signerBitmap |= uint8(1 << index);
        }
    }

    function _signerIndex(address account) internal view returns (uint256) {
        if (account == _signer0) return 0;
        if (account == _signer1) return 1;
        if (account == _signer2) return 2;
        if (account == _signer3) return 3;
        if (account == _signer4) return 4;
        return SIGNER_COUNT;
    }
}
