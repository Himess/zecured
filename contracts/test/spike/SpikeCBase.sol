// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {SpikeCollateral} from "./SpikeCollateral.sol";
import {SpikeRegistry} from "./SpikeRegistry.sol";
import {SpikeAdapter} from "./SpikeAdapter.sol";

/// @notice SPIKE-ONLY shared fixture: forks Base Sepolia and deploys MockUSDC, MockZecOracle and the spike
///         Registry/Collateral/Adapter against the REAL Morpho Blue deployment.
abstract contract SpikeCBase is Test {
    using MarketParamsLib for MarketParams;

    // Verified on-chain constants (docs/ref/FACTS.md).
    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    uint256 internal constant BASE_SEPOLIA_CHAIN_ID = 84532;

    string internal constant DEFAULT_RPC_URL = "https://sepolia.base.org";
    /// @dev Pinned for reproducibility and so Foundry caches fork state on disk between runs.
    ///      Override with SPIKE_FORK_BLOCK=<n>; SPIKE_FORK_BLOCK=0 forks the latest block.
    uint256 internal constant DEFAULT_FORK_BLOCK = 47_948_000;

    uint256 internal constant ZEC = 1e8; // zatoshi
    uint256 internal constant USDC = 1e6;
    uint256 internal constant PRICE_40_USD = 40 * 1e34; // Morpho scale: USD-per-ZEC * 1e34

    struct System {
        MockUSDC usdc;
        MockZecOracle oracle;
        SpikeRegistry registry;
        SpikeCollateral collateral;
        SpikeAdapter adapter;
        MarketParams marketParams;
        Id id;
    }

    function _selectBaseSepoliaFork() internal returns (uint256 forkBlock) {
        string memory rpcUrl = vm.envOr("BASE_SEPOLIA_RPC_URL", string(""));
        if (bytes(rpcUrl).length == 0) rpcUrl = DEFAULT_RPC_URL;
        forkBlock = vm.envOr("SPIKE_FORK_BLOCK", DEFAULT_FORK_BLOCK);
        if (forkBlock == 0) vm.createSelectFork(rpcUrl);
        else vm.createSelectFork(rpcUrl, forkBlock);
        assertEq(block.chainid, BASE_SEPOLIA_CHAIN_ID, "fork is not Base Sepolia");
        assertGt(address(MORPHO).code.length, 0, "Morpho Blue not deployed at the expected address");
    }

    /// @dev Deploys mocks + spike contracts and creates the market on the real Morpho Blue.
    function _deploySystem(bool guarded) internal returns (System memory s) {
        s.usdc = new MockUSDC(address(this));
        s.oracle = new MockZecOracle(address(this), PRICE_40_USD);
        s.registry = new SpikeRegistry(MORPHO, guarded);
        s.collateral = s.registry.collateral();
        s.marketParams = MarketParams({
            loanToken: address(s.usdc),
            collateralToken: address(s.collateral),
            oracle: address(s.oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV
        });
        MORPHO.createMarket(s.marketParams);
        s.id = s.marketParams.id();
        s.registry.setMarket(s.marketParams);
        s.adapter = new SpikeAdapter(MORPHO, s.registry);
        s.registry.setAdapter(address(s.adapter));

        vm.label(address(MORPHO), "MorphoBlue");
        vm.label(ADAPTIVE_CURVE_IRM, "AdaptiveCurveIRM");
        vm.label(address(s.usdc), guarded ? "MockUSDC" : "MockUSDC(unguarded system)");
        vm.label(address(s.oracle), guarded ? "MockZecOracle" : "MockZecOracle(unguarded system)");
        vm.label(address(s.registry), guarded ? "SpikeRegistry" : "SpikeRegistry(unguarded)");
        vm.label(address(s.collateral), guarded ? "SpikeCollateral(guarded)" : "SpikeCollateral(unguarded)");
        vm.label(address(s.adapter), guarded ? "SpikeAdapter" : "SpikeAdapter(unguarded)");
    }

    function _supplyAsLender(System memory s, address lender, uint256 amount) internal {
        s.usdc.mint(lender, amount);
        vm.startPrank(lender);
        s.usdc.approve(address(MORPHO), amount);
        MORPHO.supply(s.marketParams, amount, 0, lender, "");
        vm.stopPrank();
    }
}
