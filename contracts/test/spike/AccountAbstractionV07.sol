// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice SPIKE-ONLY minimal ERC-4337 EntryPoint v0.7 types, written from eth-infinitism/account-abstraction
///         v0.7.0 (PackedUserOperation.sol, IEntryPoint.sol, IStakeManager.sol, INonceManager.sol, IPaymaster.sol).

struct PackedUserOperation {
    address sender;
    uint256 nonce;
    bytes initCode; // factory (20 bytes) || factoryData
    bytes callData;
    bytes32 accountGasLimits; // verificationGasLimit (high 128) || callGasLimit (low 128)
    uint256 preVerificationGas;
    bytes32 gasFees; // maxPriorityFeePerGas (high 128) || maxFeePerGas (low 128)
    bytes paymasterAndData; // paymaster (20) || pmVerificationGasLimit (16) || pmPostOpGasLimit (16) || data
    bytes signature;
}

interface IEntryPointV07 {
    error FailedOp(uint256 opIndex, string reason);
    error FailedOpWithRevert(uint256 opIndex, string reason, bytes inner);
    error SenderAddressResult(address sender);

    event UserOperationEvent(
        bytes32 indexed userOpHash,
        address indexed sender,
        address indexed paymaster,
        uint256 nonce,
        bool success,
        uint256 actualGasCost,
        uint256 actualGasUsed
    );

    function handleOps(PackedUserOperation[] calldata ops, address payable beneficiary) external;
    function getUserOpHash(PackedUserOperation calldata userOp) external view returns (bytes32);
    function getNonce(address sender, uint192 key) external view returns (uint256 nonce);
    function getSenderAddress(bytes memory initCode) external;
    function depositTo(address account) external payable;
    function balanceOf(address account) external view returns (uint256);
}

interface IPaymasterV07 {
    function validatePaymasterUserOp(PackedUserOperation calldata userOp, bytes32 userOpHash, uint256 maxCost)
        external
        returns (bytes memory context, uint256 validationData);

    /// @dev `mode` is IPaymaster.PostOpMode (an enum, ABI-encoded as uint8).
    function postOp(uint8 mode, bytes calldata context, uint256 actualGasCost, uint256 actualUserOpFeePerGas) external;
}

/// @dev Safe4337Module v0.3.0: the UserOp callData must call one of these two functions on the Safe.
interface ISafe4337Execute {
    function executeUserOp(address to, uint256 value, bytes memory data, uint8 operation) external;
    function getOperationHash(PackedUserOperation calldata userOp) external view returns (bytes32);
}
