// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, Market, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IIrm} from "morpho-blue/src/interfaces/IIrm.sol";
import {MathLib} from "morpho-blue/src/libraries/MathLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice R-24 against the real Morpho Blue and AdaptiveCurveIRM on a Base Sepolia fork: a position becomes
///         liquidatable through interest alone, with no other activity on the market. Morpho's stored totals still say
///         it is healthy (what the liquidator used to read); the totals after interest (MorphoBalancesLib's
///         `expectedMarketBalances`: the IRM's `borrowRateView` times the elapsed time, `wTaylorCompounded`) say it is
///         not, and the liquidation goes through without anyone accruing first. The exact numbers are pinned: the bots'
///         Rust test (`bots/src/chain.rs`, `r24_fork_vector`) replays them through the bots' own accrual and liquidation
///         plan.
contract AccruedDebtTest is ZecuredFixture {
    using MathLib for uint256;
    using SharesMathLib for uint256;
    using MorphoBalancesLib for IMorpho;

    address internal account = makeAddr("zecured.r24.account");

    uint256 internal constant BORROWED = 1_249_000; // 62.45 % of 0.05 ZEC at $40
    uint256 internal constant ELAPSED = 365 days;

    /// What the bots read: the stored market, the IRM's rate on it, the position and the price; and what Morpho does.
    struct Vector {
        Market stored;
        uint256 rate;
        uint256 totalSupplyAssets;
        uint256 totalSupplyShares;
        uint256 totalBorrowAssets;
        uint256 totalBorrowShares;
        uint256 shares;
        uint256 collateral;
        uint256 price;
        uint256 maxBorrow;
        uint256 storedDebt;
        uint256 accruedDebt;
    }

    function _read(uint256 shares) internal view returns (Vector memory v) {
        v.stored = MORPHO.market(marketId);
        v.rate = IIrm(ADAPTIVE_CURVE_IRM).borrowRateView(params, v.stored);
        (v.totalSupplyAssets, v.totalSupplyShares, v.totalBorrowAssets, v.totalBorrowShares) =
            MORPHO.expectedMarketBalances(params);
        Position memory p = _position(account);
        v.shares = shares;
        v.collateral = p.collateral;
        v.price = oracle.price();
        v.maxBorrow = v.collateral.mulDivDown(v.price, ORACLE_PRICE_SCALE).wMulDown(LLTV);
        v.storedDebt = shares.toAssetsUp(v.stored.totalBorrowAssets, v.stored.totalBorrowShares);
        v.accruedDebt = shares.toAssetsUp(v.totalBorrowAssets, v.totalBorrowShares);
    }

    function test_r24_liquidatableThroughInterestAlone() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        uint256 shares = _borrow(account, BORROWED);
        uint256 start = block.timestamp;

        skip(ELAPSED); // nobody touches the market
        Vector memory v = _read(shares);
        assertEq(v.stored.lastUpdate, start, "no accrual since the borrow");
        assertLe(v.storedDebt, v.maxBorrow, "on the stored totals: healthy");
        uint256 interest = uint256(v.stored.totalBorrowAssets).wMulDown(v.rate.wTaylorCompounded(ELAPSED));
        assertEq(v.totalBorrowAssets, v.stored.totalBorrowAssets + interest, "rate x elapsed, wTaylorCompounded");
        assertEq(v.totalSupplyAssets, v.stored.totalSupplyAssets + interest);
        assertGt(v.accruedDebt, v.maxBorrow, "with the interest since: liquidatable");

        // The liquidation goes through on interest alone: the Adapter (Morpho) accrues before it checks.
        _fundAndApproveAdapter(liquidator, v.accruedDebt); // exact approval: Morpho must charge the accrued debt
        vm.prank(liquidator);
        (uint256 seized, uint256 repaid) = adapter.liquidate(account, 0, shares, LIQUIDATOR_COMMITMENT);
        assertEq(repaid, v.accruedDebt, "Morpho charged exactly the accrued debt");
        assertEq(_position(account).borrowShares, 0);
        assertEq(_outstanding(vaultId), VAULT_005 - seized);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), VAULT_005 - seized, "I1");
        _assertPinnedVector(v, seized, repaid);
    }

    /// The vector `bots/src/chain.rs` (`r24_fork_vector`) replays: keep both in step.
    function _assertPinnedVector(Vector memory v, uint256 seized, uint256 repaid) internal view {
        assertEq(block.timestamp, 1_823_200_288, "now");
        assertEq(v.stored.totalSupplyAssets, 1_000_000_000_000);
        assertEq(v.stored.totalSupplyShares, 1e18);
        assertEq(v.stored.totalBorrowAssets, 1_249_000);
        assertEq(v.stored.totalBorrowShares, 1_249_000_000_000);
        assertEq(v.stored.lastUpdate, 1_791_664_288);
        assertEq(v.stored.fee, 0);
        assertEq(v.rate, 85_220_420, "about 0.27 % a year: the AdaptiveCurveIRM at a utilization near zero");
        assertEq(v.totalSupplyAssets, 1_000_000_003_361);
        assertEq(v.totalSupplyShares, 1e18);
        assertEq(v.totalBorrowAssets, 1_252_361);
        assertEq(v.totalBorrowShares, 1_249_000_000_000);
        assertEq(v.shares, 1_249_000_000_000);
        assertEq(v.collateral, 5_000_000);
        assertEq(v.price, 40e34);
        assertEq(v.storedDebt, 1_249_000);
        assertEq(v.accruedDebt, 1_252_361);
        assertEq(v.maxBorrow, 1_250_000);
        assertEq(seized, 3_527_772);
        assertEq(repaid, 1_252_361);
    }
}
