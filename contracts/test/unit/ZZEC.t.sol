// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {OneTx, TransientProbe} from "../utils/OneTx.sol";

/// @notice Unit tests of the zZEC transfer guard (docs/INTERFACES.md § 3.1, I2). No fork: Registry, Adapter and Morpho
///         are plain addresses here; the real-Morpho behaviour is covered by the fork tests.
contract ZZECTest is Test {
    ZZEC internal zzec;
    OneTx internal oneTx;

    address internal registry = makeAddr("zecured.zzec.registry");
    address internal adapter = makeAddr("zecured.zzec.adapter");
    address internal morpho = makeAddr("zecured.zzec.morpho");
    address internal account = makeAddr("zecured.zzec.account");
    address internal stranger = makeAddr("zecured.zzec.stranger");

    function setUp() public {
        zzec = new ZZEC(registry, adapter, morpho);
        oneTx = new OneTx();
    }

    // ------------------------------------------------------------------ helpers

    /// The legitimate way into Morpho: the Registry mints to itself and Morpho pulls it (supplyCollateral).
    function _supplyToMorpho(uint256 amount) internal {
        vm.prank(registry);
        zzec.mint(registry, amount);
        vm.prank(registry);
        zzec.approve(morpho, amount);
        vm.prank(morpho);
        assertTrue(zzec.transferFrom(registry, morpho, amount));
    }

    function _call(address sender, bytes memory data) internal view returns (OneTx.Call memory) {
        return OneTx.Call({sender: sender, target: address(zzec), data: data});
    }

    function _arm(address receiver, uint256 maxAmount) internal view returns (OneTx.Call memory) {
        return _call(receiver, abi.encodeCall(IZZEC.expectFromMorpho, (maxAmount)));
    }

    function _morphoTransfer(address to, uint256 value) internal view returns (OneTx.Call memory) {
        return _call(morpho, abi.encodeCall(zzec.transfer, (to, value)));
    }

    function _run(OneTx.Call memory a, OneTx.Call memory b) internal {
        OneTx.Call[] memory calls = new OneTx.Call[](2);
        calls[0] = a;
        calls[1] = b;
        oneTx.run(calls);
    }

    function _run(OneTx.Call memory a, OneTx.Call memory b, OneTx.Call memory c) internal {
        OneTx.Call[] memory calls = new OneTx.Call[](3);
        calls[0] = a;
        calls[1] = b;
        calls[2] = c;
        oneTx.run(calls);
    }

    /// Moves `amount` from Morpho to the Adapter the legitimate way (armed by the Adapter, same transaction).
    function _morphoToAdapter(uint256 amount) internal {
        _run(_arm(adapter, amount), _morphoTransfer(adapter, amount));
    }

    // ------------------------------------------------------------------ metadata and wiring

    function test_metadataAndWiring() public view {
        assertEq(zzec.name(), "Zecured ZEC Receipt");
        assertEq(zzec.symbol(), "zZEC");
        assertEq(zzec.decimals(), 8);
        assertEq(zzec.registry(), registry);
        assertEq(zzec.adapter(), adapter);
        assertEq(zzec.morpho(), morpho);
        assertEq(zzec.totalSupply(), 0);
        (address recipient, uint256 maxAmount) = zzec.pendingExpectation();
        assertEq(recipient, address(0));
        assertEq(maxAmount, 0);
    }

    function test_constructor_rejectsZeroAddresses() public {
        vm.expectRevert(IZZEC.ZeroAddress.selector);
        new ZZEC(address(0), adapter, morpho);
        vm.expectRevert(IZZEC.ZeroAddress.selector);
        new ZZEC(registry, address(0), morpho);
        vm.expectRevert(IZZEC.ZeroAddress.selector);
        new ZZEC(registry, adapter, address(0));
    }

    // ------------------------------------------------------------------ mint / burn

    function test_mint_byRegistryToRegistry() public {
        vm.prank(registry);
        zzec.mint(registry, 1_000_000);
        assertEq(zzec.balanceOf(registry), 1_000_000);
        assertEq(zzec.totalSupply(), 1_000_000);
    }

    function test_mint_revertsForAnyoneButTheRegistry() public {
        address[4] memory callers = [adapter, morpho, account, stranger];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.OnlyRegistry.selector, callers[i]));
            zzec.mint(registry, 1);
        }
    }

    function test_mint_revertsToAnyoneButTheRegistry() public {
        address[4] memory receivers = [adapter, morpho, account, stranger];
        for (uint256 i; i < receivers.length; ++i) {
            vm.prank(registry);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.MintOnlyToRegistry.selector, receivers[i]));
            zzec.mint(receivers[i], 1);
        }
    }

    function test_burn_fromRegistryAndFromAdapter() public {
        vm.prank(registry);
        zzec.mint(registry, 300);
        vm.prank(registry);
        zzec.burn(registry, 100);
        assertEq(zzec.totalSupply(), 200);

        // The Adapter receives zZEC only from Morpho; the Registry burns it there.
        vm.prank(registry);
        zzec.approve(morpho, 200);
        vm.prank(morpho);
        assertTrue(zzec.transferFrom(registry, morpho, 200));
        _morphoToAdapter(150);
        vm.prank(registry);
        zzec.burn(adapter, 150);
        assertEq(zzec.balanceOf(adapter), 0);
        assertEq(zzec.totalSupply(), 50);
    }

    function test_burn_revertsForAnyoneButTheRegistry() public {
        vm.prank(registry);
        zzec.mint(registry, 10);
        address[4] memory callers = [adapter, morpho, account, stranger];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.OnlyRegistry.selector, callers[i]));
            zzec.burn(registry, 1);
        }
    }

    function test_burn_revertsFromMorphoOrAnAccount() public {
        _supplyToMorpho(10);
        vm.prank(registry);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.BurnOnlyFromRegistryOrAdapter.selector, morpho));
        zzec.burn(morpho, 1);
        vm.prank(registry);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.BurnOnlyFromRegistryOrAdapter.selector, account));
        zzec.burn(account, 0);
    }

    function test_burn_moreThanBalanceReverts() public {
        vm.prank(registry);
        zzec.mint(registry, 10);
        vm.prank(registry);
        vm.expectRevert(abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, adapter, 0, 1));
        zzec.burn(adapter, 1);
    }

    // ------------------------------------------------------------------ Registry -> Morpho

    function test_registryToMorpho_pulledByMorpho() public {
        _supplyToMorpho(1_000_000);
        assertEq(zzec.balanceOf(morpho), 1_000_000);
        assertEq(zzec.balanceOf(registry), 0);
        assertEq(zzec.allowance(registry, morpho), 0);
    }

    function test_registryToMorpho_directTransferReverts() public {
        vm.prank(registry);
        zzec.mint(registry, 10);
        vm.prank(registry);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, registry, morpho));
        zzec.transfer(morpho, 10);
    }

    function test_registryOut_byAnotherSpenderReverts() public {
        vm.prank(registry);
        zzec.mint(registry, 10);
        vm.prank(registry);
        zzec.approve(stranger, 10);
        vm.startPrank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, registry, morpho));
        zzec.transferFrom(registry, morpho, 10);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, registry, stranger));
        zzec.transferFrom(registry, stranger, 10);
        vm.stopPrank();
    }

    function test_registryToAnyoneElseReverts() public {
        vm.prank(registry);
        zzec.mint(registry, 10);
        address[3] memory receivers = [adapter, account, stranger];
        for (uint256 i; i < receivers.length; ++i) {
            vm.prank(registry);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, registry, receivers[i]));
            zzec.transfer(receivers[i], 1);
        }
    }

    // ------------------------------------------------------------------ Morpho -> Registry / Adapter

    function test_morphoOut_withoutExpectationReverts() public {
        _supplyToMorpho(100);
        address[4] memory receivers = [registry, adapter, account, stranger];
        for (uint256 i; i < receivers.length; ++i) {
            vm.prank(morpho);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, receivers[i], 1));
            zzec.transfer(receivers[i], 1);
        }
    }

    function test_morphoOut_armedByRegistry_isOneShot() public {
        _supplyToMorpho(100);
        _run(_arm(registry, 60), _morphoTransfer(registry, 60));
        assertEq(zzec.balanceOf(registry), 60);
        assertEq(zzec.balanceOf(morpho), 40);

        // A second transfer in the same transaction finds the expectation consumed.
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, registry, 10));
        _run(_arm(registry, 10), _morphoTransfer(registry, 10), _morphoTransfer(registry, 10));
    }

    function test_morphoOut_armedByAdapter() public {
        _supplyToMorpho(100);
        _morphoToAdapter(100);
        assertEq(zzec.balanceOf(adapter), 100);
        assertEq(zzec.balanceOf(morpho), 0);
    }

    function test_morphoOut_lessThanMaxIsAllowed_andConsumesTheExpectation() public {
        _supplyToMorpho(100);
        OneTx.Call[] memory calls = new OneTx.Call[](3);
        calls[0] = _arm(adapter, 100);
        calls[1] = _morphoTransfer(adapter, 30); // the repaidShares liquidation path: max is an upper bound
        calls[2] = _call(address(this), abi.encodeCall(IZZEC.pendingExpectation, ()));
        bytes[] memory results = oneTx.run(calls);
        (address recipient, uint256 maxAmount) = abi.decode(results[2], (address, uint256));
        assertEq(recipient, address(0), "consumed");
        assertEq(maxAmount, 0, "consumed");
        assertEq(zzec.balanceOf(adapter), 30);
    }

    function test_morphoOut_aboveMaxReverts() public {
        _supplyToMorpho(100);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, registry, 51));
        _run(_arm(registry, 50), _morphoTransfer(registry, 51));
    }

    function test_morphoOut_toAnotherReceiverThanTheArmingOneReverts() public {
        _supplyToMorpho(100);
        // Armed by the Registry: neither the Adapter nor an account can receive.
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, adapter, 1));
        _run(_arm(registry, 100), _morphoTransfer(adapter, 1));
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, account, 1));
        _run(_arm(registry, 100), _morphoTransfer(account, 1));
        // Armed by the Adapter: the Registry cannot receive.
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, registry, 1));
        _run(_arm(adapter, 100), _morphoTransfer(registry, 1));
    }

    function test_expectFromMorpho_onlyRegistryOrAdapter() public {
        address[3] memory callers = [morpho, account, stranger];
        for (uint256 i; i < callers.length; ++i) {
            vm.prank(callers[i]);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.OnlyRegistryOrAdapter.selector, callers[i]));
            zzec.expectFromMorpho(1);
        }
    }

    function test_rearmingOverwritesThePreviousExpectation() public {
        _supplyToMorpho(100);
        // The Adapter's later arming replaces the Registry's: only the Adapter can receive.
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, registry, 1));
        _run(_arm(registry, 100), _arm(adapter, 100), _morphoTransfer(registry, 1));
    }

    /// The expectation lives in EIP-1153 transient storage, so it dies with the transaction. Only meaningful under
    /// `forge test --isolate` (without it, Foundry keeps transient storage across a test's top-level calls).
    function test_expectationDoesNotSurviveTheTransaction() public {
        TransientProbe probe = new TransientProbe();
        probe.set();
        bool isolated = probe.get() == 0;
        if (!isolated) vm.skip(true, "needs --isolate");

        _supplyToMorpho(100);
        vm.prank(registry);
        zzec.expectFromMorpho(100); // transaction 1
        (address recipient,) = zzec.pendingExpectation();
        assertEq(recipient, address(0), "transient storage cleared after the arming transaction");
        vm.prank(morpho);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.UnexpectedTransferFromMorpho.selector, registry, 100));
        zzec.transfer(registry, 100); // transaction 2
    }

    // ------------------------------------------------------------------ accounts and the Adapter cannot move zZEC

    function test_accountTransfersRevert() public {
        vm.startPrank(account);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, account, stranger));
        zzec.transfer(stranger, 0);
        vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, account, morpho));
        zzec.transfer(morpho, 0);
        vm.stopPrank();
    }

    function test_adapterCannotTransferOut() public {
        _supplyToMorpho(100);
        _morphoToAdapter(100);
        address[4] memory receivers = [registry, morpho, account, stranger];
        for (uint256 i; i < receivers.length; ++i) {
            vm.prank(adapter);
            vm.expectRevert(abi.encodeWithSelector(IZZEC.TransferNotAllowed.selector, adapter, receivers[i]));
            zzec.transfer(receivers[i], 1);
        }
    }

    // ------------------------------------------------------------------ fuzz: the whole transfer matrix

    /// Any (sender, spender, receiver, arming, max, value) combination succeeds exactly when the guard's rules allow it.
    function testFuzz_transferMatrix(
        uint8 fromSeed,
        uint8 toSeed,
        uint8 spenderSeed,
        uint8 armSeed,
        uint64 maxAmount,
        uint64 value
    ) public {
        address[5] memory actors = [registry, adapter, morpho, account, stranger];
        address from = actors[fromSeed % 5];
        address to = actors[toSeed % 5];
        address spender = actors[spenderSeed % 5]; // == from: plain transfer; otherwise transferFrom
        address armedBy = armSeed % 3 == 0 ? address(0) : (armSeed % 3 == 1 ? registry : adapter);

        // Balances: the Registry and Morpho hold zZEC the legitimate way; everyone else holds none.
        uint256 supply = type(uint64).max;
        _supplyToMorpho(supply / 2);
        vm.prank(registry);
        zzec.mint(registry, supply / 2);
        if (spender != from) {
            vm.prank(from);
            zzec.approve(spender, type(uint256).max);
        }

        bool allowed;
        if (from == morpho) allowed = to == armedBy && value <= maxAmount;
        else allowed = from == registry && to == morpho && spender == morpho;
        allowed = allowed && value <= zzec.balanceOf(from);

        OneTx.Call[] memory calls = new OneTx.Call[](armedBy == address(0) ? 1 : 2);
        if (armedBy != address(0)) calls[0] = _arm(armedBy, maxAmount);
        calls[calls.length - 1] = spender == from
            ? _call(from, abi.encodeCall(zzec.transfer, (to, value)))
            : _call(spender, abi.encodeCall(zzec.transferFrom, (from, to, value)));

        uint256 toBefore = zzec.balanceOf(to);
        try oneTx.run(calls) {
            assertTrue(allowed, "a forbidden transfer succeeded");
            if (from != to) assertEq(zzec.balanceOf(to), toBefore + value);
        } catch {
            assertFalse(allowed, "an allowed transfer reverted");
        }
        // Nobody but Registry, Adapter and Morpho ever holds zZEC (I2).
        assertEq(zzec.balanceOf(account), 0);
        assertEq(zzec.balanceOf(stranger), 0);
    }
}
