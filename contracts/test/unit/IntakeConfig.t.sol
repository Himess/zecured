// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {ZecuredIntake} from "../../src/ZecuredIntake.sol";
import {IZecuredIntake, IntakeConfig, WorkTier} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";

/// @notice MOCK (tests only): the three Registry views the Intake's constructor reads; it names whoever asks as its
///         Intake, so that the constructor's own rules decide.
contract MockIntakeRegistry {
    function intake() external view returns (address) {
        return msg.sender;
    }

    function DUST_LIMIT() external pure returns (uint64) {
        return 10_000;
    }

    function signers() external pure returns (address[5] memory s) {
        (s[0], s[1], s[2], s[3], s[4]) = (address(0x51), address(0x52), address(0x53), address(0x54), address(0x55));
    }
}

/// @notice The Intake's constructor rules, R-18 above all, with a Registry stub that names the Intake (no fork: the
///         constructor reads nothing but the Registry).
contract IntakeConfigTest is Test {
    MockIntakeRegistry internal stub;

    function setUp() public {
        stub = new MockIntakeRegistry();
    }

    function _good() internal view returns (IntakeConfig memory c) {
        c.registry = address(stub);
        c.verifier = IRiscZeroVerifier(address(0x1001));
        c.versions = address(0x1002);
        c.relay = IZcashHeaderRelay(address(0x1003));
        c.workTiers = new WorkTier[](2);
        c.workTiers[0] = WorkTier({maxAmountZat: 1e8, minWork: 1_000_000});
        c.workTiers[1] = WorkTier({maxAmountZat: 1e9, minWork: 3_000_000});
        c.finalizeDepth = 6;
        c.finalizeDelay = 150;
        c.maxPause = 30 days;
        c.pendingRefundDelay = 1 days;
        c.minRefundZat = 20_000;
    }

    function _deploy(IntakeConfig memory c) internal returns (ZecuredIntake) {
        return new ZecuredIntake(c);
    }

    function _expectInvalid(IntakeConfig memory c) internal {
        vm.expectRevert(IZecuredIntake.InvalidConfig.selector);
        new ZecuredIntake(c);
    }

    function test_aGoodConfigDeploys() public {
        ZecuredIntake intake = _deploy(_good());
        assertEq(intake.finalizeDelay(), 150);
        assertEq(intake.pendingRefundDelay(), 1 days);
        assertEq(intake.workTiers().length, 2);
    }

    /// R-18 (the review's finding): a zero `finalizeDelay` let a proof finalize on height alone (which an attacker
    /// feeding a branch controls), a zero `pendingRefundDelay` made the account's way out immediate, and nothing kept a
    /// larger amount from asking less work than a smaller one. v3's constructor accepted all three.
    function test_R18_refusesUnsafeWindowsAndTiers() public {
        IntakeConfig memory c = _good();
        c.finalizeDelay = 0;
        _expectInvalid(c);
        c = _good();
        c.pendingRefundDelay = 0;
        _expectInvalid(c);
        c = _good();
        c.workTiers[1].minWork = c.workTiers[0].minWork - 1;
        _expectInvalid(c);
        // Equal work for a larger amount is allowed (never descending), as are the other rules.
        c = _good();
        c.workTiers[1].minWork = c.workTiers[0].minWork;
        _deploy(c);
        c = _good();
        c.workTiers[1].maxAmountZat = c.workTiers[0].maxAmountZat; // amounts still ascend strictly
        _expectInvalid(c);
        c = _good();
        c.finalizeDepth = 0;
        _expectInvalid(c);
    }
}
