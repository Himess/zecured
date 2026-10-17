// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {IMorpho, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MathLib, WAD} from "morpho-blue/src/libraries/MathLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {UtilsLib} from "morpho-blue/src/libraries/UtilsLib.sol";
import {
    ORACLE_PRICE_SCALE,
    LIQUIDATION_CURSOR,
    MAX_LIQUIDATION_INCENTIVE_FACTOR
} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ISettlementAdapter, PreLiquidationTerms} from "../../src/interfaces/ISettlementAdapter.sol";
import {DemoShockOracle} from "../../src/mocks/DemoShockOracle.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Shared helpers of the pre-liquidation suites: the app's preset terms, loans, the price that puts a position at
///         a given LTV, and an independent restatement of the Adapter's arithmetic (Morpho's PreLiquidation, on the
///         totals after interest as MorphoBalancesLib computes them).
abstract contract PreLiquidationBase is ZecuredFixture {
    using MathLib for uint256;
    using SharesMathLib for uint256;
    using MorphoBalancesLib for IMorpho;

    bytes32 internal constant PRE_COMMITMENT = keccak256("zecured.test.commitment.preLiquidator");
    address internal preLiquidator = makeAddr("zecured.preLiquidator");
    address internal account = makeAddr("zecured.preLiquidation.account");

    /// The app's preset ("protect my loan"): from 55 % LTV, 10 % to 50 % of the debt for a 2 % to 5 % incentive.
    function _preset() internal view returns (PreLiquidationTerms memory) {
        return PreLiquidationTerms({
            preLltv: 0.55e18,
            preLcf1: 0.1e18,
            preLcf2: 0.5e18,
            preLif1: 1.02e18,
            preLif2: 1.05e18,
            expiry: uint64(block.timestamp + 30 days)
        });
    }

    function _setTerms(address acct, PreLiquidationTerms memory t) internal {
        vm.prank(acct);
        adapter.setPreLiquidation(t);
    }

    /// A vault of `amount` zat whose account borrowed exactly `assets` USDC units, both authorizations given.
    function _openLoan(address acct, uint64 amount, uint256 assets) internal returns (bytes32 vaultId) {
        vaultId = _attest(acct, amount);
        _authorizeBoth(acct);
        _borrow(acct, assets);
    }

    function _marketPrice() internal view returns (uint256) {
        return IOracle(marketOracle).price();
    }

    /// The (MOCK) base price that puts `acct`'s position at `ltv` (WAD) on the debt with interest accrued: price =
    /// debt / (ltv * collateral), rounded down (so the LTV lands at or just above `ltv`).
    function _priceForLtv(address acct, uint256 ltv) internal view returns (uint256) {
        uint256 debt = MORPHO.expectedBorrowAssets(params, acct);
        return debt * WAD * ORACLE_PRICE_SCALE / (ltv * _position(acct).collateral);
    }

    /// Independent restatement of the Adapter's quote: what Morpho's state after interest says about `acct`.
    struct Quote {
        uint256 borrowShares;
        uint256 totalBorrowAssets;
        uint256 totalBorrowShares;
        uint256 price;
        uint256 collateralQuoted;
        uint256 borrowed;
        uint256 ltv;
        uint256 q;
        uint256 preLcf;
        uint256 preLif;
        uint256 repayable;
    }

    function _quote(address acct, PreLiquidationTerms memory t) internal view returns (Quote memory r) {
        (,, r.totalBorrowAssets, r.totalBorrowShares) = MORPHO.expectedMarketBalances(params);
        Position memory p = _position(acct);
        r.borrowShares = p.borrowShares;
        r.price = _marketPrice();
        r.collateralQuoted = uint256(p.collateral) * r.price / ORACLE_PRICE_SCALE;
        r.borrowed = _toAssetsUp(r.borrowShares, r.totalBorrowAssets, r.totalBorrowShares);
        if (r.collateralQuoted == 0) return r;
        r.ltv = (r.borrowed * WAD + r.collateralQuoted - 1) / r.collateralQuoted; // rounded up
        if (r.ltv <= t.preLltv || r.ltv > LLTV) return r;
        r.q = (r.ltv - t.preLltv) * WAD / (LLTV - t.preLltv);
        r.preLcf = t.preLcf1 + r.q * (t.preLcf2 - t.preLcf1) / WAD;
        r.preLif = t.preLif1 + r.q * (t.preLif2 - t.preLif1) / WAD;
        r.repayable = r.borrowShares * r.preLcf / WAD;
    }

    /// The collateral `repaidShares` buy at the quote: the shares' assets rounded down, times the incentive, at the
    /// price, each step rounded down.
    function _seizedFor(Quote memory r, uint256 repaidShares) internal pure returns (uint256) {
        uint256 assets = repaidShares * (r.totalBorrowAssets + 1) / (r.totalBorrowShares + 1e6);
        return (assets * r.preLif / WAD) * ORACLE_PRICE_SCALE / r.price;
    }

    function _toAssetsUp(uint256 shares, uint256 totalAssets, uint256 totalShares) internal pure returns (uint256) {
        return (shares * (totalAssets + 1) + (totalShares + 1e6) - 1) / (totalShares + 1e6);
    }

    /// LTV in WAD on the debt with interest accrued (rounded down).
    function _ltv(address acct) internal view returns (uint256) {
        uint256 value = uint256(_position(acct).collateral) * _marketPrice() / ORACLE_PRICE_SCALE;
        return MORPHO.expectedBorrowAssets(params, acct) * WAD / value;
    }

    function _lif() internal pure returns (uint256) {
        return
            UtilsLib.min(MAX_LIQUIDATION_INCENTIVE_FACTOR, WAD.wDivDown(WAD - LIQUIDATION_CURSOR.wMulDown(WAD - LLTV)));
    }

    function _preLiquidate(address acct, uint256 repaidShares) internal returns (uint256 repaid, uint64 seized) {
        vm.prank(preLiquidator);
        return adapter.preLiquidate(acct, repaidShares, PRE_COMMITMENT);
    }

    function _expectPreLiquidateRevert(address acct, uint256 repaidShares, bytes memory err) internal {
        vm.prank(preLiquidator);
        vm.expectRevert(err);
        adapter.preLiquidate(acct, repaidShares, PRE_COMMITMENT);
    }
}

