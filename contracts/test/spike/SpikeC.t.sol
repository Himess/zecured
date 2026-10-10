// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {stdError} from "forge-std/StdError.sol";
import {IMorpho, MarketParams, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {EventsLib} from "morpho-blue/src/libraries/EventsLib.sol";
import {MathLib, WAD} from "morpho-blue/src/libraries/MathLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {UtilsLib} from "morpho-blue/src/libraries/UtilsLib.sol";
import {
    ORACLE_PRICE_SCALE,
    LIQUIDATION_CURSOR,
    MAX_LIQUIDATION_INCENTIVE_FACTOR
} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {SpikeCBase} from "./SpikeCBase.sol";
import {SpikeCollateral} from "./SpikeCollateral.sol";

/// @notice Spike C: Morpho Blue on a Base Sepolia fork. The account never holds the collateral token; a trusted
///         registry supplies/withdraws collateral on its behalf; a third party repays through Morpho callbacks.
contract SpikeCTest is SpikeCBase {
    using MathLib for uint256;
    using SharesMathLib for uint256;
    using MorphoBalancesLib for IMorpho;

    System internal s; // guarded system (collateral token prototypes the zZEC transfer guard)

    address internal lender = makeAddr("zecured.spikeC.lender");
    address internal account = makeAddr("zecured.spikeC.account");
    address internal payout = makeAddr("zecured.spikeC.payout");
    address internal marketMaker = makeAddr("zecured.spikeC.marketMaker");
    address internal liquidator = makeAddr("zecured.spikeC.liquidator");
    address internal attacker = makeAddr("zecured.spikeC.attacker");

    uint256 internal constant COLLATERAL = 10 * ZEC; // $400 at $40
    uint256 internal constant BORROW = 160 * USDC; // 40% LTV (the UI's safe max)
    uint256 internal constant LENDER_SUPPLY = 100_000 * USDC;
    uint256 internal constant PRICE_25_50_USD = 2550e32; // LTV 160/255 = 62.75% > LLTV

    function setUp() public {
        _selectBaseSepoliaFork();
        s = _deploySystem(true);
        _supplyAsLender(s, lender, LENDER_SUPPLY);
        vm.label(lender, "lender");
        vm.label(account, "account");
        vm.label(payout, "payout");
        vm.label(marketMaker, "marketMaker");
        vm.label(liquidator, "liquidator");
        vm.label(attacker, "attacker");
        assertEq(account.code.length, 0, "account must be fresh");
        assertEq(account.balance, 0, "account must hold no ETH");
    }

    // ------------------------------------------------------------------ helpers

    function _position(address user) internal view returns (Position memory) {
        return MORPHO.position(s.id, user);
    }

    function _assertAccountHoldsNoCollateral() internal view {
        assertEq(s.collateral.balanceOf(account), 0, "account must never hold the collateral token");
    }

    function _assertNoPendingExpectation() internal view {
        (address recipient, uint256 maxAmount) = s.collateral.pendingExpectation();
        assertEq(recipient, address(0), "expectation flag must be consumed");
        assertEq(maxAmount, 0, "expectation flag must be consumed");
    }

    /// The account's whole first session: two authorizations and a borrow straight to another receiver.
    function _authorizeBothAndBorrow(uint256 assets) internal returns (uint256 shares) {
        vm.startPrank(account);
        MORPHO.setAuthorization(address(s.registry), true);
        MORPHO.setAuthorization(address(s.adapter), true);
        uint256 borrowed;
        (borrowed, shares) = MORPHO.borrow(s.marketParams, assets, 0, account, payout);
        vm.stopPrank();
        assertEq(borrowed, assets);
    }

    function _liquidationIncentiveFactor() internal pure returns (uint256) {
        return
            UtilsLib.min(MAX_LIQUIDATION_INCENTIVE_FACTOR, WAD.wDivDown(WAD - LIQUIDATION_CURSOR.wMulDown(WAD - LLTV)));
    }

    // ------------------------------------------------------------------ market + lender

    function test_01_createMarketOnRealMorpho() public {
        assertTrue(MORPHO.isIrmEnabled(ADAPTIVE_CURVE_IRM), "AdaptiveCurveIRM enabled");
        assertTrue(MORPHO.isLltvEnabled(LLTV), "LLTV 62.5% enabled");

        MarketParams memory onchain = MORPHO.idToMarketParams(s.id);
        assertEq(onchain.loanToken, address(s.usdc));
        assertEq(onchain.collateralToken, address(s.collateral));
        assertEq(onchain.oracle, address(s.oracle));
        assertEq(onchain.irm, ADAPTIVE_CURVE_IRM);
        assertEq(onchain.lltv, LLTV);
        assertEq(s.collateral.decimals(), 8);
        assertEq(s.usdc.decimals(), 6);

        Market memory m = MORPHO.market(s.id);
        assertEq(m.lastUpdate, block.timestamp);
        assertEq(m.fee, 0);

        vm.expectRevert(bytes("market already created"));
        MORPHO.createMarket(s.marketParams);
    }

    function test_02_lenderSuppliesUsdc() public view {
        Market memory m = MORPHO.market(s.id);
        assertEq(m.totalSupplyAssets, LENDER_SUPPLY);
        assertEq(m.totalBorrowAssets, 0);
        assertGt(_position(lender).supplyShares, 0);
        assertEq(MORPHO.expectedSupplyAssets(s.marketParams, lender), LENDER_SUPPLY);
        assertEq(s.usdc.balanceOf(address(MORPHO)), LENDER_SUPPLY);
    }

    // ------------------------------------------------------------------ registry supplies on behalf

    function test_03_registrySuppliesCollateralOnBehalfOfFreshAccount() public {
        _assertAccountHoldsNoCollateral();

        vm.expectEmit(true, true, true, true, address(MORPHO));
        emit EventsLib.SupplyCollateral(s.id, address(s.registry), account, COLLATERAL);
        s.registry.depositFor(account, COLLATERAL);

        Position memory p = _position(account);
        assertEq(p.collateral, COLLATERAL);
        assertEq(p.borrowShares, 0);
        assertEq(p.supplyShares, 0);
        _assertAccountHoldsNoCollateral();
        assertEq(s.collateral.balanceOf(address(s.registry)), 0, "registry holds it only inside depositFor");
        assertEq(s.collateral.balanceOf(address(MORPHO)), COLLATERAL);
        assertEq(s.collateral.totalSupply(), COLLATERAL);
        // supplyCollateral(onBehalf) needs no authorization from the account: it never sent a transaction.
        assertFalse(MORPHO.isAuthorized(account, address(s.registry)));
        assertEq(account.code.length, 0);
        assertEq(account.balance, 0);
    }

    // ------------------------------------------------------------------ borrow, self-repay, registry release

    function test_04_authorizeBorrowToOtherReceiver_repay_registryReleases() public {
        s.registry.depositFor(account, COLLATERAL);
        uint256 shares = _authorizeBothAndBorrow(BORROW);

        assertTrue(MORPHO.isAuthorized(account, address(s.registry)));
        assertTrue(MORPHO.isAuthorized(account, address(s.adapter)));
        assertEq(s.usdc.balanceOf(payout), BORROW, "borrow receiver can be any address");
        assertEq(s.usdc.balanceOf(account), 0, "account never touched the USDC");
        assertEq(_position(account).borrowShares, shares);
        _assertAccountHoldsNoCollateral();

        // Release while debt remains: Morpho's health check stops the registry.
        vm.prank(account);
        vm.expectRevert(bytes("insufficient collateral"));
        s.registry.requestRelease();

        // Interest accrues on the real AdaptiveCurveIRM.
        skip(30 days);
        uint256 debt = MORPHO.expectedBorrowAssets(s.marketParams, account);
        assertGt(debt, BORROW, "interest accrued");
        emit log_named_decimal_uint("debt after 30 days (USDC)", debt, 6);

        // The account gets USDC back (borrowed amount + interest from elsewhere) and repays by shares.
        vm.prank(payout);
        assertTrue(s.usdc.transfer(account, BORROW));
        s.usdc.mint(account, debt - BORROW);
        vm.startPrank(account);
        s.usdc.approve(address(MORPHO), debt); // exact: repay-by-shares pulls exactly the expected amount
        (uint256 repaid, uint256 repaidShares) = MORPHO.repay(s.marketParams, 0, shares, account, "");
        vm.stopPrank();
        assertEq(repaid, debt, "MorphoBalancesLib.expectedBorrowAssets == assets pulled by repay(shares)");
        assertEq(repaidShares, shares);
        assertEq(_position(account).borrowShares, 0);
        assertEq(s.usdc.balanceOf(account), 0);
        _assertAccountHoldsNoCollateral();

        // The registry withdraws the collateral to itself (account's authorization) and burns it.
        vm.prank(account);
        uint256 released = s.registry.requestRelease();
        assertEq(released, COLLATERAL);
        assertEq(_position(account).collateral, 0);
        assertEq(s.collateral.totalSupply(), 0, "released collateral is burned");
        assertEq(s.collateral.balanceOf(address(MORPHO)), 0);
        _assertAccountHoldsNoCollateral();
        _assertNoPendingExpectation();
    }

    // ------------------------------------------------------------------ third-party close through onMorphoRepay

    function test_05_adapterClosesWithMarketMakerUsdcPulledInsideOnMorphoRepay() public {
        s.registry.depositFor(account, COLLATERAL);
        _authorizeBothAndBorrow(BORROW);
        skip(7 days);
        uint256 debt = MORPHO.expectedBorrowAssets(s.marketParams, account);
        assertGt(debt, BORROW);

        s.usdc.mint(marketMaker, 1_000 * USDC);
        uint256 mmBefore = s.usdc.balanceOf(marketMaker);
        vm.startPrank(marketMaker);
        s.usdc.approve(address(s.adapter), debt); // exact allowance: any over-pull would revert
        (uint256 repaid, uint256 withdrawn) = s.adapter.closeWithCollateral(account);
        vm.stopPrank();

        assertEq(repaid, debt);
        assertEq(withdrawn, COLLATERAL);
        assertEq(mmBefore - s.usdc.balanceOf(marketMaker), repaid, "market maker paid exactly repaidAssets");
        assertEq(s.usdc.allowance(marketMaker, address(s.adapter)), 0);
        assertEq(s.usdc.balanceOf(address(s.adapter)), 0, "adapter keeps no USDC dust");
        assertEq(s.usdc.allowance(address(s.adapter), address(MORPHO)), 0, "approval fully consumed");

        // What the adapter saw inside onMorphoRepay: Morpho had already burned the debt shares, the collateral was
        // still in Morpho, and the callback amount equals what repay() returned.
        assertEq(s.adapter.seenRepaidAssets(), repaid);
        assertEq(s.adapter.seenAccountBorrowShares(), 0);
        assertEq(s.adapter.seenAccountCollateral(), COLLATERAL);
        assertEq(s.adapter.seenAdapterCollateralBalance(), 0);

        Position memory p = _position(account);
        assertEq(p.borrowShares, 0);
        assertEq(p.collateral, 0);
        assertEq(s.collateral.balanceOf(address(s.adapter)), COLLATERAL, "collateral withdrawn to the adapter");
        assertEq(s.usdc.balanceOf(payout), BORROW, "borrower keeps the borrowed USDC");
        _assertAccountHoldsNoCollateral();
        _assertNoPendingExpectation();

        // recordClose stand-in: the registry burns what the adapter received.
        s.adapter.burnHeld();
        assertEq(s.collateral.totalSupply(), 0);
    }

    function test_06_adapterCannotWithdrawWithoutTheAccountsAuthorization() public {
        s.registry.depositFor(account, COLLATERAL);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(s.registry), true); // registry only, adapter NOT authorized
        MORPHO.borrow(s.marketParams, BORROW, 0, account, payout);
        vm.stopPrank();
        uint256 sharesBefore = _position(account).borrowShares;

        s.usdc.mint(marketMaker, 1_000 * USDC);
        vm.startPrank(marketMaker);
        s.usdc.approve(address(s.adapter), type(uint256).max);
        vm.expectRevert(bytes("unauthorized"));
        s.adapter.closeWithCollateral(account);
        vm.stopPrank();

        // Atomic: the repay that ran before the failing withdrawCollateral is rolled back too.
        assertEq(_position(account).borrowShares, sharesBefore);
        assertEq(_position(account).collateral, COLLATERAL);
        assertEq(s.usdc.balanceOf(marketMaker), 1_000 * USDC);
        _assertAccountHoldsNoCollateral();
    }

    // ------------------------------------------------------------------ liquidation through the adapter

    function test_07_partialLiquidationViaAdapter_seizesAbout70pct_thenRegistryReleasesTheRest() public {
        s.registry.depositFor(account, COLLATERAL);
        uint256 shares = _authorizeBothAndBorrow(BORROW);

        // Healthy at $40: liquidation is refused.
        s.usdc.mint(liquidator, 1_000 * USDC);
        vm.prank(liquidator);
        s.usdc.approve(address(s.adapter), type(uint256).max);
        vm.prank(liquidator);
        vm.expectRevert(bytes("position is healthy"));
        s.adapter.liquidate(account, 0, shares);

        // Price drop: $40 -> $25.50, LTV = 160 / 255 = 62.75% > 62.5%.
        s.oracle.setPrice(PRICE_25_50_USD);

        // Expected values with Morpho's own formulas (no time passed, so no accrual).
        uint256 lif = _liquidationIncentiveFactor();
        assertApproxEqAbs(lif, 1.1268e18, 0.0001e18, "LIF = 1 / (1 - 0.3 * (1 - 0.625))");
        Market memory m = MORPHO.market(s.id);
        uint256 expectedSeized = shares.toAssetsDown(m.totalBorrowAssets, m.totalBorrowShares).wMulDown(lif)
            .mulDivDown(ORACLE_PRICE_SCALE, PRICE_25_50_USD);
        uint256 expectedRepaid = shares.toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);

        uint256 liquidatorBefore = s.usdc.balanceOf(liquidator);
        vm.prank(liquidator);
        (uint256 seized, uint256 repaid) = s.adapter.liquidate(account, 0, shares); // repay ALL debt shares
        assertEq(seized, expectedSeized);
        assertEq(repaid, expectedRepaid);
        assertEq(liquidatorBefore - s.usdc.balanceOf(liquidator), repaid, "liquidator paid exactly repaidAssets");

        // ~70% of the collateral is seized when all debt is repaid just past LLTV (LTV * LIF = 0.6275 * 1.1268).
        uint256 seizedFraction = seized * WAD / COLLATERAL;
        emit log_named_decimal_uint("seized fraction", seizedFraction, 18);
        assertGe(seizedFraction, 0.7e18);
        assertLe(seizedFraction, 0.71e18);
        assertApproxEqRel(seizedFraction, LLTV.wMulDown(lif), 0.005e18, "close to LLTV * LIF = 70.4%");

        // Callback ordering: the seized collateral was ALREADY in the adapter when onMorphoLiquidate ran.
        assertEq(s.adapter.seenAdapterCollateralBalance(), seized);
        assertEq(s.adapter.seenRepaidAssets(), repaid);
        assertEq(s.adapter.seenAccountBorrowShares(), 0);
        assertEq(s.adapter.seenAccountCollateral(), COLLATERAL - seized);

        Position memory p = _position(account);
        assertEq(p.borrowShares, 0, "all debt repaid, no bad debt realized");
        assertEq(p.collateral, COLLATERAL - seized);
        _assertAccountHoldsNoCollateral();
        _assertNoPendingExpectation();

        // The remaining ~29% can still be released through the registry.
        vm.prank(account);
        uint256 released = s.registry.requestRelease();
        assertEq(released, COLLATERAL - seized);
        assertEq(_position(account).collateral, 0);
        _assertAccountHoldsNoCollateral();

        // Conservation: what is left of the token supply is exactly what the adapter seized.
        assertEq(s.collateral.totalSupply(), seized);
        assertEq(s.collateral.balanceOf(address(s.adapter)), seized);
        assertEq(s.collateral.balanceOf(address(MORPHO)), 0);
    }

    function test_08_liquidationBySeizedAssets_exactAmountKnownUpFront() public {
        s.registry.depositFor(account, COLLATERAL);
        _authorizeBothAndBorrow(BORROW);
        s.oracle.setPrice(PRICE_25_50_USD);

        uint256 seizeTarget = 3 * ZEC;
        Market memory m = MORPHO.market(s.id);
        uint256 expectedRepaidShares = seizeTarget.mulDivUp(PRICE_25_50_USD, ORACLE_PRICE_SCALE)
            .wDivUp(_liquidationIncentiveFactor()).toSharesUp(m.totalBorrowAssets, m.totalBorrowShares);
        uint256 expectedRepaid = expectedRepaidShares.toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);

        s.usdc.mint(liquidator, 1_000 * USDC);
        vm.startPrank(liquidator);
        s.usdc.approve(address(s.adapter), expectedRepaid);
        (uint256 seized, uint256 repaid) = s.adapter.liquidate(account, seizeTarget, 0);
        vm.stopPrank();

        assertEq(seized, seizeTarget, "seizedAssets path: exact collateral amount");
        assertEq(repaid, expectedRepaid);
        assertEq(s.collateral.balanceOf(address(s.adapter)), seizeTarget);
        assertEq(s.adapter.seenAdapterCollateralBalance(), seizeTarget);
        assertGt(_position(account).borrowShares, 0, "debt remains after a partial seize");
        assertEq(_position(account).collateral, COLLATERAL - seizeTarget);
        _assertAccountHoldsNoCollateral();
        _assertNoPendingExpectation();

        // While debt remains the registry cannot release anything.
        vm.prank(account);
        vm.expectRevert(bytes("insufficient collateral"));
        s.registry.requestRelease();
    }

    // ------------------------------------------------------------------ authorization

    function test_09_unauthorizedWithdrawCollateralReverts() public {
        s.registry.depositFor(account, COLLATERAL);

        vm.prank(attacker);
        vm.expectRevert(bytes("unauthorized"));
        MORPHO.withdrawCollateral(s.marketParams, COLLATERAL, account, attacker);

        vm.prank(attacker);
        vm.expectRevert(bytes("unauthorized"));
        MORPHO.borrow(s.marketParams, 1, 0, account, attacker);

        // The registry itself is refused until the account authorizes it.
        vm.prank(account);
        vm.expectRevert(bytes("unauthorized"));
        s.registry.requestRelease();

        assertEq(_position(account).collateral, COLLATERAL);
        _assertAccountHoldsNoCollateral();
    }

    function test_10_gotcha_authorizationIsAllOrNothing_repayNeedsNone() public {
        s.registry.depositFor(account, COLLATERAL);

        vm.startPrank(account);
        MORPHO.setAuthorization(address(s.registry), true);
        // Re-setting the same value reverts: a UserOp batch must skip authorizations that are already set.
        vm.expectRevert(bytes("already set"));
        MORPHO.setAuthorization(address(s.registry), true);
        vm.stopPrank();

        // An authorized contract can do EVERYTHING on behalf of the account, in every market, including
        // borrowing to an arbitrary receiver. The real Registry/Adapter must have no such code path.
        vm.prank(address(s.registry));
        MORPHO.borrow(s.marketParams, 1 * USDC, 0, account, attacker);
        assertEq(s.usdc.balanceOf(attacker), 1 * USDC);

        // repay(onBehalf) needs no authorization: anybody can repay anybody's debt.
        s.usdc.mint(marketMaker, 2 * USDC);
        vm.startPrank(marketMaker);
        s.usdc.approve(address(MORPHO), type(uint256).max);
        MORPHO.repay(s.marketParams, 0, _position(account).borrowShares, account, "");
        vm.stopPrank();
        assertEq(_position(account).borrowShares, 0);

        // Revocation works and is immediate.
        vm.prank(account);
        MORPHO.setAuthorization(address(s.registry), false);
        vm.prank(account);
        vm.expectRevert(bytes("unauthorized"));
        s.registry.requestRelease();
    }

    // ------------------------------------------------------------------ zZEC transfer-guard prototype

    function test_11_guard_accountCannotPullItsCollateralAnywhere() public {
        s.registry.depositFor(account, COLLATERAL);

        // Morpho itself allows these (msg.sender == onBehalf), the token refuses; Morpho wraps the token's revert
        // reason into "transfer reverted".
        vm.startPrank(account);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.withdrawCollateral(s.marketParams, COLLATERAL, account, account);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.withdrawCollateral(s.marketParams, COLLATERAL, account, address(s.registry));
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.withdrawCollateral(s.marketParams, COLLATERAL, account, address(s.adapter));
        vm.stopPrank();

        // Only Registry/Adapter can arm the flag, and only for themselves.
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(SpikeCollateral.NotTrusted.selector, attacker));
        s.collateral.expectFromMorpho(COLLATERAL);

        assertEq(_position(account).collateral, COLLATERAL);
        _assertAccountHoldsNoCollateral();
    }

    function test_12_guard_directMorphoLiquidationAndFlashLoanRevert() public {
        s.registry.depositFor(account, COLLATERAL);
        uint256 shares = _authorizeBothAndBorrow(BORROW);
        s.oracle.setPrice(PRICE_25_50_USD);

        s.usdc.mint(liquidator, 1_000 * USDC);
        vm.startPrank(liquidator);
        s.usdc.approve(address(MORPHO), type(uint256).max);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.liquidate(s.marketParams, account, 0, shares, "");
        vm.stopPrank();

        vm.prank(attacker);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.flashLoan(address(s.collateral), COLLATERAL, "");

        assertEq(_position(account).collateral, COLLATERAL);
        assertEq(_position(account).borrowShares, shares);
    }

    function test_14_gotcha_staleRepaidSharesRevertAfterAFrontRunRepay() public {
        s.registry.depositFor(account, COLLATERAL);
        uint256 shares = _authorizeBothAndBorrow(BORROW);
        s.oracle.setPrice(PRICE_25_50_USD);

        // Anyone can repay 1 borrow share on behalf of the account (costs 1 micro-USDC)...
        s.usdc.mint(attacker, 1);
        vm.startPrank(attacker);
        s.usdc.approve(address(MORPHO), 1);
        MORPHO.repay(s.marketParams, 0, 1, account, "");
        vm.stopPrank();

        // ...which makes a liquidation with the previously read share count underflow inside Morpho.
        s.usdc.mint(liquidator, 1_000 * USDC);
        vm.startPrank(liquidator);
        s.usdc.approve(address(s.adapter), type(uint256).max);
        vm.expectRevert(stdError.arithmeticError);
        s.adapter.liquidate(account, 0, shares);
        // Reading the shares at execution time (what the real adapter should do: min(requested, current)) works.
        s.adapter.liquidate(account, 0, _position(account).borrowShares);
        vm.stopPrank();
        assertEq(_position(account).borrowShares, 0);
    }

    // ------------------------------------------------------------------ D7 vault sizes (0.01 / 0.05 TAZ)

    /// docs/INTERFACES.md 3.3: a liquidation must seize >= 0.01 ZEC (1,000,000 zat) or the whole outstanding.
    uint256 internal constant INTERFACES_MIN_SEIZE = 1_000_000;

    function test_15_d7_vault001_repayingAllDebtSeizesLessThanTheInterfacesMinimum() public {
        uint256 vaultAmount = 1_000_000; // 0.01 ZEC (docs/DECISIONS.md D7)
        uint256 borrowAssets = 160_000; // 0.16 USDC = 40% LTV at $40
        s.registry.depositFor(account, vaultAmount);
        uint256 shares = _authorizeBothAndBorrow(borrowAssets);
        s.oracle.setPrice(PRICE_25_50_USD); // LTV 62.75%

        s.usdc.mint(liquidator, 1 * USDC);
        vm.startPrank(liquidator);
        s.usdc.approve(address(s.adapter), type(uint256).max);
        // Seizing the whole vault is impossible here: its value / LIF exceeds the debt, so the repaidShares that
        // Morpho derives from seizedAssets exceed the position and underflow.
        vm.expectRevert(stdError.arithmeticError);
        s.adapter.liquidate(account, vaultAmount, 0);
        // Repaying all the debt instead seizes ~70.7% of 0.01 ZEC, which is below the 0.01 ZEC minimum.
        (uint256 seized, uint256 repaid) = s.adapter.liquidate(account, 0, shares);
        vm.stopPrank();
        emit log_named_uint("seized zat when all debt is repaid (0.01 ZEC vault)", seized);
        assertEq(repaid, borrowAssets);
        assertLt(seized, INTERFACES_MIN_SEIZE, "an adapter enforcing INTERFACES.md 3.3 would reject this");
        assertApproxEqRel(seized * WAD / vaultAmount, 0.707e18, 0.001e18);

        // The remainder (~0.0029 ZEC, ~20x a 15,000 zat ZIP-317 fee) is still releasable.
        vm.prank(account);
        assertEq(s.registry.requestRelease(), vaultAmount - seized);
    }

    function test_16_d7_vault001_wholeVaultSeizeOnlyPastLtvOneOverLif_withBadDebt() public {
        uint256 vaultAmount = 1_000_000; // 0.01 ZEC
        uint256 borrowAssets = 160_000;
        s.registry.depositFor(account, vaultAmount);
        _authorizeBothAndBorrow(borrowAssets);
        s.usdc.mint(liquidator, 1 * USDC);
        vm.prank(liquidator);
        s.usdc.approve(address(s.adapter), type(uint256).max);

        // $18.00: LTV = 0.16 / 0.18 = 88.9% > 1 / LIF = 88.75%. Only past that point can the whole vault be seized.
        s.oracle.setPrice(1800e32);
        uint256 lenderAssetsBefore = MORPHO.expectedSupplyAssets(s.marketParams, lender);
        vm.prank(liquidator);
        (uint256 seized, uint256 repaid) = s.adapter.liquidate(account, vaultAmount, 0);
        assertEq(seized, vaultAmount);
        assertLt(repaid, borrowAssets, "the liquidator repays less than the debt");

        // Morpho realizes the shortfall as bad debt, socialized to the lenders.
        Position memory p = _position(account);
        assertEq(p.collateral, 0);
        assertEq(p.borrowShares, 0);
        uint256 lenderLoss = lenderAssetsBefore - MORPHO.expectedSupplyAssets(s.marketParams, lender);
        emit log_named_uint("lender loss from bad debt (USDC units)", lenderLoss);
        assertGt(lenderLoss, 0);
    }

    function test_17_d7_vault005_partialLiquidationClearsTheInterfacesMinimum() public {
        uint256 vaultAmount = 5_000_000; // 0.05 ZEC (docs/DECISIONS.md D7)
        uint256 borrowAssets = 800_000; // 0.80 USDC = 40% LTV at $40
        s.registry.depositFor(account, vaultAmount);
        uint256 shares = _authorizeBothAndBorrow(borrowAssets);
        s.oracle.setPrice(PRICE_25_50_USD);

        s.usdc.mint(liquidator, 1 * USDC);
        vm.startPrank(liquidator);
        s.usdc.approve(address(s.adapter), type(uint256).max);
        (uint256 seized,) = s.adapter.liquidate(account, 0, shares);
        vm.stopPrank();
        emit log_named_uint("seized zat when all debt is repaid (0.05 ZEC vault)", seized);
        assertGe(seized, INTERFACES_MIN_SEIZE);
        assertGe(vaultAmount - seized, INTERFACES_MIN_SEIZE, "the remainder is not dust either");

        vm.prank(account);
        assertEq(s.registry.requestRelease(), vaultAmount - seized);
        _assertAccountHoldsNoCollateral();
    }

    function test_13_gotcha_withoutTheGuardTheAccountCanPullCollateralToItself() public {
        System memory u = _deploySystem(false);
        u.registry.depositFor(account, COLLATERAL);

        // Morpho's only check is msg.sender == onBehalf: with a plain ERC-20 the account takes its collateral.
        vm.prank(account);
        MORPHO.withdrawCollateral(u.marketParams, COLLATERAL, account, account);
        assertEq(u.collateral.balanceOf(account), COLLATERAL, "this is what the zZEC guard must prevent");
    }
}
