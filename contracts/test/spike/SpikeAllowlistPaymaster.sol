// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IMorphoBase, MarketParams} from "morpho-blue/src/interfaces/IMorpho.sol";
import {PackedUserOperation, IPaymasterV07, ISafe4337Execute} from "./AccountAbstractionV07.sol";
import {SafeAddresses, ISafe, ISafeProxyFactory, IMultiSendCallOnly} from "./SafeInterfaces.sol";
import {SpikeRegistry} from "./SpikeRegistry.sol";

/// @title SpikeAllowlistPaymaster
/// @notice MOCK, SPIKE-ONLY (throwaway, test/spike/). Stand-in for the Pimlico/CDP paymaster so a sponsored
///         ERC-4337 flow can run on a fork without an API key. It sponsors only the docs/INTERFACES.md § 4.1 allowlist,
///         checked ON-CHAIN from the UserOp calldata:
///         - initCode: SafeProxyFactory.createProxyWithNonce(Safe 1.4.1, setup(1 owner, threshold 1,
///           SafeModuleSetup, ..., fallbackHandler = Safe4337Module, payment = 0), salt)
///         - callData: Safe4337Module.executeUserOp(to, 0, data, op), op = CALL to an allowed target, or
///           DELEGATECALL to MultiSendCallOnly whose every inner call is allowed:
///             Morpho.setAuthorization(registry|adapter, *), Morpho.borrow/repay(this market, onBehalf = sender,
///             repay without callback data), USDC.approve(Morpho, *), USDC.transfer(*, *),
///             Registry.requestRelease(), Adapter.invalidateNonce(uint256)
///         - no ETH value anywhere (the account never receives or sends ETH).
///         It reads only immutables (no storage), returns an empty context and has no postOp work, so it would
///         also be acceptable to bundlers as an unstaked paymaster under ERC-7562.
contract SpikeAllowlistPaymaster is IPaymasterV07 {
    uint8 internal constant OPERATION_CALL = 0;
    uint8 internal constant OPERATION_DELEGATECALL = 1;
    uint256 internal constant PAYMASTER_DATA_OFFSET = 52;
    /// The real SettlementAdapter API (docs/INTERFACES.md § 3); the spike adapter does not implement it.
    bytes4 internal constant INVALIDATE_NONCE_SELECTOR = bytes4(keccak256("invalidateNonce(uint256)"));

    address public immutable entryPoint;
    address public immutable morpho;
    bytes32 public immutable marketId;
    address public immutable registry;
    address public immutable adapter;
    address public immutable usdc;

    error OnlyEntryPoint();
    error UnexpectedPaymasterData();
    error BadInitCode();
    error BadCallData();
    error ValueNotAllowed();
    error NotAllowed(address target, bytes4 selector);

    constructor(
        address entryPoint_,
        address morpho_,
        MarketParams memory marketParams_,
        address registry_,
        address adapter_
    ) {
        entryPoint = entryPoint_;
        morpho = morpho_;
        marketId = keccak256(abi.encode(marketParams_));
        registry = registry_;
        adapter = adapter_;
        usdc = marketParams_.loanToken;
    }

    function validatePaymasterUserOp(PackedUserOperation calldata userOp, bytes32, uint256)
        external
        view
        returns (bytes memory context, uint256 validationData)
    {
        if (msg.sender != entryPoint) revert OnlyEntryPoint();
        if (userOp.paymasterAndData.length != PAYMASTER_DATA_OFFSET) revert UnexpectedPaymasterData();
        if (userOp.initCode.length != 0) _checkInitCode(userOp.initCode);
        _checkCallData(userOp.sender, userOp.callData);
        return ("", 0); // empty context: no postOp; validationData 0: valid, no time range
    }

    function postOp(uint8, bytes calldata, uint256, uint256) external view {
        if (msg.sender != entryPoint) revert OnlyEntryPoint();
    }

    // ------------------------------------------------------------------ checks

    function _checkInitCode(bytes calldata initCode) internal pure {
        if (address(bytes20(initCode[:20])) != SafeAddresses.SAFE_PROXY_FACTORY) revert BadInitCode();
        bytes calldata factoryData = initCode[20:];
        if (bytes4(factoryData[:4]) != ISafeProxyFactory.createProxyWithNonce.selector) revert BadInitCode();
        bytes calldata factoryArgs = factoryData[4:];
        if (_addressArg(factoryArgs, 0) != SafeAddresses.SAFE_SINGLETON) revert BadInitCode();

        bytes calldata initializer = _bytesArg(factoryArgs, 1);
        if (bytes4(initializer[:4]) != ISafe.setup.selector) revert BadInitCode();
        bytes calldata setupArgs = initializer[4:];
        // setup(owners, threshold, to, data, fallbackHandler, paymentToken, payment, paymentReceiver)
        if (_uintArg(setupArgs, 1) != 1) revert BadInitCode();
        if (_addressArg(setupArgs, 2) != SafeAddresses.SAFE_MODULE_SETUP) revert BadInitCode();
        if (_addressArg(setupArgs, 4) != SafeAddresses.SAFE_4337_MODULE) revert BadInitCode();
        if (_uintArg(setupArgs, 6) != 0) revert BadInitCode();
    }

    function _checkCallData(address sender, bytes calldata callData) internal view {
        if (bytes4(callData[:4]) != ISafe4337Execute.executeUserOp.selector) revert BadCallData();
        bytes calldata args = callData[4:];
        address to = _addressArg(args, 0);
        if (_uintArg(args, 1) != 0) revert ValueNotAllowed();
        bytes calldata data = _bytesArg(args, 2);
        uint256 operation = _uintArg(args, 3);

        if (operation == OPERATION_CALL) {
            _checkCall(sender, to, 0, data);
        } else if (operation == OPERATION_DELEGATECALL && to == SafeAddresses.MULTI_SEND_CALL_ONLY) {
            if (bytes4(data[:4]) != IMultiSendCallOnly.multiSend.selector) revert BadCallData();
            bytes calldata txs = _bytesArg(data[4:], 0);
            // MultiSend packing: operation (1) | to (20) | value (32) | data length (32) | data
            uint256 i;
            while (i < txs.length) {
                if (uint8(txs[i]) != OPERATION_CALL) revert BadCallData();
                address innerTo = address(bytes20(txs[i + 1:i + 21]));
                uint256 innerValue = uint256(bytes32(txs[i + 21:i + 53]));
                uint256 innerLength = uint256(bytes32(txs[i + 53:i + 85]));
                _checkCall(sender, innerTo, innerValue, txs[i + 85:i + 85 + innerLength]);
                i += 85 + innerLength;
            }
        } else {
            revert BadCallData();
        }
    }

    function _checkCall(address sender, address to, uint256 value, bytes calldata data) internal view {
        if (value != 0) revert ValueNotAllowed();
        bytes4 selector = bytes4(data[:4]);
        bytes calldata args = data[4:];

        if (to == morpho) {
            if (selector == IMorphoBase.setAuthorization.selector) {
                address authorized = _addressArg(args, 0);
                if (authorized == registry || authorized == adapter) return;
            } else if (selector == IMorphoBase.borrow.selector) {
                (MarketParams memory mp,,, address onBehalf,) =
                    abi.decode(args, (MarketParams, uint256, uint256, address, address));
                if (keccak256(abi.encode(mp)) == marketId && onBehalf == sender) return;
            } else if (selector == IMorphoBase.repay.selector) {
                (MarketParams memory mp,,, address onBehalf, bytes memory callbackData) =
                    abi.decode(args, (MarketParams, uint256, uint256, address, bytes));
                if (keccak256(abi.encode(mp)) == marketId && onBehalf == sender && callbackData.length == 0) return;
            }
        } else if (to == usdc) {
            if (selector == IERC20.approve.selector && _addressArg(args, 0) == morpho) return;
            if (selector == IERC20.transfer.selector) return;
        } else if (to == registry) {
            if (selector == SpikeRegistry.requestRelease.selector) return;
        } else if (to == adapter) {
            if (selector == INVALIDATE_NONCE_SELECTOR) return;
        }
        revert NotAllowed(to, selector);
    }

    // ------------------------------------------------------------------ ABI helpers (calldata, bounds-checked)

    function _uintArg(bytes calldata args, uint256 index) internal pure returns (uint256) {
        return uint256(bytes32(args[index * 32:index * 32 + 32]));
    }

    function _addressArg(bytes calldata args, uint256 index) internal pure returns (address) {
        uint256 word = _uintArg(args, index);
        if (word > type(uint160).max) revert BadCallData();
        return address(uint160(word));
    }

    function _bytesArg(bytes calldata args, uint256 index) internal pure returns (bytes calldata) {
        uint256 offset = _uintArg(args, index);
        uint256 length = uint256(bytes32(args[offset:offset + 32]));
        return args[offset + 32:offset + 32 + length];
    }
}
