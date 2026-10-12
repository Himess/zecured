// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {DemoShockOracle} from "../../src/mocks/DemoShockOracle.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";

/// @notice The demo shock wrapper (MOCK / DEMO): the market's price is the base price times an owner-set shock, and
///         no single lowering can break Morpho's one-step assumption (price >= old * LLTV * LIF = 0.704 here).
contract DemoShockOracleTest is Test {
    uint256 internal constant PRICE_1265 = 1265e34; // $1,265 per ZEC, Morpho scale for zZEC / USDC
    MockZecOracle internal base;
    DemoShockOracle internal oracle;
    address internal owner = makeAddr("zecured.shock.owner");

    function setUp() public {
        vm.warp(1_760_000_000);
        base = new MockZecOracle(owner, PRICE_1265);
        oracle = new DemoShockOracle(owner, IOracle(address(base)));
    }

    function test_withoutShockTheMarketRunsOnTheBasePrice() public {
        assertEq(oracle.shockBps(), 10_000);
        assertEq(oracle.price(), PRICE_1265);
        vm.prank(owner);
        base.setPrice(1300e34);
        assertEq(oracle.price(), 1300e34, "the base price passes through");
    }

    function test_aDeepShockIsMadeInSteps() public {
        vm.startPrank(owner);
        vm.expectEmit(address(oracle));
        emit DemoShockOracle.ShockSet(10_000, 7_500);
        oracle.setShock(7_500);
        assertEq(oracle.price(), PRICE_1265 * 3 / 4);
        // Not again right away: the liquidation bot gets time between steps.
        vm.expectRevert(abi.encodeWithSelector(DemoShockOracle.StepTooSoon.selector, block.timestamp + 2 minutes));
        oracle.setShock(5_625);
        vm.warp(block.timestamp + 2 minutes);
        oracle.setShock(5_625); // exactly 75 % of 7,500
        vm.warp(block.timestamp + 2 minutes);
        // Below 75 % of the current shock in one step.
        vm.expectRevert(abi.encodeWithSelector(DemoShockOracle.StepTooDeep.selector, 5_625, 4_218));
        oracle.setShock(4_218);
        vm.stopPrank();
    }

    function test_raisingIsNeverLimited() public {
        vm.startPrank(owner);
        oracle.setShock(7_500);
        oracle.setShock(10_000); // at once, back to the real price
        assertEq(oracle.price(), PRICE_1265);
        vm.stopPrank();
    }

    function test_revert_outOfRange() public {
        vm.startPrank(owner);
        vm.expectRevert(abi.encodeWithSelector(DemoShockOracle.InvalidShock.selector, 0));
        oracle.setShock(0);
        vm.expectRevert(abi.encodeWithSelector(DemoShockOracle.InvalidShock.selector, 10_001));
        oracle.setShock(10_001);
        vm.stopPrank();
    }

    function test_revert_onlyTheOwner() public {
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, address(this)));
        oracle.setShock(9_000);
    }

    function test_aRevertingBaseStopsTheMarket() public {
        vm.mockCallRevert(address(base), abi.encodeCall(IOracle.price, ()), "stale");
        vm.expectRevert("stale");
        oracle.price();
    }

    function test_revert_zeroBase() public {
        vm.expectRevert(DemoShockOracle.ZeroAddress.selector);
        new DemoShockOracle(owner, IOracle(address(0)));
    }

    /// One lowering keeps the effective price above Morpho's bound for this market: LLTV 62.5 %, liquidation
    /// incentive 1 / (0.3 * 0.625 + 0.7) = 1.12676, so 0.704 of the old price.
    function testFuzz_oneLoweringStaysAboveMorphosBound(uint256 from, uint256 to) public {
        from = bound(from, 7_500, 10_000); // reachable from 10,000 in one step
        to = bound(to, 1, from);
        vm.startPrank(owner);
        if (from != 10_000) oracle.setShock(from);
        vm.warp(block.timestamp + 2 minutes);
        uint256 before = oracle.price();
        try oracle.setShock(to) {
            uint256 lif = uint256(1e18) * 1e18 / (0.3e18 * 0.625e18 / 1e18 + 0.7e18); // 1.12676e18
            assertGe(oracle.price() * 1e36, before * 0.625e18 * lif, "below old * LLTV * LIF");
        } catch {}
        vm.stopPrank();
    }
}
