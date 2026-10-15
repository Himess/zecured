// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {CompensationMath} from "../../src/libraries/CompensationMath.sol";

/// @notice Symbolic checks of the compensator's allocation (design notes T-04, invariant I25), for every amount below
///         2^128 USDC units (far above any real one). `CompensationMath.allocate` is `take` followed by `ratioOf` on
///         each layer; together the two checks give I25's order: a layer with a non-zero ratio took something (C2), so
///         every earlier layer took its whole total (C1) and has ratio 1e18 (C2).
/// @dev Not checked here: the rounding of a partly paid layer (`ratioOf(taken, total)` below 1e18 and
///      `scale(total, ratio) <= taken`). Both go through OpenZeppelin's 512-bit `Math.mulDiv`, which the default solver
///      cannot decide in reasonable time (it ran out of memory even on 32-bit inputs); the unit tests fuzz them
///      (`test/unit/CompensationMath.t.sol`).
contract CompensationMathCheck is Test {
    uint256 internal constant WAD = 1e18;

    /// C1: the layers take from the funds in order, never more than their totals, never more than the funds, and they
    ///     hold nothing back: either every layer took its whole total, or the funds are used up.
    function check_takeFillsTheLayersInOrderWithinTheFunds(uint128 funds, uint128 l2, uint128 l3, uint128 l4)
        public
        pure
    {
        (uint256 t2, uint256 t3, uint256 t4) = CompensationMath.take(funds, l2, l3, l4);
        assert(t2 <= l2 && t3 <= l3 && t4 <= l4);
        assert(t2 + t3 + t4 <= funds);
        if (t3 != 0) assert(t2 == l2);
        if (t4 != 0) assert(t3 == l3);
        uint256 total = uint256(l2) + l3 + l4;
        if (total <= funds) assert(t2 == l2 && t3 == l3 && t4 == l4);
        else assert(t2 + t3 + t4 == funds);
    }

    /// C2: a layer that took its whole total (an empty layer too) is paid in full; a layer that took nothing of a
    ///     non-zero total is paid nothing.
    function check_aWholeLayerIsPaidInFullAndAnUntouchedOneNothing(uint128 total) public pure {
        assert(CompensationMath.ratioOf(total, total) == WAD);
        if (total != 0) assert(CompensationMath.ratioOf(0, total) == 0);
    }
}
