// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
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
import {IZecuredRegistry, VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {ISettlementAdapter} from "../../src/interfaces/ISettlementAdapter.sol";
import {CloseRequest, DeleverageRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice SettlementAdapter against the real Morpho Blue on a Base Sepolia fork: liquidation (both Morpho paths,
///         share cap, D11 floor), close from collateral (every check of docs/INTERFACES.md § 3.3, the mmAmount formula,
///         I7, I8), the Morpho callbacks and nonce invalidation.
contract SettlementAdapterTest is ZecuredFixture {
    using MathLib for uint256;
    using SharesMathLib for uint256;
    using MorphoBalancesLib for IMorpho;

    address internal account;
    uint256 internal accountKey;

    function setUp() public override {
        super.setUp();
        (account, accountKey) = makeAddrAndKey("zecured.adapter.account");
        vm.label(account, "account");
    }

    // ------------------------------------------------------------------ helpers

    /// A vault of `amount` zat with a loan of `ltvBps` of its value at $40, both authorizations given.
    function _openLoan(address acct, uint64 amount, uint256 ltvBps) internal returns (bytes32 vaultId, uint256 shares) {
        vaultId = _attest(acct, amount);
        _authorizeBoth(acct);
        uint256 value = uint256(amount) * PRICE_40 / ORACLE_PRICE_SCALE; // USDC units
        shares = _borrow(acct, value * ltvBps / 10_000);
    }

    function _lif() internal pure returns (uint256) {
        return
            UtilsLib.min(MAX_LIQUIDATION_INCENTIVE_FACTOR, WAD.wDivDown(WAD - LIQUIDATION_CURSOR.wMulDown(WAD - LLTV)));
    }

    function _liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares)
        internal
        returns (uint256 seized, uint256 repaid)
    {
        vm.prank(liquidator);
        return adapter.liquidate(borrower, seizedAssets, repaidShares, LIQUIDATOR_COMMITMENT);
    }

    function _close(CloseRequest memory req, bytes memory sig) internal {
        vm.prank(req.executor);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function _deleverageRequest(
        bytes32 vaultId,
        uint16 marginBps,
        uint256 repayAssets,
        uint64 maxCollateral,
        uint256 nonce
    ) internal view returns (DeleverageRequest memory) {
        return DeleverageRequest({
            vaultId: vaultId,
            account: account,
            executor: marketMaker,
            marginBps: marginBps,
            repayAssets: repayAssets,
            maxCollateral: maxCollateral,
            nonce: nonce,
            deadline: uint64(block.timestamp + 10 minutes)
        });
    }

    function _signDeleverage(DeleverageRequest memory req) internal view returns (bytes memory) {
        return _sign(accountKey, adapter.hashDeleverageRequest(req));
    }

    function _expectDeleverageRevert(DeleverageRequest memory req, bytes memory sig, bytes memory err) internal {
        vm.prank(marketMaker);
        vm.expectRevert(err);
        adapter.deleverageWithCollateral(req, sig, MM_COMMITMENT, 0);
    }

    /// LTV in basis points: debt over collateral value at the oracle price.
    function _ltvBps(address acct) internal view returns (uint256) {
        Position memory p = _position(acct);
        uint256 value = uint256(p.collateral) * oracle.price() / ORACLE_PRICE_SCALE;
        return MORPHO.expectedBorrowAssets(params, acct) * 10_000 / value;
    }

    // ================================================================== liquidate

    /// 0.01 ZEC vault at 40 % LTV, price $40 -> $25.50 (LTV 62.75 %): repaying all debt seizes ~70.7 %.
    function test_liquidate_repayAllShares_partialSeize() public {
        (bytes32 vaultId, uint256 shares) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        _fundAndApproveAdapter(liquidator, 1 * USDC);

        Market memory m = MORPHO.market(marketId);
        uint256 expectedSeized = shares.toAssetsDown(m.totalBorrowAssets, m.totalBorrowShares).wMulDown(_lif())
            .mulDivDown(ORACLE_PRICE_SCALE, PRICE_25_50);
        uint256 expectedRepaid = shares.toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);
        bytes32 expectedClaimId = registry.claimIdOf(vaultId, 0);
        uint256 liquidatorBefore = usdc.balanceOf(liquidator);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.LiquidationRelease(
            vaultId, expectedClaimId, uint64(expectedSeized), LIQUIDATOR_COMMITMENT
        );
        (uint256 seized, uint256 repaid) = _liquidate(account, 0, shares);

        assertEq(seized, expectedSeized);
        assertEq(repaid, expectedRepaid);
        assertApproxEqRel(seized * WAD / VAULT_001, 0.707e18, 0.002e18);
        assertGe(seized, MIN_SEIZE, "D11");
        assertEq(liquidatorBefore - usdc.balanceOf(liquidator), repaid, "exactly repaidAssets pulled");
        assertEq(usdc.balanceOf(address(adapter)), 0);
        assertEq(usdc.allowance(address(adapter), address(MORPHO)), 0);

        _assertClaim(
            expectedClaimId, vaultId, ClaimKind.Liquidation, uint64(seized), LIQUIDATOR_COMMITMENT, ClaimStatus.Pending
        );
        assertEq(_outstanding(vaultId), VAULT_001 - seized);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "stays Active while outstanding > 0");
        Position memory p = _position(account);
        assertEq(p.collateral, VAULT_001 - seized);
        assertEq(p.borrowShares, 0);
        assertEq(zzec.totalSupply(), VAULT_001 - seized, "I1");
        _assertZZECOnlyInMorpho(_list(account, liquidator));
    }

    /// Exact-seize path: seizedAssets is known upfront and debt remains.
    function test_liquidate_seizedAssetsPath() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_005, 4_000);
        oracle.setPrice(PRICE_25_50);
        _fundAndApproveAdapter(liquidator, 10 * USDC);
        uint256 sharesBefore = _position(account).borrowShares;

        (uint256 seized, uint256 repaid) = _liquidate(account, 1_000_000, 0);
        assertEq(seized, 1_000_000);
        assertGt(repaid, 0);
        assertGt(_position(account).borrowShares, 0, "debt remains");
        assertLt(_position(account).borrowShares, sharesBefore);
        assertEq(_outstanding(vaultId), VAULT_005 - 1_000_000);
        assertEq(_position(account).collateral, VAULT_005 - 1_000_000);
        assertEq(zzec.totalSupply(), VAULT_005 - 1_000_000, "I1");
        _assertZZECOnlyInMorpho(_list(account, liquidator));
    }

    /// repaidShares above the position is capped (no Morpho underflow), also after a front-running 1-share repay.
    function test_liquidate_capsRepaidSharesAtTheBorrowersShares() public {
        (, uint256 shares) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        // Griefer repays one share on behalf of the account (costs 1 unit).
        usdc.mint(stranger, 1);
        vm.startPrank(stranger);
        usdc.approve(address(MORPHO), 1);
        MORPHO.repay(params, 0, 1, account, "");
        vm.stopPrank();

        _fundAndApproveAdapter(liquidator, 1 * USDC);
        _liquidate(account, 0, shares); // the stale share count would underflow inside Morpho without the cap
        assertEq(_position(account).borrowShares, 0);

        // type(uint256).max also works as "all shares" on another vault.
        address other = makeAddr("zecured.adapter.other");
        oracle.setPrice(PRICE_40);
        _openLoan(other, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        _liquidate(other, 0, type(uint256).max);
        assertEq(_position(other).borrowShares, 0);
    }

    function test_liquidate_revertsOnInconsistentInputOrZeroCommitment() public {
        _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        vm.startPrank(liquidator);
        vm.expectRevert(ISettlementAdapter.InconsistentInput.selector);
        adapter.liquidate(account, 0, 0, LIQUIDATOR_COMMITMENT);
        vm.expectRevert(ISettlementAdapter.InconsistentInput.selector);
        adapter.liquidate(account, 1, 1, LIQUIDATOR_COMMITMENT);
        vm.expectRevert(ISettlementAdapter.ZeroCommitment.selector);
        adapter.liquidate(account, 0, 1, bytes32(0));
        vm.stopPrank();
    }

    function test_liquidate_revertsForANonVaultBorrowerOrAClosedVault() public {
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NotAVaultAccount.selector, stranger));
        adapter.liquidate(stranger, 0, 1, LIQUIDATOR_COMMITMENT);

        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultNotActive.selector, vaultId));
        adapter.liquidate(account, 0, 1, LIQUIDATOR_COMMITMENT);
    }

    function test_liquidate_revertsWithoutDebtOrOnAHealthyPosition() public {
        _attest(account, VAULT_001);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NoDebt.selector, account));
        adapter.liquidate(account, 0, 1, LIQUIDATOR_COMMITMENT);

        _authorizeBoth(account);
        uint256 shares = _borrow(account, 160_000); // 40 % LTV at $40: healthy
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        vm.expectRevert(bytes("position is healthy"));
        adapter.liquidate(account, 0, shares, LIQUIDATOR_COMMITMENT);
        vm.prank(liquidator);
        vm.expectRevert(bytes("position is healthy"));
        adapter.liquidate(account, MIN_SEIZE, 0, LIQUIDATOR_COMMITMENT);
    }

    /// D11: seizing less than 100,000 zat (and less than the whole remaining outstanding) is refused.
    function test_liquidate_revertsBelowTheMinimumSeize() public {
        _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.SeizeBelowMinimum.selector, MIN_SEIZE - 1, MIN_SEIZE));
        adapter.liquidate(account, MIN_SEIZE - 1, 0, LIQUIDATOR_COMMITMENT);
        // Exactly the floor is fine.
        (uint256 seized,) = _liquidate(account, MIN_SEIZE, 0);
        assertEq(seized, MIN_SEIZE);
    }

    /// D11 puts no floor on the remainder: repaying all debt at $18.50 seizes ~974,500 zat and leaves ~25,500 zat,
    /// which stays with the borrower (released through requestRelease; dust on Zcash if below the payout fee).
    function test_liquidate_subFloorRemainderIsAllowed() public {
        (bytes32 vaultId, uint256 shares) = _openLoan(account, VAULT_001, 4_000); // 160,000 units of debt
        oracle.setPrice(1850e32); // $18.50: LTV 86.5 %
        _fundAndApproveAdapter(liquidator, 1 * USDC);

        (uint256 seized,) = _liquidate(account, 0, shares);
        uint256 remainder = VAULT_001 - seized;
        assertGt(remainder, 0);
        assertLt(remainder, MIN_SEIZE, "sub-floor remainder");
        assertEq(_position(account).borrowShares, 0, "all debt repaid");
        assertEq(_outstanding(vaultId), remainder);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));

        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        _assertClaim(claimId, vaultId, ClaimKind.Release, uint64(remainder), RELEASE_COMMITMENT, ClaimStatus.Pending);
        assertEq(zzec.totalSupply(), 0, "I1");
    }

    /// A partial liquidation leaves 150,000 zat with debt remaining; a later liquidation (all remaining shares)
    /// seizes >= MIN_SEIZE and leaves a sub-floor remainder without debt.
    function test_liquidate_partialLeavingDebt_thenALaterLiquidation() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(1850e32); // $18.50
        _fundAndApproveAdapter(liquidator, 1 * USDC);

        (uint256 seized1,) = _liquidate(account, 850_000, 0);
        assertEq(seized1, 850_000);
        assertEq(_outstanding(vaultId), 150_000);
        assertEq(_position(account).collateral, 150_000);
        assertGt(_position(account).borrowShares, 0, "debt remains");

        (uint256 seized2,) = _liquidate(account, 0, type(uint256).max);
        assertGe(seized2, MIN_SEIZE);
        assertLt(150_000 - seized2, MIN_SEIZE);
        assertEq(_position(account).borrowShares, 0);
        assertEq(_outstanding(vaultId), 150_000 - seized2);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
        _assertClaim(
            registry.claimIdOf(vaultId, 0),
            vaultId,
            ClaimKind.Liquidation,
            850_000,
            LIQUIDATOR_COMMITMENT,
            ClaimStatus.Pending
        );
        _assertClaim(
            registry.claimIdOf(vaultId, 1),
            vaultId,
            ClaimKind.Liquidation,
            uint64(seized2),
            LIQUIDATOR_COMMITMENT,
            ClaimStatus.Pending
        );
        assertEq(zzec.totalSupply(), 150_000 - seized2, "I1");
        _assertZZECOnlyInMorpho(_list(account, liquidator));
    }

    /// Below MIN_SEIZE only the vault's whole remaining outstanding may be seized.
    function test_liquidate_wholeSubFloorRemainderIsAllowed() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(1850e32);
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        _liquidate(account, 940_000, 0); // leaves 60,000 zat and some debt (healthy again at $18.50)
        assertEq(_outstanding(vaultId), 60_000);
        assertGt(_position(account).borrowShares, 0);

        oracle.setPrice(10e34); // $10.00: LTV ~94 % > 1/LIF, the whole remainder can be seized
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.SeizeBelowMinimum.selector, 50_000, MIN_SEIZE));
        adapter.liquidate(account, 50_000, 0, LIQUIDATOR_COMMITMENT);

        (uint256 seized,) = _liquidate(account, 60_000, 0);
        assertEq(seized, 60_000);
        assertEq(_outstanding(vaultId), 0);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(_position(account).borrowShares, 0, "Morpho realized the bad debt");
        assertEq(zzec.totalSupply(), 0, "I1");
    }

    /// Past 1/LIF (88.75 % LTV) the whole vault can be seized; Morpho realizes the shortfall as bad debt and the vault
    /// is closed with outstanding 0.
    function test_liquidate_wholeVault_closesTheVault() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(1800e32); // $18.00: LTV 88.9 %
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        (uint256 seized, uint256 repaid) = _liquidate(account, VAULT_001, 0);
        assertEq(seized, VAULT_001);
        assertLt(repaid, 160_000, "bad debt");
        assertEq(_outstanding(vaultId), 0);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        Position memory p = _position(account);
        assertEq(p.collateral, 0);
        assertEq(p.borrowShares, 0, "Morpho realized the bad debt");
        assertEq(zzec.totalSupply(), 0, "I1");
        _assertZZECOnlyInMorpho(_list(account, liquidator));
    }

    /// USDC is pulled only from the caller, inside onMorphoLiquidate; someone else's approval is never used.
    function test_liquidate_pullsUsdcOnlyFromTheCaller() public {
        (, uint256 shares) = _openLoan(account, VAULT_001, 4_000);
        oracle.setPrice(PRICE_25_50);
        _fundAndApproveAdapter(marketMaker, 1 * USDC); // a third party with an open approval
        usdc.mint(liquidator, 1 * USDC); // the liquidator did not approve
        vm.prank(liquidator);
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientAllowance.selector, address(adapter), 0, 160_000)
        );
        adapter.liquidate(account, 0, shares, LIQUIDATOR_COMMITMENT);
        assertEq(usdc.balanceOf(marketMaker), 1 * USDC);
    }

    // ================================================================== closeWithCollateral

    function test_close_repaysAllDebtAndCreatesTwoClaims() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        skip(7 days);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        assertGt(debt, 160_000, "interest accrued");
        _fundAndApproveAdapter(marketMaker, debt); // exact approval: any over-pull would revert

        uint16 margin = 100;
        uint256 mmAmount = _expectedMmAmount(debt, margin);
        uint64 userAmount = uint64(VAULT_001 - mmAmount);
        CloseRequest memory req = _closeRequest(vaultId, account, margin, userAmount, 1);
        bytes memory sig = _signClose(accountKey, req);
        bytes32 userClaimId = registry.claimIdOf(vaultId, 0);
        bytes32 mmClaimId = registry.claimIdOf(vaultId, 1);
        uint256 shares = _position(account).borrowShares;

        vm.expectEmit(address(MORPHO));
        emit EventsLib.Repay(marketId, address(adapter), account, debt, shares);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.CloseRelease(
            vaultId, userClaimId, mmClaimId, userAmount, uint64(mmAmount), USER_COMMITMENT, MM_COMMITMENT
        );
        _close(req, sig);

        // I7: mmAmount + userAmount == burned zZEC == outstanding before the close.
        assertEq(mmAmount + userAmount, VAULT_001);
        assertEq(zzec.totalSupply(), 0);
        _assertClaim(userClaimId, vaultId, ClaimKind.CloseUser, userAmount, USER_COMMITMENT, ClaimStatus.Pending);
        _assertClaim(
            mmClaimId, vaultId, ClaimKind.CloseMarketMaker, uint64(mmAmount), MM_COMMITMENT, ClaimStatus.Pending
        );
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(_outstanding(vaultId), 0);
        Position memory p = _position(account);
        assertEq(p.borrowShares, 0);
        assertEq(p.collateral, 0);
        assertEq(usdc.balanceOf(marketMaker), 0, "the market maker paid exactly the debt");
        assertEq(usdc.balanceOf(address(adapter)), 0);
        assertEq(usdc.allowance(address(adapter), address(MORPHO)), 0);
        assertEq(usdc.balanceOf(borrowReceiver), 160_000, "the borrower keeps the loan");
        assertTrue(adapter.nonceUsed(account, 1));
        _assertZZECOnlyInMorpho(_list(account, marketMaker));
    }

    // ================================================================== self-deleverage (S18)

    /// The executor repays part of the debt and is paid in collateral with its margin; the vault stays open with a
    /// lower LTV, and the payment is an ordinary third-party claim.
    function test_deleverage_repaysPartOfTheDebtAndLowersTheLtv() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_005, 5_500); // 1.1 USDC on 2 USDC of collateral
        uint256 ltvBefore = _ltvBps(account);
        uint256 debtBefore = MORPHO.expectedBorrowAssets(params, account);
        uint256 repay = 500_000; // 0.5 USDC
        uint16 margin = 100;
        uint256 mmAmount = _expectedMmAmount(repay, margin);
        assertEq(mmAmount, 1_262_500, "0.5 USDC at $40 plus 1 %");
        DeleverageRequest memory req = _deleverageRequest(vaultId, margin, repay, uint64(mmAmount), 7);
        bytes memory sig = _signDeleverage(req);
        _fundAndApproveAdapter(marketMaker, repay); // exact approval: any over-pull would revert
        bytes32 claimId = registry.claimIdOf(vaultId, 0);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.LiquidationRelease(vaultId, claimId, uint64(mmAmount), MM_COMMITMENT);
        vm.prank(marketMaker);
        assertEq(adapter.deleverageWithCollateral(req, sig, MM_COMMITMENT, uint64(mmAmount)), uint64(mmAmount));

        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "the vault stays open");
        assertEq(_outstanding(vaultId), VAULT_005 - mmAmount);
        assertEq(_position(account).collateral, VAULT_005 - mmAmount);
        assertApproxEqAbs(MORPHO.expectedBorrowAssets(params, account), debtBefore - repay, 1);
        assertLt(_ltvBps(account), ltvBefore, "LTV falls");
        assertEq(zzec.totalSupply(), VAULT_005 - mmAmount, "I1");
        _assertClaim(claimId, vaultId, ClaimKind.Liquidation, uint64(mmAmount), MM_COMMITMENT, ClaimStatus.Pending);
        assertEq(usdc.balanceOf(marketMaker), 0, "paid exactly the signed repayment");
        assertEq(usdc.allowance(address(adapter), address(MORPHO)), 0);
        assertTrue(adapter.nonceUsed(account, 7));
        _assertZZECOnlyInMorpho(_list(account, marketMaker));
    }

    /// I8 and the request's own bounds: only its executor, before its deadline, once (the nonce is shared with
    /// closes), within the margin cap, with a commitment and a repayment, for the account's own vault, signed by the
    /// account; the collateral within what the borrower signed and the executor accepts; the D11 floor.
    function test_deleverage_revertsOutsideTheRequest() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_005, 5_500);
        _fundAndApproveAdapter(marketMaker, 2 * USDC);
        DeleverageRequest memory req = _deleverageRequest(vaultId, 100, 500_000, 1_262_500, 1);
        bytes memory sig = _signDeleverage(req);

        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NotExecutor.selector, stranger, marketMaker));
        adapter.deleverageWithCollateral(req, sig, MM_COMMITMENT, 0);

        DeleverageRequest memory r = _deleverageRequest(vaultId, 100, 500_000, 1_262_500, 1);
        r.deadline = uint64(block.timestamp - 1);
        _expectDeleverageRevert(
            r, _signDeleverage(r), abi.encodeWithSelector(ISettlementAdapter.RequestExpired.selector, r.deadline)
        );
        r = _deleverageRequest(vaultId, 501, 500_000, 2_000_000, 1);
        _expectDeleverageRevert(
            r, _signDeleverage(r), abi.encodeWithSelector(ISettlementAdapter.MarginTooHigh.selector, 501, 500)
        );
        r = _deleverageRequest(vaultId, 100, 0, 2_000_000, 1);
        _expectDeleverageRevert(r, _signDeleverage(r), abi.encodeWithSelector(ISettlementAdapter.ZeroRepay.selector));
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.ZeroCommitment.selector);
        adapter.deleverageWithCollateral(req, sig, bytes32(0), 0);
        r = _deleverageRequest(keccak256("another vault"), 100, 500_000, 1_262_500, 1);
        _expectDeleverageRevert(
            r,
            _signDeleverage(r),
            abi.encodeWithSelector(ISettlementAdapter.VaultAccountMismatch.selector, r.vaultId, account)
        );

        // Signed by someone else, or over another request (here: a higher maximum).
        bytes memory foreign = _sign(signerKeys[0], adapter.hashDeleverageRequest(req));
        _expectDeleverageRevert(req, foreign, abi.encodeWithSelector(ISettlementAdapter.InvalidUserSignature.selector));
        r = _deleverageRequest(vaultId, 100, 500_000, 1_262_501, 1);
        _expectDeleverageRevert(r, sig, abi.encodeWithSelector(ISettlementAdapter.InvalidUserSignature.selector));

        // More collateral than the borrower signed for (a price drop since signing), or less than the executor accepts.
        r = _deleverageRequest(vaultId, 100, 500_000, 1_262_499, 1);
        _expectDeleverageRevert(
            r,
            _signDeleverage(r),
            abi.encodeWithSelector(ISettlementAdapter.DeleverageAboveMaximum.selector, 1_262_500, 1_262_499)
        );
        vm.prank(marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(ISettlementAdapter.MarketMakerAmountBelowMinimum.selector, 1_262_500, 1_262_501)
        );
        adapter.deleverageWithCollateral(req, sig, MM_COMMITMENT, 1_262_501);
        // Below the D11 floor.
        r = _deleverageRequest(vaultId, 0, 39_999, 2_000_000, 1); // 99,998 zat (rounded up)
        _expectDeleverageRevert(
            r,
            _signDeleverage(r),
            abi.encodeWithSelector(ISettlementAdapter.SeizeBelowMinimum.selector, 99_998, 100_000)
        );

        // The nonce: used once, and shared with closes.
        vm.prank(marketMaker);
        adapter.deleverageWithCollateral(req, sig, MM_COMMITMENT, 0);
        _expectDeleverageRevert(
            req, sig, abi.encodeWithSelector(ISettlementAdapter.NonceAlreadyUsed.selector, account, 1)
        );
        CloseRequest memory close = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory closeSig = _signClose(accountKey, close);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NonceAlreadyUsed.selector, account, 1));
        adapter.closeWithCollateral(close, closeSig, MM_COMMITMENT, type(uint256).max, 0);
    }

    /// A deleverage must leave at least MIN_SEIZE in the vault: taking (almost) everything is a close.
    function test_deleverage_revertsWhenItWouldEmptyTheVault() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 6_000); // 0.24 USDC on 0.4 USDC of collateral
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        // 0.24 USDC at 5 %: 630,000 zat, leaving 370,000: allowed. 0.36 USDC worth would not be, but the debt is only
        // 0.24 USDC; so with a lower price: at $25 the same 0.24 USDC is 1,008,000 zat, more than the vault.
        oracle.setPrice(25e34);
        DeleverageRequest memory r = _deleverageRequest(vaultId, 500, 230_000, VAULT_001, 1); // 966,000 zat
        bytes memory sig = _signDeleverage(r);
        _expectDeleverageRevert(
            r, sig, abi.encodeWithSelector(ISettlementAdapter.DeleverageRemainderTooSmall.selector, 34_000, 100_000)
        );
    }

    /// The mmAmount formula at both ends of the margin range (0 and MAX_MARGIN_BPS), rounded up.
    function test_close_marginBounds() public {
        uint16[2] memory margins = [uint16(0), uint16(500)];
        for (uint256 i; i < margins.length; ++i) {
            (address acct, uint256 key) = makeAddrAndKey(string.concat("zecured.adapter.margin", vm.toString(i)));
            (bytes32 vaultId,) = _openLoan(acct, VAULT_005, 3_333);
            uint256 debt = MORPHO.expectedBorrowAssets(params, acct);
            _fundAndApproveAdapter(marketMaker, debt);
            uint256 mmAmount = _expectedMmAmount(debt, margins[i]);
            CloseRequest memory req = _closeRequest(vaultId, acct, margins[i], 0, i);
            _close(req, _signClose(key, req));
            (,, uint64 cMm,,) = registry.claim(registry.claimIdOf(vaultId, 1));
            (,, uint64 cUser,,) = registry.claim(registry.claimIdOf(vaultId, 0));
            assertEq(cMm, mmAmount);
            assertEq(uint256(cMm) + cUser, VAULT_005, "I7");
            // ceil: mmAmount * price >= debt * (1 + margin) in loan units.
            assertGe(uint256(cMm) * oracle.price() * 10_000, debt * (10_000 + margins[i]) * ORACLE_PRICE_SCALE);
            assertLt((uint256(cMm) - 1) * oracle.price() * 10_000, debt * (10_000 + margins[i]) * ORACLE_PRICE_SCALE);
        }
    }

    function test_close_revertsForAnyoneButTheExecutor() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        address[3] memory callers = [stranger, account, liquidator];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NotExecutor.selector, callers[i], marketMaker));
            adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        }
    }

    function test_close_revertsWhenExpired() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.warp(uint256(req.deadline) + 1);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.RequestExpired.selector, req.deadline));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        // block.timestamp == deadline is still valid.
        vm.warp(req.deadline);
        _fundAndApproveAdapter(marketMaker, MORPHO.expectedBorrowAssets(params, account));
        _close(req, sig);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
    }

    function test_close_revertsOnAReusedNonce() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 7);
        bytes memory sig = _signClose(accountKey, req);
        _close(req, sig);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NonceAlreadyUsed.selector, account, 7));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsOnAnInvalidatedNonce() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 9);
        bytes memory sig = _signClose(accountKey, req);
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.NonceInvalidated(account, 9);
        vm.prank(account);
        adapter.invalidateNonce(9);
        assertTrue(adapter.nonceUsed(account, 9));
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NonceAlreadyUsed.selector, account, 9));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        assertGt(_position(account).collateral, 0, "nothing withdrawn");
    }

    function test_close_revertsWhenTheMarginExceedsTheCap() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 501, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.MarginTooHigh.selector, 501, 500));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsOnZeroCommitments() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.ZeroCommitment.selector);
        adapter.closeWithCollateral(req, sig, bytes32(0), type(uint256).max, 0);

        req.userCommitment = bytes32(0);
        sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.ZeroCommitment.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsWhenTheVaultIsNotTheAccounts() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        (address other, uint256 otherKey) = makeAddrAndKey("zecured.adapter.other");
        bytes32 otherVault = _attest(other, VAULT_001);

        // The account signs a request naming another account's vault.
        CloseRequest memory req = _closeRequest(otherVault, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultAccountMismatch.selector, otherVault, account));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // A request for a vault id of zero / an account without a vault.
        req = _closeRequest(bytes32(0), stranger, 100, 0, 1);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultAccountMismatch.selector, bytes32(0), stranger));
        adapter.closeWithCollateral(req, "", MM_COMMITMENT, type(uint256).max, 0);

        // The other account signs for the first account's vault: the vault binding fails before the signature.
        req = _closeRequest(vaultId, other, 100, 0, 1);
        sig = _signClose(otherKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultAccountMismatch.selector, vaultId, other));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsForAClosedVault() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.VaultNotActive.selector, vaultId));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsOnAnInvalidSignature() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);

        // Signed by someone else.
        (, uint256 strangerKey) = makeAddrAndKey("zecured.adapter.strangerKey");
        bytes memory sig = _signClose(strangerKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // Signed by the account, but for different terms (the executor raises minUserAmount... or lowers it).
        sig = _signClose(accountKey, req);
        req.minUserAmount = 1;
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // Empty and garbage signatures.
        req.minUserAmount = 0;
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
        adapter.closeWithCollateral(req, "", MM_COMMITMENT, type(uint256).max, 0);
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
        adapter.closeWithCollateral(req, new bytes(65), MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsWithoutDebt() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.NoDebt.selector, account));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsBelowTheUsersMinimum() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        _fundAndApproveAdapter(marketMaker, debt);
        uint256 userAmount = VAULT_001 - _expectedMmAmount(debt, 100);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, uint64(userAmount + 1), 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(ISettlementAdapter.UserAmountBelowMinimum.selector, userAmount, userAmount + 1)
        );
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // A price drop after signing shrinks the user's part below the signed minimum.
        req = _closeRequest(vaultId, account, 100, uint64(userAmount), 2);
        sig = _signClose(accountKey, req);
        oracle.setPrice(39e34);
        vm.prank(marketMaker);
        vm.expectPartialRevert(ISettlementAdapter.UserAmountBelowMinimum.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    function test_close_revertsWhenDebtPlusMarginExceedsTheCollateral() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 6_000);
        oracle.setPrice(20e34); // collateral now worth $0.20 against $0.24 of debt
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        _fundAndApproveAdapter(marketMaker, debt);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        uint256 mmAmount = _expectedMmAmount(debt, 100); // 1,212,000 zat > 1,000,000
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.CloseExceedsCollateral.selector, mmAmount, VAULT_001));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    /// Without the account's Morpho authorization of the Adapter, the close reverts atomically (the repay too).
    function test_close_revertsWithoutTheAdaptersAuthorization() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true); // the Adapter is NOT authorized
        MORPHO.borrow(params, 160_000, 0, account, borrowReceiver);
        vm.stopPrank();
        uint256 sharesBefore = _position(account).borrowShares;
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(bytes("unauthorized"));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        assertEq(_position(account).borrowShares, sharesBefore, "repay rolled back");
        assertEq(usdc.balanceOf(marketMaker), 1 * USDC);
        assertFalse(adapter.nonceUsed(account, 1), "nonce not consumed");
    }

    function test_close_revertsWhenTheExecutorDidNotApproveEnough() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        skip(1 days);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        _fundAndApproveAdapter(marketMaker, debt - 1);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        vm.prank(marketMaker);
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientAllowance.selector, address(adapter), debt - 1, debt)
        );
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    // ================================================================== defensive checks (unreachable with real Morpho)

    /// The Adapter checks that Morpho delivered exactly the reported zZEC. Forced by mocking zZEC.balanceOf(adapter).
    function test_liquidateAndClose_revertWhenTheReceivedAmountDiffers() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_005, 4_000);
        // Y5: the amount comes from the guard's receipt, never from a balance difference.
        vm.mockCall(address(zzec), abi.encodeCall(zzec.settleExpectation, ()), abi.encode(uint256(0)));

        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.UnexpectedCollateralReceived.selector, 0, VAULT_005));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        oracle.setPrice(PRICE_25_50);
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISettlementAdapter.UnexpectedCollateralReceived.selector, 0, 300_000));
        adapter.liquidate(account, 300_000, 0, LIQUIDATOR_COMMITMENT);
    }

    /// A zero oracle price is refused before any division (MockZecOracle itself cannot be set to 0).
    function test_close_revertsOnAZeroPrice() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        vm.mockCall(address(oracle), abi.encodeCall(oracle.price, ()), abi.encode(uint256(0)));
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.ZeroPrice.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
    }

    // ================================================================== callbacks and nonces

    function test_callbacks_onlyFromMorphoAndOnlyInsideAnOperation() public {
        vm.startPrank(stranger);
        vm.expectRevert(ISettlementAdapter.OnlyMorpho.selector);
        adapter.onMorphoRepay(1, hex"01");
        vm.expectRevert(ISettlementAdapter.OnlyMorpho.selector);
        adapter.onMorphoLiquidate(1, hex"01");
        vm.stopPrank();

        vm.startPrank(address(MORPHO));
        vm.expectRevert(ISettlementAdapter.UnexpectedCallback.selector);
        adapter.onMorphoRepay(1, hex"01");
        vm.expectRevert(ISettlementAdapter.UnexpectedCallback.selector);
        adapter.onMorphoLiquidate(1, hex"01");
        vm.stopPrank();
    }

    function test_invalidateNonce_isIdempotentAndPerAccount() public {
        assertFalse(adapter.nonceUsed(account, 5));
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.NonceInvalidated(account, 5);
        vm.prank(account);
        adapter.invalidateNonce(5);
        assertTrue(adapter.nonceUsed(account, 5));
        assertFalse(adapter.nonceUsed(stranger, 5), "only the caller's nonce");

        // Invalidating again neither reverts nor changes anything.
        vm.expectEmit(address(adapter));
        emit ISettlementAdapter.NonceInvalidated(account, 5);
        vm.prank(account);
        adapter.invalidateNonce(5);
        assertTrue(adapter.nonceUsed(account, 5));
        assertFalse(adapter.nonceUsed(stranger, 5));
    }

    /// A cancel that races the market maker's close does not revert (the nonce was already consumed by the close).
    function test_invalidateNonce_afterTheNonceWasUsedByAClose() public {
        (bytes32 vaultId,) = _openLoan(account, VAULT_001, 4_000);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 11);
        _close(req, _signClose(accountKey, req));
        assertTrue(adapter.nonceUsed(account, 11));

        vm.prank(account);
        adapter.invalidateNonce(11);
        assertTrue(adapter.nonceUsed(account, 11));
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed), "the close stands");
    }
}
