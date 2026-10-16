// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {DeployZecured} from "../../script/DeployZecured.s.sol";

/// Exposes the deploy script's config reader and checks.
contract DeployZecuredHarness is DeployZecured {
    function readAndCheck(string memory name) external view returns (NetworkConfig memory c) {
        c = _readConfigAt(name, string.concat("config/zecured-", name, ".json"));
        _checkConfig(c);
    }
}

/// Every network config the deploy script reads passes its checks and the constructors' own rules; the mainnet
/// template (config/zecured-base-mainnet.json) also has the values docs/MAINNET.md argues for. No fork, no RPC.
contract DeployConfigTest is Test {
    DeployZecuredHarness internal script;

    function setUp() public {
        script = new DeployZecuredHarness();
    }

    function _load(string memory name) internal view returns (DeployZecured.NetworkConfig memory c) {
        c = script.readAndCheck(name);
        assertEq(c.name, name);
        _constructorRules(c);
    }

    /// What the constructors would refuse: a mistake here is caught before anything is broadcast.
    function _constructorRules(DeployZecured.NetworkConfig memory c) internal pure {
        // SignerBondV2: a claim created before an exit falls due while the exiting amount is slashable.
        assertGe(c.unbondingPeriod, uint256(c.challengeWindow) + c.livenessPeriod + c.commitGrace, "unbonding");
        assertGt(c.minBond, 0, "minBond");
        assertLe(c.bountyBps, 10_000, "bountyBps");
        // ZecuredIntake: refunds above the Registry's dust limit (10,000), tiers ascending, at most four.
        assertGt(c.minRefundZat, 10_000, "minRefundZat");
        assertGt(c.workTiers.length, 0, "tiers");
        assertLe(c.workTiers.length, 4, "tiers");
        for (uint256 i = 1; i < c.workTiers.length; ++i) {
            assertGt(c.workTiers[i].maxAmountZat, c.workTiers[i - 1].maxAmountZat, "tiers ascend");
        }
        assertGt(c.maxPause, 0, "maxPause");
        assertGt(c.finalizeDepth, 0, "finalizeDepth");
        // ZecuredRegistry: a fee bears its own 5,000-zatoshi output and stays above dust.
        if (c.feeBps != 0) assertGt(c.minFeeZat, 10_000, "minFeeZat");
        // ZecuredCompensator: at most a year.
        assertGt(c.incidentWindow, 0, "incidentWindow");
        assertLe(c.incidentWindow, 365 days, "incidentWindow");
        assertTrue(c.leftoverSink != address(0), "leftoverSink");
    }

    function test_judgeConfigs() public view {
        DeployZecured.NetworkConfig memory c = _load("base-sepolia");
        assertEq(c.chainId, 84532);
        assertEq(c.zcashNetwork, "zcash-testnet");
        assertFalse(c.localFork);
        c = _load("base-sepolia-theft-demo");
        assertTrue(c.theftDemo);
        c = _load("anvil");
        assertTrue(c.localFork);
    }

    /// The mainnet template: docs/MAINNET.md's parameter table.
    function test_mainnetTemplate() public view {
        DeployZecured.NetworkConfig memory c = _load("base-mainnet");
        assertEq(c.chainId, 8453);
        assertEq(c.zcashNetwork, "zcash-mainnet");
        assertFalse(c.localFork);
        assertFalse(c.theftDemo);
        assertEq(c.loanToken, 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, "Circle USDC on Base");
        assertLe(c.coverageBps, 5_000, "k <= 50 % with independent operators");
        assertTrue(c.autoPauseOnCase, "an open F1/F2 case pauses minting");
        assertTrue(c.journalV2, "journal v2");
        assertEq(c.branchIds[0], 0x37A5165B, "NU6.3 (Ironwood)");
        assertEq(c.branchIds[1], 0x77190AD9, "NU7");
        assertGe(c.finalizeDelay, 30 minutes, "a feeder outage and a slow fake branch");
        assertGe(c.challengeWindow, 72 hours);
        assertGe(c.livenessPeriod, 72 hours);
        assertGe(c.versionDepositDelay, 14 days);
        assertGe(c.versionViolationDelay, 7 days);
        assertGe(c.versionEstopDelay, 1 days);
        assertLe(c.workTiers[c.workTiers.length - 1].maxAmountZat, c.maxTotalZat, "no deposit above the cap");
    }
}
