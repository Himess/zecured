// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IMorpho, MarketParams, Id, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    DepositProofConfig
} from "../../src/interfaces/IZecuredRegistry.sol";
import {DepositAttestation} from "../../src/libraries/ZecuredEIP712.sol";

/// @notice MOCK: the part of Morpho Blue the Registry calls, for symbolic execution (Halmos has no fork mode; the fork
///         suites run against the real Morpho). Collateral moves through the real zZEC, so its guard applies to every
///         transfer. No interest, no debt, no health check: the D14 LTV limit of a partial release is not exercised here.
contract MockMorphoForHalmos {
    mapping(address account => uint256) public collateralOf;
    mapping(address account => mapping(address authorized => bool)) public isAuthorized;

    function market(Id) external pure returns (Market memory m) {}

    function createMarket(MarketParams memory) external {}

    function setAuthorization(address authorized, bool newIsAuthorized) external {
        isAuthorized[msg.sender][authorized] = newIsAuthorized;
    }

    function position(Id, address account) external view returns (Position memory p) {
        p.collateral = uint128(collateralOf[account]);
    }

    function supplyCollateral(MarketParams memory params, uint256 assets, address onBehalf, bytes memory) external {
        collateralOf[onBehalf] += assets;
        IERC20(params.collateralToken).transferFrom(msg.sender, address(this), assets);
    }

    function withdrawCollateral(MarketParams memory params, uint256 assets, address onBehalf, address receiver)
        external
    {
        require(msg.sender == onBehalf || isAuthorized[onBehalf][msg.sender], "unauthorized");
        collateralOf[onBehalf] -= assets;
        IERC20(params.collateralToken).transfer(receiver, assets);
    }
}

/// @notice MOCK: stands in for zZEC while the Registry's constructor checks the wiring, then receives the code of a
///         real zZEC deployed for that Registry (vm.etch). Halmos does not derive CREATE addresses, so the usual
///         prediction of the Registry's address does not work here.
contract ZZECWiringStub {
    address internal immutable _adapter;
    address internal immutable _morpho;

    constructor(address adapter_, address morpho_) {
        _adapter = adapter_;
        _morpho = morpho_;
    }

    function registry() external view returns (address) {
        return msg.sender;
    }

    function adapter() external view returns (address) {
        return _adapter;
    }

    function morpho() external view returns (address) {
        return _morpho;
    }
}

/// @notice Test harness: the real Registry, with its internal I6 check callable. Every entry point that takes signer
///         signatures calls it: attestDeposit, cancelVault, confirmPayout and commitSpend with 3, attestDepositWithProof
///         with all 5.
contract RegistryHarness is ZecuredRegistry {
    constructor(
        IMorpho morpho_,
        IZZEC zzec_,
        address adapter_,
        address loanToken_,
        address oracle_,
        address irm_,
        uint256 lltv_,
        address[5] memory signers_,
        DepositProofConfig memory proofs_,
        ISignerBond signerBond_
    ) ZecuredRegistry(morpho_, zzec_, adapter_, loanToken_, oracle_, irm_, lltv_, signers_, proofs_, signerBond_) {}

    function verifySignerSignatures(bytes32 digest, bytes[] calldata sigs, uint256 required)
        external
        view
        returns (uint8 signerBitmap)
    {
        return _verifySignerSignatures(digest, sigs, required);
    }
}

