// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ISettlementAdapter} from "../../src/interfaces/ISettlementAdapter.sol";
import {VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Regressions for the contracts review of 2026-10-11 (docs/SECURITY_REVIEW.md, "Contracts").
contract ReviewRegressionsTest is ZecuredFixture {
    using MorphoBalancesLib for IMorpho;

    function _setPriceForLtv(address account, uint256 ltvBps, uint64 collateral) internal {
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        oracle.setPrice(debt * 10_000 * 1e36 / (ltvBps * collateral));
    }

    /// L-1: a position at the D14 remainder floor (100,000 zat) borrowed near LLTV used to be unliquidatable between
    /// 62.5 % and 88.75 % LTV: repaying all its debt seizes less than MIN_SEIZE, and seizing all of it needs more
    /// shares than exist. A seizure below MIN_SEIZE is now accepted when it clears the whole debt.
    function test_smallPositionIsLiquidatableByRepayingAllItsDebt() public {
        address account = makeAddr("review.l1.account");
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        registry.requestPartialRelease(vaultId, 900_000, RELEASE_COMMITMENT);
        assertEq(_outstanding(vaultId), 100_000);
        _borrow(account, 24_000); // 60 % of the remainder's value at $40
        _fundAndApproveAdapter(liquidator, 1_000_000);

        for (uint256 ltvBps = 6_300; ltvBps <= 8_800; ltvBps += 500) {
            uint256 snap = vm.snapshotState();
            _setPriceForLtv(account, ltvBps, 100_000);
            uint256 shares = _position(account).borrowShares;
            vm.prank(liquidator);
            (uint256 seized,) = adapter.liquidate(account, 0, shares, LIQUIDATOR_COMMITMENT);
            assertLt(seized, MIN_SEIZE_ZAT, "below the floor");
            assertEq(_position(account).borrowShares, 0, "debt cleared");
            assertEq(_outstanding(vaultId), 100_000 - seized, "the rest stays with the borrower");
            assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
            vm.revertToState(snap);
        }
    }

    /// L-1: the floor still applies to a partial liquidation that leaves debt behind.
    function test_smallSeizureThatLeavesDebtIsStillRefused() public {
        address account = makeAddr("review.l1.partial");
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        _borrow(account, 240_000); // 60 % of 1,000,000 zat at $40
        _setPriceForLtv(account, 7_000, 1_000_000);
        _fundAndApproveAdapter(liquidator, 1_000_000);
        vm.prank(liquidator);
        vm.expectPartialRevert(ISettlementAdapter.SeizeBelowMinimum.selector);
        adapter.liquidate(account, 50_000, 0, LIQUIDATOR_COMMITMENT);
        assertEq(_outstanding(vaultId), VAULT_001);
    }

    /// L-2: the borrower can borrow more after signing a close request; the executor's own cap now bounds what it
    /// repays.
    function test_closeRespectsTheExecutorsRepayCap() public {
        (address account, uint256 key) = makeAddrAndKey("review.l2.account");
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        _borrow(account, 200_000);
        uint256 quotedRepay = MORPHO.expectedBorrowAssets(params, account);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 15_001, 7);
        bytes memory sig = _signClose(key, req);
        usdc.mint(marketMaker, 10_000_000);
        vm.prank(marketMaker);
        usdc.approve(address(adapter), type(uint256).max);

        _borrow(account, 1_040_000); // after signing

        uint256 cap = quotedRepay * 10_050 / 10_000; // the quote plus 0.5 % for interest
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.RepayAboveMaximum.selector, debt, cap));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, cap, 0);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "nothing closed");

        // With a cap covering the actual debt the same request still executes.
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, debt, 0);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
    }

    /// Y7: the price can rise between the market maker's quote and its transaction, shrinking the zatoshi its USDC buys;
    /// its own floor `minMmAmount` bounds that.
    function test_closeRespectsTheExecutorsZecFloor() public {
        (address account, uint256 key) = makeAddrAndKey("review.y7.account");
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        _borrow(account, 200_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 8);
        bytes memory sig = _signClose(key, req);
        usdc.mint(marketMaker, 10_000_000);
        vm.prank(marketMaker);
        usdc.approve(address(adapter), type(uint256).max);

        // The quote at $40: debt * (1 + 1 %) / price, rounded up (the Adapter's formula).
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        uint64 quotedMm = uint64(Math.mulDiv(debt * 10_100, ORACLE_PRICE_SCALE, 10_000 * PRICE_40, Math.Rounding.Ceil));
        oracle.setPrice(PRICE_40 * 11 / 10); // +10 % before the market maker's transaction lands
        uint256 debtNow = MORPHO.expectedBorrowAssets(params, account);
        uint64 mmNow = uint64(
            Math.mulDiv(debtNow * 10_100, ORACLE_PRICE_SCALE, 10_000 * (PRICE_40 * 11 / 10), Math.Rounding.Ceil)
        );
        assertLt(mmNow, quotedMm);
        vm.prank(marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(ISettlementAdapter.MarketMakerAmountBelowMinimum.selector, mmNow, quotedMm)
        );
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, quotedMm);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "nothing closed");

        // A floor the actual share meets: the same request executes.
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, mmNow);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
    }

    uint256 internal constant MIN_SEIZE_ZAT = 100_000;
}
