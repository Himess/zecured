// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IMorphoBase, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {SpikeCBase} from "./SpikeCBase.sol";
import {SpikeCloseRequestVerifier} from "./SpikeCloseRequestVerifier.sol";
import {
    SafeAddresses,
    ISafe,
    ISafeProxyFactory,
    ISafeMessageHashes,
    ISafeModuleSetup,
    ISafe4337Module,
    IMultiSendCallOnly
} from "./SafeInterfaces.sol";

/// @notice Spike C, task 5: ERC-1271 with a real Safe v1.4.1 deployed on a Base Sepolia fork.
///
/// How the owner signature must be produced for `isValidSignature(digest, sig)` on a Safe v1.4.1 whose fallback
/// handler is CompatibilityFallbackHandler or Safe4337Module (which inherits it):
///   digest      = the app's EIP-712 digest (here: CloseRequest under the adapter's domain)
///   safeDomain  = keccak256(abi.encode(keccak256("EIP712Domain(uint256 chainId,address verifyingContract)"),
///                                      chainId, safeAddress))
///   structHash  = keccak256(abi.encode(keccak256("SafeMessage(bytes message)"), keccak256(abi.encode(digest))))
///   safeHash    = keccak256(0x1901 || safeDomain || structHash)
///   sig         = ECDSA(ownerKey, safeHash) as r || s || v (65 bytes, v = 27/28)
/// i.e. eth_signTypedData_v4 with domain {chainId, verifyingContract: safe}, type SafeMessage(bytes message) and
/// message = the 32 digest bytes. (eth_sign over safeHash with v + 4 is also accepted.)
contract SafeErc1271Test is SpikeCBase {
    bytes32 internal constant SAFE_MSG_TYPEHASH = keccak256("SafeMessage(bytes message)");
    bytes32 internal constant SAFE_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(uint256 chainId,address verifyingContract)");
    bytes4 internal constant ERC1271_MAGIC_VALUE = 0x1626ba7e;
    uint8 internal constant OPERATION_DELEGATECALL = 1;

    address internal owner;
    uint256 internal ownerPk;
    address internal stranger;
    uint256 internal strangerPk;
    address internal marketMaker;
    SpikeCloseRequestVerifier internal verifier;

    function setUp() public {
        _selectBaseSepoliaFork();
        (owner, ownerPk) = makeAddrAndKey("zecured.spikeC.safeOwner");
        (stranger, strangerPk) = makeAddrAndKey("zecured.spikeC.stranger");
        marketMaker = makeAddr("zecured.spikeC.safeMarketMaker");
        verifier = new SpikeCloseRequestVerifier();
    }

    // ------------------------------------------------------------------ helpers

    /// @dev with4337Module = false: plain Safe + CompatibilityFallbackHandler.
    ///      with4337Module = true: the permissionless.js-style 4337 Safe: SafeModuleSetup.enableModules([module])
    ///      delegatecalled from setup, and Safe4337Module as the fallback handler.
    function _deploySafe(bool with4337Module, uint256 saltNonce) internal returns (ISafe safe) {
        address[] memory owners = new address[](1);
        owners[0] = owner;
        address setupTo;
        bytes memory setupData;
        address fallbackHandler = SafeAddresses.COMPATIBILITY_FALLBACK_HANDLER;
        if (with4337Module) {
            address[] memory modules = new address[](1);
            modules[0] = SafeAddresses.SAFE_4337_MODULE;
            setupTo = SafeAddresses.SAFE_MODULE_SETUP;
            setupData = abi.encodeCall(ISafeModuleSetup.enableModules, (modules));
            fallbackHandler = SafeAddresses.SAFE_4337_MODULE;
        }
        bytes memory initializer = abi.encodeCall(
            ISafe.setup, (owners, 1, setupTo, setupData, fallbackHandler, address(0), 0, payable(address(0)))
        );
        safe = ISafe(
            ISafeProxyFactory(SafeAddresses.SAFE_PROXY_FACTORY)
                .createProxyWithNonce(SafeAddresses.SAFE_SINGLETON, initializer, saltNonce)
        );
        assertEq(safe.getThreshold(), 1);
        assertEq(safe.getOwners().length, 1);
        assertEq(safe.getOwners()[0], owner);
    }

    /// @dev permissionless.js default (`useMultiSendForSetup = true`): setup.to = MultiSend (not CallOnly), which
    ///      delegatecalls SafeModuleSetup.enableModules([Safe4337Module]); fallback handler = Safe4337Module.
    ///      Different initializer => different CREATE2 address than `_deploySafe(true, ..)` for the same owner/salt.
    function _deploySafeViaMultiSendSetup(uint256 saltNonce) internal returns (ISafe safe) {
        address[] memory owners = new address[](1);
        owners[0] = owner;
        address[] memory modules = new address[](1);
        modules[0] = SafeAddresses.SAFE_4337_MODULE;
        bytes memory enableModules = abi.encodeCall(ISafeModuleSetup.enableModules, (modules));
        bytes memory txs = abi.encodePacked(
            uint8(OPERATION_DELEGATECALL),
            SafeAddresses.SAFE_MODULE_SETUP,
            uint256(0),
            uint256(enableModules.length),
            enableModules
        );
        bytes memory initializer = abi.encodeCall(
            ISafe.setup,
            (
                owners,
                1,
                SafeAddresses.MULTI_SEND,
                abi.encodeCall(IMultiSendCallOnly.multiSend, (txs)),
                SafeAddresses.SAFE_4337_MODULE,
                address(0),
                0,
                payable(address(0))
            )
        );
        safe = ISafe(
            ISafeProxyFactory(SafeAddresses.SAFE_PROXY_FACTORY)
                .createProxyWithNonce(SafeAddresses.SAFE_SINGLETON, initializer, saltNonce)
        );
    }

    function _closeRequest(address account) internal view returns (SpikeCloseRequestVerifier.CloseRequest memory) {
        return SpikeCloseRequestVerifier.CloseRequest({
            vaultId: keccak256("spike-vault-1"),
            account: account,
            executor: marketMaker,
            marginBps: 100,
            minUserAmount: uint64(6 * ZEC),
            userCommitment: keccak256("spike-user-commitment"),
            nonce: 1,
            deadline: uint64(block.timestamp + 10 minutes)
        });
    }

    /// @dev The hash a Safe owner must sign so that the Safe accepts `digest` via ERC-1271.
    function _safeMessageHash(address safe, bytes32 digest) internal view returns (bytes32) {
        bytes32 safeDomainSeparator = keccak256(abi.encode(SAFE_DOMAIN_TYPEHASH, block.chainid, safe));
        bytes32 structHash = keccak256(abi.encode(SAFE_MSG_TYPEHASH, keccak256(abi.encode(digest))));
        return keccak256(abi.encodePacked(bytes2(0x1901), safeDomainSeparator, structHash));
    }

    function _sign(uint256 privateKey, bytes32 hash) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, hash);
        return abi.encodePacked(r, s, v);
    }

    function _packCall(address to, bytes memory data) internal pure returns (bytes memory) {
        // MultiSend encoding: operation (1 byte) | to (20) | value (32) | data length (32) | data
        return abi.encodePacked(uint8(0), to, uint256(0), uint256(data.length), data);
    }

    function _execAsOwner(ISafe safe, address to, bytes memory data, uint8 operation) internal {
        bytes32 safeTxHash =
            safe.getTransactionHash(to, 0, data, operation, 0, 0, 0, address(0), address(0), safe.nonce());
        bytes memory signature = _sign(ownerPk, safeTxHash);
        assertTrue(safe.execTransaction(to, 0, data, operation, 0, 0, 0, address(0), payable(address(0)), signature));
    }

    function _assertErc1271(ISafe safe) internal view {
        SpikeCloseRequestVerifier.CloseRequest memory req = _closeRequest(address(safe));
        bytes32 digest = verifier.hashCloseRequest(req);
        bytes32 safeHash = _safeMessageHash(address(safe), digest);

        // Our off-chain recipe matches the Safe's own domain separator and the handler's own message hash.
        assertEq(safe.domainSeparator(), keccak256(abi.encode(SAFE_DOMAIN_TYPEHASH, block.chainid, address(safe))));
        assertEq(ISafeMessageHashes(address(safe)).getMessageHash(abi.encode(digest)), safeHash);

        // Accepted: owner's ECDSA signature over the SafeMessage-wrapped digest.
        bytes memory signature = _sign(ownerPk, safeHash);
        assertEq(signature.length, 65);
        assertTrue(SignatureChecker.isValidSignatureNow(address(safe), digest, signature), "OZ SignatureChecker");
        assertEq(IERC1271(address(safe)).isValidSignature(digest, signature), ERC1271_MAGIC_VALUE);
        assertTrue(verifier.isValidCloseRequestSignature(req, signature), "adapter-style EIP-712 check");

        // Also accepted: eth_sign flavour (EIP-191 over safeHash, v + 4).
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerPk, MessageHashUtils.toEthSignedMessageHash(safeHash));
        assertTrue(SignatureChecker.isValidSignatureNow(address(safe), digest, abi.encodePacked(r, s, v + 4)));

        // Rejected: the owner signing the raw digest (the most likely integration bug).
        assertFalse(SignatureChecker.isValidSignatureNow(address(safe), digest, _sign(ownerPk, digest)));
        // Rejected: EIP-191 over safeHash without the +4 marker.
        assertFalse(SignatureChecker.isValidSignatureNow(address(safe), digest, abi.encodePacked(r, s, v)));
        // Rejected: a non-owner.
        assertFalse(SignatureChecker.isValidSignatureNow(address(safe), digest, _sign(strangerPk, safeHash)));
        // Rejected: empty signature (would need an on-chain SignMessageLib approval).
        assertFalse(SignatureChecker.isValidSignatureNow(address(safe), digest, ""));
        // Rejected: any change to the signed request.
        req.minUserAmount += 1;
        assertFalse(verifier.isValidCloseRequestSignature(req, signature));
    }

    // ------------------------------------------------------------------ tests

    function test_safeV141And4337ModuleAreDeployedOnBaseSepolia() public view {
        assertEq(SafeAddresses.SAFE_PROXY_FACTORY.codehash, SafeAddresses.SAFE_PROXY_FACTORY_CODEHASH);
        assertEq(SafeAddresses.SAFE_SINGLETON.codehash, SafeAddresses.SAFE_SINGLETON_CODEHASH);
        assertEq(SafeAddresses.SAFE_L2_SINGLETON.codehash, SafeAddresses.SAFE_L2_SINGLETON_CODEHASH);
        assertEq(
            SafeAddresses.COMPATIBILITY_FALLBACK_HANDLER.codehash, SafeAddresses.COMPATIBILITY_FALLBACK_HANDLER_CODEHASH
        );
        assertEq(SafeAddresses.MULTI_SEND_CALL_ONLY.codehash, SafeAddresses.MULTI_SEND_CALL_ONLY_CODEHASH);
        assertEq(SafeAddresses.MULTI_SEND.codehash, SafeAddresses.MULTI_SEND_CODEHASH);
        assertEq(ISafe(SafeAddresses.SAFE_SINGLETON).VERSION(), "1.4.1");
        assertGt(SafeAddresses.SAFE_4337_MODULE.code.length, 0);
        assertGt(SafeAddresses.SAFE_MODULE_SETUP.code.length, 0);
        assertEq(ISafe4337Module(SafeAddresses.SAFE_4337_MODULE).SUPPORTED_ENTRYPOINT(), SafeAddresses.ENTRY_POINT_V07);
        assertGt(SafeAddresses.ENTRY_POINT_V07.code.length, 0);
    }

    function test_erc1271_safeWithCompatibilityFallbackHandler() public {
        ISafe safe = _deploySafe(false, 1);
        _assertErc1271(safe);
    }

    function test_erc1271_safeWith4337ModuleAsFallbackHandler() public {
        ISafe safe = _deploySafe(true, 2);
        assertTrue(safe.isModuleEnabled(SafeAddresses.SAFE_4337_MODULE));
        address handler = address(uint160(uint256(vm.load(address(safe), SafeAddresses.FALLBACK_HANDLER_STORAGE_SLOT))));
        assertEq(handler, SafeAddresses.SAFE_4337_MODULE);
        _assertErc1271(safe);
    }

    function test_erc1271_permissionlessDefaultSetupViaMultiSend() public {
        ISafe safe = _deploySafeViaMultiSendSetup(6);
        assertTrue(safe.isModuleEnabled(SafeAddresses.SAFE_4337_MODULE));
        address handler = address(uint160(uint256(vm.load(address(safe), SafeAddresses.FALLBACK_HANDLER_STORAGE_SLOT))));
        assertEq(handler, SafeAddresses.SAFE_4337_MODULE);
        assertEq(safe.getOwners()[0], owner);
        _assertErc1271(safe);
    }

    function test_erc1271_signatureIsBoundToOneSafe() public {
        ISafe safeA = _deploySafe(true, 3);
        ISafe safeB = _deploySafe(true, 4); // same owner, different Safe
        bytes32 digest = keccak256("some EIP-712 digest");
        bytes memory signatureForA = _sign(ownerPk, _safeMessageHash(address(safeA), digest));
        assertTrue(SignatureChecker.isValidSignatureNow(address(safeA), digest, signatureForA));
        assertFalse(SignatureChecker.isValidSignatureNow(address(safeB), digest, signatureForA));
    }

    /// A Safe (4337 configuration) as the Morpho account: one batched Safe transaction does what the first UserOp
    /// will do (2 x setAuthorization + borrow to another receiver); then a signed CloseRequest is ERC-1271-valid and
    /// the adapter closes the position with market-maker USDC. The Safe never holds collateral, USDC or ETH.
    function test_safeAsMorphoAccount_batchedAuthorizeAndBorrow_thenSignedClose() public {
        System memory sys = _deploySystem(true);
        _supplyAsLender(sys, makeAddr("zecured.spikeC.safeLender"), 100_000 * USDC);
        address payout = makeAddr("zecured.spikeC.safePayout");

        ISafe safe = _deploySafe(true, 5);
        address account = address(safe);
        sys.registry.depositFor(account, 10 * ZEC);
        assertEq(sys.collateral.balanceOf(account), 0);

        bytes memory batch = abi.encodePacked(
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(sys.registry), true))),
            _packCall(address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(sys.adapter), true))),
            _packCall(
                address(MORPHO), abi.encodeCall(IMorphoBase.borrow, (sys.marketParams, 160 * USDC, 0, account, payout))
            )
        );
        _execAsOwner(
            safe,
            SafeAddresses.MULTI_SEND_CALL_ONLY,
            abi.encodeCall(IMultiSendCallOnly.multiSend, (batch)),
            OPERATION_DELEGATECALL
        );

        assertTrue(MORPHO.isAuthorized(account, address(sys.registry)));
        assertTrue(MORPHO.isAuthorized(account, address(sys.adapter)));
        assertEq(sys.usdc.balanceOf(payout), 160 * USDC);
        assertEq(sys.usdc.balanceOf(account), 0);
        assertEq(sys.collateral.balanceOf(account), 0);
        assertEq(account.balance, 0, "the Safe never needed ETH");

        SpikeCloseRequestVerifier.CloseRequest memory req = _closeRequest(account);
        bytes memory userSig = _sign(ownerPk, _safeMessageHash(account, verifier.hashCloseRequest(req)));
        assertTrue(verifier.isValidCloseRequestSignature(req, userSig));

        sys.usdc.mint(req.executor, 1_000 * USDC);
        vm.startPrank(req.executor);
        sys.usdc.approve(address(sys.adapter), type(uint256).max);
        sys.adapter.closeWithCollateral(account);
        vm.stopPrank();

        Position memory p = MORPHO.position(sys.id, account);
        assertEq(p.borrowShares, 0);
        assertEq(p.collateral, 0);
        assertEq(sys.collateral.balanceOf(address(sys.adapter)), 10 * ZEC);
        assertEq(sys.collateral.balanceOf(account), 0);
    }
}
