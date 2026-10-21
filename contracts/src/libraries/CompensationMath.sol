// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";

/// @title CompensationMath
/// @notice The pure arithmetic of `ZecuredCompensator` (design notes T-04), kept apart so that it can be checked on its
///         own (unit fuzzing and Halmos, invariant I25). Amounts are USDC units; zatoshi are priced with Morpho's oracle
///         scale (`zat * price / 1e36` is USDC units); ratios are 1e18 = paid in full.
/// @dev Every rounding favours the pool: what a layer pays out never exceeds what `allocate` set aside for it.
///      - `take` fills the layers in order: a layer takes its whole total while the funds last, the first layer they
///        do not cover takes everything that is left, and every later layer takes nothing.
///      - `ratioOf` turns what a layer took into its ratio: 1e18 when it took its whole total (an empty layer too),
///        else `taken * 1e18 / total` rounded down.
///      - Payouts are `scale(item, ratio)`, rounded down, where each item is rounded down too, so the payouts of a layer
///        sum to at most `scale(total, ratio)`, which is at most what the layer took.
library CompensationMath {
    /// @notice Ratio of a layer paid in full.
    uint256 internal constant WAD = 1e18;
    /// @notice Morpho's oracle scale.
    uint256 internal constant PRICE_SCALE = 1e36;
    uint256 internal constant BPS = 10_000;
    /// @notice The lenders' layer counts a victim's debt at tally time plus this allowance (1 %), for the interest that
    ///         accrues until the vault is settled: without it, a vault whose share is exactly its tally-time debt (an
    ///         underwater vault, or any vault when layer 4 gets nothing) could never be repaid in full and retired.
    ///         1 % covers a week at about 50 % APR. What the allowance does not use goes back to the pool (`shareOf`).
    uint256 internal constant INTEREST_ALLOWANCE_BPS = 100;

    /// @notice Splits `funds` over the three layers, in order (I25).
    /// @return r2 Ratio of layer 2 (third-party claimants).
    /// @return r3 Ratio of layer 3 (lenders).
    /// @return r4 Ratio of layer 4 (owners).
    /// @return used What the incident sets aside: what the three layers took, at most `funds`.
    function allocate(uint256 funds, uint256 layer2, uint256 layer3, uint256 layer4)
        internal
        pure
        returns (uint256 r2, uint256 r3, uint256 r4, uint256 used)
    {
        (uint256 taken2, uint256 taken3, uint256 taken4) = take(funds, layer2, layer3, layer4);
        r2 = ratioOf(taken2, layer2);
        r3 = ratioOf(taken3, layer3);
        r4 = ratioOf(taken4, layer4);
        used = taken2 + taken3 + taken4;
    }

    /// @notice What each layer takes from `funds`, in order: at most its total, and only what the earlier layers left.
    function take(uint256 funds, uint256 layer2, uint256 layer3, uint256 layer4)
        internal
        pure
        returns (uint256 taken2, uint256 taken3, uint256 taken4)
    {
        taken2 = Math.min(funds, layer2);
        uint256 left = funds - taken2;
        taken3 = Math.min(left, layer3);
        left -= taken3;
        taken4 = Math.min(left, layer4);
    }

    /// @notice The ratio of a layer that took `taken` of its `total` (`taken <= total`): 1e18 if it took it all,
    ///         else `taken * 1e18 / total`, rounded down, so `scale(total, ratio) <= taken`.
    function ratioOf(uint256 taken, uint256 total) internal pure returns (uint256) {
        if (taken >= total) return WAD;
        return Math.mulDiv(taken, WAD, total);
    }

    /// @notice USDC value of `zat` zatoshi at `price` (Morpho scale), rounded down.
    function valueOf(uint256 zat, uint256 price) internal pure returns (uint256) {
        return Math.mulDiv(zat, price, PRICE_SCALE);
    }

    /// @notice `amount * ratio / WAD`, rounded down: what a total of `amount` in a layer of ratio `ratio` is paid.
    function scale(uint256 amount, uint256 ratio) internal pure returns (uint256) {
        return Math.mulDiv(amount, ratio, WAD);
    }

    /// @notice A victim's part of layer 3: its debt at tally time plus the interest allowance (rounded up).
    function lendersOf(uint256 debt) internal pure returns (uint256) {
        return debt + Math.mulDiv(debt, INTEREST_ALLOWANCE_BPS, BPS, Math.Rounding.Ceil);
    }

    /// @notice A victim's equity at `price`, its part of layer 4: its collateral's value less its part of layer 3,
    ///         floored at zero. A victim's two parts sum to max(collateral value, debt plus allowance).
    function equityAt(uint256 outstanding, uint256 debt, uint256 price) internal pure returns (uint256) {
        return Math.saturatingSub(valueOf(outstanding, price), lendersOf(debt));
    }

    /// @notice A victim's share of a finalized incident, `lendersOf(debt) * r3 + equityAt(...) * r4`, and `cap`: once
    ///         the debt `D` it has at settlement is repaid from the share, its account keeps at most `cap - D` (floored
    ///         at zero) of what is left, and the rest goes back to the pool.
    /// @dev `cap` is the account's equity without the allowance, `max(0, value - debt) * r4`, plus the tally-time `debt`.
    ///      So interest and new borrowing since the tally come out of the account's equity first and the allowance only
    ///      covers what the equity cannot; debt repaid directly since the tally comes back to the account; and the
    ///      allowance that was not needed returns to the pool, except the part that was the account's own equity.
    function shareOf(uint256 outstanding, uint256 debt, uint256 price, uint256 r3, uint256 r4)
        internal
        pure
        returns (uint256 share, uint256 cap)
    {
        uint256 value = valueOf(outstanding, price);
        uint256 lenders = lendersOf(debt);
        share = scale(lenders, r3) + scale(Math.saturatingSub(value, lenders), r4);
        cap = scale(Math.saturatingSub(value, debt), r4) + debt;
    }
}
