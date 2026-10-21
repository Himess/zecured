// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {ZecuredSwap} from "../../src/ZecuredSwap.sol";
import {ZecuredVersions} from "../../src/ZecuredVersions.sol";
import {VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {
    SwapQuote,
    Order,
    OrderStatus,
    PendingSettlement,
    SIDE_BUY,
    SIDE_SELL
} from "../../src/interfaces/IZecuredSwap.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {SwapFixture} from "../utils/SwapFixture.sol";

/// Drives ZecuredSwap through every path: open (both sides, three takers), prove (two Swap versions), a reorg of a
/// proof's anchor, drop, the removal of a Swap version, finalize, refund and time. Every action checks its own
/// preconditions, so none reverts (`fail_on_revert`); the ghosts record what moved.
contract SwapHandler is Test {
    ZecuredSwap internal immutable swap;
    MockUSDC internal immutable usdc;
    MockHeaderRelay internal immutable relay;
    MockRiscZeroVerifier internal immutable verifier;
    ZecuredVersions internal immutable versions;
    address internal immutable maker;
    uint256 internal immutable makerKey;
    uint256[3] internal removerKeys;
    bytes32 internal immutable imageA;
    bytes32 internal immutable imageB;
    address[3] internal takers;

    bytes32[] public orderIds;
    mapping(bytes32 orderId => OrderStatus) public terminal;
    uint256 public ghostIn;
    uint256 public ghostOut;
    uint256 public ghostFinalized;
    uint256 public ghostRefunded;
    bool public versionBRemoved;
    uint256 internal nonce = 1_000;
    uint256 internal anchors;

    constructor(SwapFixtureView memory f) {
        swap = f.swap;
        usdc = f.usdc;
        relay = f.relay;
        verifier = f.verifier;
        versions = f.versions;
        maker = f.maker;
        makerKey = f.makerKey;
        removerKeys = f.removerKeys;
        imageA = f.imageA;
        imageB = f.imageB;
        for (uint256 i; i < 3; ++i) {
            takers[i] = makeAddr(string.concat("zecured.swap.invariant.taker.", vm.toString(i)));
            vm.prank(takers[i]);
            usdc.approve(address(swap), type(uint256).max);
        }
    }

    function orderCount() external view returns (uint256) {
        return orderIds.length;
    }

    // ------------------------------------------------------------------ actions

    function open(uint256 seed, bool sell) external {
        uint64 zatoshi = uint64(bound(seed, 1, swap.maxOrderZat()));
        uint256 amount = bound(seed >> 64, 1, swap.maxOrderUsdc());
        address taker = takers[(seed >> 160) % 3];
        uint8 side = sell ? SIDE_SELL : SIDE_BUY;
        SwapQuote memory q = SwapQuote({
            maker: maker,
            taker: taker,
            side: side,
            zatoshi: zatoshi,
            usdc: amount,
            makerReceiverCommitment: sell ? keccak256(abi.encode("maker", nonce)) : bytes32(0),
            payWindow: swap.maxPayWindow(),
            quoteDeadline: uint64(block.timestamp + 60),
            nonce: ++nonce
        });
        usdc.mint(sell ? maker : taker, amount);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(makerKey, swap.hashSwapQuote(q));
        vm.prank(taker);
        bytes32 id = swap.open(q, abi.encodePacked(r, s, v), sell ? bytes32(0) : keccak256(abi.encode("taker", nonce)));
        orderIds.push(id);
        ghostIn += amount;
    }

    function prove(uint256 seed, bool withB) external {
        (bool found, bytes32 id) = _pick(seed);
        if (!found) return;
        Order memory o = swap.order(id);
        if (o.status != OrderStatus.Open || block.timestamp > o.payDeadline) return;
        if (swap.pendingSettlement(id).anchor != bytes32(0) && !swap.settlementStale(id)) return;
        bytes32 image = withB && !versionBRemoved ? imageB : imageA;
        bytes32 anchor = keccak256(abi.encode("zecured.swap.invariant.anchor", ++anchors));
        relay.setBlock(anchor, true, swap.minConfirmationWorkFor(o.zatoshi));
        bytes memory journal = abi.encode(uint8(1), uint32(0x77190AD9), anchor, id, o.receiverCommitment, o.zatoshi);
        swap.proveSettlement(id, journal, verifier.mockSeal(image, sha256(journal)), image);
    }

    function finalize(uint256 seed) external {
        (bool found, bytes32 id) = _pick(seed);
        if (!found) return;
        PendingSettlement memory p = swap.pendingSettlement(id);
        if (p.anchor == bytes32(0) || swap.settlementStale(id)) return;
        (, uint256 height,) = relay.bestChainTip();
        if (height < p.readyHeight) relay.setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
        uint256 amount = swap.order(id).usdc;
        swap.finalizeSettlement(id);
        terminal[id] = OrderStatus.Settled;
        ghostOut += amount;
        ++ghostFinalized;
    }

    /// The anchor of a pending settlement leaves the relay's best chain (a fake branch loses).
    function reorg(uint256 seed) external {
        (bool found, bytes32 id) = _pick(seed);
        if (!found) return;
        PendingSettlement memory p = swap.pendingSettlement(id);
        if (p.anchor == bytes32(0)) return;
        relay.setBlock(p.anchor, false, 0);
    }

    function drop(uint256 seed) external {
        (bool found, bytes32 id) = _pick(seed);
        if (!found || !swap.settlementStale(id)) return;
        swap.dropSettlement(id);
    }

    /// Three signers remove the second Swap version at once (a broken guest): its pending settlements go stale.
    function removeVersionB() external {
        if (versionBRemoved) return;
        bytes32 versionId = versions.versionIdOf(VersionKind.Swap, verifier.SELECTOR(), imageB);
        uint256 n = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes32 digest = versions.hashRemoveVersion(versionId, n, deadline);
        bytes[] memory sigs = new bytes[](3);
        for (uint256 i; i < 3; ++i) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(removerKeys[i], digest);
            sigs[i] = abi.encodePacked(r, s, v);
        }
        versions.removeVersion(versionId, n, deadline, sigs);
        versionBRemoved = true;
    }

    function refund(uint256 seed) external {
        (bool found, bytes32 id) = _pick(seed);
        if (!found) return;
        Order memory o = swap.order(id);
        if (o.status != OrderStatus.Open) return;
        if (block.timestamp <= o.payDeadline) vm.warp(uint256(o.payDeadline) + 1);
        if (swap.pendingSettlement(id).anchor != bytes32(0) && !swap.settlementStale(id)) return;
        swap.refund(id);
        terminal[id] = OrderStatus.Refunded;
        ghostOut += o.usdc;
        ++ghostRefunded;
    }

    function warp(uint256 secs) external {
        vm.warp(block.timestamp + bound(secs, 1, 3 hours));
    }

    function _pick(uint256 seed) internal view returns (bool, bytes32) {
        if (orderIds.length == 0) return (false, bytes32(0));
        return (true, orderIds[seed % orderIds.length]);
    }
}

