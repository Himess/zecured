// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The zZEC transfer guard against the real Morpho Blue (I2): every path that would move a vault's collateral
///         without the Registry or the Adapter fails, and Morpho reports it as "transfer reverted".
contract TransferGuardTest is ZecuredFixture {
    address internal account = makeAddr("zecured.guard.account");
    bytes32 internal vaultId;

    function setUp() public override {
        super.setUp();
        vaultId = _attest(account, VAULT_005);
    }

    function _assertCollateralUntouched() internal view {
        assertEq(_position(account).collateral, VAULT_005);
        assertEq(_outstanding(vaultId), VAULT_005);
        assertEq(zzec.totalSupply(), VAULT_005);
        _assertZZECOnlyInMorpho(_list(account, stranger));
    }

    /// Morpho itself allows these (msg.sender == onBehalf); the token refuses them.
    function test_accountCannotWithdrawItsCollateralAnywhere() public {
        _authorizeBoth(account);
        address[5] memory receivers = [account, stranger, address(registry), address(adapter), address(MORPHO)];
        vm.startPrank(account);
        for (uint256 i; i < receivers.length; ++i) {
            vm.expectRevert(bytes("transfer reverted"));
            MORPHO.withdrawCollateral(params, 1, account, receivers[i]);
            vm.expectRevert(bytes("transfer reverted"));
            MORPHO.withdrawCollateral(params, VAULT_005, account, receivers[i]);
        }
        vm.stopPrank();
        _assertCollateralUntouched();
    }

    function test_strangerCannotWithdrawOnBehalfOfTheAccount() public {
        vm.prank(stranger);
        vm.expectRevert(bytes("unauthorized"));
        MORPHO.withdrawCollateral(params, VAULT_005, account, stranger);
        vm.prank(stranger);
        vm.expectRevert(bytes("unauthorized"));
        MORPHO.borrow(params, 1, 0, account, stranger);
        _assertCollateralUntouched();
    }

    /// Only the Adapter can liquidate: Morpho transfers the seized zZEC to its caller, which the token refuses.
    function test_directMorphoLiquidateReverts() public {
        _authorizeBoth(account);
        uint256 shares = _borrow(account, 800_000); // 40 % LTV at $40
        oracle.setPrice(PRICE_25_50);
        usdc.mint(liquidator, 10 * USDC);
        vm.startPrank(liquidator);
        usdc.approve(address(MORPHO), type(uint256).max);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.liquidate(params, account, 0, shares, "");
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.liquidate(params, account, MIN_SEIZE, 0, "");
        vm.stopPrank();
        assertEq(_position(account).borrowShares, shares);
        _assertCollateralUntouched();
    }

    /// Morpho flash-lends any token it holds, collateral included; the token refuses the outbound transfer.
    function test_zzecFlashLoanReverts() public {
        vm.prank(stranger);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.flashLoan(address(zzec), VAULT_005, "");
        vm.prank(stranger);
        vm.expectRevert(bytes("transfer reverted"));
        MORPHO.flashLoan(address(zzec), 1, hex"01");
        _assertCollateralUntouched();
    }

    /// Nobody but the Registry ever holds zZEC, so nobody can supply zZEC collateral to another position.
    function test_nobodyElseCanSupplyZzecCollateral() public {
        vm.prank(stranger);
        vm.expectRevert(bytes("transferFrom reverted"));
        MORPHO.supplyCollateral(params, 1, stranger, "");
        vm.prank(account);
        vm.expectRevert(bytes("transferFrom reverted"));
        MORPHO.supplyCollateral(params, 1, account, "");
        _assertCollateralUntouched();
    }

    /// The Registry's own position cannot be used either: the Registry never holds zZEC at rest and has no function
    /// that supplies or withdraws for itself.
    function test_morphoHoldsExactlyTheVaultCollateral() public view {
        Position memory p = _position(address(registry));
        assertEq(p.collateral, 0);
        p = _position(address(adapter));
        assertEq(p.collateral, 0);
        assertEq(zzec.balanceOf(address(MORPHO)), VAULT_005);
    }
}
