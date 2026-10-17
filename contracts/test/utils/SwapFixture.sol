// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {ZecuredSwap} from "../../src/ZecuredSwap.sol";
import {ZecuredVersions} from "../../src/ZecuredVersions.sol";
import {Version, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {WorkTier} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {
    SwapQuote,
    SwapConfig,
    PaymentJournal,
    Order,
    PendingSettlement,
    SIDE_BUY,
    SIDE_SELL
} from "../../src/interfaces/IZecuredSwap.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockVersionsHost} from "../mocks/MockVersionsHost.sol";

/// @notice ZecuredSwap on MOCK USDC, the MOCK header relay (the test sets the best chain and the work) and the MOCK
///         verifier, with a real ZecuredVersions whose five signers are test keys: what the payment guest proves is
///         tier2/payment-statement's subject (its journal is built here as the guest commits it). No fork, no RPC.
abstract contract SwapFixture is Test {
    uint32 internal constant NU7 = 0x77190AD9;
    uint32 internal constant NU6_3 = 0x37A5165B;
    bytes32 internal constant PAYMENT_IMAGE = keccak256("zecured.test.paymentImageId");
    bytes32 internal constant PAYMENT_IMAGE_B = keccak256("zecured.test.paymentImageIdB");
    bytes32 internal constant DEPOSIT_IMAGE = keccak256("zecured.test.depositImageId");
    bytes32 internal constant VIOLATION_IMAGE = keccak256("zecured.test.violationImageId");
    uint64 internal constant VERSION_DELAY = 600;
    // Work tiers: up to 1 ZEC 1,000,000 of work, up to 10 ZEC 3,500,000.
    uint64 internal constant TIER1_ZAT = 100_000_000;
    uint256 internal constant TIER1_WORK = 1_000_000;
    uint64 internal constant TIER2_ZAT = 1_000_000_000;
    uint256 internal constant TIER2_WORK = 3_500_000;
    uint64 internal constant FINALIZE_DEPTH = 6;
    uint64 internal constant FINALIZE_DELAY = 150;
    uint256 internal constant MAX_USDC = 200e6;
    uint64 internal constant MAX_ZAT = 1_000_000_000;
    uint32 internal constant MIN_WINDOW = 1 hours;
    uint32 internal constant PAY_WINDOW = 6 hours;
    uint256 internal constant TIP = 4_500_000;

    MockUSDC internal usdcToken;
    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    ZecuredVersions internal versions;
    ZecuredSwap internal swap;

    uint256[5] internal signerKeys;
    address[5] internal signerSet;
    address internal maker;
    uint256 internal makerKey;
    address internal taker = makeAddr("zecured.swap.taker");
    address internal stranger = makeAddr("zecured.swap.stranger");
    bytes32 internal takerCommitment = keccak256("zecured.swap.takerReceiverCommitment");
    bytes32 internal makerCommitment = keccak256("zecured.swap.makerReceiverCommitment");
    uint256 internal nextNonce = 1;

    function setUp() public virtual {
        vm.warp(1_760_000_000);
        _createSigners();
        (maker, makerKey) = makeAddrAndKey("zecured.swap.maker");
        usdcToken = new MockUSDC(address(this));
        relay = new MockHeaderRelay();
        relay.setTipHeight(TIP);
        verifier = new MockRiscZeroVerifier();
        versions = _deployVersions(address(verifier));
        swap = new ZecuredSwap(_config(address(usdcToken), 0, address(0)));
        usdcToken.mint(taker, 1_000e6);
        usdcToken.mint(maker, 1_000e6);
        vm.prank(taker);
        usdcToken.approve(address(swap), type(uint256).max);
        vm.prank(maker);
        usdcToken.approve(address(swap), type(uint256).max);
    }

    function _createSigners() internal {
        for (uint256 i; i < 5; ++i) {
            (signerSet[i], signerKeys[i]) = makeAddrAndKey(string.concat("zecured.signer.", vm.toString(i)));
        }
        for (uint256 i = 1; i < 5; ++i) {
            for (uint256 j = i; j > 0 && signerSet[j] < signerSet[j - 1]; --j) {
                (signerSet[j], signerSet[j - 1]) = (signerSet[j - 1], signerSet[j]);
                (signerKeys[j], signerKeys[j - 1]) = (signerKeys[j - 1], signerKeys[j]);
            }
        }
    }

    /// A real version registry: a deposit, a violation and two Swap versions (PAYMENT_IMAGE, PAYMENT_IMAGE_B), NU7.
    function _deployVersions(address router) internal returns (ZecuredVersions) {
        MockVersionsHost host = new MockVersionsHost(signerSet, router);
        uint32[4] memory nu7;
        nu7[0] = NU7;
        uint32[4] memory none;
        bytes4 selector = MockRiscZeroVerifier(router).SELECTOR();
        Version[] memory initial = new Version[](4);
        initial[0] = Version(VersionKind.Deposit, selector, DEPOSIT_IMAGE, nu7, 0, false);
        initial[1] = Version(VersionKind.Violation, selector, VIOLATION_IMAGE, none, 0, false);
        initial[2] = Version(VersionKind.Swap, selector, PAYMENT_IMAGE, nu7, 0, false);
        initial[3] = Version(VersionKind.Swap, selector, PAYMENT_IMAGE_B, nu7, 0, false);
        return new ZecuredVersions(address(host), router, VERSION_DELAY, VERSION_DELAY, VERSION_DELAY, initial);
    }

    function _tiers() internal pure returns (WorkTier[] memory t) {
        t = new WorkTier[](2);
        t[0] = WorkTier({maxAmountZat: TIER1_ZAT, minWork: TIER1_WORK});
        t[1] = WorkTier({maxAmountZat: TIER2_ZAT, minWork: TIER2_WORK});
    }

    function _config(address token, uint16 feeBps, address feeRecipient) internal view returns (SwapConfig memory c) {
        c = SwapConfig({
            usdc: token,
            verifier: address(verifier),
            versions: address(versions),
            relay: address(relay),
            workTiers: _tiers(),
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY,
            maxOrderUsdc: MAX_USDC,
            maxOrderZat: MAX_ZAT,
            minPayWindow: MIN_WINDOW,
            maxPayWindow: PAY_WINDOW,
            feeBps: feeBps,
            feeRecipient: feeRecipient
        });
    }

    // ------------------------------------------------------------------ quotes

    /// A quote of `maker` for `taker` (Sell: the maker's receiver commitment).
    function _quote(uint8 side, uint64 zatoshi, uint256 usdc) internal returns (SwapQuote memory q) {
        q = SwapQuote({
            maker: maker,
            taker: taker,
            side: side,
            zatoshi: zatoshi,
            usdc: usdc,
            makerReceiverCommitment: side == SIDE_SELL ? makerCommitment : bytes32(0),
            payWindow: PAY_WINDOW,
            quoteDeadline: uint64(block.timestamp + 120),
            nonce: nextNonce++
        });
    }

    function _sign(ZecuredSwap s, SwapQuote memory q, uint256 key) internal view returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 sig) = vm.sign(key, s.hashSwapQuote(q));
        return abi.encodePacked(r, sig, v);
    }

    /// The taker opens `q` signed by the maker; returns the order id.
    function _open(SwapQuote memory q) internal returns (bytes32) {
        bytes memory sig = _sign(swap, q, makerKey);
        vm.prank(q.taker == address(0) ? taker : q.taker);
        return swap.open(q, sig, q.side == SIDE_BUY ? takerCommitment : bytes32(0));
    }

    function _openBuy(uint64 zatoshi, uint256 usdc) internal returns (bytes32) {
        return _open(_quote(SIDE_BUY, zatoshi, usdc));
    }

    function _openSell(uint64 zatoshi, uint256 usdc) internal returns (bytes32) {
        return _open(_quote(SIDE_SELL, zatoshi, usdc));
    }

    // ------------------------------------------------------------------ proofs

    function _anchor(bytes32 orderId) internal pure returns (bytes32) {
        return keccak256(abi.encode("zecured.swap.anchor", orderId));
    }

    /// The journal the payment guest commits (tier2/payment-statement, `PaymentJournal::abi_encode`).
    function _journal(bytes32 orderId, bytes32 commitment, uint64 zatoshi, bytes32 anchor, uint32 branch)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(
            PaymentJournal({
                kind: 1,
                branchId: branch,
                anchor: anchor,
                orderId: orderId,
                receiverCommitment: commitment,
                zatoshi: zatoshi
            })
        );
    }

    /// The honest payment proof of `orderId`: its journal, the MOCK seal of `image`, the anchor on the best chain with
    /// the tier's work.
    function _proof(bytes32 orderId, bytes32 image) internal returns (bytes memory journal, bytes memory seal) {
        Order memory o = _order(orderId);
        bytes32 anchor = _anchor(orderId);
        relay.setBlock(anchor, true, swap.minConfirmationWorkFor(o.zatoshi));
        journal = _journal(orderId, o.receiverCommitment, o.zatoshi, anchor, NU7);
        seal = verifier.mockSeal(image, sha256(journal));
    }

    function _prove(bytes32 orderId) internal {
        (bytes memory journal, bytes memory seal) = _proof(orderId, PAYMENT_IMAGE);
        swap.proveSettlement(orderId, journal, seal, PAYMENT_IMAGE);
    }

    /// The relay reaches the pending settlement's ready height and its ready time passes.
    function _makeReady(bytes32 orderId) internal {
        PendingSettlement memory p = swap.pendingSettlement(orderId);
        relay.setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
    }

    function _order(bytes32 orderId) internal view returns (Order memory) {
        return swap.order(orderId);
    }

    // ------------------------------------------------------------------ versions

    function _threeSigs(bytes32 digest) internal view returns (bytes[] memory sigs) {
        sigs = new bytes[](3);
        uint256[3] memory idx = [uint256(0), 2, 4];
        for (uint256 i; i < 3; ++i) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKeys[idx[i]], digest);
            sigs[i] = abi.encodePacked(r, s, v);
        }
    }

    /// Three signers remove a version at once (a broken guest).
    function _removeVersion(bytes32 image) internal {
        bytes32 id = versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), image);
        uint256 nonce = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        versions.removeVersion(id, nonce, deadline, _threeSigs(versions.hashRemoveVersion(id, nonce, deadline)));
    }
}
