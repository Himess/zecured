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

    function check(NetworkConfig memory c) external pure {
        _checkConfig(c);
    }

    function payoutLifetime(NetworkConfig memory c) external pure returns (uint256) {
        return _payoutLifetime(c);
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
        // R-18: the Intake waits in time too, and a pending activation's account for its delay; tiers never ask less
        // work for a larger amount.
        assertGt(c.finalizeDelay, 0, "finalizeDelay");
        assertGt(c.pendingRefundDelay, 0, "pendingRefundDelay");
        for (uint256 i = 1; i < c.workTiers.length; ++i) {
            assertGe(c.workTiers[i].minWork, c.workTiers[i - 1].minWork, "tiers never descend in work");
        }
        // R-18, SignerBondV2: a claim disputed and reopened after a partial exit started is due while it is slashable.
        assertLe(uint256(c.disputeWindow) + c.inclusionWindow, c.unbondingPeriod, "dispute + inclusion <= unbonding");
        // R-11, ZecuredSpendLogic: dormancy before abandonment.
        assertGt(c.refundDormancy, 0, "refundDormancy");
        assertLe(c.refundDormancy, c.refundAbandonPeriod, "refundDormancy <= refundAbandonPeriod");
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
        _assertTestnetJournal(c);
        // R-11: the bonds set the capacity under a 5 TAZ ceiling; an unclaimed refund leaves the cap after 48 hours.
        assertEq(c.maxTotalZat, 5e8, "5 TAZ");
        assertEq(c.refundDormancy, 48 hours);
        c = _load("base-sepolia-theft-demo");
        assertTrue(c.theftDemo);
        _assertTestnetJournal(c);
        c = _load("anvil");
        assertTrue(c.localFork);
        _assertTestnetJournal(c);
    }

    /// The guests commit journal v2 only (statement v2), and the Testnet build proves the NU7 epoch only.
    function _assertTestnetJournal(DeployZecured.NetworkConfig memory c) internal pure {
        assertTrue(c.journalV2, "journal v2");
        assertEq(c.branchIds[0], 0x77190AD9, "NU7");
        assertEq(c.branchIds[1], 0, "the Testnet build proves no other epoch");
    }

    /// The mainnet template: docs/MAINNET.md's parameter table.
    function test_mainnetTemplate() public view {
        DeployZecured.NetworkConfig memory c = _load("base-mainnet");
        assertEq(c.chainId, 8453);
        assertEq(c.zcashNetwork, "zcash-mainnet");
        assertFalse(c.localFork);
        assertFalse(c.theftDemo);
        assertEq(c.loanToken, 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, "Circle USDC on Base");
        assertEq(c.pricePrimary, 0x69e5BC4988a9AF30Ec827C5609c0D41028446ec0, "Chainlink ZEC/USD, Standard proxy");
        assertEq(c.capPriceMaxAge, 90_000, "the price feed's maxSourceAge");
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
        assertEq(c.refundDormancy, 7 days, "R-11");
        assertGe(c.commitGrace, 20_400, "R-10: a payout's lifetime on Zcash Mainnet");
    }

    /// The mainnet beta (invite-only, small caps): the template's windows, tiers and price feed, with its own caps,
    /// bonds and bounty.
    function test_mainnetBetaConfig() public view {
        DeployZecured.NetworkConfig memory c = _load("base-mainnet-beta");
        DeployZecured.NetworkConfig memory t = script.readAndCheck("base-mainnet");
        assertEq(c.chainId, 8453);
        assertEq(c.zcashNetwork, "zcash-mainnet");
        assertFalse(c.localFork);
        assertFalse(c.theftDemo);
        assertEq(c.maxTotalZat, 1e8, "1 ZEC");
        assertEq(c.minBond, 300e6, "300 USDC");
        assertEq(c.coverageBps, 5_000);
        assertEq(c.feeBps, 50);
        assertEq(c.bountyCap, 100e6, "100 USDC");
        assertEq(c.refundDormancy, 7 days);
        // As the template.
        assertEq(c.loanToken, t.loanToken);
        assertEq(c.pricePrimary, t.pricePrimary);
        assertEq(c.capPriceMaxAge, t.capPriceMaxAge);
        assertEq(c.challengeWindow, t.challengeWindow);
        assertEq(c.livenessPeriod, t.livenessPeriod);
        assertEq(c.commitGrace, t.commitGrace);
        assertEq(c.unbondingPeriod, t.unbondingPeriod);
        assertEq(c.disputeWindow, t.disputeWindow);
        assertEq(c.inclusionWindow, t.inclusionWindow);
        assertEq(c.finalizeDelay, t.finalizeDelay);
        assertEq(c.finalizeDepth, t.finalizeDepth);
        assertEq(c.pendingRefundDelay, t.pendingRefundDelay);
        assertEq(c.refundAbandonPeriod, t.refundAbandonPeriod);
        assertEq(c.versionDepositDelay, t.versionDepositDelay);
        assertEq(c.versionViolationDelay, t.versionViolationDelay);
        assertEq(c.versionEstopDelay, t.versionEstopDelay);
        assertEq(c.slashConfirmationWork, t.slashConfirmationWork);
        assertEq(c.workTiers.length, t.workTiers.length);
        for (uint256 i; i < c.workTiers.length; ++i) {
            assertEq(c.workTiers[i].maxAmountZat, t.workTiers[i].maxAmountZat);
            assertEq(c.workTiers[i].minWork, t.workTiers[i].minWork);
        }
        assertEq(c.branchIds[0], t.branchIds[0]);
        assertEq(c.branchIds[1], t.branchIds[1]);
        assertTrue(c.autoPauseOnCase);
    }

    // ------------------------------------------------------------------ R-10, R-18: what the script refuses

    /// R-10 (the review's finding): the theft demo and the local fork had a 120 s `commitGrace`, far shorter than the
    /// time a committed payout may still be mined, so the bonds could pay a claim whose payout then landed too. A
    /// payout's lifetime is twice the target time of its 136 blocks (target height, 120-block expiry, 5 blocks of tip
    /// skew, 10 of reorg margin): 6,800 s on Zcash Testnet, 20,400 s on Zcash Mainnet until NU7.
    function test_R10_commitGraceCoversAPayoutsLifetime() public {
        DeployZecured.NetworkConfig memory c = script.readAndCheck("base-sepolia");
        assertEq(script.payoutLifetime(c), 6_800);
        DeployZecured.NetworkConfig memory m = script.readAndCheck("base-mainnet");
        assertEq(script.payoutLifetime(m), 20_400);
        c.commitGrace = 6_799;
        vm.expectRevert(bytes("DeployZecured: commitGrace is shorter than a payout's lifetime (R-10)"));
        script.check(c);
        c.commitGrace = 120; // the v3 theft demo's
        vm.expectRevert(bytes("DeployZecured: commitGrace is shorter than a payout's lifetime (R-10)"));
        script.check(c);
        c.commitGrace = 6_800;
        script.check(c);
        c.zcashNetwork = "zcash-regtest";
        vm.expectRevert(bytes("DeployZecured: zcashNetwork is zcash-testnet or zcash-mainnet"));
        script.check(c);
    }

    /// R-18 (the review's finding): nothing checked these before; now the script refuses each of them, as the
    /// constructors do.
    function test_R18_theScriptRefusesUnsafeWindowsAndTiers() public {
        DeployZecured.NetworkConfig memory c = script.readAndCheck("base-mainnet");
        DeployZecured.NetworkConfig memory bad = script.readAndCheck("base-mainnet");
        bad.finalizeDelay = 0;
        vm.expectRevert(bytes("DeployZecured: finalizeDelay is not zero (R-18)"));
        script.check(bad);
        bad = script.readAndCheck("base-mainnet");
        bad.pendingRefundDelay = 0;
        vm.expectRevert(bytes("DeployZecured: pendingRefundDelay is not zero (R-18)"));
        script.check(bad);
        bad = script.readAndCheck("base-mainnet");
        bad.workTiers[1].minWork = bad.workTiers[0].minWork - 1;
        vm.expectRevert(bytes("DeployZecured: a work tier below the floor"));
        script.check(bad); // below the floor already
        bad = script.readAndCheck("base-mainnet");
        bad.workTiers[2].minWork = bad.workTiers[1].minWork - 1;
        vm.expectRevert(bytes("DeployZecured: work tiers ascend in amount and never descend in work (R-18)"));
        script.check(bad);
        bad = script.readAndCheck("base-mainnet");
        bad.unbondingPeriod = bad.disputeWindow + bad.inclusionWindow - 1;
        vm.expectRevert(bytes("DeployZecured: the dispute and inclusion windows outlast the unbonding (R-18)"));
        script.check(bad);
        bad = script.readAndCheck("base-mainnet");
        bad.refundDormancy = 0;
        vm.expectRevert(bytes("DeployZecured: 0 < refundDormancy <= refundAbandonPeriod (R-11)"));
        script.check(bad);
        bad.refundDormancy = bad.refundAbandonPeriod + 1;
        vm.expectRevert(bytes("DeployZecured: 0 < refundDormancy <= refundAbandonPeriod (R-11)"));
        script.check(bad);
        script.check(c);
    }
}