/// @notice Symbolic checks of Registry rules with Halmos (docs/TESTING.md § 1b), on the real Registry and
///         the real zZEC, with Morpho a MOCK:
///         R1  I6: fewer than the required number of distinct signers is always rejected, and the signer bitmap names
///             exactly who signed (Tier 3 slashes by it);
///         R2  I7: a close splits exactly the outstanding into its two claims and burns exactly that;
///         R3  I9: a liquidation seizes more than zero and at most the outstanding, and burns exactly that;
///         R4  I1, I3: whichever exit runs, by whoever, what it burns equals what its new claims release and what
///             leaves the outstanding, and the supply stays equal to the outstanding.
/// @dev Halmos treats ecrecover as an uninterpreted function: a signature recovers to whatever address the solver
///      picks. R1 therefore checks the counting for every possible recovery, not the cryptography; the setup assumes
///      that three signatures recover to signers 0, 1 and 2, which is what valid signatures do.
contract RegistryRulesCheck is Test {
    address internal constant ADAPTER = address(0x2000);
    address internal constant ACCOUNT = address(0x4000);
    address internal constant SIGNER0 = address(0x5101);
    address internal constant SIGNER1 = address(0x5102);
    address internal constant SIGNER2 = address(0x5103);
    address internal constant SIGNER3 = address(0x5104);
    address internal constant SIGNER4 = address(0x5105);
    bytes32 internal constant VAULT = keccak256("vault");
    bytes32 internal constant TAG = keccak256("deposit tag");

    MockMorphoForHalmos internal morpho;
    RegistryHarness internal registry;
    ZZEC internal zzec;

    function setUp() public {
        morpho = new MockMorphoForHalmos();
        address token = address(new ZZECWiringStub(ADAPTER, address(morpho)));
        DepositProofConfig memory noProofs; // all zero: attestations only
        registry = new RegistryHarness(
            IMorpho(address(morpho)),
            IZZEC(token),
            ADAPTER,
            address(0x6001), // loan token
            address(0x6002), // oracle
            address(0x6003), // IRM
            0.77e18,
            [SIGNER0, SIGNER1, SIGNER2, SIGNER3, SIGNER4],
            noProofs,
            ISignerBond(address(0))
        );
        vm.etch(token, address(new ZZEC(address(registry), ADAPTER, address(morpho))).code);
        zzec = ZZEC(token);
        // The smart account's setup batch authorizes the Registry and the Adapter on Morpho.
        vm.startPrank(ACCOUNT);
        morpho.setAuthorization(address(registry), true);
        morpho.setAuthorization(ADAPTER, true);
        vm.stopPrank();
    }

    // ------------------------------------------------------------------ R1 (I6)

    function check_I6_onlyEnoughDistinctSignersPass(
        uint8 count,
        bool allFive,
        bytes32 digest,
        bytes32[6] memory r,
        bytes32[6] memory s,
        uint8[6] memory v
    ) public view {
        uint256 required = allFive ? 5 : 3;
        // Concrete lengths (Halmos needs them), every one from 0 to 6.
        if (count == 0) _checkSignatures(0, required, digest, r, s, v);
        else if (count == 1) _checkSignatures(1, required, digest, r, s, v);
        else if (count == 2) _checkSignatures(2, required, digest, r, s, v);
        else if (count == 3) _checkSignatures(3, required, digest, r, s, v);
        else if (count == 4) _checkSignatures(4, required, digest, r, s, v);
        else if (count == 5) _checkSignatures(5, required, digest, r, s, v);
        else _checkSignatures(6, required, digest, r, s, v);
    }

    function _checkSignatures(
        uint256 n,
        uint256 required,
        bytes32 digest,
        bytes32[6] memory r,
        bytes32[6] memory s,
        uint8[6] memory v
    ) internal view {
        bytes[] memory sigs = new bytes[](n);
        for (uint256 i; i < n; ++i) {
            sigs[i] = abi.encodePacked(r[i], s[i], v[i]);
        }
        (bool ok, bytes memory ret) = address(registry)
            .staticcall(abi.encodeCall(RegistryHarness.verifySignerSignatures, (digest, sigs, required)));
        if (!ok) return;
        assert(n >= required);
        uint256 signed;
        for (uint256 i; i < n; ++i) {
            uint256 index = _signerIndex(ecrecover(digest, v[i], r[i], s[i]));
            assert(index < 5); // a signer
            assert(signed & (1 << index) == 0); // a different one
            signed |= 1 << index;
        }
        assert(abi.decode(ret, (uint8)) == signed);
    }

    /// R1 at an entry point: cancelVault takes 3 of 5, like attestDeposit, confirmPayout and commitSpend.
    function check_I6_cancelVaultNeedsThreeDistinctSigners(
        uint8 count,
        bytes32 vaultId,
        bytes32[4] memory r,
        bytes32[4] memory s,
        uint8[4] memory v
    ) public {
        if (count == 0) _checkCancel(0, vaultId, r, s, v);
        else if (count == 1) _checkCancel(1, vaultId, r, s, v);
        else if (count == 2) _checkCancel(2, vaultId, r, s, v);
        else if (count == 3) _checkCancel(3, vaultId, r, s, v);
        else _checkCancel(4, vaultId, r, s, v);
    }

    function _checkCancel(uint256 n, bytes32 vaultId, bytes32[4] memory r, bytes32[4] memory s, uint8[4] memory v)
        internal
    {
        bytes[] memory sigs = new bytes[](n);
        for (uint256 i; i < n; ++i) {
            sigs[i] = abi.encodePacked(r[i], s[i], v[i]);
        }
        bytes32 digest = registry.hashVaultCancellation(vaultId);
        (bool ok,) = address(registry).call(abi.encodeCall(IZecuredRegistry.cancelVault, (vaultId, sigs)));
        if (!ok) return;
        assert(n >= 3);
        uint256 signed;
        for (uint256 i; i < n; ++i) {
            uint256 index = _signerIndex(ecrecover(digest, v[i], r[i], s[i]));
            assert(index < 5 && signed & (1 << index) == 0);
            signed |= 1 << index;
        }
        (,, VaultStatus status) = registry.vault(vaultId);
        assert(status == VaultStatus.Cancelled);
    }

    // ------------------------------------------------------------------ R2 (I7)

    function check_I7_closeSplitsExactlyTheOutstanding(
        uint64 amount,
        uint64 held,
        address caller,
        bytes32 vaultId,
        uint64 mmAmount,
        uint64 userAmount,
        bytes32 mmCommitment,
        bytes32 userCommitment
    ) public {
        _activeVault(amount, held);
        (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        uint64 claims = registry.claimCount(vaultId);
        uint256 supply = zzec.totalSupply();
        uint256 adapterHeld = zzec.balanceOf(ADAPTER);

        vm.prank(caller);
        (bool ok,) = address(registry)
            .call(
                abi.encodeCall(
                    IZecuredRegistry.recordClose, (vaultId, mmAmount, userAmount, mmCommitment, userCommitment)
                )
            );
        if (!ok) return;

        assert(caller == ADAPTER && status == VaultStatus.Active);
        assert(uint256(mmAmount) + userAmount == outstanding);
        assert(zzec.totalSupply() == supply - outstanding && zzec.balanceOf(ADAPTER) == adapterHeld - outstanding);
        (, uint64 left, VaultStatus closed) = registry.vault(vaultId);
        assert(left == 0 && closed == VaultStatus.Closed);
        assert(registry.claimCount(vaultId) == claims + 2);
        _assertClaim(registry.claimIdOf(vaultId, claims), vaultId, ClaimKind.CloseUser, userAmount, userCommitment);
        _assertClaim(
            registry.claimIdOf(vaultId, claims + 1), vaultId, ClaimKind.CloseMarketMaker, mmAmount, mmCommitment
        );
        assert(zzec.totalSupply() == left); // I1
    }

    // ------------------------------------------------------------------ R3 (I9)

    function check_I9_liquidationSeizesAtMostTheOutstanding(
        uint64 amount,
        uint64 held,
        address caller,
        bytes32 vaultId,
        uint64 seized,
        bytes32 commitment
    ) public {
        _activeVault(amount, held);
        (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        uint64 claims = registry.claimCount(vaultId);
        uint256 supply = zzec.totalSupply();
        uint256 adapterHeld = zzec.balanceOf(ADAPTER);

        vm.prank(caller);
        (bool ok,) =
            address(registry).call(abi.encodeCall(IZecuredRegistry.recordLiquidation, (vaultId, seized, commitment)));
        if (!ok) return;

        assert(caller == ADAPTER && status == VaultStatus.Active);
        assert(seized > 0 && seized <= outstanding);
        assert(zzec.totalSupply() == supply - seized && zzec.balanceOf(ADAPTER) == adapterHeld - seized);
        (, uint64 left, VaultStatus afterwards) = registry.vault(vaultId);
        assert(left == outstanding - seized);
        assert(afterwards == (left == 0 ? VaultStatus.Closed : VaultStatus.Active));
        assert(registry.claimCount(vaultId) == claims + 1);
        _assertClaim(registry.claimIdOf(vaultId, claims), vaultId, ClaimKind.Liquidation, seized, commitment);
        assert(zzec.totalSupply() == left); // I1
    }

    // ------------------------------------------------------------------ R4 (I1, I3)

    function check_I1_everyExitBurnsExactlyWhatItsClaimsRelease(
        uint64 amount,
        uint64 held,
        uint8 op,
        address caller,
        bytes32 vaultId,
        uint64 x,
        uint64 y,
        bytes32 c1,
        bytes32 c2
    ) public {
        _activeVault(amount, held);
        (, uint64 outstanding,) = registry.vault(VAULT);
        uint64 claims = registry.claimCount(VAULT);
        uint256 supply = zzec.totalSupply();

        bytes memory data;
        if (op == 0) data = abi.encodeCall(IZecuredRegistry.requestRelease, (vaultId, c1));
        else if (op == 1) data = abi.encodeCall(IZecuredRegistry.requestPartialRelease, (vaultId, x, c1));
        else if (op == 2) data = abi.encodeCall(IZecuredRegistry.recordLiquidation, (vaultId, x, c1));
        else data = abi.encodeCall(IZecuredRegistry.recordClose, (vaultId, x, y, c1, c2));
        vm.prank(caller);
        (bool ok,) = address(registry).call(data);
        if (!ok) return;

        // Releases only by the vault's account, settlements only by the Adapter.
        assert(caller == (op <= 1 ? ACCOUNT : ADAPTER));
        (, uint64 left,) = registry.vault(VAULT);
        uint64 created = registry.claimCount(VAULT) - claims;
        uint256 released;
        for (uint64 i; i < created; ++i) {
            (,, uint64 claimAmount,,) = registry.claim(registry.claimIdOf(VAULT, claims + i));
            released += claimAmount;
        }
        uint256 burned = supply - zzec.totalSupply();
        assert(burned == released && burned == outstanding - left);
        assert(zzec.totalSupply() == left); // I1
    }

    // ------------------------------------------------------------------ helpers

    /// A vault activated through the real attestDeposit, then `held` of its collateral taken by the Adapter (as in a
    /// liquidation or a close, with the Adapter's own zZEC expectation).
    function _activeVault(uint64 amount, uint64 held) internal {
        vm.assume(held <= amount);
        DepositAttestation memory att =
            DepositAttestation({vaultId: VAULT, account: ACCOUNT, amount: amount, depositTag: TAG});
        bytes32 digest = registry.hashDepositAttestation(att);
        bytes32 s = bytes32(uint256(1));
        vm.assume(ecrecover(digest, 27, bytes32(uint256(1)), s) == SIGNER0);
        vm.assume(ecrecover(digest, 27, bytes32(uint256(2)), s) == SIGNER1);
        vm.assume(ecrecover(digest, 27, bytes32(uint256(3)), s) == SIGNER2);
        bytes[] memory sigs = new bytes[](3);
        sigs[0] = abi.encodePacked(bytes32(uint256(1)), s, uint8(27));
        sigs[1] = abi.encodePacked(bytes32(uint256(2)), s, uint8(27));
        sigs[2] = abi.encodePacked(bytes32(uint256(3)), s, uint8(27));
        registry.attestDeposit(att, sigs);

        vm.startPrank(ADAPTER);
        zzec.expectFromMorpho(held);
        morpho.withdrawCollateral(registry.marketParams(), held, ACCOUNT, ADAPTER);
        zzec.settleExpectation();
        vm.stopPrank();
    }

    function _assertClaim(bytes32 claimId, bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment)
        internal
        view
    {
        (bytes32 claimVault, ClaimKind claimKind, uint64 claimAmount, bytes32 claimCommitment, ClaimStatus status) =
            registry.claim(claimId);
        assert(claimVault == vaultId && claimKind == kind && claimAmount == amount);
        assert(claimCommitment == commitment && status == ClaimStatus.Pending);
    }

    function _signerIndex(address a) internal pure returns (uint256) {
        if (a == SIGNER0) return 0;
        if (a == SIGNER1) return 1;
        if (a == SIGNER2) return 2;
        if (a == SIGNER3) return 3;
        if (a == SIGNER4) return 4;
        return 5;
    }
}
