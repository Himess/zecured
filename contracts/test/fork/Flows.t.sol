// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, IMorphoBase, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice End-to-end vault lifecycles on the real Morpho Blue (Base Sepolia fork), with the invariants checked after
///         every step: I1 (supply == outstanding), I2 (zZEC only in Morpho at rest), I3 (every burn has a claim),
///         I7 (close conservation), I9 (seized <= outstanding).
contract FlowsTest is ZecuredFixture {
    using MorphoBalancesLib for IMorpho;

    address internal account;
    uint256 internal accountKey;

    function setUp() public override {
        super.setUp();
        (account, accountKey) = makeAddrAndKey("zecured.flows.account");
        vm.label(account, "account");
    }

    function _assertInvariants(bytes32 vaultId, uint64 attested) internal view {
        (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        // I1 (single vault in these flows).
        assertEq(zzec.totalSupply(), status == VaultStatus.Active ? outstanding : 0, "I1");
        // Morpho collateral == outstanding.
        assertEq(_position(account).collateral, outstanding, "collateral == outstanding");
        // I2.
        _assertZZECOnlyInMorpho(_list(account, borrowReceiver));
        // I3 on Base: everything burned is covered by claims.
        uint256 claimed;
        for (uint64 i; i < registry.claimCount(vaultId); ++i) {
            (,, uint64 amount,,) = registry.claim(registry.claimIdOf(vaultId, i));
            claimed += amount;
        }
        assertEq(claimed + outstanding, attested, "attested == outstanding + claims");
    }

    /// Neither the Registry nor the Adapter (nor anyone else, from here on) borrows or withdraws loan assets.
    function _expectNoBorrowOrWithdrawFromHereOn() internal {
        vm.expectCall(address(MORPHO), abi.encodeWithSelector(IMorphoBase.borrow.selector), 0);
        vm.expectCall(address(MORPHO), abi.encodeWithSelector(IMorphoBase.withdraw.selector), 0);
    }

    function _confirm(bytes32 vaultId, bytes32[] memory ids) internal {
        registry.confirmPayout(vaultId, ids, _threeSigs(registry.hashPayoutConfirmation(vaultId, ids)));
        for (uint256 i; i < ids.length; ++i) {
            (,,,, ClaimStatus status) = registry.claim(ids[i]);
            assertEq(uint8(status), uint8(ClaimStatus.Paid));
        }
    }

    // ------------------------------------------------------------------ P2: repay + release

    function test_flow_attestBorrowRepayRelease() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _assertInvariants(vaultId, VAULT_005);

        _authorizeBoth(account);
        _borrow(account, 800_000); // 0.80 USDC = 40 % LTV at $40
        assertEq(usdc.balanceOf(borrowReceiver), 800_000);
        assertEq(usdc.balanceOf(account), 0, "the account never touches the USDC");
        _assertInvariants(vaultId, VAULT_005);

        skip(30 days);
        uint256 repaid = _repayAll(account);
        assertGt(repaid, 800_000, "real AdaptiveCurveIRM interest");
        assertEq(_position(account).borrowShares, 0);
        _assertInvariants(vaultId, VAULT_005);

        _expectNoBorrowOrWithdrawFromHereOn();
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        _assertClaim(claimId, vaultId, ClaimKind.Release, VAULT_005, RELEASE_COMMITMENT, ClaimStatus.Pending);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        _assertInvariants(vaultId, VAULT_005);

        bytes32[] memory ids = new bytes32[](1);
        ids[0] = claimId;
        _confirm(vaultId, ids);
    }

    // ------------------------------------------------------------------ P6: close from collateral

    function test_flow_closeFromCollateral() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        _borrow(account, 160_000);
        skip(7 days);
        _expectNoBorrowOrWithdrawFromHereOn();

        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        uint256 mmAmount = _expectedMmAmount(debt, 100);
        uint64 quotedUser = uint64(VAULT_001 - mmAmount);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, quotedUser - 1_000, 1);
        bytes memory sig = _signClose(accountKey, req);
        _fundAndApproveAdapter(marketMaker, debt); // exact: pulled inside onMorphoRepay
        uint256 outstandingBefore = _outstanding(vaultId);
        uint256 supplyBefore = zzec.totalSupply();

        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT);

        bytes32 userClaimId = registry.claimIdOf(vaultId, 0);
        bytes32 mmClaimId = registry.claimIdOf(vaultId, 1);
        (,, uint64 userAmount,,) = registry.claim(userClaimId);
        (,, uint64 mmClaimAmount,,) = registry.claim(mmClaimId);
        // I7: mmAmount + userAmount == burned zZEC == outstanding before the close.
        assertEq(uint256(mmClaimAmount) + userAmount, outstandingBefore, "I7 sum");
        assertEq(supplyBefore - zzec.totalSupply(), outstandingBefore, "I7 burned");
        assertEq(mmClaimAmount, mmAmount);
        assertEq(userAmount, quotedUser);
        assertGe(userAmount, req.minUserAmount);
        assertEq(usdc.balanceOf(marketMaker), 0, "market maker paid exactly the debt");
        assertEq(_position(account).borrowShares, 0);
        _assertInvariants(vaultId, VAULT_001);

        // One payout transaction pays both outputs: the signers confirm both claims together, user claim first.
        bytes32[] memory ids = new bytes32[](2);
        (ids[0], ids[1]) = (userClaimId, mmClaimId);
        _confirm(vaultId, ids);
    }

    // ------------------------------------------------------------------ P3: partial liquidation, then release

    function test_flow_partialLiquidationThenRelease() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        uint256 shares = _borrow(account, 160_000);
        _expectNoBorrowOrWithdrawFromHereOn();
        oracle.setPrice(PRICE_25_50);

        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        (uint256 seized,) = adapter.liquidate(account, 0, shares, LIQUIDATOR_COMMITMENT);
        assertGe(seized, MIN_SEIZE);
        uint64 remainder = uint64(VAULT_001 - seized);
        assertGe(remainder, MIN_SEIZE, "D11");
        assertEq(_outstanding(vaultId), remainder);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active));
        _assertInvariants(vaultId, VAULT_001);

        // All debt was repaid by the liquidator: the borrower releases the rest.
        vm.prank(account);
        bytes32 releaseClaim = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        assertEq(releaseClaim, registry.claimIdOf(vaultId, 1));
        _assertClaim(releaseClaim, vaultId, ClaimKind.Release, remainder, RELEASE_COMMITMENT, ClaimStatus.Pending);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        _assertInvariants(vaultId, VAULT_001);

        bytes32[] memory ids = new bytes32[](1);
        ids[0] = registry.claimIdOf(vaultId, 0);
        _confirm(vaultId, ids);
        ids[0] = releaseClaim;
        _confirm(vaultId, ids);
    }

    // ------------------------------------------------------------------ P3 then P6: liquidation, then close

    function test_flow_liquidationThenClose() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        _borrow(account, 800_000);
        _expectNoBorrowOrWithdrawFromHereOn();
        oracle.setPrice(PRICE_25_50);

        _fundAndApproveAdapter(liquidator, 10 * USDC);
        vm.prank(liquidator);
        (uint256 seized,) = adapter.liquidate(account, 1_000_000, 0, LIQUIDATOR_COMMITMENT);
        assertEq(seized, 1_000_000);
        assertGt(_position(account).borrowShares, 0, "debt remains");
        _assertInvariants(vaultId, VAULT_005);

        // The borrower exits the rest through a market maker.
        skip(1 hours);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        uint64 outstandingBefore = _outstanding(vaultId);
        assertEq(outstandingBefore, VAULT_005 - 1_000_000);
        CloseRequest memory req = _closeRequest(vaultId, account, 250, 0, 77);
        bytes memory sig = _signClose(accountKey, req);
        _fundAndApproveAdapter(marketMaker, debt);
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT);

        (,, uint64 userAmount,,) = registry.claim(registry.claimIdOf(vaultId, 1));
        (,, uint64 mmAmount,,) = registry.claim(registry.claimIdOf(vaultId, 2));
        assertEq(uint256(userAmount) + mmAmount, outstandingBefore, "I7");
        assertEq(mmAmount, _expectedMmAmount(debt, 250));
        _assertClaim(
            registry.claimIdOf(vaultId, 0),
            vaultId,
            ClaimKind.Liquidation,
            1_000_000,
            LIQUIDATOR_COMMITMENT,
            ClaimStatus.Pending
        );
        _assertClaim(
            registry.claimIdOf(vaultId, 1),
            vaultId,
            ClaimKind.CloseUser,
            userAmount,
            USER_COMMITMENT,
            ClaimStatus.Pending
        );
        _assertClaim(
            registry.claimIdOf(vaultId, 2),
            vaultId,
            ClaimKind.CloseMarketMaker,
            mmAmount,
            MM_COMMITMENT,
            ClaimStatus.Pending
        );
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        _assertInvariants(vaultId, VAULT_005);
    }

    // ------------------------------------------------------------------ several vaults side by side (I1, I4)

    function test_flow_severalVaultsKeepSeparateBooks() public {
        address[3] memory accounts =
            [makeAddr("zecured.flows.a"), makeAddr("zecured.flows.b"), makeAddr("zecured.flows.c")];
        bytes32[3] memory ids;
        for (uint256 i; i < 3; ++i) {
            ids[i] = _attest(accounts[i], i == 1 ? VAULT_005 : VAULT_001);
            _authorizeBoth(accounts[i]);
            assertEq(registry.vaultOf(accounts[i]), ids[i]);
        }
        assertEq(zzec.totalSupply(), 2 * VAULT_001 + VAULT_005, "I1");

        _borrow(accounts[0], 160_000);
        vm.prank(accounts[2]);
        registry.requestRelease(ids[2], RELEASE_COMMITMENT);
        assertEq(zzec.totalSupply(), VAULT_001 + VAULT_005, "I1 after a release");

        // One vault's account cannot release another vault.
        vm.prank(accounts[1]);
        vm.expectRevert();
        registry.requestRelease(ids[0], RELEASE_COMMITMENT);
        for (uint256 i; i < 3; ++i) {
            Position memory p = _position(accounts[i]);
            assertEq(p.collateral, _outstanding(ids[i]));
            assertEq(zzec.balanceOf(accounts[i]), 0);
        }
    }
}