/// What the handler needs from the fixture.
struct SwapFixtureView {
    ZecuredSwap swap;
    MockUSDC usdc;
    MockHeaderRelay relay;
    MockRiscZeroVerifier verifier;
    ZecuredVersions versions;
    address maker;
    uint256 makerKey;
    uint256[3] removerKeys;
    bytes32 imageA;
    bytes32 imageB;
}

/// Invariants of the escrow (docs/INVARIANTS.md I36 to I39, "locked USDC = open orders"): the USDC the contract holds
/// is exactly the sum of its open orders, every unit that came in either is still locked or went out once (to the
/// payer's counterparty or back to the locker), a pending settlement exists only for an open order, and a settled or
/// refunded order never changes again.
contract ZecuredSwapInvariantTest is SwapFixture {
    SwapHandler internal handler;

    function setUp() public override {
        super.setUp();
        handler = new SwapHandler(
            SwapFixtureView({
                swap: swap,
                usdc: usdcToken,
                relay: relay,
                verifier: verifier,
                versions: versions,
                maker: maker,
                makerKey: makerKey,
                removerKeys: [signerKeys[0], signerKeys[2], signerKeys[4]],
                imageA: PAYMENT_IMAGE,
                imageB: PAYMENT_IMAGE_B
            })
        );
        usdcToken.transferOwnership(address(handler));
        // The fixture's own balances stay out of the accounting.
        uint256 makerBalance = usdcToken.balanceOf(maker);
        vm.prank(maker);
        usdcToken.transfer(address(0xdead), makerBalance);
        targetContract(address(handler));
    }

    /// The handler's actions really move orders (they are not all early returns): a fixed walk through every path.
    function test_handlerWalksEveryPath() public {
        handler.open(1, false);
        handler.open(2, true);
        handler.open(3, false);
        handler.prove(0, false);
        handler.finalize(0);
        assertEq(handler.ghostFinalized(), 1);
        handler.prove(1, true);
        handler.removeVersionB();
        assertTrue(swap.settlementStale(handler.orderIds(1)));
        handler.drop(1);
        assertEq(swap.pendingSettlement(handler.orderIds(1)).anchor, bytes32(0));
        handler.prove(2, false);
        handler.reorg(2);
        handler.refund(2);
        assertEq(handler.ghostRefunded(), 1);
        handler.refund(1);
        assertEq(handler.ghostRefunded(), 2);
        assertEq(swap.lockedUsdc(), 0);
        assertEq(usdcToken.balanceOf(address(swap)), 0);
        invariant_everyUnitMovesOnce();
        invariant_pendingOnlyForOpenOrders_andTerminalStatesStay();
    }

    function _openSum() internal view returns (uint256 sum, uint256 open) {
        uint256 n = handler.orderCount();
        for (uint256 i; i < n; ++i) {
            Order memory o = swap.order(handler.orderIds(i));
            if (o.status == OrderStatus.Open) {
                sum += o.usdc;
                ++open;
            }
        }
    }

    function invariant_lockedUsdcIsTheOpenOrders() public view {
        (uint256 sum,) = _openSum();
        assertEq(swap.lockedUsdc(), sum);
    }

    function invariant_theBalanceIsWhatIsLocked() public view {
        assertEq(usdcToken.balanceOf(address(swap)), swap.lockedUsdc());
    }

    function invariant_everyUnitMovesOnce() public view {
        assertEq(handler.ghostIn() - handler.ghostOut(), usdcToken.balanceOf(address(swap)));
        (, uint256 open) = _openSum();
        assertEq(open + handler.ghostFinalized() + handler.ghostRefunded(), handler.orderCount());
    }

    function invariant_pendingOnlyForOpenOrders_andTerminalStatesStay() public view {
        uint256 n = handler.orderCount();
        for (uint256 i; i < n; ++i) {
            bytes32 id = handler.orderIds(i);
            Order memory o = swap.order(id);
            if (swap.pendingSettlement(id).anchor != bytes32(0)) assertEq(uint8(o.status), uint8(OrderStatus.Open));
            OrderStatus t = handler.terminal(id);
            if (t != OrderStatus.None) assertEq(uint8(o.status), uint8(t));
            assertTrue(o.status != OrderStatus.None);
        }
    }
}