/// @notice Pre-liquidation on the SettlementAdapter against the real Morpho Blue and AdaptiveCurveIRM on a Base Sepolia
///         fork: the account's terms and their bounds, the LTV band's edges, the close factor's cap, the interpolation
///         and rounding of Morpho's PreLiquidation, interest accrued with no other market activity, expired, cleared
///         and refused cases, the D11 floors, I1, I2 and I9, and the position's health.
contract PreLiquidationTest is PreLiquidationBase {
    using MathLib for uint256;
    using SharesMathLib for uint256;
    using MorphoBalancesLib for IMorpho;

    MockSignerBond internal gates;

    /// A MOCK slasher (and bond), so that a vault can be frozen and compromised.
    function _mockBondAndSlasher() internal override returns (address) {
        gates = new MockSignerBond();
        return address(gates);
    }

    // ================================================================== terms

    function test_setPreLiquidation_storesTheTermsAndEmits() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.PreLiquidationSet(account, t);
        _setTerms(account, t);
        PreLiquidationTerms memory stored = adapter.preLiquidationTerms(account);
        assertEq(stored.preLltv, t.preLltv);
        assertEq(stored.preLcf1, t.preLcf1);
        assertEq(stored.preLcf2, t.preLcf2);
        assertEq(stored.preLif1, t.preLif1);
        assertEq(stored.preLif2, t.preLif2);
        assertEq(stored.expiry, t.expiry);

        // New terms replace the old ones.
        t.preLltv = 0.5e18;
        _setTerms(account, t);
        assertEq(adapter.preLiquidationTerms(account).preLltv, 0.5e18);
    }

    /// The bounds, at both sides of every edge: preLltv < LLTV, 1 <= preLif1 <= preLif2 <= LIF(LLTV),
    /// 0 < preLcf1 <= preLcf2 <= 1, expiry > now.
    function test_setPreLiquidation_boundValidation() public {
        _openLoan(account, VAULT_005, 1_000_000);
        uint256 lif = _lif();
        assertEq(lif, 1_126_760_563_380_281_690, "Morpho's incentive factor at a 62.5 % LLTV");
        vm.startPrank(account);

        PreLiquidationTerms memory t = _preset();
        t.preLltv = LLTV;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLltv.selector, LLTV, LLTV));
        adapter.setPreLiquidation(t);
        t.preLltv = LLTV - 1;
        adapter.setPreLiquidation(t);

        t = _preset();
        t.preLcf1 = 0;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLcf.selector, 0, t.preLcf2));
        adapter.setPreLiquidation(t);
        t.preLcf1 = t.preLcf2 + 1;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLcf.selector, t.preLcf1, t.preLcf2));
        adapter.setPreLiquidation(t);
        t.preLcf1 = 1;
        t.preLcf2 = WAD + 1;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLcf.selector, 1, WAD + 1));
        adapter.setPreLiquidation(t);
        t.preLcf2 = WAD;
        adapter.setPreLiquidation(t);
        t.preLcf1 = WAD; // a flat close factor is fine
        adapter.setPreLiquidation(t);

        t = _preset();
        t.preLif1 = WAD - 1;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLif.selector, WAD - 1, t.preLif2, lif));
        adapter.setPreLiquidation(t);
        t.preLif1 = t.preLif2 + 1;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLif.selector, t.preLif1, t.preLif2, lif));
        adapter.setPreLiquidation(t);
        t.preLif1 = WAD;
        t.preLif2 = lif + 1;
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidPreLif.selector, WAD, lif + 1, lif));
        adapter.setPreLiquidation(t);
        t.preLif2 = lif; // up to a liquidation's incentive
        adapter.setPreLiquidation(t);
        t.preLif2 = WAD; // no incentive at all is fine too
        adapter.setPreLiquidation(t);

        t = _preset();
        t.expiry = uint64(block.timestamp);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.InvalidExpiry.selector, t.expiry));
        adapter.setPreLiquidation(t);
        t.expiry = uint64(block.timestamp + 1);
        adapter.setPreLiquidation(t);
        vm.stopPrank();
    }

    /// Only the account of an Active vault sets terms: nothing about a vault reaches the Adapter before it is active,
    /// and an activated vault is its account's for good, so the terms never apply to another vault.
    function test_setPreLiquidation_onlyTheAccountOfAnActiveVault() public {
        PreLiquidationTerms memory t = _preset();
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NotAVaultAccount.selector, stranger));
        adapter.setPreLiquidation(t);

        // A reserved (pending) vault.
        address pendingAccount = makeAddr("zecured.preLiquidation.pending");
        bytes32 pendingVault = _vaultIdFor(pendingAccount);
        _mockProof(pendingVault, pendingAccount, VAULT_001, _depositTagFor(pendingAccount), keccak256("anchor.pending"));
        assertEq(uint8(_status(pendingVault)), uint8(VaultStatus.Pending));
        vm.prank(pendingAccount);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultNotActive.selector, pendingVault));
        adapter.setPreLiquidation(t);

        // A closed vault.
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultNotActive.selector, vaultId));
        adapter.setPreLiquidation(t);
    }

    function test_clearPreLiquidation_isIdempotentAndEndsTheTerms() public {
        _openLoan(account, VAULT_005, 1_000_000);
        _setTerms(account, _preset());
        oracle.setPrice(_priceForLtv(account, 0.6e18));

        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.PreLiquidationCleared(account);
        vm.prank(account);
        adapter.clearPreLiquidation();
        assertEq(adapter.preLiquidationTerms(account).expiry, 0);
        assertEq(adapter.preLiquidationTerms(account).preLltv, 0);
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        _expectPreLiquidateRevert(
            account, 1e9, abi.encodeWithSelector(ISettlementAdapter.PreLiquidationInactive.selector, account)
        );

        // Again: no revert, the same event (also for an account that never set any).
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.PreLiquidationCleared(account);
        vm.prank(account);
        adapter.clearPreLiquidation();
        vm.prank(stranger);
        adapter.clearPreLiquidation();
    }

    /// The terms apply while block.timestamp < expiry: the last second they do, from `expiry` on they do not.
    function test_preLiquidate_expiredTerms() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);

        vm.warp(t.expiry - 1);
        oracle.setPrice(_priceForLtv(account, 0.62e18));
        PreLiquidationTerms memory small = t;
        small.preLcf2 = 0.1e18; // so that the position is still in the band after one round
        small.preLcf1 = 0.05e18;
        _setTerms(account, small);
        Quote memory r = _quote(account, small);
        _preLiquidate(account, r.repayable);
        assertGt(_quote(account, small).ltv, small.preLltv, "still in the band");

        vm.warp(small.expiry);
        _expectPreLiquidateRevert(
            account,
            r.repayable / 2,
            abi.encodeWithSelector(ISettlementAdapter.PreLiquidationInactive.selector, account)
        );
    }

    // ================================================================== the band

    /// ltv == preLltv exactly is refused: 1.1 USDC on 0.05 ZEC at $40 is 55.00 %; one wei of price lower, it is in.
    function test_preLiquidate_ltvEqualToPreLltvReverts() public {
        _openLoan(account, VAULT_005, 1_100_000); // a fresh market: exactly 1,100,000 units of debt
        _setTerms(account, _preset());
        assertEq(MORPHO.expectedBorrowAssets(params, account), 1_100_000);
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        uint256 shares = _position(account).borrowShares;
        _expectPreLiquidateRevert(
            account,
            shares / 10,
            abi.encodeWithSelector(ISettlementAdapter.NotPreLiquidatable.selector, 1_100_000, 1_100_000)
        );

        oracle.setPrice(PRICE_40 - 1); // collateral 1,999,999 units: 55 % of it is 1,099,999
        Quote memory r = _quote(account, _preset());
        assertGt(r.ltv, 0.55e18, "just above preLltv");
        assertLt(r.ltv, 0.55e18 + 1e12);
        assertLt(r.q, 1e13, "q close to zero: the factors at preLltv");
        assertApproxEqRel(r.repayable, shares / 10, 1e14, "preLcf1");
        (, uint64 seized) = _preLiquidate(account, r.repayable);
        assertEq(seized, _seizedFor(r, r.repayable));
    }

    /// ltv == LLTV exactly is allowed (Morpho's position is still healthy there), at the factors' far end; one wei of
    /// price lower the position is Morpho's to liquidate and pre-liquidation refuses it.
    function test_preLiquidate_ltvEqualToLltvPasses() public {
        _openLoan(account, VAULT_005, 1_100_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        uint256 shares = _position(account).borrowShares;

        oracle.setPrice(3520e32 - 1); // $35.20 less a wei: collateral 1,759,999 units, LLTV of it 1,099,999
        vm.prank(preLiquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.PositionLiquidatable.selector, 1_100_000, 1_099_999));
        adapter.preLiquidate(account, shares / 2, PRE_COMMITMENT);

        oracle.setPrice(3520e32); // $35.20: collateral 1,760,000 units, 62.5 % of it is the debt exactly
        Quote memory r = _quote(account, t);
        assertEq(r.ltv, LLTV);
        assertEq(r.q, WAD);
        assertEq(r.preLcf, t.preLcf2);
        assertEq(r.preLif, t.preLif2);
        assertEq(r.repayable, shares / 2);
        (uint256 repaid, uint64 seized) = _preLiquidate(account, r.repayable);
        assertEq(repaid, 550_000);
        // 550,000 units * 1.05 at $35.20: 1,640,625 zat.
        assertEq(seized, 1_640_625);
        assertEq(seized, _seizedFor(r, r.repayable));
    }

    /// Past the LLTV the Adapter's `liquidate` is the way, with Morpho's full incentive.
    function test_preLiquidate_pastTheLltvIsALiquidation() public {
        _openLoan(account, VAULT_005, 1_100_000);
        _setTerms(account, _preset());
        oracle.setPrice(_priceForLtv(account, 0.7e18));
        _fundAndApproveAdapter(preLiquidator, 2 * USDC);
        vm.prank(preLiquidator);
        vm.expectPartialRevert(ISettlementAdapter.PositionLiquidatable.selector);
        adapter.preLiquidate(account, 1e9, PRE_COMMITMENT);
        uint256 shares = _position(account).borrowShares;
        vm.prank(preLiquidator);
        (uint256 seized,) = adapter.liquidate(account, 0, shares, PRE_COMMITMENT);
        assertGt(seized, 0);
    }

    // ================================================================== close factor, interpolation, rounding

    /// repaidShares <= borrowShares * preLcf at the position's LTV; one share more is refused.
    function test_preLiquidate_closeFactorCap() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.5875e18)); // the band's middle
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        Quote memory r = _quote(account, t);
        assertApproxEqAbs(r.preLcf, 0.3e18, 1e14, "about 30 % at the middle");
        _expectPreLiquidateRevert(
            account,
            r.repayable + 1,
            abi.encodeWithSelector(ISettlementAdapter.PreLiquidationTooLarge.selector, r.repayable + 1, r.repayable)
        );
        _expectPreLiquidateRevert(
            account,
            r.borrowShares,
            abi.encodeWithSelector(ISettlementAdapter.PreLiquidationTooLarge.selector, r.borrowShares, r.repayable)
        );
        _preLiquidate(account, r.repayable);
        assertEq(_position(account).borrowShares, r.borrowShares - r.repayable);
    }

    /// The factors are linear in q across the band, as the independent restatement computes them, at both ends and in
    /// between; the Adapter takes exactly the collateral it says, and pulls exactly Morpho's repayment.
    function test_preLiquidate_interpolation() public {
        PreLiquidationTerms memory t = _preset();
        uint256[4] memory ltvs = [uint256(0.551e18), 0.5875e18, 0.61e18, 0.6249e18];
        for (uint256 i; i < ltvs.length; ++i) {
            address acct = makeAddr(string.concat("zecured.preLiquidation.interp", vm.toString(i)));
            oracle.setPrice(PRICE_40);
            bytes32 vaultId = _openLoan(acct, VAULT_005, 1_000_000);
            _setTerms(acct, t);
            oracle.setPrice(_priceForLtv(acct, ltvs[i]));
            Quote memory r = _quote(acct, t);
            assertGt(r.ltv, t.preLltv);
            assertLe(r.ltv, LLTV);
            // Linear: preLcf - preLcf1 and preLif - preLif1 are the same fraction q of their ranges.
            assertApproxEqAbs(r.preLcf, t.preLcf1 + (ltvs[i] - t.preLltv) * 0.4e18 / 0.075e18, 1e14, "preLcf");
            assertApproxEqAbs(r.preLif, t.preLif1 + (ltvs[i] - t.preLltv) * 0.03e18 / 0.075e18, 1e13, "preLif");

            uint256 expectedRepaid = _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares);
            uint256 expectedSeized = _seizedFor(r, r.repayable);
            _fundAndApproveAdapter(preLiquidator, expectedRepaid); // exact approval: any over-pull would revert
            vm.expectEmit(address(adapter));
            emit ISettlementAdapter.PreLiquidated(vaultId, acct, preLiquidator, expectedRepaid, uint64(expectedSeized));
            (uint256 repaid, uint64 seized) = _preLiquidate(acct, r.repayable);
            assertEq(repaid, expectedRepaid, "Morpho's repayment of the shares, rounded up");
            assertEq(seized, expectedSeized, "the restated seizure");
            assertEq(usdc.balanceOf(preLiquidator), 0, "exactly the repayment pulled");
        }
    }

    /// Over random positions in the band and repayments up to the cap: the Adapter seizes exactly the restated amount,
    /// rounded down at every step (never more than the repayment's value times the incentive), or refuses below the
    /// D11 floor; the LTV always falls.
    function testFuzz_preLiquidate_roundingAndFloors(uint256 ltvSeed, uint256 sharesSeed) public {
        bytes32 vaultId = _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, bound(ltvSeed, 0.5501e18, 0.6249e18)));
        Quote memory r = _quote(account, t);
        if (r.ltv <= t.preLltv || r.ltv > LLTV) return; // rounding at the band's very edges: covered above
        uint256 repaidShares = bound(sharesSeed, 1, r.repayable);
        uint256 expected = _seizedFor(r, repaidShares);
        uint256 assetsDown = repaidShares * (r.totalBorrowAssets + 1) / (r.totalBorrowShares + 1e6);
        // Rounded down: seized * price <= value * incentive < (seized + 1) * price, on the rounded-down value.
        assertLe(expected * r.price, (assetsDown * r.preLif / WAD) * ORACLE_PRICE_SCALE);
        assertGt((expected + 1) * r.price, (assetsDown * r.preLif / WAD) * ORACLE_PRICE_SCALE);
        _fundAndApproveAdapter(preLiquidator, 2 * USDC);
        uint256 ltvBefore = _ltv(account);

        if (expected < MIN_SEIZE) {
            _expectPreLiquidateRevert(
                account,
                repaidShares,
                abi.encodeWithSelector(ISettlementAdapter.SeizeBelowMinimum.selector, expected, MIN_SEIZE)
            );
            return;
        }
        (, uint64 seized) = _preLiquidate(account, repaidShares);
        assertEq(seized, expected);
        assertEq(_outstanding(vaultId), VAULT_005 - seized, "I9");
        assertEq(zzec.totalSupply(), VAULT_005 - seized, "I1");
        assertLt(_ltv(account), ltvBefore, "the LTV falls");
        _assertZZECOnlyInMorpho(_list(account, preLiquidator));
    }

    // ================================================================== interest (R-24's scenario)

    /// Interest accrued since the market's last update, with no other activity on the market, is what puts the position
    /// in the band: on Morpho's stored totals it is below preLltv, on the totals after interest above, and the Adapter
    /// accrues before it reads (the bots must read the same accrued debt, R-24).
    function test_preLiquidate_interestAccruedWithNoOtherActivity() public {
        // 5 units below 55 % of 2 USDC of collateral at $40.
        _openLoan(account, VAULT_005, 1_099_995);
        PreLiquidationTerms memory t = _preset();
        t.expiry = uint64(block.timestamp + 400 days);
        _setTerms(account, t);
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        uint256 shares = _position(account).borrowShares;
        _expectPreLiquidateRevert(
            account,
            shares / 10,
            abi.encodeWithSelector(ISettlementAdapter.NotPreLiquidatable.selector, 1_099_995, 1_100_000)
        );

        skip(365 days); // nobody touches the market
        Market memory stored = MORPHO.market(marketId);
        assertLt(stored.lastUpdate, block.timestamp, "no accrual since");
        uint256 storedDebt = shares.toAssetsUp(stored.totalBorrowAssets, stored.totalBorrowShares);
        assertEq(storedDebt, 1_099_995, "Morpho's stored totals: still below the band");
        Quote memory r = _quote(account, t);
        assertGt(r.borrowed, 1_100_000, "with the interest since: in the band");
        assertGt(r.ltv, t.preLltv);

        uint256 expectedSeized = _seizedFor(r, r.repayable);
        (uint256 repaid, uint64 seized) = _preLiquidate(account, r.repayable);
        assertEq(seized, expectedSeized, "seized on the accrued totals");
        assertEq(repaid, _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares));
        assertEq(MORPHO.market(marketId).lastUpdate, block.timestamp, "the Adapter accrued");
    }

    // ================================================================== effects: I1, I2, I9, claim, health

    function test_preLiquidate_recordsALiquidationClaimAndKeepsTheInvariants() public {
        bytes32 vaultId = _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        Quote memory r = _quote(account, t);
        uint256 expectedSeized = _seizedFor(r, r.repayable);
        uint256 expectedRepaid = _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares);
        _fundAndApproveAdapter(preLiquidator, expectedRepaid);
        bytes32 claimId = registry.claimIdOf(vaultId, 0);
        uint256 ltvBefore = _ltv(account);
        uint256 debtBefore = MORPHO.expectedBorrowAssets(params, account);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.LiquidationRelease(vaultId, claimId, uint64(expectedSeized), PRE_COMMITMENT);
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.PreLiquidated(vaultId, account, preLiquidator, expectedRepaid, uint64(expectedSeized));
        (uint256 repaid, uint64 seized) = _preLiquidate(account, r.repayable);

        assertEq(seized, expectedSeized);
        assertEq(repaid, expectedRepaid);
        // I9 and I1: the claim is exactly what left the vault, which stays open; the supply follows.
        assertLe(seized, VAULT_005, "I9");
        assertEq(_outstanding(vaultId), VAULT_005 - seized);
        assertEq(_position(account).collateral, VAULT_005 - seized, "collateral == outstanding");
        assertEq(zzec.totalSupply(), VAULT_005 - seized, "I1");
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
        _assertClaim(claimId, vaultId, ClaimKind.Liquidation, seized, PRE_COMMITMENT, ClaimStatus.Pending);
        ClaimDetails memory c = registry.claimDetails(claimId);
        assertEq(c.claimant, preLiquidator, "the pre-liquidator is the claimant (T-04, T-05)");
        // Health: the debt and the LTV fall; the remainder of the collateral is worth more than the debt left.
        assertApproxEqAbs(MORPHO.expectedBorrowAssets(params, account), debtBefore - repaid, 1);
        assertLt(_ltv(account), ltvBefore, "the LTV falls");
        assertEq(usdc.balanceOf(address(adapter)), 0);
        assertEq(usdc.allowance(address(adapter), address(MORPHO)), 0);
        // I2: no zZEC held anywhere but Morpho, no expectation left armed.
        _assertZZECOnlyInMorpho(_list(account, preLiquidator));
    }

    /// Pre-liquidations follow one another while the LTV is in the band; each lowers it, and once it is at or below
    /// preLltv the next one is refused. (Small close factors, 5 % to 10 %, so that it takes several rounds.)
    function test_preLiquidate_repeatedUntilTheBandIsLeft() public {
        bytes32 vaultId = _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        t.preLcf1 = 0.05e18;
        t.preLcf2 = 0.1e18;
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.62e18));
        _fundAndApproveAdapter(preLiquidator, 2 * USDC);
        uint256 last = _ltv(account);
        uint256 rounds;
        for (; rounds < 20; ++rounds) {
            Quote memory r = _quote(account, t);
            if (r.ltv <= t.preLltv) break;
            if (_seizedFor(r, r.repayable) < MIN_SEIZE) break;
            _preLiquidate(account, r.repayable);
            uint256 ltvNow = _ltv(account);
            assertLt(ltvNow, last, "every pre-liquidation lowers the LTV");
            last = ltvNow;
        }
        assertGt(rounds, 1, "more than one round");
        Quote memory end = _quote(account, t);
        if (end.ltv <= t.preLltv) {
            vm.prank(preLiquidator);
            vm.expectPartialRevert(ISettlementAdapter.NotPreLiquidatable.selector);
            adapter.preLiquidate(account, 1e9, PRE_COMMITMENT);
        }
        assertEq(zzec.totalSupply(), _outstanding(vaultId), "I1");
    }

    // ================================================================== refusals

    function test_preLiquidate_revertsOnZeroInputs() public {
        _openLoan(account, VAULT_005, 1_000_000);
        _setTerms(account, _preset());
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        vm.startPrank(preLiquidator);
        vm.expectRevert(ISettlementAdapter.ZeroCommitment.selector);
        adapter.preLiquidate(account, 1e9, bytes32(0));
        vm.expectRevert(ISettlementAdapter.ZeroRepay.selector);
        adapter.preLiquidate(account, 0, PRE_COMMITMENT);
        vm.stopPrank();
        // An account without terms (and without a vault).
        _expectPreLiquidateRevert(
            stranger, 1e9, abi.encodeWithSelector(ISettlementAdapter.PreLiquidationInactive.selector, stranger)
        );
    }

    function test_preLiquidate_revertsWithoutDebt() public {
        _attest(account, VAULT_005);
        _authorizeBoth(account);
        _setTerms(account, _preset());
        _expectPreLiquidateRevert(account, 1e9, abi.encodeWithSelector(ISettlementAdapter.NoDebt.selector, account));
    }

    /// A closed vault, a frozen vault (an open theft case) and a compromised one are never pre-liquidated, and nothing
    /// moves.
    function test_preLiquidate_revertsForAVaultNotActiveOrFrozen() public {
        bytes32 vaultId = _openLoan(account, VAULT_005, 1_000_000);
        _setTerms(account, _preset());
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        uint256 shares = _position(account).borrowShares;

        gates.setFrozen(vaultId, true);
        _expectPreLiquidateRevert(
            account, shares / 10, abi.encodeWithSelector(ISettlementAdapter.VaultFrozen.selector, vaultId)
        );
        gates.setCompromised(vaultId, true); // compromised vaults are frozen for good
        _expectPreLiquidateRevert(
            account, shares / 10, abi.encodeWithSelector(ISettlementAdapter.VaultFrozen.selector, vaultId)
        );
        assertEq(_position(account).borrowShares, shares, "nothing repaid");
        assertEq(usdc.balanceOf(preLiquidator), 1 * USDC);

        gates.setCompromised(vaultId, false);
        gates.setFrozen(vaultId, false);
        _repayAll(account);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        _expectPreLiquidateRevert(
            account, shares / 10, abi.encodeWithSelector(ISettlementAdapter.VaultNotActive.selector, vaultId)
        );
    }

    /// D11: at least MIN_SEIZE taken, and at least MIN_SEIZE left in the vault.
    function test_preLiquidate_floors() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        Quote memory r = _quote(account, t);
        // Shares whose collateral is just under the floor.
        uint256 small = r.repayable / 20;
        uint256 smallSeized = _seizedFor(r, small);
        assertLt(smallSeized, MIN_SEIZE);
        _expectPreLiquidateRevert(
            account,
            small,
            abi.encodeWithSelector(ISettlementAdapter.SeizeBelowMinimum.selector, smallSeized, MIN_SEIZE)
        );

        // A 300,000 zat vault (0.01 ZEC less a partial release) whose terms allow repaying all of it at the full
        // incentive: at the LLTV that takes about 211,000 zat and would leave less than the floor.
        address small2 = makeAddr("zecured.preLiquidation.smallVault");
        bytes32 vaultId = _attest(small2, VAULT_001);
        _authorizeBoth(small2);
        vm.prank(small2);
        registry.requestPartialRelease(vaultId, 700_000, RELEASE_COMMITMENT);
        assertEq(_outstanding(vaultId), 300_000);
        _borrow(small2, 50_000); // 0.05 USDC on 0.12 USDC
        PreLiquidationTerms memory all = _preset();
        all.preLcf1 = WAD;
        all.preLcf2 = WAD;
        all.preLif1 = _lif();
        all.preLif2 = _lif();
        _setTerms(small2, all);
        oracle.setPrice(_priceForLtv(small2, 0.62e18));
        Quote memory s = _quote(small2, all);
        uint256 seizedAll = _seizedFor(s, s.repayable);
        assertGt(seizedAll + MIN_SEIZE, 300_000);
        _expectPreLiquidateRevert(
            small2,
            s.repayable,
            abi.encodeWithSelector(
                ISettlementAdapter.PreLiquidationRemainderTooSmall.selector, 300_000 - seizedAll, MIN_SEIZE
            )
        );
    }

    /// USDC comes only from the caller, inside onMorphoRepay; someone else's approval is never used.
    function test_preLiquidate_pullsUsdcOnlyFromTheCaller() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        Quote memory r = _quote(account, t);
        uint256 expectedRepaid = _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares);
        _fundAndApproveAdapter(marketMaker, 1 * USDC); // a third party with an open approval
        usdc.mint(preLiquidator, 1 * USDC); // the caller did not approve
        vm.prank(preLiquidator);
        vm.expectRevert(
            abi.encodeWithSelector(
                IERC20Errors.ERC20InsufficientAllowance.selector, address(adapter), 0, expectedRepaid
            )
        );
        adapter.preLiquidate(account, r.repayable, PRE_COMMITMENT);
        assertEq(usdc.balanceOf(marketMaker), 1 * USDC);
    }

    /// The account can withdraw its consent on Morpho too: without the Adapter's authorization the pre-liquidation
    /// reverts as a whole (the repayment with it).
    function test_preLiquidate_revertsWithoutTheAdaptersAuthorization() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        vm.prank(account);
        MORPHO.setAuthorization(address(adapter), false);
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        _fundAndApproveAdapter(preLiquidator, 1 * USDC);
        Quote memory r = _quote(account, t);
        _expectPreLiquidateRevert(account, r.repayable, bytes("unauthorized"));
        assertEq(_position(account).borrowShares, r.borrowShares, "repay rolled back");
        assertEq(usdc.balanceOf(preLiquidator), 1 * USDC);
    }
}

