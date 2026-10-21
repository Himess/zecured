// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Errors} from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {SignerBondV2} from "../../src/SignerBondV2.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {ISignerBond, BondConfig} from "../../src/interfaces/v3/ISignerBond.sol";
import {ClaimDetails, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {IZecuredPriceFeed} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {ZecuredPriceFeed} from "../../src/ZecuredPriceFeed.sol";
import {MockAggregator} from "../mocks/MockAggregator.sol";
import {
    MockBondRegistry,
    MockBondSlasher,
    MockBondIntake,
    MockBondCompensator,
    MockBondOracle,
    MockBondPlainOracle,
    MockHookToken,
    MockReentrantAccount,
    MockReentryProbe
} from "../mocks/BondV2Mocks.sol";

/// @notice Test harness: SignerBondV2 with its pro-rata deduction callable on its own.
contract SignerBondV2Harness is SignerBondV2 {
    constructor(BondConfig memory cfg) SignerBondV2(cfg) {}

    function takeProRata(uint256 amount) external {
        _takeProRata(amount, totalSlashable());
    }
}

/// @notice Unit tests of SignerBondV2 (design notes T-01 and T-05, with a bound on deferrals; rules in the v3
///         `ISignerBond`; v4: liveness receipts R-5, compensator entitlements R-7, stale prices R-9, R-18). No fork: the
///         Registry, the slasher, the Intake, the compensator and the oracle are MOCKs (test/mocks/BondV2Mocks.sol) and
///         the token is MockUSDC. The parameters are the judge deployment's (docs/MAINNET.md § 2, judge column): five
///         bonds of 400 USDC (2,000 in all), a 100 USDC minimum, ZEC at $1,200.
contract SignerBondV2Test is Test {
    uint256 internal constant START = 1_790_000_000;
    uint256 internal constant MIN_BOND = 100e6;
    uint256 internal constant BOND = 400e6;
    uint64 internal constant UNBONDING = 2 days + 6 hours;
    uint64 internal constant WINDOW = 1 days;
    uint256 internal constant BOUNTY_BPS = 1_000;
    uint256 internal constant BOUNTY_CAP = 50e6;
    uint64 internal constant LIVENESS = 1 days;
    uint64 internal constant GRACE = 2 hours;
    uint16 internal constant PREMIUM = 500;
    /// $1,200 per ZEC on Morpho's scale: USDC units per zatoshi times 1e36, i.e. 1,200 * 1e6 / 1e8 * 1e36.
    uint256 internal constant PRICE = 1_200e34;

    bytes32 internal constant VAULT = keccak256("bondv2.vault");
    bytes32 internal constant CLAIM = keccak256("bondv2.claim");
    bytes32 internal constant INPUT = keccak256("bondv2.input");
    bytes32 internal constant COMMITMENT = keccak256("bondv2.commitment");
    bytes32 internal constant CASE = keccak256("bondv2.case");

    MockUSDC internal usdc;
    MockBondRegistry internal registry;
    MockBondSlasher internal slasher;
    MockBondIntake internal intake;
    MockBondCompensator internal compensator;
    MockBondOracle internal oracle;
    SignerBondV2 internal bond;

    address[5] internal signers;
    uint256[5] internal signerKeys;
    address internal stranger = makeAddr("bondv2.stranger");
    address internal watcher = makeAddr("bondv2.watcher");
    address internal liquidator = makeAddr("bondv2.liquidator");
    address internal account = makeAddr("bondv2.account");

    function setUp() public {
        vm.warp(START);
        for (uint256 i; i < 5; ++i) {
            (signers[i], signerKeys[i]) = makeAddrAndKey(string.concat("bondv2.signer", vm.toString(i)));
        }
        // Sorted, as the Registry keeps them: a signer's index is its position (its key moves with it).
        for (uint256 i = 1; i < 5; ++i) {
            for (uint256 j = i; j > 0 && signers[j - 1] > signers[j]; --j) {
                (signers[j - 1], signers[j]) = (signers[j], signers[j - 1]);
                (signerKeys[j - 1], signerKeys[j]) = (signerKeys[j], signerKeys[j - 1]);
            }
        }
        usdc = new MockUSDC(address(this));
        registry = new MockBondRegistry(signers);
        slasher = new MockBondSlasher();
        intake = new MockBondIntake();
        compensator = new MockBondCompensator(IERC20(address(usdc)));
        oracle = new MockBondOracle(PRICE);
        _replaceBond(_config());
        for (uint8 i; i < 5; ++i) {
            _deposit(i, BOND);
        }
    }

    // ================================================================== helpers

    function _config() internal view returns (BondConfig memory) {
        return BondConfig({
            registry: address(registry),
            slasher: address(slasher),
            intake: address(intake),
            compensator: address(compensator),
            usdc: IERC20(address(usdc)),
            oracle: IOracle(address(oracle)),
            minBond: MIN_BOND,
            unbondingPeriod: UNBONDING,
            challengeWindow: WINDOW,
            bountyBps: BOUNTY_BPS,
            bountyCap: BOUNTY_CAP,
            livenessPeriod: LIVENESS,
            commitGrace: GRACE,
            livenessPremiumBps: PREMIUM
        });
    }

    /// A new bond (without deposits) that the MOCK Registry and compensator now answer to.
    function _replaceBond(BondConfig memory cfg) internal returns (SignerBondV2 b) {
        b = new SignerBondV2(cfg);
        _use(b, cfg.usdc);
    }

    function _use(SignerBondV2 b, IERC20 token) internal {
        bond = b;
        registry.setSignerBond(address(b), token);
        compensator.setSignerBond(address(b));
    }

    function _deposit(uint8 i, uint256 amount) internal {
        usdc.mint(signers[i], amount);
        vm.startPrank(signers[i]);
        usdc.approve(address(bond), amount);
        bond.deposit(amount);
        vm.stopPrank();
    }

    function _unbond(uint8 i, uint256 amount) internal {
        vm.prank(signers[i]);
        bond.requestUnbond(amount);
    }

    function _slash(bytes32 caseId, uint8 bitmap) internal returns (uint256) {
        vm.prank(address(slasher));
        return bond.slash(caseId, bitmap, watcher);
    }

    /// A pending claim of `VAULT` with a destination, created now, without a receipt.
    function _claimUnarmed(bytes32 claimId, ClaimKind kind, uint64 zat, address claimant) internal {
        registry.setClaim(
            claimId,
            ClaimDetails({
                vaultId: VAULT,
                kind: kind,
                status: ClaimStatus.Pending,
                amount: zat,
                commitment: COMMITMENT,
                createdAt: uint40(block.timestamp),
                bondPaid: false,
                claimant: claimant
            })
        );
    }

    /// A pending claim of `VAULT` with a destination, payable from now: for the kinds the bonds pay, a signer's node
    /// received its destination now (R-5).
    function _claim(bytes32 claimId, ClaimKind kind, uint64 zat, address claimant) internal {
        _claimUnarmed(claimId, kind, zat, claimant);
        if (kind != ClaimKind.Fee && kind != ClaimKind.Retired) _arm(bond, claimId, uint64(block.timestamp), 0);
    }

    /// Signer `index`'s `DestinationReceived` receipt for the claim's current commitment, under `b`'s domain.
    function _receipt(SignerBondV2 b, bytes32 claimId, uint64 receivedAt, uint256 index)
        internal
        view
        returns (bytes memory)
    {
        bytes32 commitment = registry.claimDetails(claimId).commitment;
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(signerKeys[index], b.hashDestinationReceived(claimId, commitment, receivedAt));
        return abi.encodePacked(r, s, v);
    }

    function _arm(SignerBondV2 b, bytes32 claimId, uint64 receivedAt, uint256 index) internal {
        b.armLiveness(claimId, receivedAt, _receipt(b, claimId, receivedAt, index));
    }

    /// The liveness payment, computed independently: the claim's value at `price` plus the premium, rounded down.
    function _due(uint64 zat, uint256 price) internal pure returns (uint256) {
        return uint256(zat) * price * (10_000 + PREMIUM) / 10_000 / 1e36;
    }

    /// I19: the bond holds exactly the bonds, the unbondings and the unclaimed bounties (only `watcher` gets any here).
    function _assertAccounting() internal view {
        assertEq(usdc.balanceOf(address(bond)), bond.totalSlashable() + bond.bountyOf(watcher), "I19");
    }

    function _count(Vm.Log[] memory logs, bytes32 topic0) internal view returns (uint256 n) {
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == address(bond) && logs[i].topics.length != 0 && logs[i].topics[0] == topic0) ++n;
        }
    }

    function _assertReentryRefused(MockReentryProbe probe) internal view {
        assertTrue(probe.probeTried(), "the hook tried to re-enter");
        assertFalse(probe.probeSucceeded(), "and failed");
        assertEq(bytes4(probe.probeRevertData()), ReentrancyGuardTransient.ReentrancyGuardReentrantCall.selector);
    }

    // ================================================================== constructor

    function test_constructor_storesTheConfig() public view {
        assertEq(bond.registry(), address(registry));
        assertEq(bond.slasher(), address(slasher));
        assertEq(bond.intake(), address(intake));
        assertEq(bond.compensator(), address(compensator));
        assertEq(address(bond.usdc()), address(usdc));
        assertEq(address(bond.oracle()), address(oracle));
        assertEq(bond.minBond(), MIN_BOND);
        assertEq(bond.unbondingPeriod(), UNBONDING);
        assertEq(bond.challengeWindow(), WINDOW);
        assertEq(bond.bountyBps(), BOUNTY_BPS);
        assertEq(bond.bountyCap(), BOUNTY_CAP);
        assertEq(bond.livenessPeriod(), LIVENESS);
        assertEq(bond.commitGrace(), GRACE);
        assertEq(bond.livenessPremiumBps(), PREMIUM);
        assertFalse(bond.halted());
        assertEq(bond.bondOf(5), 0, "there is no sixth signer");
    }

    function test_constructor_rejectsAZeroAddress() public {
        for (uint256 field; field < 6; ++field) {
            BondConfig memory cfg = _config();
            if (field == 0) cfg.registry = address(0);
            else if (field == 1) cfg.slasher = address(0);
            else if (field == 2) cfg.intake = address(0);
            else if (field == 3) cfg.compensator = address(0);
            else if (field == 4) cfg.usdc = IERC20(address(0));
            else cfg.oracle = IOracle(address(0));
            vm.expectRevert(ISignerBond.ZeroAddress.selector);
            new SignerBondV2(cfg);
        }
    }

    function test_constructor_rejectsAnInvalidConfig() public {
        BondConfig memory cfg = _config();
        cfg.minBond = 0;
        _expectInvalid(cfg);

        cfg = _config();
        cfg.challengeWindow = 0;
        _expectInvalid(cfg);

        cfg = _config();
        cfg.livenessPeriod = 0;
        _expectInvalid(cfg);

        cfg = _config();
        cfg.unbondingPeriod = WINDOW + LIVENESS + GRACE - 1; // a claim due at its latest would outlive the unbonding
        _expectInvalid(cfg);

        cfg = _config();
        cfg.bountyBps = 10_001;
        _expectInvalid(cfg);

        // The bound is computed without overflow: two maximal periods need an unbonding no uint64 holds.
        cfg = _config();
        cfg.challengeWindow = type(uint64).max;
        cfg.livenessPeriod = type(uint64).max;
        cfg.unbondingPeriod = type(uint64).max;
        _expectInvalid(cfg);
    }

    function _expectInvalid(BondConfig memory cfg) internal {
        vm.expectRevert(ISignerBond.InvalidConfig.selector);
        new SignerBondV2(cfg);
    }

    function test_constructor_acceptsTheBoundaries() public {
        BondConfig memory cfg = _config();
        cfg.unbondingPeriod = WINDOW + LIVENESS;
        cfg.bountyBps = 10_000;
        cfg.bountyCap = 0;
        cfg.commitGrace = 0;
        cfg.livenessPremiumBps = 0;
        SignerBondV2 b = new SignerBondV2(cfg);
        assertEq(b.unbondingPeriod(), WINDOW + LIVENESS);
        assertEq(b.bountyBps(), 10_000);
        assertEq(b.bountyCap(), 0);
        assertEq(b.commitGrace(), 0);
        assertEq(b.livenessPremiumBps(), 0);
        cfg = _config();
        cfg.unbondingPeriod = WINDOW + LIVENESS + GRACE;
        assertEq(new SignerBondV2(cfg).unbondingPeriod(), WINDOW + LIVENESS + GRACE);
    }

    // ================================================================== deposit

    function test_deposit_addsToTheSignersBond_anyAmountAnyTime() public {
        usdc.mint(signers[2], 150e6);
        vm.startPrank(signers[2]);
        usdc.approve(address(bond), 150e6);
        vm.expectEmit(address(bond));
        emit ISignerBond.BondDeposited(2, 100e6, BOND + 100e6);
        bond.deposit(100e6);
        vm.expectEmit(address(bond));
        emit ISignerBond.BondDeposited(2, 50e6, BOND + 150e6);
        bond.deposit(50e6);
        vm.stopPrank();
        assertEq(bond.bondOf(2), BOND + 150e6);
        assertEq(usdc.balanceOf(signers[2]), 0);
        assertEq(usdc.balanceOf(address(bond)), 5 * BOND + 150e6);
        _assertAccounting();
    }

    function test_deposit_reverts() public {
        usdc.mint(stranger, 1e6);
        vm.startPrank(stranger);
        usdc.approve(address(bond), 1e6);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.deposit(1e6);
        vm.stopPrank();

        vm.prank(signers[0]);
        vm.expectRevert(ISignerBond.ZeroAmount.selector);
        bond.deposit(0);

        usdc.mint(signers[0], 1e6);
        vm.prank(signers[0]); // no allowance
        vm.expectRevert(abi.encodeWithSelector(IERC20Errors.ERC20InsufficientAllowance.selector, address(bond), 0, 1e6));
        bond.deposit(1e6);

        _slash(CASE, 0); // the first slash halts, even one that takes nothing
        vm.startPrank(signers[0]);
        usdc.approve(address(bond), 1e6);
        vm.expectRevert(ISignerBond.BondsHalted.selector);
        bond.deposit(1e6);
        vm.stopPrank();
    }

    // ================================================================== effective bond, fully bonded

    function test_effectiveBond_equalBondsCountInFull() public view {
        assertEq(bond.effectiveBond(), 5 * BOND);
        assertTrue(bond.fullyBonded());
        assertEq(bond.totalSlashable(), 5 * BOND);
    }

    function test_effectiveBond_unequalBondsCountByTheThreeSmallest() public {
        _deposit(0, 600e6);
        _deposit(4, 1_600e6);
        // [1,000, 400, 400, 400, 2,000]: the sum is 4,200, the three smallest 1,200, and 5/3 of that 2,000.
        assertEq(bond.effectiveBond(), 2_000e6);
    }

    function test_effectiveBond_roundsDown() public {
        for (uint8 i; i < 4; ++i) {
            _deposit(i, 1);
        }
        // The three smallest are 1,200 USDC and 2 units: 5/3 of that is 2,000 USDC and 3.33 units, rounded down.
        assertEq(bond.effectiveBond(), 2_000e6 + 3);
    }

    function test_effectiveBond_leavesUnbondingOut() public {
        _unbond(3, 300e6);
        // [400, 400, 400, 100, 400]: the sum is 1,700, the three smallest 900, and 5/3 of that 1,500.
        assertEq(bond.effectiveBond(), 1_500e6);
        assertEq(bond.totalSlashable(), 5 * BOND, "the unbonding stays slashable");
    }

    /// Against an independent computation: the cheapest coalition of three, by brute force over all ten triples.
    function testFuzz_effectiveBond_isFiveThirdsOfTheCheapestThree(uint256[5] memory amounts) public {
        _replaceBond(_config());
        uint256[5] memory bonds;
        uint256 sum;
        for (uint8 i; i < 5; ++i) {
            bonds[i] = bound(amounts[i], 0, 1e15); // up to a billion USDC each
            sum += bonds[i];
            if (bonds[i] != 0) _deposit(i, bonds[i]);
        }
        uint256 cheapest = type(uint256).max;
        for (uint256 i; i < 5; ++i) {
            for (uint256 j = i + 1; j < 5; ++j) {
                for (uint256 k = j + 1; k < 5; ++k) {
                    uint256 s = bonds[i] + bonds[j] + bonds[k];
                    if (s < cheapest) cheapest = s;
                }
            }
        }
        uint256 expected = cheapest * 5 / 3;
        if (sum < expected) expected = sum;
        uint256 effective = bond.effectiveBond();
        assertEq(effective, expected);
        assertLe(effective, sum);
        assertLe(3 * effective, 5 * cheapest, "any three signers lose at least 3/5 of it");
    }

    /// A deposit never lowers the effective bond (test/halmos/SignerBondV2Rules.t.sol proves the steps of this but its
    /// two monotonicity steps, which the solvers cannot do), and raises it by at most 5/3 of the deposit, rounded up: the
    /// sum grows by the deposit and the three smallest bonds by at most that much.
    function testFuzz_deposit_neverLowersTheEffectiveBond(uint256[5] memory amounts, uint8 signer, uint256 amount)
        public
    {
        _replaceBond(_config());
        for (uint8 i; i < 5; ++i) {
            amounts[i] = bound(amounts[i], 0, 1e15); // up to a billion USDC each
            if (amounts[i] != 0) _deposit(i, amounts[i]);
        }
        amount = bound(amount, 1, 1e15);
        uint256 before = bond.effectiveBond();
        _deposit(signer % 5, amount);
        uint256 afterwards = bond.effectiveBond();
        assertGe(afterwards, before, "never lower");
        assertLe(afterwards, before + (5 * amount + 2) / 3, "at most 5/3 of the deposit higher");
    }

    function test_fullyBonded_needsMinBondFromAllFive() public {
        _replaceBond(_config());
        assertFalse(bond.fullyBonded());
        for (uint8 i; i < 4; ++i) {
            _deposit(i, MIN_BOND);
        }
        assertFalse(bond.fullyBonded(), "four of five");
        _deposit(4, MIN_BOND - 1);
        assertFalse(bond.fullyBonded(), "one unit short");
        _deposit(4, 1);
        assertTrue(bond.fullyBonded(), "exactly minBond");
    }

    // ================================================================== requestUnbond

    function test_requestUnbond_partialMovesTheAmountToUnbonding() public {
        uint64 readyAt = uint64(block.timestamp) + UNBONDING;
        vm.prank(signers[1]);
        vm.expectEmit(address(bond));
        emit ISignerBond.UnbondRequested(1, 300e6, readyAt);
        bond.requestUnbond(300e6);
        assertEq(bond.bondOf(1), MIN_BOND, "exactly minBond is left");
        (uint256 amount, uint64 at) = bond.unbondingOf(1);
        assertEq(amount, 300e6);
        assertEq(at, readyAt);
        assertTrue(bond.fullyBonded());
        assertEq(bond.effectiveBond(), 1_500e6);
        assertEq(bond.totalSlashable(), 5 * BOND);
        assertEq(usdc.balanceOf(address(bond)), 5 * BOND, "nothing leaves yet");
        _assertAccounting();
    }

    function test_requestUnbond_partialNeedsTheCoverageToHoldWithoutIt() public {
        // Without 300 USDC of signer 1 the effective bond is 1,500 USDC: the Intake is asked about exactly that.
        intake.setRequiredBond(1_500e6 + 1);
        vm.prank(signers[1]);
        vm.expectRevert(ISignerBond.CoverageWouldBreak.selector);
        bond.requestUnbond(300e6);
        intake.setRequiredBond(1_500e6);
        _unbond(1, 300e6);
        assertEq(bond.bondOf(1), MIN_BOND);
    }

    function test_requestUnbond_partialIsLockedWhileACaseIsOpen() public {
        slasher.setOpenCaseCount(2);
        vm.prank(signers[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.CaseOpen.selector, 2));
        bond.requestUnbond(100e6);
    }

    function test_requestUnbond_partialMustLeaveMinBond() public {
        vm.prank(signers[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.BelowMinBond.selector, MIN_BOND - 1, MIN_BOND));
        bond.requestUnbond(BOND - MIN_BOND + 1);
    }

    function test_requestUnbond_rejectsZeroTooMuchAndStrangers() public {
        vm.prank(signers[1]);
        vm.expectRevert(ISignerBond.ZeroAmount.selector);
        bond.requestUnbond(0);
        vm.prank(signers[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.InsufficientBond.selector, BOND + 1, BOND));
        bond.requestUnbond(BOND + 1);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.requestUnbond(1);
    }

    /// v1's rule (review S5): no vault open, no case open, and the challenge window passed since the last vault settled.
    function test_requestUnbond_fullExitFollowsTheV1Rule() public {
        // A vault is open (and none ever settled: the window counts from time zero).
        registry.setOpenVaultCount(1);
        vm.prank(signers[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 1, 0, WINDOW));
        bond.requestUnbond(BOND);

        // It settled now; a case is open.
        registry.setOpenVaultCount(0);
        registry.setLastVaultSettledAt(uint64(block.timestamp));
        slasher.setOpenCaseCount(1);
        uint64 unlocksAt = uint64(block.timestamp) + WINDOW;
        vm.prank(signers[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 0, 1, unlocksAt));
        bond.requestUnbond(BOND);

        slasher.setOpenCaseCount(0);
        vm.warp(unlocksAt - 1);
        vm.prank(signers[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 0, 0, unlocksAt));
        bond.requestUnbond(BOND);

        // Once the window has passed the whole bond can leave; a full exit does not ask the Intake.
        vm.warp(unlocksAt);
        intake.setRequiredBond(type(uint256).max);
        _unbond(2, BOND);
        assertEq(bond.bondOf(2), 0);
        (uint256 amount, uint64 readyAt) = bond.unbondingOf(2);
        assertEq(amount, BOND);
        assertEq(readyAt, unlocksAt + UNBONDING);
        assertFalse(bond.fullyBonded());
        assertEq(bond.effectiveBond(), uint256(4_000e6) / 3, "[400, 400, 0, 400, 400]: 5/3 of 800 USDC, rounded down");
        assertEq(bond.totalSlashable(), 5 * BOND, "still slashable until withdrawn");
    }

    function test_requestUnbond_addsToARunningUnbondingAndRestartsItsClock() public {
        _unbond(0, 100e6);
        vm.warp(block.timestamp + 1 days);
        uint64 readyAt = uint64(block.timestamp) + UNBONDING;
        vm.prank(signers[0]);
        vm.expectEmit(address(bond));
        emit ISignerBond.UnbondRequested(0, 50e6, readyAt);
        bond.requestUnbond(50e6);
        (uint256 amount, uint64 at) = bond.unbondingOf(0);
        assertEq(amount, 150e6);
        assertEq(at, readyAt);
        assertEq(bond.bondOf(0), BOND - 150e6);
    }

    // ================================================================== cancelUnbond

    function test_cancelUnbond_returnsTheWholeUnbonding() public {
        _unbond(3, 100e6);
        _unbond(3, 50e6);
        slasher.setOpenCaseCount(1); // allowed with a case open: the amount stays slashable either way
        vm.prank(signers[3]);
        vm.expectEmit(address(bond));
        emit ISignerBond.UnbondCancelled(3, 150e6);
        bond.cancelUnbond();
        assertEq(bond.bondOf(3), BOND);
        (uint256 amount, uint64 readyAt) = bond.unbondingOf(3);
        assertEq(amount, 0);
        assertEq(readyAt, 0);
        assertEq(bond.effectiveBond(), 5 * BOND);

        vm.prank(signers[3]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NothingUnbonding.selector, 3));
        bond.cancelUnbond();
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.cancelUnbond();
        _assertAccounting();
    }

    // ================================================================== withdrawUnbonded

    function test_withdrawUnbonded_paysOnceReady() public {
        _unbond(4, 300e6);
        uint64 readyAt = uint64(block.timestamp) + UNBONDING;
        address to = makeAddr("bondv2.signer4.cold");
        vm.warp(readyAt - 1);
        vm.prank(signers[4]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.UnbondingNotReady.selector, readyAt));
        bond.withdrawUnbonded(to);

        vm.warp(readyAt);
        vm.prank(signers[4]);
        vm.expectEmit(address(bond));
        emit ISignerBond.BondWithdrawn(4, to, 300e6);
        bond.withdrawUnbonded(to);
        assertEq(usdc.balanceOf(to), 300e6);
        (uint256 amount, uint64 at) = bond.unbondingOf(4);
        assertEq(amount, 0);
        assertEq(at, 0);
        assertEq(bond.bondOf(4), MIN_BOND);
        assertEq(bond.totalSlashable(), 5 * BOND - 300e6);
        _assertAccounting();

        vm.prank(signers[4]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NothingUnbonding.selector, 4));
        bond.withdrawUnbonded(to);
    }

    function test_withdrawUnbonded_isLockedWhileACaseIsOpen() public {
        _unbond(4, 300e6);
        vm.warp(block.timestamp + UNBONDING);
        slasher.setOpenCaseCount(1);
        vm.prank(signers[4]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.CaseOpen.selector, 1));
        bond.withdrawUnbonded(signers[4]);
    }

    function test_withdrawUnbonded_rejectsTheZeroAddressAndStrangers() public {
        _unbond(4, 300e6);
        vm.warp(block.timestamp + UNBONDING);
        vm.prank(signers[4]);
        vm.expectRevert(ISignerBond.ZeroAddress.selector);
        bond.withdrawUnbonded(address(0));
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.withdrawUnbonded(stranger);
    }

    /// A halt stops deposits (and minting); the unbonding rules go on.
    function test_halt_stopsDepositsButNotUnbonding() public {
        _unbond(0, 100e6);
        _slash(CASE, 0x02);
        assertTrue(bond.halted());
        _unbond(2, 100e6);
        vm.prank(signers[2]);
        bond.cancelUnbond();
        assertEq(bond.bondOf(2), BOND);
        vm.warp(block.timestamp + UNBONDING);
        vm.prank(signers[0]);
        bond.withdrawUnbonded(signers[0]);
        assertEq(usdc.balanceOf(signers[0]), 100e6);
        _assertAccounting();
    }

    // ================================================================== slash

    function test_slash_onlyTheSlasher() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotTheSlasher.selector, stranger));
        bond.slash(CASE, 0x1f, watcher);
    }

    /// An unbonding is slashable until it is withdrawn, even once it is ready.
    function test_slash_takesTheBondAndTheUnbondingOfTheNamedSigners() public {
        _unbond(1, 300e6);
        vm.warp(block.timestamp + UNBONDING); // ready, not withdrawn
        uint8 bitmap = (1 << 1) | (1 << 3);
        vm.prank(address(slasher));
        vm.expectEmit(address(bond));
        emit ISignerBond.BondSlashed(CASE, 1, BOND);
        vm.expectEmit(address(bond));
        emit ISignerBond.BondSlashed(CASE, 3, BOND);
        vm.expectEmit(address(bond));
        emit ISignerBond.BountyCredited(CASE, watcher, BOUNTY_CAP); // 10 % of 800 is 80, capped at 50
        vm.expectEmit(address(bond));
        emit ISignerBond.Halted(CASE);
        assertEq(bond.slash(CASE, bitmap, watcher), 2 * BOND);

        assertEq(bond.bondOf(1), 0);
        assertEq(bond.bondOf(3), 0);
        (uint256 unbonding, uint64 readyAt) = bond.unbondingOf(1);
        assertEq(unbonding, 0);
        assertEq(readyAt, 0);
        assertEq(bond.bondOf(0) + bond.bondOf(2) + bond.bondOf(4), 3 * BOND, "the others keep theirs");
        assertEq(bond.bountyOf(watcher), BOUNTY_CAP);
        assertEq(usdc.balanceOf(address(compensator)), 2 * BOND - BOUNTY_CAP);
        assertEq(compensator.onFundsCalls(), 1);
        assertEq(compensator.lastCaseId(), CASE);
        assertEq(compensator.lastAmount(), 2 * BOND - BOUNTY_CAP);
        assertTrue(bond.halted());
        _assertAccounting();
    }

    function test_slash_bountyIsTenPercentUpToTheCap_andAddsUp() public {
        _slash(CASE, 0x01);
        assertEq(bond.bountyOf(watcher), 40e6, "10 % of 400");
        _slash(keccak256("bondv2.case2"), 0x1e);
        assertEq(bond.bountyOf(watcher), 40e6 + BOUNTY_CAP, "10 % of 1,600, capped at 50");
        assertEq(usdc.balanceOf(address(compensator)), 5 * BOND - 40e6 - BOUNTY_CAP);
        assertEq(compensator.onFundsCalls(), 2);
        assertEq(bond.totalSlashable(), 0);
        _assertAccounting();
    }

    function test_slash_withoutABountyEverythingGoesToTheCompensator() public {
        BondConfig memory cfg = _config();
        cfg.bountyBps = 0;
        _replaceBond(cfg);
        for (uint8 i; i < 5; ++i) {
            _deposit(i, BOND);
        }
        vm.recordLogs();
        _slash(CASE, 0x1f);
        assertEq(_count(vm.getRecordedLogs(), ISignerBond.BountyCredited.selector), 0);
        assertEq(bond.bountyOf(watcher), 0);
        assertEq(compensator.lastAmount(), 5 * BOND);
        assertEq(usdc.balanceOf(address(compensator)), 5 * BOND);
    }

    function test_slash_aBountyOfEverythingLeavesTheCompensatorNothing() public {
        BondConfig memory cfg = _config();
        cfg.bountyBps = 10_000;
        cfg.bountyCap = type(uint256).max;
        _replaceBond(cfg);
        _deposit(0, BOND);
        _slash(CASE, 0x01);
        assertEq(bond.bountyOf(watcher), BOND);
        assertEq(compensator.onFundsCalls(), 0, "onFunds only for a non-zero amount");
        assertEq(usdc.balanceOf(address(compensator)), 0);
        assertTrue(bond.halted());
        _assertAccounting();
    }

    function test_slash_theFirstHaltsEvenIfItTookNothing_andOnlyTheFirst() public {
        vm.prank(address(slasher));
        vm.expectEmit(address(bond));
        emit ISignerBond.Halted(CASE);
        assertEq(bond.slash(CASE, 0, watcher), 0);
        assertTrue(bond.halted());
        assertEq(compensator.onFundsCalls(), 0);
        assertEq(bond.totalSlashable(), 5 * BOND);

        vm.recordLogs();
        assertEq(_slash(keccak256("bondv2.case2"), 0x01), BOND);
        assertEq(_count(vm.getRecordedLogs(), ISignerBond.Halted.selector), 0, "halted once");
        assertTrue(bond.halted());
    }

    function test_slash_ignoresBitsBeyondTheFifthSigner() public {
        assertEq(_slash(CASE, 0xe0), 0);
        assertEq(bond.totalSlashable(), 5 * BOND);
    }

    function test_slash_paysTheCompensatorBeforeTellingIt_andCannotBeReentered() public {
        compensator.armProbe(address(bond), abi.encodeCall(ISignerBond.slash, (CASE, 0x1f, watcher)));
        _slash(CASE, 0x03);
        assertEq(compensator.balanceAtCall(), 2 * BOND - BOUNTY_CAP, "the USDC arrived before onFunds");
        assertTrue(compensator.haltedAtCall(), "halted before the call");
        assertEq(compensator.slashableAtCall(), 3 * BOND, "the bonds were taken before the call");
        _assertReentryRefused(compensator);
        assertEq(bond.totalSlashable(), 3 * BOND);
    }

    // ================================================================== claimBounty

    function test_claimBounty_paysTheBeneficiaryOnce() public {
        _slash(CASE, 0x01);
        vm.prank(watcher);
        vm.expectEmit(address(bond));
        emit ISignerBond.BountyClaimed(watcher, 40e6);
        bond.claimBounty();
        assertEq(usdc.balanceOf(watcher), 40e6);
        assertEq(bond.bountyOf(watcher), 0);
        _assertAccounting();
        vm.prank(watcher);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NothingToClaim.selector, watcher));
        bond.claimBounty();
    }

    // ================================================================== slashLiveness (T-05)

    function test_slashLiveness_paysTheClaimantFromEveryBondProRata() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        // 0.1 ZEC at $1,200 is 120 USDC; with the 5 % premium 126 USDC, 25.2 USDC from each of the equal bonds.
        uint256 due = 126e6;
        assertEq(_due(0.1e8, PRICE), due);
        vm.prank(liquidator);
        vm.expectEmit(address(bond));
        emit ISignerBond.LivenessPaid(CLAIM, liquidator, due, PRICE);
        assertEq(bond.slashLiveness(CLAIM), due);
        for (uint8 i; i < 5; ++i) {
            assertEq(bond.bondOf(i), BOND - 25.2e6);
        }
        assertEq(usdc.balanceOf(liquidator), due);
        assertEq(bond.totalSlashable(), 5 * BOND - due);
        assertEq(registry.markBondPaidCalls(), 1);
        assertEq(registry.lastMarked(), CLAIM);
        assertTrue(registry.claimDetails(CLAIM).bondPaid);
        assertFalse(bond.halted(), "a liveness payment does not halt");
        _assertAccounting();
    }

    function test_slashLiveness_paysEveryEligibleKind() public {
        ClaimKind[5] memory kinds = [
            ClaimKind.Release, ClaimKind.Liquidation, ClaimKind.CloseUser, ClaimKind.CloseMarketMaker, ClaimKind.Refund
        ];
        for (uint256 k; k < 5; ++k) {
            _claim(keccak256(abi.encode("bondv2.kind", k)), kinds[k], 0.01e8, account);
        }
        vm.warp(block.timestamp + LIVENESS);
        for (uint256 k; k < 5; ++k) {
            vm.prank(account);
            assertEq(bond.slashLiveness(keccak256(abi.encode("bondv2.kind", k))), _due(0.01e8, PRICE));
        }
        assertEq(registry.markBondPaidCalls(), 5);
        assertEq(usdc.balanceOf(account), 5 * _due(0.01e8, PRICE));
        _assertAccounting();
    }

    function test_slashLiveness_neverPaysAFeeOrARetiredClaim() public {
        _claim(CLAIM, ClaimKind.Fee, 0.01e8, liquidator);
        bytes32 retired = keccak256("bondv2.retired");
        _claim(retired, ClaimKind.Retired, 0.01e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, retired));
        bond.slashLiveness(retired);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
        (dueAt, amount) = bond.livenessDue(retired);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
    }

    function test_slashLiveness_onlyPendingClaims() public {
        ClaimStatus[4] memory statuses =
            [ClaimStatus.None, ClaimStatus.Paid, ClaimStatus.WrittenOff, ClaimStatus.Compensated];
        _claim(CLAIM, ClaimKind.Liquidation, 0.01e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        for (uint256 s; s < 4; ++s) {
            ClaimDetails memory c = registry.claimDetails(CLAIM);
            c.status = statuses[s];
            registry.setClaim(CLAIM, c);
            vm.prank(liquidator);
            vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
            bond.slashLiveness(CLAIM);
            (uint64 dueAt,) = bond.livenessDue(CLAIM);
            assertEq(dueAt, 0);
        }
        // An unknown claim reads as all zeros (status None).
        bytes32 unknown = keccak256("bondv2.unknown");
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, unknown));
        bond.slashLiveness(unknown);
    }

    /// I26: a claim is paid from the bonds at most once, also once three signers gave it a Reimburse destination.
    function test_slashLiveness_paysAClaimOnce() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        bond.slashLiveness(CLAIM);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);

        ClaimDetails memory c = registry.claimDetails(CLAIM);
        assertEq(c.commitment, bytes32(0), "the Registry zeroed it");
        c.commitment = keccak256("bondv2.reimburse");
        registry.setClaim(CLAIM, c);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
        assertEq(usdc.balanceOf(liquidator), 126e6, "paid once");
    }

    /// A Refund claim whose account has not given its destination yet has no commitment.
    function test_slashLiveness_needsADestination() public {
        _claim(CLAIM, ClaimKind.Refund, 0.01e8, account);
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.commitment = bytes32(0);
        registry.setClaim(CLAIM, c);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
    }

    /// DECISIONS D47: only the claimant can turn its claim into USDC.
    function test_slashLiveness_onlyTheClaimant() public {
        _claim(CLAIM, ClaimKind.CloseMarketMaker, 0.01e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotTheClaimant.selector, CLAIM, stranger));
        bond.slashLiveness(CLAIM);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, block.timestamp, "anyone can read when it is due");

        // A claim without a claimant (never the case for these kinds) is payable by no one.
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.claimant = address(0);
        registry.setClaim(CLAIM, c);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotTheClaimant.selector, CLAIM, liquidator));
        bond.slashLiveness(CLAIM);
        (dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
    }

    /// DECISIONS D46 and D47: the claims of a frozen vault are the compensator's.
    function test_slashLiveness_notWhileTheVaultIsFrozen() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.01e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        slasher.setVaultFrozen(VAULT, true);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
        assertEq(amount, 0);

        slasher.setVaultFrozen(VAULT, false); // the case was dismissed
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), _due(0.01e8, PRICE));
    }

    function test_slashLiveness_dueAtTheDeadline() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        uint64 deadline = uint64(block.timestamp) + LIVENESS;
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, deadline);
        assertEq(amount, 126e6);
        vm.warp(deadline - 1);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, CLAIM, deadline));
        bond.slashLiveness(CLAIM);
        vm.warp(deadline);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    /// DECISIONS D47: a payout committed less than `commitGrace` ago may still be mined; its claims wait.
    function test_slashLiveness_anInFlightPayoutWaitsCommitGrace() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        uint64 deadline = uint64(block.timestamp) + LIVENESS;
        registry.setClaimInput(CLAIM, INPUT);
        registry.setLastCommitAt(INPUT, uint40(deadline - 1 hours)); // committed an hour before the deadline
        uint64 protectedUntil = deadline - 1 hours + GRACE;
        vm.warp(deadline);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, CLAIM, protectedUntil));
        bond.slashLiveness(CLAIM);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, protectedUntil);
        vm.warp(protectedUntil - 1);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, CLAIM, protectedUntil));
        bond.slashLiveness(CLAIM);
        vm.warp(protectedUntil);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    function test_slashLiveness_anOldCommitmentDoesNotDelay() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        registry.setClaimInput(CLAIM, INPUT);
        registry.setLastCommitAt(INPUT, uint40(block.timestamp)); // its grace ends long before the deadline
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    function test_slashLiveness_anUnboundClaimIgnoresCommitments() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        registry.setLastCommitAt(INPUT, type(uint40).max); // the claim is bound to no input
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    /// The design review's case: three colluders re-commit the claim's input every `commitGrace - 1` seconds, forever.
    /// Each commitment protects an in-flight payout for `commitGrace`, but none made after the deadline extends that:
    /// the claim is due `livenessPeriod + commitGrace` after it became payable, at the latest.
    function test_slashLiveness_reCommittingForeverDefersItByCommitGraceAtMost() public {
        _claim(CLAIM, ClaimKind.Release, 0.05e8, account);
        registry.setClaimInput(CLAIM, INPUT);
        uint64 createdAt = uint64(block.timestamp);
        uint64 deadline = createdAt + LIVENESS;
        uint64 latest = deadline + GRACE;
        uint256 commits;
        for (uint64 t = createdAt; t < latest; t += GRACE - 1) {
            vm.warp(t);
            registry.setLastCommitAt(INPUT, uint40(t)); // a fresh commitment, every time
            uint64 expected = t >= deadline ? latest : (t + GRACE > deadline ? t + GRACE : deadline);
            (uint64 dueAt,) = bond.livenessDue(CLAIM);
            assertEq(dueAt, expected);
            assertLe(dueAt, latest);
            vm.prank(account);
            vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, CLAIM, dueAt));
            bond.slashLiveness(CLAIM);
            ++commits;
        }
        assertGt(commits, 10);
        // They commit once more at the very second the claim falls due: it is paid all the same.
        vm.warp(latest);
        registry.setLastCommitAt(INPUT, uint40(latest));
        (uint64 finalDueAt,) = bond.livenessDue(CLAIM);
        assertEq(finalDueAt, latest);
        vm.prank(account);
        assertEq(bond.slashLiveness(CLAIM), _due(0.05e8, PRICE));
    }

    function test_slashLiveness_refusesToPayNothing() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.01e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        oracle.setPrice(0);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, block.timestamp);
        assertEq(amount, 0);
        vm.prank(liquidator);
        vm.expectRevert(ISignerBond.ZeroAmount.selector);
        bond.slashLiveness(CLAIM);

        // A claim worth less than one USDC unit: 1 zatoshi at $0.09 per ZEC.
        oracle.setPrice(9e32);
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.amount = 1;
        registry.setClaim(CLAIM, c);
        vm.prank(liquidator);
        vm.expectRevert(ISignerBond.ZeroAmount.selector);
        bond.slashLiveness(CLAIM);
        assertEq(registry.markBondPaidCalls(), 0);
    }

    function test_slashLiveness_waitsUntilTheBondsCoverIt() public {
        _claim(CLAIM, ClaimKind.Liquidation, 2e8, liquidator); // 2 ZEC: 2,520 USDC with the premium
        vm.warp(block.timestamp + LIVENESS);
        uint256 due = _due(2e8, PRICE);
        assertEq(due, 2_520e6);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.InsufficientBond.selector, due, 5 * BOND));
        bond.slashLiveness(CLAIM);
        // The claimant can wait: once the bonds cover it, it is paid, from every last unit.
        _deposit(0, 520e6);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), due);
        assertEq(bond.totalSlashable(), 0);
        _assertAccounting();
    }

    function test_slashLiveness_takesFromTheBondFirstThenFromTheUnbonding() public {
        _unbond(1, 300e6); // signer 1: a bond of 100 and an unbonding of 300; 400 slashable like every other
        (, uint64 readyAt) = bond.unbondingOf(1);
        oracle.setPrice(1_000e34); // 1 ZEC at $1,000 with the premium: 1,050 USDC, 210 from each signer
        _claim(CLAIM, ClaimKind.Liquidation, 1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 1_050e6);
        assertEq(bond.bondOf(1), 0);
        (uint256 unbonding, uint64 stillReadyAt) = bond.unbondingOf(1);
        assertEq(unbonding, 300e6 - 110e6);
        assertEq(stillReadyAt, readyAt);
        for (uint8 i; i < 5; ++i) {
            if (i != 1) assertEq(bond.bondOf(i), BOND - 210e6);
        }
        assertFalse(bond.fullyBonded(), "signer 1 must top up before minting resumes");
        _assertAccounting();
    }

    function test_slashLiveness_canTakeEverySlashableUnit() public {
        for (uint8 i; i < 5; ++i) {
            _deposit(i, 20e6); // 420 each, 2,100 in all
        }
        _unbond(1, 300e6);
        oracle.setPrice(1_000e34);
        _claim(CLAIM, ClaimKind.Liquidation, 2e8, liquidator); // 2,100 USDC with the premium
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 2_100e6);
        assertEq(bond.totalSlashable(), 0);
        (uint256 unbonding, uint64 readyAt) = bond.unbondingOf(1);
        assertEq(unbonding, 0);
        assertEq(readyAt, 0, "an emptied unbonding is cleared");
        _assertAccounting();
    }

    /// Exits never stop: a halt does not stop liveness payments.
    function test_slashLiveness_worksAfterAHalt() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        _slash(CASE, 0x01);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
        assertEq(bond.bondOf(0), 0);
        for (uint8 i = 1; i < 5; ++i) {
            assertEq(bond.bondOf(i), BOND - 31.5e6, "126 from the four bonds left");
        }
        _assertAccounting();
    }

    function test_slashLiveness_marksTheClaimBeforePaying_andCannotBeReentered() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        registry.armProbe(address(bond), abi.encodeCall(ISignerBond.slashLiveness, (CLAIM)));
        vm.prank(liquidator);
        bond.slashLiveness(CLAIM);
        assertEq(registry.slashableAtMark(), 5 * BOND - 126e6, "the bonds were reduced before the Registry call");
        assertEq(registry.claimantBalanceAtMark(), 0, "the claimant is paid after the claim is marked");
        _assertReentryRefused(registry);
        assertEq(usdc.balanceOf(liquidator), 126e6);
    }

    /// The payment is the claim's value at the oracle's price plus the premium, rounded down once, or a revert.
    function testFuzz_slashLiveness_paysTheValuePlusThePremium(uint64 zat, uint256 price) public {
        zat = uint64(bound(zat, 1, 10e8)); // up to 10 ZEC
        price = bound(price, 0, 10_000e34); // up to $10,000 per ZEC
        oracle.setPrice(price);
        _claim(CLAIM, ClaimKind.Liquidation, zat, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        uint256 due = _due(zat, price);
        (uint64 dueAt, uint256 quoted) = bond.livenessDue(CLAIM);
        assertEq(dueAt, block.timestamp);
        assertEq(quoted, due);
        vm.prank(liquidator);
        if (due == 0) {
            vm.expectRevert(ISignerBond.ZeroAmount.selector);
            bond.slashLiveness(CLAIM);
        } else if (due > 5 * BOND) {
            vm.expectRevert(abi.encodeWithSelector(ISignerBond.InsufficientBond.selector, due, 5 * BOND));
            bond.slashLiveness(CLAIM);
        } else {
            assertEq(bond.slashLiveness(CLAIM), due);
            assertEq(usdc.balanceOf(liquidator), due);
            assertEq(bond.totalSlashable(), 5 * BOND - due);
        }
        _assertAccounting();
    }

    /// The pro-rata deduction (I26: the total taken is exactly the amount): every signer pays its share of the amount,
    /// rounded down, plus at most four units of the rounding; nobody pays more than it has; the bond goes first.
    function testFuzz_takeProRata_isExactAndFair(uint256[5] memory bonds, uint256[5] memory unbondings, uint256 amount)
        public
    {
        SignerBondV2Harness h = new SignerBondV2Harness(_config());
        _use(h, IERC20(address(usdc)));
        uint256[5] memory own;
        for (uint8 i; i < 5; ++i) {
            // A bond is zero (a full exit, or never bonded) or at least minBond; up to 10 million USDC each.
            bonds[i] = bonds[i] % 4 == 0 ? 0 : bound(bonds[i], MIN_BOND, 1e13);
            unbondings[i] = bound(unbondings[i], 0, 1e13);
            own[i] = bonds[i] + unbondings[i];
            if (own[i] != 0) _deposit(i, own[i]);
            if (unbondings[i] != 0) _unbond(i, unbondings[i]);
        }
        uint256 total = h.totalSlashable();
        vm.assume(total != 0);
        amount = bound(amount, 1, total);
        h.takeProRata(amount);
        uint256 taken;
        for (uint8 i; i < 5; ++i) {
            uint256 b = h.bondOf(i);
            (uint256 u,) = h.unbondingOf(i);
            uint256 t = own[i] - b - u; // underflows if anyone gained
            taken += t;
            uint256 share = amount * own[i] / total;
            assertGe(t, share, "at least its share, rounded down");
            assertLe(t, share + 4, "at most four units of rounding above it");
            if (t <= bonds[i]) {
                assertEq(b, bonds[i] - t);
                assertEq(u, unbondings[i]);
            } else {
                assertEq(b, 0, "the bond goes first");
                assertEq(u, unbondings[i] - (t - bonds[i]));
            }
        }
        assertEq(taken, amount, "exactly the amount");
        assertEq(h.totalSlashable(), total - amount);
    }

    // ================================================================== R-5: liveness receipts

    /// R-5 (the review's finding): v3 paid any claim with a destination once overdue, so a claimant who never handed
    /// the signers its salt and receiver (nobody could pay it) took 105 % of the claim from the bonds, when it chose.
    /// Without a receipt there is no liveness: not due, not payable, however long it waits.
    function test_R5_aClaimWithoutAReceiptIsNeverPaidFromTheBonds() public {
        _claimUnarmed(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS + GRACE + 365 days);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0, "not due");
        assertEq(amount, 0);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        (bytes32 commitment, uint64 payableAt) = bond.livenessArming(CLAIM);
        assertEq(commitment, bytes32(0));
        assertEq(payableAt, 0);
    }

    /// R-5: anyone relays any signer's receipt; the claim is then payable from the receipt, at the earliest from its
    /// creation, and due `livenessPeriod` later.
    function test_R5_aSignersReceiptArmsTheClaim() public {
        _claimUnarmed(CLAIM, ClaimKind.Release, 0.1e8, account);
        uint64 createdAt = uint64(block.timestamp);
        vm.warp(createdAt + 3 hours);
        uint64 receivedAt = createdAt + 2 hours; // the node got the destination two hours after the claim
        bytes memory receipt = _receipt(bond, CLAIM, receivedAt, 3);
        vm.expectEmit(address(bond));
        emit ISignerBond.LivenessArmed(CLAIM, COMMITMENT, receivedAt);
        vm.prank(stranger); // anyone relays
        bond.armLiveness(CLAIM, receivedAt, receipt);
        (bytes32 commitment, uint64 payableAt) = bond.livenessArming(CLAIM);
        assertEq(commitment, COMMITMENT);
        assertEq(payableAt, receivedAt);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, receivedAt + LIVENESS);
        vm.warp(dueAt - 1);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, CLAIM, dueAt));
        bond.slashLiveness(CLAIM);
        vm.warp(dueAt);
        vm.prank(account);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    /// R-5: one honest signer suffices: every signer's receipt arms the claim.
    function test_R5_everySignersReceiptCounts() public {
        for (uint256 i; i < 5; ++i) {
            bytes32 id = keccak256(abi.encode("bondv2.receipt", i));
            _claimUnarmed(id, ClaimKind.CloseUser, 0.01e8, account);
            _arm(bond, id, uint64(block.timestamp), i);
            (, uint64 payableAt) = bond.livenessArming(id);
            assertEq(payableAt, block.timestamp);
        }
    }

    /// R-5: a receipt nobody in the signer set made, one a signer made for another destination or claim, or a malformed
    /// signature arms nothing.
    function test_R5_aFakeOrNonSignerReceiptIsRefused() public {
        _claimUnarmed(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        uint64 at = uint64(block.timestamp);
        bytes32 digest = bond.hashDestinationReceived(CLAIM, COMMITMENT, at);
        (address outsider, uint256 outsiderKey) = makeAddrAndKey("bondv2.outsider");
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(outsiderKey, digest);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, outsider));
        bond.armLiveness(CLAIM, at, abi.encodePacked(r, s, v));

        // A signer's receipt for another destination, another claim, or another time recovers to someone else.
        bytes32[3] memory others = [
            bond.hashDestinationReceived(CLAIM, keccak256("bondv2.otherDest"), at),
            bond.hashDestinationReceived(keccak256("bondv2.otherClaim"), COMMITMENT, at),
            bond.hashDestinationReceived(CLAIM, COMMITMENT, at - 1)
        ];
        for (uint256 i; i < 3; ++i) {
            (v, r, s) = vm.sign(signerKeys[0], others[i]);
            bytes memory sig = abi.encodePacked(r, s, v);
            address recovered = ECDSA.recover(digest, sig);
            vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, recovered));
            bond.armLiveness(CLAIM, at, sig);
        }
        // Not a signature at all.
        vm.expectRevert(abi.encodeWithSelector(ECDSA.ECDSAInvalidSignatureLength.selector, 3));
        bond.armLiveness(CLAIM, at, hex"010203");
        (, uint64 payableAt) = bond.livenessArming(CLAIM);
        assertEq(payableAt, 0, "nothing armed");
    }

    /// R-5: a receipt dated after the block it is used in is refused.
    function test_R5_aReceiptFromTheFutureIsRefused() public {
        _claimUnarmed(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        uint64 future = uint64(block.timestamp + 1);
        bytes memory receipt = _receipt(bond, CLAIM, future, 0);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.ReceiptFromTheFuture.selector, future));
        bond.armLiveness(CLAIM, future, receipt);
        vm.warp(future);
        bond.armLiveness(CLAIM, future, receipt); // valid from that second on
    }

    /// R-5: a receipt dated before the claim existed (the node held the destination already) counts from the claim's
    /// creation.
    function test_R5_aReceiptBeforeTheClaimCountsFromItsCreation() public {
        vm.warp(block.timestamp + 10 days);
        _claimUnarmed(CLAIM, ClaimKind.CloseMarketMaker, 0.1e8, liquidator);
        uint64 createdAt = uint64(block.timestamp);
        _arm(bond, CLAIM, createdAt - 5 days, 2);
        (, uint64 payableAt) = bond.livenessArming(CLAIM);
        assertEq(payableAt, createdAt);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, createdAt + LIVENESS);
    }

    /// R-5: arming again for the same destination keeps the earliest payable time: a later receipt (a colluder's) never
    /// restarts the clock, an earlier one (another node's) advances it.
    function test_R5_armingAgainKeepsTheEarliestReceipt() public {
        _claimUnarmed(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        uint64 t0 = uint64(block.timestamp);
        vm.warp(t0 + 6 hours);
        _arm(bond, CLAIM, t0 + 2 hours, 0);
        _arm(bond, CLAIM, t0 + 5 hours, 1);
        (, uint64 payableAt) = bond.livenessArming(CLAIM);
        assertEq(payableAt, t0 + 2 hours, "the later receipt does not restart it");
        _arm(bond, CLAIM, t0 + 1 hours, 4);
        (, payableAt) = bond.livenessArming(CLAIM);
        assertEq(payableAt, t0 + 1 hours, "an earlier receipt advances it");
    }

    /// R-5: a new destination voids the arming (a Refund claim's account may change it): the old receipt neither counts
    /// nor arms the new one; a receipt for the new destination does, from the destination's own time.
    function test_R5_aNewDestinationVoidsTheArming() public {
        _claim(CLAIM, ClaimKind.Refund, 0.01e8, account);
        bytes memory oldReceipt = _receipt(bond, CLAIM, uint64(block.timestamp), 0);
        vm.warp(block.timestamp + 12 hours);
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.commitment = keccak256("bondv2.newDest");
        c.createdAt = uint40(block.timestamp); // setRefundDestination restarts the clock
        registry.setClaim(CLAIM, c);
        vm.warp(block.timestamp + LIVENESS);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0, "armed for the old destination only");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        vm.expectPartialRevert(ISignerBond.NotASigner.selector);
        bond.armLiveness(CLAIM, uint64(c.createdAt) - 12 hours, oldReceipt);

        _arm(bond, CLAIM, uint64(c.createdAt), 1);
        (dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, uint64(c.createdAt) + LIVENESS);
        vm.prank(account);
        assertEq(bond.slashLiveness(CLAIM), _due(0.01e8, PRICE));
    }

    /// R-5: the same destination given again restarts a refund's clock too: the arming counts from the new creation.
    function test_R5_theSameDestinationGivenAgainRestartsTheClock() public {
        _claim(CLAIM, ClaimKind.Refund, 0.01e8, account);
        vm.warp(block.timestamp + 20 hours);
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.createdAt = uint40(block.timestamp); // same commitment, new createdAt
        registry.setClaim(CLAIM, c);
        (uint64 dueAt,) = bond.livenessDue(CLAIM);
        assertEq(dueAt, block.timestamp + LIVENESS);
    }

    /// R-5: only a claim the bonds could pay can be armed.
    function test_R5_onlyAPayableKindOfClaimIsArmed() public {
        _claimUnarmed(CLAIM, ClaimKind.Fee, 0.01e8, address(0));
        bytes memory receipt = _receipt(bond, CLAIM, uint64(block.timestamp), 0);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.armLiveness(CLAIM, uint64(block.timestamp), receipt);
        _claimUnarmed(CLAIM, ClaimKind.Refund, 0.01e8, account);
        ClaimDetails memory c = registry.claimDetails(CLAIM);
        c.commitment = bytes32(0); // no destination yet
        registry.setClaim(CLAIM, c);
        receipt = _receipt(bond, CLAIM, uint64(block.timestamp), 0);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.armLiveness(CLAIM, uint64(block.timestamp), receipt);
    }

    /// R-5: the receipt's EIP-712 digest, recomputed from the type string and the domain ("ZecuredSignerBond", "1").
    function test_R5_receiptDigest() public view {
        bytes32 structHash = vm.eip712HashStruct(
            "DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)",
            abi.encode(CLAIM, COMMITMENT, uint64(1_234))
        );
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("ZecuredSignerBond"),
                keccak256("1"),
                block.chainid,
                address(bond)
            )
        );
        assertEq(bond.domainSeparator(), domain);
        assertEq(
            bond.hashDestinationReceived(CLAIM, COMMITMENT, 1_234), MessageHashUtils.toTypedDataHash(domain, structHash)
        );
    }

    // ================================================================== R-7: the compensator's claims

    /// R-7 (the review's finding): an executed F3 left the claim pending, so its claimant also took liveness from the
    /// bonds and was paid about twice. A claim the compensator owes is not paid from the bonds.
    function test_R7_aClaimWithACompensatorEntitlementIsNotPaidFromTheBonds() public {
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        compensator.setEntitlement(
            CLAIM, Entitlement({incident: 1, layer: 2, zat: 0.04e8, to: liquidator, paid: false})
        );
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
        // Paid or not, an entitlement keeps it out.
        compensator.setEntitlement(CLAIM, Entitlement({incident: 1, layer: 2, zat: 0, to: liquidator, paid: true}));
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, CLAIM));
        bond.slashLiveness(CLAIM);
    }

    // ================================================================== R-9: never at a stale price

    /// R-9 (the review's finding): the bonds paid at the oracle's last price however old it was. While the feed is
    /// stale (no source fresh) no liveness is paid; it is paid once a source is fresh again.
    function test_R9_noLivenessWhileThePriceIsStale() public {
        assertTrue(bond.priceStateChecked(), "the oracle reports a state");
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        oracle.setState(IZecuredPriceFeed.FeedState.Stale);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(CLAIM);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
        vm.prank(liquidator);
        vm.expectRevert(ISignerBond.PriceStale.selector);
        bond.slashLiveness(CLAIM);
        // Any fresh state pays.
        oracle.setState(IZecuredPriceFeed.FeedState.Diverged);
        vm.prank(liquidator);
        assertEq(bond.slashLiveness(CLAIM), 126e6);
    }

    /// R-9 with the mainnet feed itself: a ZecuredPriceFeed over two sources reports its state; once neither source is
    /// fresh no liveness is paid, and once one is fresh again it is paid at the feed's price.
    function test_R9_withTheZecuredPriceFeed() public {
        MockAggregator primary = new MockAggregator(8);
        MockAggregator secondary = new MockAggregator(8);
        primary.set(1_200e8, block.timestamp);
        secondary.set(1_200e8, block.timestamp);
        ZecuredPriceFeed feed = new ZecuredPriceFeed(address(primary), 25 hours, address(secondary), 25 hours, 500);
        BondConfig memory cfg = _config();
        cfg.oracle = IOracle(address(feed));
        SignerBondV2 b = _replaceBond(cfg);
        assertTrue(b.priceStateChecked(), "the feed reports a state");
        for (uint8 i; i < 5; ++i) {
            _deposit(i, BOND);
        }
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + 25 hours + 1); // due, and both sources one second past their max age
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        vm.prank(liquidator);
        vm.expectRevert(ISignerBond.PriceStale.selector);
        b.slashLiveness(CLAIM);
        secondary.set(1_000e8, block.timestamp); // one source fresh again, at $1,000
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        vm.prank(liquidator);
        assertEq(b.slashLiveness(CLAIM), _due(0.1e8, 1_000e34));
    }

    /// R-9: an oracle without a feed state (the testnets' demo oracles) is never seen as stale.
    function test_R9_anOracleWithoutAStateIsNotChecked() public {
        BondConfig memory cfg = _config();
        cfg.oracle = IOracle(address(new MockBondPlainOracle(PRICE)));
        SignerBondV2 b = _replaceBond(cfg);
        assertFalse(b.priceStateChecked());
        for (uint8 i; i < 5; ++i) {
            _deposit(i, BOND);
        }
        _claim(CLAIM, ClaimKind.Liquidation, 0.1e8, liquidator);
        vm.warp(block.timestamp + LIVENESS);
        vm.prank(liquidator);
        assertEq(b.slashLiveness(CLAIM), 126e6);
    }

    // ================================================================== R-18: the unbonding outlasts a dispute

    /// R-18 (the review's finding): nothing on chain kept `disputeWindow + inclusionWindow <= unbondingPeriod`, so a
    /// signer could start a partial exit just after a confirmation and be gone when the disputed claim reopened.
    function test_R18_theUnbondingOutlastsTheDisputeAndInclusionWindows() public {
        BondConfig memory cfg = _config();
        registry.setDisputeWindows(UNBONDING - 6 hours, 6 hours);
        new SignerBondV2(cfg); // exactly the unbonding: accepted
        registry.setDisputeWindows(UNBONDING - 6 hours, 6 hours + 1);
        vm.expectRevert(ISignerBond.InvalidConfig.selector);
        new SignerBondV2(cfg);
    }

    // ================================================================== reentrancy through the token

    /// With a token that calls its recipient back (MOCK; USDC has no such hook), a signer, a claimant or a bounty
    /// beneficiary cannot re-enter the bond while it pays them.
    function test_tokenCallbacksCannotReenterAPayment() public {
        MockHookToken token = new MockHookToken();
        MockReentrantAccount evil = new MockReentrantAccount();
        address[5] memory set = signers;
        set[0] = address(evil); // signer 0 is a contract
        registry.setSigners(set);
        BondConfig memory cfg = _config();
        cfg.usdc = IERC20(address(token));
        SignerBondV2 b = _replaceBond(cfg);
        for (uint8 i = 1; i < 5; ++i) {
            token.mint(signers[i], BOND);
            vm.startPrank(signers[i]);
            token.approve(address(b), BOND);
            b.deposit(BOND);
            vm.stopPrank();
        }
        token.mint(address(evil), BOND);
        evil.exec(address(token), abi.encodeCall(IERC20.approve, (address(b), BOND)));
        evil.exec(address(b), abi.encodeCall(ISignerBond.deposit, (BOND)));
        evil.exec(address(b), abi.encodeCall(ISignerBond.requestUnbond, (BOND - MIN_BOND)));
        vm.warp(block.timestamp + UNBONDING);
        token.setHooked(address(evil), true);

        // 1. Its unbonding.
        evil.armProbe(address(b), abi.encodeCall(ISignerBond.withdrawUnbonded, (address(evil))));
        evil.exec(address(b), abi.encodeCall(ISignerBond.withdrawUnbonded, (address(evil))));
        _assertReentryRefused(evil);
        assertEq(token.balanceOf(address(evil)), BOND - MIN_BOND, "paid once");

        // 2. A liveness payment it claims.
        registry.setClaim(
            CLAIM,
            ClaimDetails({
                vaultId: VAULT,
                kind: ClaimKind.CloseMarketMaker,
                status: ClaimStatus.Pending,
                amount: 0.1e8,
                commitment: COMMITMENT,
                createdAt: uint40(block.timestamp),
                bondPaid: false,
                claimant: address(evil)
            })
        );
        _arm(b, CLAIM, uint64(block.timestamp), 1);
        vm.warp(block.timestamp + LIVENESS);
        evil.armProbe(address(b), abi.encodeCall(ISignerBond.slashLiveness, (CLAIM)));
        evil.exec(address(b), abi.encodeCall(ISignerBond.slashLiveness, (CLAIM)));
        _assertReentryRefused(evil);
        assertEq(token.balanceOf(address(evil)), BOND - MIN_BOND + 126e6, "paid once");

        // 3. A bounty it is owed.
        vm.prank(address(slasher));
        b.slash(CASE, 0x02, address(evil));
        uint256 bounty = b.bountyOf(address(evil));
        assertGt(bounty, 0);
        evil.armProbe(address(b), abi.encodeCall(ISignerBond.claimBounty, ()));
        evil.exec(address(b), abi.encodeCall(ISignerBond.claimBounty, ()));
        _assertReentryRefused(evil);
        assertEq(token.balanceOf(address(evil)), BOND - MIN_BOND + 126e6 + bounty, "paid once");
        assertEq(token.balanceOf(address(b)), b.totalSlashable(), "I19");
    }
}
