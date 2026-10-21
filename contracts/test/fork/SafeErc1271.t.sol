// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IMorpho, IMorphoBase} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ISettlementAdapter} from "../../src/interfaces/ISettlementAdapter.sol";
import {CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {SafeAddresses, ISafe, ISafeProxyFactory, IMultiSend, SafeTestLib} from "../utils/SafeTestLib.sol";

/// @notice CloseRequest signatures from a REAL Safe v1.4.1 smart account on the Base Sepolia fork (ERC-1271 through
///         OpenZeppelin SignatureChecker), signed with the SafeMessage recipe of docs/spikes/SPIKE_C.md.
contract SafeErc1271Test is ZecuredFixture {
    using MorphoBalancesLib for IMorpho;

    address internal owner;
    uint256 internal ownerKey;

    function setUp() public override {
        super.setUp();
        (owner, ownerKey) = makeAddrAndKey("zecured.safe.owner");
        assertGt(SafeAddresses.SINGLETON.code.length, 0, "Safe 1.4.1 singleton deployed");
        assertGt(SafeAddresses.PROXY_FACTORY.code.length, 0, "SafeProxyFactory deployed");
        assertGt(SafeAddresses.SAFE_4337_MODULE.code.length, 0, "Safe4337Module deployed");
    }

    function _deploy(bytes memory initializer, uint256 saltNonce) internal returns (ISafe safe) {
        safe = ISafe(
            ISafeProxyFactory(SafeAddresses.PROXY_FACTORY)
                .createProxyWithNonce(SafeAddresses.SINGLETON, initializer, saltNonce)
        );
        assertEq(safe.getOwners()[0], owner);
    }

    /// The account's first UserOp, executed here as an owner-signed Safe transaction: MultiSendCallOnly batch of
    /// setAuthorization(Registry), setAuthorization(Adapter) and borrow (to another receiver).
    function _authorizeAndBorrowFromSafe(ISafe safe, uint256 assets) internal {
        bytes memory batch = abi.encodePacked(
            SafeTestLib.packCall(
                address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(registry), true))
            ),
            SafeTestLib.packCall(
                address(MORPHO), abi.encodeCall(IMorphoBase.setAuthorization, (address(adapter), true))
            ),
            SafeTestLib.packCall(
                    address(MORPHO),
                    abi.encodeCall(IMorphoBase.borrow, (params, assets, 0, address(safe), borrowReceiver))
                )
        );
        _execDelegatecall(safe, SafeAddresses.MULTI_SEND_CALL_ONLY, abi.encodeCall(IMultiSend.multiSend, (batch)));
    }

    function _execDelegatecall(ISafe safe, address to, bytes memory data) internal {
        bytes memory signature = _sign(ownerKey, _safeTxHash(safe, to, data));
        assertTrue(
            safe.execTransaction(
                to, 0, data, SafeAddresses.DELEGATECALL, 0, 0, 0, address(0), payable(address(0)), signature
            )
        );
    }

    function _safeTxHash(ISafe safe, address to, bytes memory data) internal view returns (bytes32) {
        return
            safe.getTransactionHash(
                to, 0, data, SafeAddresses.DELEGATECALL, 0, 0, 0, address(0), address(0), safe.nonce()
            );
    }

    /// Full P6 with the app's account type: the vault is attested to the counterfactual Safe address, the Safe is
    /// deployed and borrows in one batch, and the owner's SafeMessage signature authorizes the market maker's close.
    function test_safe_closeWithSafeMessageSignature() public {
        bytes memory initializer = SafeTestLib.permissionlessInitializer(owner);
        address predicted = SafeTestLib.predictAddress(initializer, 1);

        // Deposit attested before the account exists (counterfactual address, no code).
        assertEq(predicted.code.length, 0);
        bytes32 vaultId = _attest(predicted, VAULT_001);
        assertEq(_position(predicted).collateral, VAULT_001);

        ISafe safe = _deploy(initializer, 1);
        assertEq(address(safe), predicted, "counterfactual address");
        assertTrue(safe.isModuleEnabled(SafeAddresses.SAFE_4337_MODULE));
        _authorizeAndBorrowFromSafe(safe, 160_000);
        assertEq(usdc.balanceOf(borrowReceiver), 160_000);
        assertEq(usdc.balanceOf(address(safe)), 0);
        assertEq(zzec.balanceOf(address(safe)), 0);
        assertEq(address(safe).balance, 0, "the Safe never needs ETH");

        skip(3 days);
        CloseRequest memory req = _closeRequest(vaultId, address(safe), 100, 500_000, 1);
        bytes32 digest = adapter.hashCloseRequest(req);
        bytes memory sig = _sign(ownerKey, SafeTestLib.safeMessageHash(address(safe), digest));
        assertEq(IERC1271(address(safe)).isValidSignature(digest, sig), IERC1271.isValidSignature.selector);

        uint256 debt = MORPHO.expectedBorrowAssets(params, address(safe));
        _fundAndApproveAdapter(marketMaker, debt);
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        (,, uint64 userAmount,,) = registry.claim(registry.claimIdOf(vaultId, 0));
        (,, uint64 mmAmount,,) = registry.claim(registry.claimIdOf(vaultId, 1));
        assertEq(uint256(userAmount) + mmAmount, VAULT_001, "I7");
        assertEq(mmAmount, _expectedMmAmount(debt, 100));
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(_position(address(safe)).collateral, 0);
        assertEq(_position(address(safe)).borrowShares, 0);
        assertEq(zzec.totalSupply(), 0);
        assertTrue(adapter.nonceUsed(address(safe), 1));
    }

    /// Wrong ERC-1271 signatures are rejected by the Adapter; the right one then works (same Safe, same request).
    function test_safe_rejectsWrongSignatures() public {
        bytes memory initializer = SafeTestLib.plainInitializer(owner);
        bytes32 vaultId = _attest(SafeTestLib.predictAddress(initializer, 2), VAULT_001);
        ISafe safe = _deploy(initializer, 2);
        _authorizeAndBorrowFromSafe(safe, 160_000);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);

        CloseRequest memory req = _closeRequest(vaultId, address(safe), 100, 0, 1);
        bytes32 digest = adapter.hashCloseRequest(req);
        bytes32 safeHash = SafeTestLib.safeMessageHash(address(safe), digest);
        (, uint256 strangerKey) = makeAddrAndKey("zecured.safe.stranger");

        bytes[] memory wrong = new bytes[](5);
        wrong[0] = _sign(ownerKey, digest); // the owner signs the raw digest (likeliest integration bug)
        wrong[1] = _sign(strangerKey, safeHash); // not an owner
        wrong[2] = _sign(ownerKey, MessageHashUtils.toEthSignedMessageHash(safeHash)); // EIP-191 without the v + 4 marker
        wrong[3] = ""; // empty (would need an on-chain SignMessageLib approval)
        // Signed for another Safe of the same owner.
        wrong[4] = _sign(ownerKey, SafeTestLib.safeMessageHash(makeAddr("zecured.safe.otherSafe"), digest));
        for (uint256 i; i < wrong.length; ++i) {
            vm.prank(marketMaker);
            vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
            adapter.closeWithCollateral(req, wrong[i], MM_COMMITMENT, type(uint256).max, 0);
        }

        // A field changed after signing.
        bytes memory sig = _sign(ownerKey, safeHash);
        req.marginBps = 101;
        vm.prank(marketMaker);
        vm.expectRevert(ISettlementAdapter.InvalidUserSignature.selector);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // The request exactly as signed goes through.
        req.marginBps = 100;
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        _assertClaim(
            registry.claimIdOf(vaultId, 1),
            vaultId,
            ClaimKind.CloseMarketMaker,
            uint64(_expectedMmAmount(160_000, 100)),
            MM_COMMITMENT,
            ClaimStatus.Pending
        );
    }
}
