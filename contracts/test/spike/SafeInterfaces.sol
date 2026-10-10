// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice SPIKE-ONLY minimal interfaces for Safe v1.4.1 and the Safe 4337 module v0.3.0, written from the
///         safe-smart-account v1.4.1 / safe-modules 4337 v0.3.0 sources (no dependency on the Safe repos).

library SafeAddresses {
    // Canonical addresses from safe-global/safe-deployments (v1.4.1) and safe-modules-deployments (4337 v0.3.0);
    // both registries list them for chain 84532. Code hashes match the registries' `codeHash` on Base Sepolia.
    address internal constant SAFE_PROXY_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant SAFE_SINGLETON = 0x41675C099F32341bf84BFc5382aF534df5C7461a;
    address internal constant SAFE_L2_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant COMPATIBILITY_FALLBACK_HANDLER = 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99;
    address internal constant MULTI_SEND = 0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526;
    address internal constant MULTI_SEND_CALL_ONLY = 0x9641d764fc13c8B624c04430C7356C1C7C8102e2;
    address internal constant SAFE_4337_MODULE = 0x75cf11467937ce3F2f357CE24ffc3DBF8fD5c226;
    address internal constant SAFE_MODULE_SETUP = 0x2dd68b007B46fBe91B9A7c3EDa5A7a1063cB5b47;
    address internal constant ENTRY_POINT_V07 = 0x0000000071727De22E5E9d8BAf0edAc6f37da032;

    bytes32 internal constant SAFE_PROXY_FACTORY_CODEHASH =
        0x50c3cdc4074750a7a974204a716c999edd37482f907608d960b2b025ee0b3317;
    bytes32 internal constant SAFE_SINGLETON_CODEHASH =
        0x1fe2df852ba3299d6534ef416eefa406e56ced995bca886ab7a553e6d0c5e1c4;
    bytes32 internal constant SAFE_L2_SINGLETON_CODEHASH =
        0xb1f926978a0f44a2c0ec8fe822418ae969bd8c3f18d61e5103100339894f81ff;
    bytes32 internal constant COMPATIBILITY_FALLBACK_HANDLER_CODEHASH =
        0x7c6007a5d711cea8dfd5d91f5940ec29c7f200fe511eb1fc1397b367af3c42f9;
    bytes32 internal constant MULTI_SEND_CODEHASH = 0x0e4f7fc66550a322d1e7688e181b75e217e662a4f3f4d6a29b22bc61217c4b77;
    bytes32 internal constant MULTI_SEND_CALL_ONLY_CODEHASH =
        0xecd5bd14a08c5d2122379900b2f272bdf107a7e92423c10dd5fe3254386c9939;

    /// keccak256("fallback_manager.handler.address")
    bytes32 internal constant FALLBACK_HANDLER_STORAGE_SLOT =
        0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5;
}

interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function execTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address payable refundReceiver,
        bytes memory signatures
    ) external payable returns (bool success);

    function getTransactionHash(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address refundReceiver,
        uint256 nonce
    ) external view returns (bytes32);

    function nonce() external view returns (uint256);
    function domainSeparator() external view returns (bytes32);
    function getOwners() external view returns (address[] memory);
    function getThreshold() external view returns (uint256);
    function isModuleEnabled(address module) external view returns (bool);
    function VERSION() external view returns (string memory);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
    function proxyCreationCode() external pure returns (bytes memory);
}

/// @dev Served by CompatibilityFallbackHandler (and by Safe4337Module, which inherits it) through the Safe's fallback.
interface ISafeMessageHashes {
    function getMessageHash(bytes memory message) external view returns (bytes32);
}

interface ISafeModuleSetup {
    function enableModules(address[] calldata modules) external;
}

interface ISafe4337Module {
    function SUPPORTED_ENTRYPOINT() external view returns (address);
}

interface IMultiSendCallOnly {
    function multiSend(bytes memory transactions) external payable;
}
