// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @notice TEST HELPER. Canonical Safe v1.4.1 and Safe 4337 module v0.3.0 deployments on Base Sepolia (listed as
///         `canonical` for 84532 in safe-deployments / safe-modules-deployments; checked in docs/spikes/SPIKE_C.md).
library SafeAddresses {
    address internal constant PROXY_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant SINGLETON = 0x41675C099F32341bf84BFc5382aF534df5C7461a; // Safe 1.4.1 (non-L2)
    address internal constant COMPATIBILITY_FALLBACK_HANDLER = 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99;
    address internal constant MULTI_SEND = 0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526;
    address internal constant MULTI_SEND_CALL_ONLY = 0x9641d764fc13c8B624c04430C7356C1C7C8102e2;
    address internal constant SAFE_4337_MODULE = 0x75cf11467937ce3F2f357CE24ffc3DBF8fD5c226;
    address internal constant SAFE_MODULE_SETUP = 0x2dd68b007B46fBe91B9A7c3EDa5A7a1063cB5b47;

    uint8 internal constant CALL = 0;
    uint8 internal constant DELEGATECALL = 1;

    bytes32 internal constant SAFE_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(uint256 chainId,address verifyingContract)");
    bytes32 internal constant SAFE_MSG_TYPEHASH = keccak256("SafeMessage(bytes message)");
}

/// @notice TEST HELPER. The parts of Safe v1.4.1 the tests use.
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
    function isModuleEnabled(address module) external view returns (bool);
}

interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
    function proxyCreationCode() external view returns (bytes memory); // pure in Safe; view here (external read)
}

interface ISafeModuleSetup {
    function enableModules(address[] calldata modules) external;
}

interface IMultiSend {
    function multiSend(bytes memory transactions) external payable;
}

/// @notice TEST HELPER. Builds the Safe configurations and signatures the app uses.
library SafeTestLib {
    /// permissionless.js' default 4337 Safe (`useMultiSendForSetup = true`): setup.to = MultiSend, which delegatecalls
    /// SafeModuleSetup.enableModules([Safe4337Module]); fallback handler = Safe4337Module (inherits
    /// CompatibilityFallbackHandler, hence ERC-1271).
    function permissionlessInitializer(address owner) internal pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = owner;
        address[] memory modules = new address[](1);
        modules[0] = SafeAddresses.SAFE_4337_MODULE;
        bytes memory enableModules = abi.encodeCall(ISafeModuleSetup.enableModules, (modules));
        bytes memory txs = abi.encodePacked(
            SafeAddresses.DELEGATECALL,
            SafeAddresses.SAFE_MODULE_SETUP,
            uint256(0),
            uint256(enableModules.length),
            enableModules
        );
        return abi.encodeCall(
            ISafe.setup,
            (
                owners,
                1,
                SafeAddresses.MULTI_SEND,
                abi.encodeCall(IMultiSend.multiSend, (txs)),
                SafeAddresses.SAFE_4337_MODULE,
                address(0),
                0,
                payable(address(0))
            )
        );
    }

    /// Plain 1-owner Safe with the CompatibilityFallbackHandler.
    function plainInitializer(address owner) internal pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = owner;
        return abi.encodeCall(
            ISafe.setup,
            (
                owners,
                1,
                address(0),
                "",
                SafeAddresses.COMPATIBILITY_FALLBACK_HANDLER,
                address(0),
                0,
                payable(address(0))
            )
        );
    }

    /// The SafeProxyFactory's CREATE2 address for (initializer, saltNonce): the counterfactual account address.
    function predictAddress(bytes memory initializer, uint256 saltNonce) internal view returns (address) {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(initializer), saltNonce));
        bytes memory deploymentData = abi.encodePacked(
            ISafeProxyFactory(SafeAddresses.PROXY_FACTORY).proxyCreationCode(),
            uint256(uint160(SafeAddresses.SINGLETON))
        );
        return address(
            uint160(
                uint256(
                    keccak256(
                        abi.encodePacked(bytes1(0xff), SafeAddresses.PROXY_FACTORY, salt, keccak256(deploymentData))
                    )
                )
            )
        );
    }

    /// One MultiSend entry (CALL, no value).
    function packCall(address to, bytes memory data) internal pure returns (bytes memory) {
        return abi.encodePacked(SafeAddresses.CALL, to, uint256(0), uint256(data.length), data);
    }

    /// docs/spikes/SPIKE_C.md recipe: the hash a Safe owner signs so that the Safe accepts `digest` via ERC-1271,
    /// i.e. EIP-712 SafeMessage(bytes message = the 32 digest bytes) under the Safe's own domain.
    function safeMessageHash(address safe, bytes32 digest) internal view returns (bytes32) {
        bytes32 domainSeparator = keccak256(abi.encode(SafeAddresses.SAFE_DOMAIN_TYPEHASH, block.chainid, safe));
        bytes32 structHash = keccak256(abi.encode(SafeAddresses.SAFE_MSG_TYPEHASH, keccak256(abi.encode(digest))));
        return keccak256(abi.encodePacked(bytes2(0x1901), domainSeparator, structHash));
    }
}
