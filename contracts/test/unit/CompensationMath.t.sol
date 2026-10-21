// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {CompensationMath} from "../../src/libraries/CompensationMath.sol";

/// @notice Unit and fuzz tests of the compensator's arithmetic (design notes T-04, invariant I25). The order of the
///         layers is also checked symbolically (`test/halmos/CompensationMath.t.sol`); the rounding is fuzzed here.
contract CompensationMathTest is Test {
    uint256 internal constant WAD = 1e18;
    uint256 internal constant USD = 1e6;
    uint256 internal constant ZEC = 1e8;
    uint256 internal constant PRICE = 40e34; // $40 per ZEC, Morpho scale for zZEC (8 decimals) / USDC (6 decimals)

    // ------------------------------------------------------------------ allocate

    function test_allocate_examples() public pure {
        uint256 r2;
        uint256 r3;
        uint256 r4;
        uint256 used;
        // Full.
        (r2, r3, r4, used) = CompensationMath.allocate(1_000, 100, 200, 300);
        assertEq(r2, WAD);
        assertEq(r3, WAD);
        assertEq(r4, WAD);
        assertEq(used, 600);
        // Partial in layer 2: nothing for the next layers.
        (r2, r3, r4, used) = CompensationMath.allocate(50, 100, 200, 300);
        assertEq(r2, WAD / 2);
        assertEq(r3, 0);
        assertEq(r4, 0);
        assertEq(used, 50);
        // Partial in layer 4.
        (r2, r3, r4, used) = CompensationMath.allocate(450, 100, 200, 300);
        assertEq(r2, WAD);
        assertEq(r3, WAD);
        assertEq(r4, WAD / 2);
        assertEq(used, 450);
        // Zero funds.
        (r2, r3, r4, used) = CompensationMath.allocate(0, 100, 0, 300);
        assertEq(r2, 0);
        assertEq(r3, WAD, "an empty layer is paid in full: nothing to pay");
        assertEq(r4, 0);
        assertEq(used, 0);
        // An empty layer in the middle takes nothing.
        (r2, r3, r4, used) = CompensationMath.allocate(200, 100, 0, 300);
        assertEq(r3, WAD);
        assertEq(r4, WAD / 3);
        assertEq(used, 200);
        // Funds that exactly cover a layer leave nothing for the next one.
        (r2, r3, r4, used) = CompensationMath.allocate(300, 100, 200, 300);
        assertEq(r3, WAD);
        assertEq(r4, 0);
        assertEq(used, 300);
    }

    function testFuzz_allocate_fillsTheLayersInOrderWithinTheFunds(uint256 funds, uint256 l2, uint256 l3, uint256 l4)
        public
        pure
    {
        funds = bound(funds, 0, 1e30);
        l2 = bound(l2, 0, 1e30);
        l3 = bound(l3, 0, 1e30);
        l4 = bound(l4, 0, 1e30);
        (uint256 r2, uint256 r3, uint256 r4, uint256 used) = CompensationMath.allocate(funds, l2, l3, l4);
        assertLe(used, funds);
        assertLe(r2, WAD);
        assertLe(r3, WAD);
        assertLe(r4, WAD);
        // I25: a layer is paid only once every earlier layer is paid in full.
        if (r2 < WAD) {
            assertTrue(l3 == 0 || r3 == 0);
            assertTrue(l4 == 0 || r4 == 0);
        }
        if (r3 < WAD) assertTrue(l4 == 0 || r4 == 0);
        // What every layer can pay out stays within what was set aside.
        assertLe(CompensationMath.scale(l2, r2) + CompensationMath.scale(l3, r3) + CompensationMath.scale(l4, r4), used);
        if (l2 + l3 + l4 <= funds) {
            assertEq(used, l2 + l3 + l4);
            assertEq(r2 + r3 + r4, 3 * WAD);
        } else {
            assertEq(used, funds, "a layer it does not cover takes all that is left");
        }
    }

    // ------------------------------------------------------------------ ratioOf and scale: the rounding

    /// A partly paid layer's ratio is below 1e18, and it pays out at most what it took, losing at most
    /// `1 + total / 1e18` units to rounding.
    function testFuzz_ratioOf_aLayerPaysAtMostWhatItTook(uint256 taken, uint256 total) public pure {
        total = bound(total, 1, type(uint128).max);
        taken = bound(taken, 0, total);
        uint256 ratio = CompensationMath.ratioOf(taken, total);
        assertLe(ratio, WAD);
        assertEq(ratio == WAD, taken == total);
        uint256 paid = CompensationMath.scale(total, ratio);
        assertLe(paid, taken);
        assertLe(taken - paid, 1 + total / WAD);
    }

    /// The items of a layer (claims, victims) are each paid on their own: together, at most what the layer took.
    function testFuzz_ratioOf_itemsOfALayerSumWithinWhatItTook(uint256 remaining, uint64[6] memory items) public pure {
        uint256 total;
        for (uint256 i; i < items.length; ++i) {
            total += items[i];
        }
        remaining = bound(remaining, 0, 1e24);
        (uint256 taken,,) = CompensationMath.take(remaining, total, 0, 0);
        uint256 ratio = CompensationMath.ratioOf(taken, total);
        uint256 paid;
        for (uint256 i; i < items.length; ++i) {
            paid += CompensationMath.scale(items[i], ratio);
        }
        assertLe(paid, taken);
    }

    // ------------------------------------------------------------------ a victim's parts and share

    function test_lendersOf_addsTheInterestAllowanceRoundedUp() public pure {
        assertEq(CompensationMath.lendersOf(0), 0);
        assertEq(CompensationMath.lendersOf(1), 2);
        assertEq(CompensationMath.lendersOf(200 * USD), 202 * USD);
    }

    function test_equityAt_isTheValueLessTheLendersPart() public pure {
        assertEq(CompensationMath.equityAt(10 * ZEC, 200 * USD, PRICE), 198 * USD, "$400 - $202");
        assertEq(CompensationMath.equityAt(10 * ZEC, 400 * USD, PRICE), 0, "under water: none");
        assertEq(CompensationMath.valueOf(10 * ZEC, PRICE), 400 * USD);
    }

    function test_shareOf_examples() public pure {
        // Everything paid in full: the share is the collateral's value, and so is the cap.
        (uint256 share, uint256 cap) = CompensationMath.shareOf(10 * ZEC, 200 * USD, PRICE, WAD, WAD);
        assertEq(share, 400 * USD);
        assertEq(cap, 400 * USD);
        // Under water, paid in full: the debt plus the allowance; the account keeps nothing beyond the debt.
        (share, cap) = CompensationMath.shareOf(5 * ZEC, 300 * USD, PRICE, WAD, WAD);
        assertEq(share, 303 * USD);
        assertEq(cap, 300 * USD);
        // Lenders in full, owners at one half: the allowance is not the owner's.
        (share, cap) = CompensationMath.shareOf(10 * ZEC, 100 * USD, PRICE, WAD, WAD / 2);
        assertEq(share, 101 * USD + 149_500_000);
        assertEq(cap, 150 * USD + 100 * USD);
        // Lenders at one half, owners nothing.
        (share, cap) = CompensationMath.shareOf(10 * ZEC, 200 * USD, PRICE, WAD / 2, 0);
        assertEq(share, 101 * USD);
        assertEq(cap, 200 * USD);
    }

    /// With the lenders paid in full, the share always reaches the cap (to one unit of rounding): an account whose debt
    /// is repaid from its share keeps its whole equity at the owners' ratio, `cap - debt`, and only the allowance it
    /// did not need goes back to the pool. The allowance itself never reaches the owner: `cap` does not count it.
    function testFuzz_shareOf_lendersInFullLeaveTheOwnerItsWholeEquity(
        uint64 outstanding,
        uint256 debt,
        uint256 price,
        uint256 r4
    ) public pure {
        price = bound(price, 1e33, 1e38);
        debt = bound(debt, 0, 1e15);
        r4 = bound(r4, 0, WAD);
        (uint256 share, uint256 cap) = CompensationMath.shareOf(outstanding, debt, price, WAD, r4);
        assertGe(share + 1, cap);
        assertGe(share, CompensationMath.lendersOf(debt), "the debt and its allowance, in full");
        uint256 value = CompensationMath.valueOf(outstanding, price);
        assertEq(cap - debt, value > debt ? CompensationMath.scale(value - debt, r4) : 0);
    }
}
