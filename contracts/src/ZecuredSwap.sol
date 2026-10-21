// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IRiscZeroVerifier} from "./interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "./relay/IZcashHeaderRelay.sol";
import {IZecuredVersions, Version, VersionKind} from "./interfaces/v3/IZecuredVersions.sol";
import {WorkTier} from "./interfaces/v3/IZecuredIntake.sol";
import {
    IZecuredSwap,
    SwapQuote,
    Order,
    OrderStatus,
    PendingSettlement,
    PaymentJournal,
    SwapConfig,
    SIDE_BUY,
    SIDE_SELL
} from "./interfaces/IZecuredSwap.sol";

/// @title ZecuredSwap
/// @notice USDC escrowed on Base, released by a zero-knowledge proof of a shielded ZEC payment made by whoever paid
///         (rules in `IZecuredSwap`). It shares Zecured's Zcash light client: the header relay, the RISC Zero router
///         and the version registry (`Swap` versions). No owner, no upgrade path, no signer.
/// @dev What the proof itself establishes is in tier2/payment-statement: an Ironwood-only v6 transaction, mined under
///      the journal's anchor, creates one note of `zatoshi` whose memo starts with "ZSWP" || orderId, to the receiver
///      whose `keccak256(salt || receiver)` is the journal's commitment. This contract adds: the seal is from an active
///      Swap version (router selector and image id) and the branch id is one it allows; the journal's order,
///      commitment and amount match the order; the anchor is on the relay's best chain with the work its tier asks;
///      then K3 (`finalizeDepth` headers and `finalizeDelay` seconds) before the USDC moves.
contract ZecuredSwap is IZecuredSwap, EIP712, ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// @notice Length of a payment journal (six ABI words).
    uint256 public constant JOURNAL_LENGTH = 6 * 32;
    /// @notice The first word of every payment journal.
    uint8 public constant JOURNAL_KIND = 1;
    /// @notice Most work tiers.
    uint256 public constant MAX_TIERS = 4;
    /// @notice The largest fee a deployment may take (1 %).
    uint16 public constant MAX_FEE_BPS = 100;
    uint256 private constant BPS = 10_000;
    bytes32 public constant SWAP_QUOTE_TYPEHASH = keccak256(
        "SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,uint32 payWindow,uint64 quoteDeadline,uint256 nonce)"
    );

    /// @inheritdoc IZecuredSwap
    address public immutable usdc;
    /// @inheritdoc IZecuredSwap
    address public immutable verifier;
    /// @inheritdoc IZecuredSwap
    address public immutable versions;
    /// @inheritdoc IZecuredSwap
    address public immutable relay;
    /// @inheritdoc IZecuredSwap
    uint64 public immutable finalizeDepth;
    /// @inheritdoc IZecuredSwap
    uint64 public immutable finalizeDelay;
    /// @inheritdoc IZecuredSwap
    uint256 public immutable maxOrderUsdc;
    /// @inheritdoc IZecuredSwap
    uint64 public immutable maxOrderZat;
    /// @inheritdoc IZecuredSwap
    uint32 public immutable minPayWindow;
    /// @inheritdoc IZecuredSwap
    uint32 public immutable maxPayWindow;
    /// @inheritdoc IZecuredSwap
    uint16 public immutable feeBps;
    /// @inheritdoc IZecuredSwap
    address public immutable feeRecipient;

    WorkTier[] private _tiers;
    mapping(bytes32 orderId => Order) private _orders;
    mapping(bytes32 orderId => PendingSettlement) private _pending;
    /// @inheritdoc IZecuredSwap
    uint256 public lockedUsdc;

    /// @param c See `SwapConfig`.
    constructor(SwapConfig memory c) EIP712("ZecuredSwap", "1") {
        if (c.usdc == address(0) || c.verifier == address(0) || c.versions == address(0) || c.relay == address(0)) {
            revert ZeroAddress();
        }
        // The versions' router is the verifier every seal goes through: a router stop then stops this contract too.
        if (IZecuredVersions(c.versions).router() != c.verifier) revert InvalidConfig();
        if (
            c.finalizeDepth == 0 || c.finalizeDelay == 0 || c.maxOrderUsdc == 0 || c.maxOrderZat == 0
                || c.minPayWindow == 0 || c.minPayWindow > c.maxPayWindow || c.feeBps > MAX_FEE_BPS
                || (c.feeBps != 0 && c.feeRecipient == address(0)) || c.workTiers.length == 0
                || c.workTiers.length > MAX_TIERS
        ) revert InvalidConfig();
        uint64 previousAmount;
        uint256 previousWork;
        for (uint256 i; i < c.workTiers.length; ++i) {
            WorkTier memory t = c.workTiers[i];
            // Tiers ascend by amount and never ask less work for more value (R-18).
            if (t.maxAmountZat <= previousAmount || t.minWork == 0 || t.minWork < previousWork) revert InvalidConfig();
            previousAmount = t.maxAmountZat;
            previousWork = t.minWork;
            _tiers.push(t);
        }
        // Every order within the caps has a tier.
        if (c.maxOrderZat > previousAmount) revert InvalidConfig();

        usdc = c.usdc;
        verifier = c.verifier;
        versions = c.versions;
        relay = c.relay;
        finalizeDepth = c.finalizeDepth;
        finalizeDelay = c.finalizeDelay;
        maxOrderUsdc = c.maxOrderUsdc;
        maxOrderZat = c.maxOrderZat;
        minPayWindow = c.minPayWindow;
        maxPayWindow = c.maxPayWindow;
        feeBps = c.feeBps;
        feeRecipient = c.feeRecipient;
    }

    // ------------------------------------------------------------------ orders

    /// @inheritdoc IZecuredSwap
    function open(SwapQuote calldata q, bytes calldata makerSig, bytes32 takerReceiverCommitment)
        external
        nonReentrant
        returns (bytes32 orderId)
    {
        if (q.side > SIDE_SELL) revert InvalidSide(q.side);
        if (q.zatoshi == 0 || q.usdc == 0) revert InvalidAmount();
        if (q.usdc > maxOrderUsdc || q.zatoshi > maxOrderZat) revert OrderTooLarge(q.usdc, q.zatoshi);
        if (q.payWindow < minPayWindow || q.payWindow > maxPayWindow) revert PayWindowOutOfRange(q.payWindow);
        if (block.timestamp > q.quoteDeadline) revert QuoteExpired(q.quoteDeadline);
        // A quote for one taker cannot be taken by anyone else (no front-running of a taker's open).
        if (q.taker != address(0) && q.taker != msg.sender) revert NotQuoteTaker(msg.sender);
        bytes32 receiver;
        if (q.side == SIDE_BUY) {
            if (q.makerReceiverCommitment != bytes32(0)) revert UnexpectedReceiverCommitment();
            receiver = takerReceiverCommitment;
        } else {
            if (takerReceiverCommitment != bytes32(0)) revert UnexpectedReceiverCommitment();
            receiver = q.makerReceiverCommitment;
        }
        if (receiver == bytes32(0)) revert MissingReceiverCommitment();

        orderId = _orderId(q.maker, q.nonce);
        if (_orders[orderId].status != OrderStatus.None) revert NonceUsed(orderId);
        if (!SignatureChecker.isValidSignatureNowCalldata(q.maker, _hashTypedDataV4(_hashQuote(q)), makerSig)) {
            revert BadMakerSignature();
        }

        // A uint64 of seconds outlasts the chain.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 payDeadline = uint64(block.timestamp) + q.payWindow;
        _orders[orderId] = Order({
            maker: q.maker,
            zatoshi: q.zatoshi,
            side: q.side,
            status: OrderStatus.Open,
            taker: msg.sender,
            payDeadline: payDeadline,
            usdc: q.usdc,
            receiverCommitment: receiver
        });
        lockedUsdc += q.usdc;
        emit OrderOpened(orderId, q.maker, msg.sender, q.side, q.zatoshi, q.usdc, receiver, payDeadline);
        _pull(q.side == SIDE_BUY ? msg.sender : q.maker, q.usdc);
    }

    /// @inheritdoc IZecuredSwap
    function proveSettlement(bytes32 orderId, bytes calldata journal, bytes calldata seal, bytes32 imageId)
        external
        nonReentrant
    {
        Order storage o = _orders[orderId];
        if (o.status != OrderStatus.Open) revert OrderNotOpen(orderId);
        if (block.timestamp > o.payDeadline) revert PayDeadlinePassed(orderId, o.payDeadline);
        // A valid pending settlement is never replaced; a stale one (a fake branch's, a removed version's) no longer
        // blocks an honest proof.
        if (_clearStalePending(orderId)) revert SettlementAlreadyPending(orderId);
        (bytes32 anchor, bytes32 versionId) = _verifyPayment(o, orderId, journal, seal, imageId);

        (, uint256 height,) = IZcashHeaderRelay(relay).bestChainTip();
        // Zcash heights and Base seconds fit a uint64 for the chains' lifetimes.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 readyHeight = uint64(height) + finalizeDepth;
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 readyTime = uint64(block.timestamp) + finalizeDelay;
        _pending[orderId] =
            PendingSettlement({anchor: anchor, versionId: versionId, readyHeight: readyHeight, readyTime: readyTime});
        emit SettlementPending(orderId, anchor, readyHeight, readyTime);
    }

    /// @inheritdoc IZecuredSwap
    function finalizeSettlement(bytes32 orderId) external nonReentrant {
        PendingSettlement memory p = _pending[orderId];
        if (p.anchor == bytes32(0)) revert NoPendingSettlement(orderId);
        // A pending settlement exists only for an open order.
        Order storage o = _orders[orderId];
        (, uint256 height,) = IZcashHeaderRelay(relay).bestChainTip();
        if (height < p.readyHeight || block.timestamp < p.readyTime) {
            revert SettlementNotReady(orderId, p.readyHeight, p.readyTime);
        }
        // Still on the best chain, and confirmation work only grows while it stays there.
        _checkAnchor(p.anchor, minConfirmationWorkFor(o.zatoshi));
        if (!IZecuredVersions(versions).isActive(p.versionId)) revert SwapVersionNotActive(p.versionId);

        delete _pending[orderId];
        o.status = OrderStatus.Settled;
        uint256 amount = o.usdc;
        lockedUsdc -= amount;
        uint256 fee = Math.mulDiv(amount, feeBps, BPS);
        emit OrderSettled(orderId);
        IERC20(usdc).safeTransfer(o.side == SIDE_BUY ? o.maker : o.taker, amount - fee);
        if (fee != 0) IERC20(usdc).safeTransfer(feeRecipient, fee);
    }

    /// @inheritdoc IZecuredSwap
    function dropSettlement(bytes32 orderId) external nonReentrant {
        PendingSettlement storage p = _pending[orderId];
        if (p.anchor == bytes32(0)) revert NoPendingSettlement(orderId);
        if (!_stale(p)) revert SettlementStillValid(orderId);
        delete _pending[orderId];
        emit SettlementDropped(orderId);
    }

    /// @inheritdoc IZecuredSwap
    function refund(bytes32 orderId) external nonReentrant {
        Order storage o = _orders[orderId];
        if (o.status != OrderStatus.Open) revert OrderNotOpen(orderId);
        if (block.timestamp <= o.payDeadline) revert PayDeadlineNotPassed(orderId, o.payDeadline);
        // A proof made in time is resolved first: finalized if it stands, dropped (here) if not.
        if (_clearStalePending(orderId)) revert SettlementIsPending(orderId);
        o.status = OrderStatus.Refunded;
        uint256 amount = o.usdc;
        lockedUsdc -= amount;
        emit OrderRefunded(orderId);
        IERC20(usdc).safeTransfer(o.side == SIDE_BUY ? o.taker : o.maker, amount);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredSwap
    function order(bytes32 orderId) external view returns (Order memory) {
        return _orders[orderId];
    }

    /// @inheritdoc IZecuredSwap
    function pendingSettlement(bytes32 orderId) external view returns (PendingSettlement memory) {
        return _pending[orderId];
    }

    /// @inheritdoc IZecuredSwap
    function settlementStale(bytes32 orderId) external view returns (bool) {
        PendingSettlement storage p = _pending[orderId];
        return p.anchor != bytes32(0) && _stale(p);
    }

    /// @inheritdoc IZecuredSwap
    function orderIdOf(address maker, uint256 nonce) external view returns (bytes32) {
        return _orderId(maker, nonce);
    }

    /// @inheritdoc IZecuredSwap
    function hashSwapQuote(SwapQuote calldata q) external view returns (bytes32) {
        return _hashTypedDataV4(_hashQuote(q));
    }

    /// @inheritdoc IZecuredSwap
    function minConfirmationWorkFor(uint64 zatoshi) public view returns (uint256) {
        uint256 n = _tiers.length;
        for (uint256 i; i < n; ++i) {
            WorkTier storage t = _tiers[i];
            if (zatoshi <= t.maxAmountZat) return t.minWork;
        }
        revert AmountAboveLargestTier(zatoshi);
    }

    /// @inheritdoc IZecuredSwap
    function workTiers() external view returns (WorkTier[] memory) {
        return _tiers;
    }

    /// @inheritdoc IZecuredSwap
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ internal

    function _orderId(address maker, uint256 nonce) internal view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), maker, nonce));
    }

    function _hashQuote(SwapQuote calldata q) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                SWAP_QUOTE_TYPEHASH,
                q.maker,
                q.taker,
                q.side,
                q.zatoshi,
                q.usdc,
                q.makerReceiverCommitment,
                q.payWindow,
                q.quoteDeadline,
                q.nonce
            )
        );
    }

    /// @dev The id ZecuredVersions gives a Swap version (`versionIdOf`).
    function _swapVersionId(bytes4 selector, bytes32 imageId) internal pure returns (bytes32) {
        return keccak256(abi.encode(VersionKind.Swap, selector, imageId));
    }

    /// @dev abi.decode rejects dirty integer words.
    function _decodeJournal(bytes calldata journal) internal pure returns (PaymentJournal memory j) {
        if (journal.length != JOURNAL_LENGTH) revert InvalidJournalLength(journal.length);
        j = abi.decode(journal, (PaymentJournal));
        if (j.kind != JOURNAL_KIND) revert InvalidJournalKind(j.kind);
    }

    /// @dev A non-zero branch id among the version's.
    function _branchAllowed(uint32[4] memory branches, uint32 b) internal pure returns (bool) {
        return b != 0 && (b == branches[0] || b == branches[1] || b == branches[2] || b == branches[3]);
    }

    /// @dev The header committing to a payment must be on the relay's best chain with `minWork` on top of it (then so
    ///      is the payment's block, which precedes it).
    function _checkAnchor(bytes32 anchor, uint256 minWork) internal view {
        uint256 work = IZcashHeaderRelay(relay).confirmationWork(anchor);
        if (!IZcashHeaderRelay(relay).isInBestChain(anchor) || work < minWork) revert AnchorNotConfirmed(anchor, work);
    }

    /// @dev A pending settlement no longer stands: its anchor left the best chain, or its Swap version is no longer
    ///      active (removed: R-14).
    function _stale(PendingSettlement storage p) internal view returns (bool) {
        return !IZcashHeaderRelay(relay).isInBestChain(p.anchor) || !IZecuredVersions(versions).isActive(p.versionId);
    }

    /// @dev Drops the order's pending settlement if it no longer stands; true if a valid one remains.
    function _clearStalePending(bytes32 orderId) internal returns (bool validPending) {
        PendingSettlement storage p = _pending[orderId];
        if (p.anchor == bytes32(0)) return false;
        if (!_stale(p)) return true;
        delete _pending[orderId];
        emit SettlementDropped(orderId);
        return false;
    }

    /// @dev What the contract adds to the proof (see the contract's @dev): an active Swap version and a branch id it
    ///      allows; this order (of this contract, on this chain), its receiver and at least its amount; an anchor with
    ///      the work of the order's tier (the value at stake is the order's, whatever the note overpays); the seal.
    ///      Returns the anchor and the version.
    function _verifyPayment(
        Order storage o,
        bytes32 orderId,
        bytes calldata journal,
        bytes calldata seal,
        bytes32 imageId
    ) internal view returns (bytes32 anchor, bytes32 versionId) {
        PaymentJournal memory j = _decodeJournal(journal);
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        Version memory version = IZecuredVersions(versions).activeVersion(VersionKind.Swap, selector, imageId);
        if (!_branchAllowed(version.branches, j.branchId)) revert BranchNotAllowed(j.branchId);
        if (j.orderId != orderId) revert JournalOrderMismatch(j.orderId);
        if (j.receiverCommitment != o.receiverCommitment) revert ReceiverMismatch(j.receiverCommitment);
        if (j.zatoshi < o.zatoshi) revert AmountTooLow(j.zatoshi, o.zatoshi);
        _checkAnchor(j.anchor, minConfirmationWorkFor(o.zatoshi));
        IRiscZeroVerifier(verifier).verify(seal, imageId, sha256(journal));
        return (j.anchor, _swapVersionId(selector, imageId));
    }

    /// @dev Exactly `amount` arrives, or the order does not open: what is locked is what is paid out.
    function _pull(address from, uint256 amount) internal {
        uint256 before = IERC20(usdc).balanceOf(address(this));
        // `from` is the taker itself (Buy) or the maker (Sell), who authorized this pull by signing the quote and by
        // its allowance.
        // forge-lint: disable-next-line(arbitrary-send-erc20)
        IERC20(usdc).safeTransferFrom(from, address(this), amount);
        uint256 received = IERC20(usdc).balanceOf(address(this)) - before;
        if (received != amount) revert TransferMismatch(received, amount);
    }
}