/// @notice Pre-liquidation on the testnet deployments' oracle: the DemoShockOracle (MOCK / DEMO) over the MOCK price,
///         lowered in the steps it allows (at most 25 % per step, two minutes apart), against the real Morpho Blue. A
///         borrower who opted in is pre-liquidated in the band, for a small incentive and a part of the debt, and only
///         past the LLTV is the position liquidated, for Morpho's full incentive.
contract PreLiquidationShockTest is PreLiquidationBase {
    using MorphoBalancesLib for IMorpho;

    DemoShockOracle internal shock;

    function _marketOracle() internal override returns (address) {
        shock = new DemoShockOracle(address(this), IOracle(address(oracle)));
        return address(shock);
    }

    function _lower(uint256 shockBps) internal {
        vm.warp(block.timestamp + shock.MIN_STEP_INTERVAL());
        shock.setShock(shockBps);
    }

    /// $40 and a 50 % loan; the shock takes the price to $36 (LTV 55.6 %: in the band), where one pre-liquidation
    /// brings the LTV back under 55 %; then to $28.80 (LTV past the LLTV): pre-liquidation refuses, a liquidation
    /// follows.
    function test_shockSteps_preLiquidationThenLiquidation() public {
        bytes32 vaultId = _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        assertEq(_marketPrice(), PRICE_40, "no shock yet");
        _fundAndApproveAdapter(preLiquidator, 2 * USDC);

        _lower(9_000); // $36.00
        Quote memory r = _quote(account, t);
        assertGt(r.ltv, t.preLltv, "in the band");
        assertLt(r.ltv, 0.56e18);
        uint256 ltvBefore = _ltv(account);
        uint256 debtBefore = MORPHO.expectedBorrowAssets(params, account);
        (uint256 repaid, uint64 seized) = _preLiquidate(account, r.repayable);
        assertEq(seized, _seizedFor(r, r.repayable));
        assertEq(repaid, _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares));
        // Near the band's start: about 13 % of the debt for about a 2.2 % incentive; the borrower keeps the loan.
        assertApproxEqRel(r.preLcf, 0.13e18, 0.01e18);
        assertApproxEqRel(r.preLif, 1.022e18, 0.001e18);
        assertLt(repaid, debtBefore / 7);
        assertLt(_ltv(account), ltvBefore);
        assertLe(_ltv(account), t.preLltv, "back under preLltv in one step");
        vm.prank(preLiquidator);
        vm.expectPartialRevert(ISettlementAdapter.NotPreLiquidatable.selector);
        adapter.preLiquidate(account, 1e9, PRE_COMMITMENT);

        _lower(7_200); // $28.80: 80 % of the previous step
        vm.prank(preLiquidator);
        vm.expectPartialRevert(ISettlementAdapter.PositionLiquidatable.selector);
        adapter.preLiquidate(account, 1e9, PRE_COMMITMENT);
        uint256 shares = _position(account).borrowShares;
        vm.prank(preLiquidator);
        (uint256 liqSeized,) = adapter.liquidate(account, 0, shares, PRE_COMMITMENT);
        assertGt(liqSeized, 0);
        assertEq(_position(account).borrowShares, 0, "the rest of the debt repaid by the liquidation");
        assertEq(zzec.totalSupply(), _outstanding(vaultId), "I1");
        _assertZZECOnlyInMorpho(_list(account, preLiquidator));
    }

    /// The same fall without the opt-in: nothing happens in the band, and the liquidation takes the full incentive on
    /// the whole debt. With it, the borrower loses only the small incentive on a part of the debt before the band ends.
    function test_shockSteps_withoutTermsNothingHappensInTheBand() public {
        address optedIn = makeAddr("zecured.preLiquidation.optedIn");
        _openLoan(account, VAULT_005, 1_000_000);
        _openLoan(optedIn, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(optedIn, t);
        _fundAndApproveAdapter(preLiquidator, 4 * USDC);

        _lower(9_000);
        _expectPreLiquidateRevert(
            account, 1e9, abi.encodeWithSelector(ISettlementAdapter.PreLiquidationInactive.selector, account)
        );
        Quote memory r = _quote(optedIn, t);
        (, uint64 preSeized) = _preLiquidate(optedIn, r.repayable);

        _lower(7_200);
        uint256 plainShares = _position(account).borrowShares;
        uint256 optedInShares = _position(optedIn).borrowShares;
        vm.startPrank(preLiquidator);
        (uint256 seizedPlain,) = adapter.liquidate(account, 0, plainShares, PRE_COMMITMENT);
        (uint256 seizedOptedIn,) = adapter.liquidate(optedIn, 0, optedInShares, PRE_COMMITMENT);
        vm.stopPrank();
        // The opted-in borrower kept more of the vault: the band's small incentive on a tenth of the debt plus
        // Morpho's on the rest, against Morpho's on all of it.
        assertGt(_outstanding(_vaultIdFor(optedIn)), _outstanding(_vaultIdFor(account)));
        assertLt(uint256(preSeized) + seizedOptedIn, seizedPlain);
    }
}
