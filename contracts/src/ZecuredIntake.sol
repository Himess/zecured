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
    WorkTier,
    IntakeConfig
} from "./interfaces/v3/IZecuredIntake.sol";
import {IZecuredRegistry, VaultStatus} from "./interfaces/v3/IZecuredRegistry.sol";
import {ISignerBond} from "./interfaces/v3/ISignerBond.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {IZecuredCapPrice} from "./interfaces/v3/IZecuredPriceFeed.sol";
import {ZecuredEIP712} from "./libraries/ZecuredEIP712.sol";

/// @title ZecuredIntake
/// @notice Everything before a vault is active (docs/TIER2.md; design notes T-01, T-02, T-03, T-08, T-09): deposit
///         proofs, pending activations (K3), and the gates of minting (the halt, the bonds, the coverage cap, the
///         pause). Minting needs no signer: a proof lands without one; the signers' only say is the five `VaultOpened`
///         signatures given when the vault opened. Every proven deposit lands, as a pending activation or as a refund:
///         from then on its note is a lineage note, so taking it without a commitment is provable (F1).
/// @dev The Registry trusts this contract for its deposit hooks and re-checks its own invariants in each (I4, I5,
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

    IZecuredRegistry internal immutable _registry;
    /// @inheritdoc IZecuredIntake
    address public immutable signerBond;
    /// @inheritdoc IZecuredIntake
    address public immutable slasher;
    /// @inheritdoc IZecuredIntake
    IRiscZeroVerifier public immutable verifier;
    /// @inheritdoc IZecuredIntake
    bytes32 public immutable imageId;
    /// @inheritdoc IZecuredIntake
    bytes4 public immutable sealSelector;
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

    uint32 private immutable _branch0;
    uint32 private immutable _branch1;
    uint32 private immutable _branch2;
    uint32 private immutable _branch3;

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

    /// @param c See `IntakeConfig`. The Registry must already be deployed and name this contract its intake.
    constructor(IntakeConfig memory c) EIP712(ZecuredEIP712.INTAKE_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION) {
        if (c.registry == address(0) || address(c.verifier) == address(0) || address(c.relay) == address(0)) {
            revert ZeroAddress();
        }
        IZecuredRegistry registry_ = IZecuredRegistry(c.registry);
        if (registry_.intake() != address(this)) revert InvalidConfig();
        if (
            c.imageId == bytes32(0) || c.sealSelector == bytes4(0) || c.finalizeDepth == 0 || c.maxPause == 0
                || c.minRefundZat <= registry_.DUST_LIMIT() || c.workTiers.length == 0 || c.workTiers.length > MAX_TIERS
        ) revert InvalidConfig();
        // The coverage cap needs bonds and a price; without bonds (tests) it is off.
        if (
            c.coverageBps != 0
                && (c.signerBond == address(0)
                    || c.capOracle == address(0)
                    || c.capPriceMaxAge == 0
                    || c.maxTotalZat == 0)
        ) revert InvalidConfig();
        uint64 previous;
        for (uint256 i; i < c.workTiers.length; ++i) {
            WorkTier memory t = c.workTiers[i];
            if (t.maxAmountZat <= previous || t.minWork == 0) revert InvalidConfig();
            previous = t.maxAmountZat;
            _tiers.push(t);
        }
        if (c.journalV2 && c.allowedBranchIds[0] == 0) revert InvalidConfig();

        _registry = registry_;
        signerBond = c.signerBond;
        slasher = c.slasher;
        verifier = c.verifier;
        imageId = c.imageId;
        sealSelector = c.sealSelector;
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
        _branch0 = c.allowedBranchIds[0];
        _branch1 = c.allowedBranchIds[1];
        _branch2 = c.allowedBranchIds[2];
        _branch3 = c.allowedBranchIds[3];
        address[5] memory s = registry_.signers();
        _signer0 = s[0];
        _signer1 = s[1];
        _signer2 = s[2];
        _signer3 = s[3];
        _signer4 = s[4];
    }

    // ------------------------------------------------------------------ deposits

    /// @inheritdoc IZecuredIntake
    /// @dev What the proof itself establishes is in tier2/statement. This contract adds: the seal is for the pinned
    ///      verifier (Y1); the anchor header is on the relay's best chain with enough work on top for the amount (so is
    ///      the deposit's block, which precedes it); all five signers described the vault in the `VaultOpened` whose
    ///      struct hash the proof computed (which binds the note, and with VaultOpened v2 its tag key, to this vault
    ///      and account without anything about Zcash reaching Base); the tag is unused. Then the deposit lands.
    function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes[] calldata openedSigs) external {
        DepositJournal memory j = _decodeJournal(journal);
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        if (selector != sealSelector) revert UnexpectedSealSelector(selector);
        _checkAnchor(j.tipHash, minConfirmationWorkFor(j.amount));
        // All five: the DKG needs every signer, so all five always sign, and one honest signer suffices to refuse a
        // vault key it did not help create.
        _verifySignerSignatures(
            MessageHashUtils.toTypedDataHash(_registry.domainSeparator(), j.vaultOpenedHash), openedSigs, SIGNER_COUNT
        );
        verifier.verify(seal, imageId, sha256(journal));
        if (_registry.depositTagUsed(j.depositTag)) revert DepositTagUsed(j.depositTag);

        (bool refund, RefundReason reason) = _refundReason(j);
        if (refund) _refund(j, reason);
        else _pend(j);
    }

    /// @inheritdoc IZecuredIntake
    function finalizeActivation(bytes32 vaultId) external {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        (, uint256 height,) = relay.bestChainTip();
        if (height < p.readyHeight || block.timestamp < p.readyTime) {
            revert ActivationNotReady(vaultId, p.readyHeight, p.readyTime);
        }
        // Still on the best chain, and confirmation work only grows while it stays there.
        _checkAnchor(p.anchor, minConfirmationWorkFor(p.amount));
        // E2: not while a violation case about this vault (its deposit spent during the wait) is open.
        if (slasher != address(0) && IZecuredSlasher(slasher).openCasesOf(vaultId) != 0) {
            revert ViolationCaseOpen(vaultId);
        }
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
        if (relay.isInBestChain(p.anchor)) revert AnchorStillOnBestChain(vaultId);
        delete _pending[vaultId];
        pendingTotal -= p.amount;
        // Nothing was minted: the note may be proven again against the chain that won.
        _registry.releaseDeposit(vaultId, p.account, p.depositTag);
        emit PendingActivationDropped(vaultId);
    }

    /// @inheritdoc IZecuredIntake
    function refundPending(bytes32 vaultId) external returns (bytes32 claimId) {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        // A reorged-out deposit may not exist: drop it instead.
        if (!relay.isInBestChain(p.anchor)) revert NotRefundableYet(vaultId);
        if (!_halted()) {
            // Only the vault's account, once a pause by signatures has lasted `pendingRefundDelay`.
            if (msg.sender != p.account) revert NotPendingAccount(vaultId, msg.sender);
            if (block.timestamp >= pausedUntil || block.timestamp < uint256(pausedSince) + pendingRefundDelay) {
                revert NotRefundableYet(vaultId);
            }
        }
        delete _pending[vaultId];
        pendingTotal -= p.amount;
        claimId = _registry.convertToRefund(vaultId, p.account, p.amount, p.depositTag);
        emit DepositRefunded(vaultId, claimId, RefundReason.PendingConverted);
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
        uint256 locked = _registry.lockedZatoshi() + pendingTotal;
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
        uint256 locked = _registry.lockedZatoshi() + pendingTotal;
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

    /// @notice The branch ids a v2 journal may name (zero entries unused).
    function allowedBranchIds() external view returns (uint32[4] memory) {
        return [_branch0, _branch1, _branch2, _branch3];
    }

    /// @inheritdoc IZecuredIntake
    function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory) {
        return _pending[vaultId];
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
    ///      words preceded by mode and branch id. abi.decode rejects dirty address and integer words.
    function _decodeJournal(bytes calldata journal) internal view returns (DepositJournal memory j) {
        if (journalV2) {
            if (journal.length != JOURNAL_V2_LENGTH) revert InvalidJournalLength(journal.length);
            j = abi.decode(journal, (DepositJournal));
            if (j.mode > MODE_REFUND) revert InvalidMode(j.mode);
            uint32 b = j.branchId;
            if (b == 0 || (b != _branch0 && b != _branch1 && b != _branch2 && b != _branch3)) {
                revert BranchNotAllowed(b);
            }
        } else {
            if (journal.length != JOURNAL_V1_LENGTH) revert InvalidJournalLength(journal.length);
            (j.tipHash, j.vaultId, j.account, j.amount, j.depositTag, j.vaultOpenedHash) =
                abi.decode(journal, (bytes32, bytes32, address, uint64, bytes32, bytes32));
        }
    }

    /// @dev The header committing to a proof's deposit must be on the relay's best chain with `minWork` on top of it
    ///      (then so is the deposit's block, which precedes it).
    function _checkAnchor(bytes32 tipHash, uint256 minWork) internal view {
        uint256 work = relay.confirmationWork(tipHash);
        if (!relay.isInBestChain(tipHash) || work < minWork) revert AnchorNotConfirmed(tipHash, work);
    }

    /// @dev K3: reserves the vault, its tag and its account until `finalizeActivation`, `dropPendingActivation` or
    ///      `refundPending`.
    function _pend(DepositJournal memory j) internal {
        _registry.reserveDeposit(j.vaultId, j.account, j.amount, j.depositTag);
        pendingTotal += j.amount;
        (, uint256 height,) = relay.bestChainTip();
        uint256 readyHeight = height + finalizeDepth;
        uint256 readyTime = block.timestamp + finalizeDelay;
        _pending[j.vaultId] = PendingActivation({
            account: j.account,
            amount: j.amount,
            depositTag: j.depositTag,
            anchor: j.tipHash,
            readyHeight: readyHeight,
            readyTime: readyTime
        });
        emit ActivationPending(j.vaultId, j.account, j.amount, j.depositTag, readyHeight, readyTime);
    }

    /// @dev T-02: whether a proven deposit lands as a refund, and why. Decided without reading the price whenever the
    ///      deposit cannot activate the vault anyway; the cap counts pending activations, so a paused or underbonded
    ///      network still checks it (I18), and its price must be fresh (`CapPriceStale` otherwise).
    function _refundReason(DepositJournal memory j) internal view returns (bool, RefundReason) {
        if (j.mode == MODE_REFUND) return (true, RefundReason.ModeRefund);
        (,, VaultStatus status) = _registry.vault(j.vaultId);
        if (status == VaultStatus.Pending) return (true, RefundReason.PendingExists);
        if (status != VaultStatus.None) return (true, RefundReason.VaultNotNew);
        if (_registry.vaultOf(j.account) != bytes32(0)) return (true, RefundReason.AccountHasVault);
        if (_halted()) return (true, RefundReason.Halted);
        if (!_capRoom(j.amount)) return (true, RefundReason.CapExceeded);
        return (false, RefundReason.ModeRefund);
    }

    /// @dev T-02: the deposit lands as a Refund claim of the vault's account.
    function _refund(DepositJournal memory j, RefundReason reason) internal {
        if (j.amount < minRefundZat) revert RefundTooSmall(j.amount, minRefundZat);
        bytes32 claimId = _registry.openRefund(j.vaultId, j.account, j.amount, j.depositTag);
        emit DepositRefunded(j.vaultId, claimId, reason);
    }

    function _halted() internal view returns (bool) {
        return signerBond != address(0) && ISignerBond(signerBond).halted();
    }

    /// @dev T-01: locked and pending zatoshi plus `amount` stay within the absolute cap and, valued at the high price,
    ///      within `coverageBps` of the effective bond. A stale price reverts: the proof is sent again later.
    function _capRoom(uint64 amount) internal view returns (bool) {
        if (coverageBps == 0) return true;
        uint256 locked = _registry.lockedZatoshi() + pendingTotal + amount;
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
