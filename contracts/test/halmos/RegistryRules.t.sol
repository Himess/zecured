// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IMorpho, MarketParams, Id, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredSpendLogic} from "../../src/registry/ZecuredSpendLogic.sol";
import {IZZEC} from "../../src/interfaces/IZZEC.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    RegistryConfig
} from "../../src/interfaces/v3/IZecuredRegistry.sol";

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

/// @notice MOCK: stands in for the spend logic while the Registry's constructor checks it was built for this Registry
///         and signer set, then receives the code of a real ZecuredSpendLogic deployed for that Registry (vm.etch), for
///         the same reason as `ZZECWiringStub`.
contract SpendLogicWiringStub {
    address[5] internal _signers;

    constructor(address[5] memory signers_) {
        _signers = signers_;
    }

    function registry() external view returns (address) {
        return msg.sender;
    }

    function signers() external view returns (address[5] memory) {
        return _signers;
    }
}

/// @notice Test harness: the real Registry, with its internal I6 check callable. Every entry point that takes signer
///         signatures calls it: confirmPayout, setReimburseDestination and (through the spend logic) commitSpend with 3;
///         the Intake checks `VaultOpened` with all 5 the same way.
contract RegistryHarness is ZecuredRegistry {
    constructor(RegistryConfig memory c) ZecuredRegistry(c) {}

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
///             leaves the outstanding, and the supply stays equal to the outstanding;
///         R5  K3, T-02: of the Intake's hooks only `activate` mints, exactly its reservation, and only the Intake
///             calls any of them; a refund never mints and its claim pays back exactly its deposit, to no destination
///             until the vault's account gives one.
/// @dev Halmos treats ecrecover as an uninterpreted function: a signature recovers to whatever address the solver
///      picks. R1 therefore checks the counting for every possible recovery, not the cryptography. Vaults open through
///      the Registry's own Intake hooks (the Intake stands at a fixed address); the deposit proof itself is the fork
///      suites' subject.
contract RegistryRulesCheck is Test {
    address internal constant ADAPTER = address(0x2000);
    address internal constant INTAKE = address(0x3000);
    address internal constant BOND = address(0x3001);
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
        address[5] memory signers = [SIGNER0, SIGNER1, SIGNER2, SIGNER3, SIGNER4];
        address logic = address(new SpendLogicWiringStub(signers));
        registry = new RegistryHarness(
            RegistryConfig({
                morpho: IMorpho(address(morpho)),
                zzec: IZZEC(token),
                adapter: ADAPTER,
                loanToken: address(0x6001),
                oracle: address(0x6002),
                irm: address(0x6003),
                lltv: 0.77e18,
                signers: signers,
                spendLogic: logic,
                intake: INTAKE,
                signerBond: BOND,
                slasher: address(0), // nothing frozen or compromised
                compensator: address(0),
                feeBps: 0,
                minFeeZat: 0,
                feeCommitment: bytes32(0),
                refundAbandonPeriod: 30 days
            })
        );
        vm.etch(token, address(new ZZEC(address(registry), ADAPTER, address(morpho))).code);
        vm.etch(logic, address(new ZecuredSpendLogic(address(registry), signers)).code);
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

    /// R1 at an entry point: setReimburseDestination takes 3 of 5, like confirmPayout and commitSpend.
    function check_I6_reimburseDestinationNeedsThreeDistinctSigners(
        uint8 count,
        bytes32 commitment,
        uint64 deadline,
        bytes32[4] memory r,
        bytes32[4] memory s,
        uint8[4] memory v
    ) public {
        // A released vault whose claim the bonds paid (T-05): its destination is now the signers' to give.
        _activeVault(1_000_000, 0);
        vm.prank(ACCOUNT);
        bytes32 claimId = registry.requestRelease(VAULT, keccak256("release"));
        vm.prank(BOND);
        registry.markBondPaid(claimId);

        if (count == 0) _checkReimburse(0, claimId, commitment, deadline, r, s, v);
        else if (count == 1) _checkReimburse(1, claimId, commitment, deadline, r, s, v);
        else if (count == 2) _checkReimburse(2, claimId, commitment, deadline, r, s, v);
        else if (count == 3) _checkReimburse(3, claimId, commitment, deadline, r, s, v);
        else _checkReimburse(4, claimId, commitment, deadline, r, s, v);
    }

    function _checkReimburse(
        uint256 n,
        bytes32 claimId,
        bytes32 commitment,
        uint64 deadline,
        bytes32[4] memory r,
        bytes32[4] memory s,
        uint8[4] memory v
    ) internal {
        bytes[] memory sigs = new bytes[](n);
        for (uint256 i; i < n; ++i) {
            sigs[i] = abi.encodePacked(r[i], s[i], v[i]);
        }
        bytes32 digest = registry.hashReimburseDestination(claimId, commitment, deadline);
        (bool ok,) = address(registry)
            .call(abi.encodeCall(IZecuredRegistry.setReimburseDestination, (claimId, commitment, deadline, sigs)));
        if (!ok) return;
        assert(n >= 3);
        uint256 signed;
        for (uint256 i; i < n; ++i) {
            uint256 index = _signerIndex(ecrecover(digest, v[i], r[i], s[i]));
            assert(index < 5 && signed & (1 << index) == 0);
            signed |= 1 << index;
        }
        assert(commitment != bytes32(0) && block.timestamp <= deadline);
        (,,, bytes32 set, ClaimStatus status) = registry.claim(claimId);
        assert(set == commitment && status == ClaimStatus.Pending);
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
        bytes32 userCommitment,
        address mmClaimant
    ) public {
        _activeVault(amount, held);
        Before memory b = _snapshot(vaultId);

        vm.prank(caller);
        (bool ok,) = address(registry)
            .call(
                abi.encodeCall(
                    IZecuredRegistry.recordClose,
                    (vaultId, mmAmount, userAmount, mmCommitment, userCommitment, mmClaimant)
                )
            );
        if (!ok) return;

        assert(caller == ADAPTER && b.status == VaultStatus.Active);
        assert(uint256(mmAmount) + userAmount == b.outstanding);
        assert(zzec.totalSupply() == b.supply - b.outstanding);
        assert(zzec.balanceOf(ADAPTER) == b.adapterHeld - b.outstanding);
        (, uint64 left, VaultStatus closed) = registry.vault(vaultId);
        assert(left == 0 && closed == VaultStatus.Closed);
        assert(registry.claimCount(vaultId) == b.claims + 2);
        _assertClaim(registry.claimIdOf(vaultId, b.claims), vaultId, ClaimKind.CloseUser, userAmount, userCommitment);
        _assertClaim(
            registry.claimIdOf(vaultId, b.claims + 1), vaultId, ClaimKind.CloseMarketMaker, mmAmount, mmCommitment
        );
        // T-04, T-05: the bonds and the compensator pay the executor; the user's claim goes to the vault's account.
        _assertClaimant(registry.claimIdOf(vaultId, b.claims + 1), mmClaimant);
        _assertClaimant(registry.claimIdOf(vaultId, b.claims), ACCOUNT);
        assert(zzec.totalSupply() == left); // I1
    }

    // ------------------------------------------------------------------ R3 (I9)

    function check_I9_liquidationSeizesAtMostTheOutstanding(
        uint64 amount,
        uint64 held,
        address caller,
        bytes32 vaultId,
        uint64 seized,
        bytes32 commitment,
        address claimant
    ) public {
        _activeVault(amount, held);
        Before memory b = _snapshot(vaultId);

        vm.prank(caller);
        (bool ok,) = address(registry)
            .call(abi.encodeCall(IZecuredRegistry.recordLiquidation, (vaultId, seized, commitment, claimant)));
        if (!ok) return;

        assert(caller == ADAPTER && b.status == VaultStatus.Active);
        assert(seized > 0 && seized <= b.outstanding);
        assert(zzec.totalSupply() == b.supply - seized && zzec.balanceOf(ADAPTER) == b.adapterHeld - seized);
        (, uint64 left, VaultStatus afterwards) = registry.vault(vaultId);
        assert(left == b.outstanding - seized);
        assert(afterwards == (left == 0 ? VaultStatus.Closed : VaultStatus.Active));
        assert(registry.claimCount(vaultId) == b.claims + 1);
        _assertClaim(registry.claimIdOf(vaultId, b.claims), vaultId, ClaimKind.Liquidation, seized, commitment);
        _assertClaimant(registry.claimIdOf(vaultId, b.claims), claimant);
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
        else if (op == 2) data = abi.encodeCall(IZecuredRegistry.recordLiquidation, (vaultId, x, c1, caller));
        else data = abi.encodeCall(IZecuredRegistry.recordClose, (vaultId, x, y, c1, c2, caller));
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

    // ------------------------------------------------------------------ R5 (K3, T-02)

    function check_K3_onlyActivationMints(
        uint64 amount,
        uint8 op,
        address caller,
        bytes32 vaultId,
        address account,
        uint64 x,
        bytes32 tag
    ) public {
        vm.assume(amount > 0);
        vm.prank(INTAKE);
        registry.reserveDeposit(VAULT, ACCOUNT, amount, TAG);
        uint256 supply = zzec.totalSupply();
        uint256 locked = registry.lockedZatoshi();
        uint64 claims = registry.claimCount(vaultId);

        bytes memory data;
        if (op == 0) data = abi.encodeCall(IZecuredRegistry.activate, (vaultId, account, x, tag));
        else if (op == 1) data = abi.encodeCall(IZecuredRegistry.releaseDeposit, (vaultId, account, tag));
        else if (op == 2) data = abi.encodeCall(IZecuredRegistry.convertToRefund, (vaultId, account, x, tag));
        else if (op == 3) data = abi.encodeCall(IZecuredRegistry.openRefund, (vaultId, account, x, tag));
        else data = abi.encodeCall(IZecuredRegistry.reserveDeposit, (vaultId, account, x, tag));
        vm.prank(caller);
        (bool ok, bytes memory ret) = address(registry).call(data);
        if (!ok) return;

        assert(caller == INTAKE);
        if (op == 0) {
            // Exactly the reservation: its vault, account, note and amount (no opening fee in this deployment).
            assert(vaultId == VAULT && account == ACCOUNT && tag == TAG && x == amount);
            assert(zzec.totalSupply() == supply + amount);
            (, uint64 outstanding, VaultStatus status) = registry.vault(VAULT);
            assert(outstanding == amount && status == VaultStatus.Active);
            return;
        }
        assert(zzec.totalSupply() == supply);
        if (op == 2 || op == 3) {
            assert(registry.lockedZatoshi() == locked + x);
            assert(registry.claimCount(vaultId) == claims + 1);
            _assertClaim(abi.decode(ret, (bytes32)), vaultId, ClaimKind.Refund, x, bytes32(0));
        } else {
            assert(registry.lockedZatoshi() == locked);
        }
    }

    // ------------------------------------------------------------------ helpers

    /// A vault's state before a call (struct: the checks have many symbolic inputs).
    struct Before {
        uint64 outstanding;
        VaultStatus status;
        uint64 claims;
        uint256 supply;
        uint256 adapterHeld;
    }

    function _snapshot(bytes32 vaultId) internal view returns (Before memory b) {
        (, b.outstanding, b.status) = registry.vault(vaultId);
        b.claims = registry.claimCount(vaultId);
        b.supply = zzec.totalSupply();
        b.adapterHeld = zzec.balanceOf(ADAPTER);
    }

    function _assertClaimant(bytes32 claimId, address claimant) internal view {
        assert(registry.claimDetails(claimId).claimant == claimant);
    }

    /// A vault reserved and activated through the Registry's real Intake hooks, then `held` of its collateral taken by
    /// the Adapter (as in a liquidation or a close, with the Adapter's own zZEC expectation).
    function _activeVault(uint64 amount, uint64 held) internal {
        vm.assume(amount > 0 && held <= amount);
        vm.startPrank(INTAKE);
        registry.reserveDeposit(VAULT, ACCOUNT, amount, TAG);
        registry.activate(VAULT, ACCOUNT, amount, TAG);
        vm.stopPrank();

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
