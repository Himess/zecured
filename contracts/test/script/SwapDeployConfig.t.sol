// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {DeploySwap} from "../../script/DeploySwap.s.sol";
import {ZecuredSwap} from "../../src/ZecuredSwap.sol";
import {SwapConfig} from "../../src/interfaces/IZecuredSwap.sol";

/// Exposes the swap deploy script's config reader and checks.
contract DeploySwapHarness is DeploySwap {
    function readAndCheck(string memory name) external view returns (SwapNetworkConfig memory c) {
        c = _readConfigAt(name, string.concat("config/swap-", name, ".json"));
        _checkConfig(c);
    }
}

/// The version registry as far as ZecuredSwap's constructor reads it (its router). MOCK, tests only.
contract RouterOnlyVersions {
    address public immutable router;

    constructor(address router_) {
        router = router_;
    }
}

/// Every swap config the deploy script reads passes its checks and the constructor's rules; the judge config has the
/// judge values of docs/SWAP.md "Parameters" and the Intake's judge work tiers and K3, the mainnet beta config the
/// beta column and the mainnet template's tiers. No fork, no RPC.
contract SwapDeployConfigTest is Test {
    DeploySwapHarness internal script;

    function setUp() public {
        script = new DeploySwapHarness();
    }

    function _load(string memory name) internal returns (DeploySwap.SwapNetworkConfig memory c) {
        c = script.readAndCheck(name);
        assertEq(c.name, name);
        // The constructor's own rules (ascending tiers, never less work for more value, the order cap within them, the
        // pay windows, the fee): a mistake here is caught before anything is broadcast.
        address router = makeAddr("router");
        new ZecuredSwap(
            SwapConfig({
                usdc: c.usdc == address(0) ? makeAddr("usdc") : c.usdc,
                verifier: router,
                versions: address(new RouterOnlyVersions(router)),
                relay: makeAddr("relay"),
                workTiers: c.workTiers,
                finalizeDepth: c.finalizeDepth,
                finalizeDelay: c.finalizeDelay,
                maxOrderUsdc: c.maxOrderUsdc,
                maxOrderZat: c.maxOrderZat,
                minPayWindow: c.minPayWindow,
                maxPayWindow: c.maxPayWindow,
                feeBps: c.feeBps,
                feeRecipient: c.feeRecipient
            })
        );
    }

    /// The swap's tiers and K3 are the lending deployment's Intake's on the same network (docs/SWAP.md "Parameters").
    function _assertIntakeTiers(DeploySwap.SwapNetworkConfig memory c, string memory lending) internal view {
        string memory json = vm.readFile(string.concat("config/zecured-", lending, ".json"));
        uint256[] memory maxAmounts = vm.parseJsonUintArray(json, ".tierMaxAmountZat");
        uint256[] memory minWorks = vm.parseJsonUintArray(json, ".tierMinWork");
        assertEq(c.workTiers.length, maxAmounts.length, "tiers");
        for (uint256 i; i < maxAmounts.length; ++i) {
            assertEq(c.workTiers[i].maxAmountZat, maxAmounts[i], "tier amount");
            assertEq(c.workTiers[i].minWork, minWorks[i], "tier work");
        }
        assertEq(c.finalizeDepth, vm.parseJsonUint(json, ".finalizeDepth"), "finalizeDepth");
        assertEq(c.finalizeDelay, vm.parseJsonUint(json, ".finalizeDelay"), "finalizeDelay");
        uint256[] memory branches = vm.parseJsonUintArray(json, ".branchIds");
        for (uint256 i; i < branches.length; ++i) {
            assertEq(c.branchIds[i], branches[i], "branch ids");
        }
        assertEq(c.zcashNetwork, vm.parseJsonString(json, ".zcashNetwork"));
    }

    function test_judgeConfig() public {
        DeploySwap.SwapNetworkConfig memory c = _load("base-sepolia");
        assertEq(c.chainId, 84_532);
        assertEq(c.zcashNetwork, "zcash-testnet");
        assertFalse(c.localFork);
        assertEq(c.usdc, 0x036CbD53842c5426634e7929541eC2318f3dCF7e, "Circle's test USDC, the lending market's");
        assertEq(c.riscZeroRouter, 0x0b144E07A0826182B6b59788c34b32Bfa86Fb711);
        assertEq(c.branchIds[0], 0x77190AD9, "NU7");
        assertEq(c.branchIds[1], 0, "the Testnet payment guest proves NU7 only");
        _assertIntakeTiers(c, "base-sepolia");
        assertEq(c.finalizeDelay, 150, "150 s");
        assertEq(c.finalizeDepth, 6, "6 headers");
        assertEq(c.maxOrderUsdc, 200e6, "200 USDC");
        assertEq(c.maxPayWindow, 6 hours, "the judge pay window");
        assertEq(c.feeBps, 0, "no fee on the judge deployment");
    }

    function test_anvilConfig() public {
        DeploySwap.SwapNetworkConfig memory c = _load("anvil");
        assertTrue(c.localFork);
        assertEq(c.usdc, address(0), "the lending deployment's MockUSDC comes from SWAP_USDC");
        _assertIntakeTiers(c, "anvil");
        assertEq(c.feeBps, 0);
    }

    function test_mainnetBetaConfig() public {
        DeploySwap.SwapNetworkConfig memory c = _load("base-mainnet-beta");
        assertEq(c.chainId, 8453);
        assertEq(c.zcashNetwork, "zcash-mainnet");
        assertFalse(c.localFork);
        assertEq(c.usdc, 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, "Circle USDC on Base");
        assertEq(c.branchIds[0], 0x37A5165B, "NU6.3");
        assertEq(c.branchIds[1], 0x77190AD9, "NU7");
        _assertIntakeTiers(c, "base-mainnet");
        assertEq(c.finalizeDelay, 1_800, "1,800 s");
        assertEq(c.maxOrderUsdc, 250e6, "250 USDC");
        assertEq(c.maxPayWindow, 24 hours, "the beta pay window");
        assertEq(c.feeBps, 0, "no fee in the beta");
    }
}
