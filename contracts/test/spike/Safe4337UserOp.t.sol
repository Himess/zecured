// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Vm.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IMorpho, IMorphoBase, MarketParams, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {SpikeCBase} from "./SpikeCBase.sol";
import {SpikeRegistry} from "./SpikeRegistry.sol";
import {SpikeAllowlistPaymaster} from "./SpikeAllowlistPaymaster.sol";
import {PackedUserOperation, IEntryPointV07, ISafe4337Execute} from "./AccountAbstractionV07.sol";
import {SafeAddresses, ISafe, ISafeProxyFactory, ISafeModuleSetup, IMultiSendCallOnly} from "./SafeInterfaces.sol";

/// @notice Spike C bonus: the real ERC-4337 path on a Base Sepolia fork, without a bundler or paymaster API key.
///         EntryPoint v0.7 (real) + Safe 1.4.1 + Safe4337Module 0.3.0 (real) + a stand-in allowlist paymaster
///         (MOCK, SpikeAllowlistPaymaster). The test contract plays the bundler by calling handleOps directly.
///         Not covered: the provider's bundler RPC, its paymaster signing service and its sponsorship policies.
contract Safe4337UserOpTest is SpikeCBase {
    using MorphoBalancesLib for IMorpho;

    IEntryPointV07 internal constant ENTRY_POINT = IEntryPointV07(SafeAddresses.ENTRY_POINT_V07);
    uint8 internal constant OPERATION_DELEGATECALL = 1;
    uint256 internal constant BORROW = 160 * USDC;

    System internal sys;
    SpikeAllowlistPaymaster internal paymaster;
    address internal owner;
    uint256 internal ownerPk;
    address internal bundler;
    address internal payout;
    address internal attacker;

    function setUp() public {
        _selectBaseSepoliaFork();
        sys = _deploySystem(true);
        _supplyAsLender(sys, makeAddr("zecured.spikeC.aaLender"), 100_000 * USDC);
        (owner, ownerPk) = makeAddrAndKey("zecured.spikeC.aaOwner");
        bundler = makeAddr("zecured.spikeC.bundler");
        payout = makeAddr("zecured.spikeC.aaPayout");
        attacker = makeAddr("zecured.spikeC.aaAttacker");

        paymaster = new SpikeAllowlistPaymaster(
            address(ENTRY_POINT), address(MORPHO), sys.marketParams, address(sys.registry), address(sys.adapter)
        );
        vm.label(address(paymaster), "SpikeAllowlistPaymaster");
        vm.label(address(ENTRY_POINT), "EntryPointV07");
        vm.deal(address(this), 1 ether);
        ENTRY_POINT.depositTo{value: 1 ether}(address(paymaster));
    }

    // ------------------------------------------------------------------ helpers

    function _initializer() internal view returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = owner;
        address[] memory modules = new address[](1);
        modules[0] = SafeAddresses.SAFE_4337_MODULE;
        return abi.encodeCall(
            ISafe.setup,
            (
                owners,
                1,
                SafeAddresses.SAFE_MODULE_SETUP,
                abi.encodeCall(ISafeModuleSetup.enableModules, (modules)),
                SafeAddresses.SAFE_4337_MODULE,
                address(0),
                0,
                payable(address(0))
            )
        );
    }

    function _initCode(uint256 saltNonce) internal view returns (bytes memory) {
        return abi.encodePacked(
            SafeAddresses.SAFE_PROXY_FACTORY,
            abi.encodeCall(
                ISafeProxyFactory.createProxyWithNonce, (SafeAddresses.SAFE_SINGLETON, _initializer(), saltNonce)
            )
        );
    }

    /// Client-side prediction of the account address (needed before deployment: attestDeposit supplies collateral
    /// on behalf of the account before its first UserOp).
    function _predictSafe(uint256 saltNonce) internal view returns (address) {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(_initializer()), saltNonce));
        bytes memory deploymentData = abi.encodePacked(
            ISafeProxyFactory(SafeAddresses.SAFE_PROXY_FACTORY).proxyCreationCode(),
            uint256(uint160(SafeAddresses.SAFE_SINGLETON))
        );
        return vm.computeCreate2Address(salt, keccak256(deploymentData), SafeAddresses.SAFE_PROXY_FACTORY);
    }

    function _senderFromEntryPoint(bytes memory initCode) internal returns (address sender) {
        try ENTRY_POINT.getSenderAddress(initCode) {
            revert("getSenderAddress must revert");
        } catch (bytes memory reason) {
            require(bytes4(reason) == IEntryPointV07.SenderAddressResult.selector, "unexpected revert");
            assembly {
                sender := mload(add(reason, 36))
            }
        }
    }

    function _packCall(address to, bytes memory data) internal pure returns (bytes memory) {
        return abi.encodePacked(uint8(0), to, uint256(0), uint256(data.length), data);
    }

    function _executeBatch(bytes memory packedCalls) internal pure returns (bytes memory) {
        return abi.encodeCall(
            ISafe4337Execute.executeUserOp,
            (
                SafeAddresses.MULTI_SEND_CALL_ONLY,
                0,
                abi.encodeCall(IMultiSendCallOnly.multiSend, (packedCalls)),
                OPERATION_DELEGATECALL
            )
        );
    }

    function _userOp(address sender, bytes memory initCode, bytes memory callData)
        internal
        view
        returns (PackedUserOperation memory op)
    {
        op.sender = sender;
        op.nonce = ENTRY_POINT.getNonce(sender, 0);
        op.initCode = initCode;
        op.callData = callData;
        op.accountGasLimits = bytes32((uint256(1_500_000) << 128) | uint256(1_000_000));
        op.preVerificationGas = 100_000;
        op.gasFees = bytes32((uint256(1 gwei) << 128) | uint256(2 gwei));
        op.paymasterAndData = abi.encodePacked(address(paymaster), uint128(300_000), uint128(0));
        // Safe4337Module signature = validAfter (6 bytes) || validUntil (6 bytes) || owner signatures.
        // The SafeOp hash covers every UserOp field except the signatures (the timestamps are included).
        op.signature = abi.encodePacked(uint48(0), uint48(0));
        bytes32 safeOpHash = ISafe4337Execute(SafeAddresses.SAFE_4337_MODULE).getOperationHash(op);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPk, safeOpHash);
        op.signature = abi.encodePacked(uint48(0), uint48(0), r, s, v);
    }

    /// Plays the bundler; returns UserOperationEvent.success (handleOps itself does not revert on execution failure).
    function _handle(PackedUserOperation memory op) internal returns (bool success) {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;
        vm.recordLogs();
        vm.prank(bundler, bundler);
        ENTRY_POINT.handleOps(ops, payable(bundler));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool found;
        for (uint256 i; i < logs.length; i++) {
            if (
                logs[i].emitter == address(ENTRY_POINT)
                    && logs[i].topics[0] == IEntryPointV07.UserOperationEvent.selector
            ) {
                (, success,,) = abi.decode(logs[i].data, (uint256, bool, uint256, uint256));
                found = true;
            }
        }
        assertTrue(found, "UserOperationEvent emitted");
    }

    function _firstOpBatch(address account) internal view returns (bytes memory) {
        return abi.encodePacked(
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(sys.registry), true))),
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(sys.adapter), true))),
            _packCall(
                address(MORPHO), abi.encodeCall(IMorphoBase.borrow, (sys.marketParams, BORROW, 0, account, payout))
            )
        );
    }

    /// attestDeposit stand-in + the account's first UserOp (deploy + 2 x setAuthorization + borrow), sponsored.
    function _depositAndFirstUserOp(uint256 saltNonce) internal returns (address account) {
        bytes memory initCode = _initCode(saltNonce);
        account = _senderFromEntryPoint(initCode);
        assertEq(account, _predictSafe(saltNonce), "client-side CREATE2 prediction == EntryPoint.getSenderAddress");
        assertEq(account.code.length, 0, "account not deployed yet");

        sys.registry.depositFor(account, 10 * ZEC); // before the account exists
        assertEq(MORPHO.position(sys.id, account).collateral, 10 * ZEC);

        assertTrue(_handle(_userOp(account, initCode, _executeBatch(_firstOpBatch(account)))), "UserOp 1 succeeded");
    }

    // ------------------------------------------------------------------ tests

    function test_userOp1_deploysSafe_authorizes_borrows_paymasterPays() public {
        uint256 paymasterDepositBefore = ENTRY_POINT.balanceOf(address(paymaster));
        address account = _depositAndFirstUserOp(1);

        assertGt(account.code.length, 0, "deployed by initCode");
        assertEq(ISafe(account).getOwners()[0], owner);
        assertTrue(ISafe(account).isModuleEnabled(SafeAddresses.SAFE_4337_MODULE));
        assertTrue(MORPHO.isAuthorized(account, address(sys.registry)));
        assertTrue(MORPHO.isAuthorized(account, address(sys.adapter)));
        assertEq(sys.usdc.balanceOf(payout), BORROW, "borrowed USDC went straight to the receiver");

        assertEq(account.balance, 0, "account never received ETH");
        assertEq(ENTRY_POINT.balanceOf(account), 0, "account has no EntryPoint deposit");
        assertLt(ENTRY_POINT.balanceOf(address(paymaster)), paymasterDepositBefore, "paymaster paid the gas");
        assertGt(bundler.balance, 0, "bundler was compensated");
        assertEq(sys.collateral.balanceOf(account), 0, "account never held the collateral token");
        emit log_named_uint(
            "paymaster gas cost (wei) for UserOp 1", paymasterDepositBefore - ENTRY_POINT.balanceOf(address(paymaster))
        );
    }

    function test_userOp2_repaysAndReleases_paymasterPays() public {
        address account = _depositAndFirstUserOp(2);
        skip(1 days);

        // The account gets USDC back (plain inbound transfers need no UserOp) and repays in one sponsored UserOp.
        uint256 debt = MORPHO.expectedBorrowAssets(sys.marketParams, account);
        vm.prank(payout);
        assertTrue(sys.usdc.transfer(account, BORROW));
        sys.usdc.mint(account, debt - BORROW);
        uint256 shares = MORPHO.position(sys.id, account).borrowShares;

        bytes memory batch = abi.encodePacked(
            _packCall(address(sys.usdc), abi.encodeCall(IERC20.approve, (address(MORPHO), debt))),
            _packCall(
                address(MORPHO), abi.encodeCall(IMorphoBase.repay, (sys.marketParams, 0, shares, account, bytes("")))
            ),
            _packCall(address(sys.registry), abi.encodeCall(SpikeRegistry.requestRelease, ()))
        );
        assertTrue(_handle(_userOp(account, "", _executeBatch(batch))), "UserOp 2 succeeded");

        Position memory p = MORPHO.position(sys.id, account);
        assertEq(p.borrowShares, 0);
        assertEq(p.collateral, 0);
        assertEq(sys.collateral.totalSupply(), 0, "released collateral burned by the registry");
        assertEq(sys.usdc.balanceOf(account), 0);
        assertEq(sys.collateral.balanceOf(account), 0, "account never held the collateral token");
        assertEq(account.balance, 0, "account never received ETH");
    }

    function test_paymasterRejectsCallsOutsideTheAllowlist() public {
        bytes memory initCode = _initCode(3);
        address account = _senderFromEntryPoint(initCode);
        sys.registry.depositFor(account, 10 * ZEC);

        // 1. setAuthorization for anyone other than Registry/Adapter.
        bytes memory batch = abi.encodePacked(
            _firstOpBatch(account),
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (attacker, true)))
        );
        _expectPaymasterRejection(
            account,
            initCode,
            _executeBatch(batch),
            abi.encodeWithSelector(
                SpikeAllowlistPaymaster.NotAllowed.selector, address(MORPHO), IMorphoBase.setAuthorization.selector
            )
        );

        // 2. borrow from another market.
        MarketParams memory otherMarket = sys.marketParams;
        otherMarket.lltv = 0.77e18;
        batch =
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.borrow, (otherMarket, 1 * USDC, 0, account, payout)));
        _expectPaymasterRejection(
            account,
            initCode,
            _executeBatch(batch),
            abi.encodeWithSelector(
                SpikeAllowlistPaymaster.NotAllowed.selector, address(MORPHO), IMorphoBase.borrow.selector
            )
        );

        // 3. withdrawCollateral is never sponsored (and the token guard would refuse it anyway).
        batch = _packCall(
            address(MORPHO), abi.encodeCall(IMorphoBase.withdrawCollateral, (sys.marketParams, 1, account, account))
        );
        _expectPaymasterRejection(
            account,
            initCode,
            _executeBatch(batch),
            abi.encodeWithSelector(
                SpikeAllowlistPaymaster.NotAllowed.selector, address(MORPHO), IMorphoBase.withdrawCollateral.selector
            )
        );

        // 4. A direct CALL (no MultiSend) to a non-allowlisted contract.
        bytes memory callData = abi.encodeCall(
            ISafe4337Execute.executeUserOp,
            (address(sys.collateral), 0, abi.encodeCall(IERC20.transfer, (attacker, 1)), uint8(0))
        );
        _expectPaymasterRejection(
            account,
            initCode,
            callData,
            abi.encodeWithSelector(
                SpikeAllowlistPaymaster.NotAllowed.selector, address(sys.collateral), IERC20.transfer.selector
            )
        );

        assertEq(account.code.length, 0, "nothing was deployed");
    }

    function _expectPaymasterRejection(
        address account,
        bytes memory initCode,
        bytes memory callData,
        bytes memory paymasterRevert
    ) internal {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _userOp(account, initCode, callData);
        vm.expectRevert(
            abi.encodeWithSelector(IEntryPointV07.FailedOpWithRevert.selector, 0, "AA33 reverted", paymasterRevert)
        );
        vm.prank(bundler, bundler);
        ENTRY_POINT.handleOps(ops, payable(bundler));
    }
}
