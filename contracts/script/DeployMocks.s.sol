// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../src/mocks/MockZecOracle.sol";
import {DryRunPlaceholderCollateral} from "./DryRunPlaceholderCollateral.sol";

/// @title DeployMocks
/// @notice Deploys MockUSDC + MockZecOracle (both MOCK) and creates the zZEC/USDC market on Morpho Blue
///         (Base Sepolia): oracle = MockZecOracle, irm = AdaptiveCurveIRM, lltv = 62.5%. Prints the market id.
/// @dev Dry run (simulation only, nothing is sent):
///        forge script script/DeployMocks.s.sol --rpc-url https://sepolia.base.org
///      Real run (later: needs the deployed zZEC address and a funded deployer key):
///        COLLATERAL_TOKEN=<zZEC> forge script script/DeployMocks.s.sol --rpc-url base_sepolia \
///          --broadcast --account <foundry-keystore-name>
///      Env: COLLATERAL_TOKEN (zZEC address; required for --broadcast), ZEC_USD_PRICE_CENTS (default 4000 = $40).
///      The broadcaster becomes the owner of both mocks.
contract DeployMocks is Script {
    using MarketParamsLib for MarketParams;

    IMorpho internal constant MORPHO = IMorpho(0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb);
    address internal constant ADAPTIVE_CURVE_IRM = 0x46415998764C29aB2a25CbeA6254146D50D22687;
    uint256 internal constant LLTV = 0.625e18;
    uint256 internal constant BASE_SEPOLIA_CHAIN_ID = 84532;

    function run() external returns (MockUSDC usdc, MockZecOracle oracle, MarketParams memory marketParams, Id id) {
        require(block.chainid == BASE_SEPOLIA_CHAIN_ID, "DeployMocks: not Base Sepolia (84532)");
        require(MORPHO.isIrmEnabled(ADAPTIVE_CURVE_IRM), "DeployMocks: IRM not enabled");
        require(MORPHO.isLltvEnabled(LLTV), "DeployMocks: LLTV not enabled");

        uint256 priceCents = vm.envOr("ZEC_USD_PRICE_CENTS", uint256(4000));
        address collateral = vm.envOr("COLLATERAL_TOKEN", address(0));
        bool dryRun = vm.isContext(VmSafe.ForgeContext.ScriptDryRun);
        require(collateral != address(0) || dryRun, "DeployMocks: set COLLATERAL_TOKEN (deployed zZEC) to broadcast");

        vm.startBroadcast();
        (, address deployer,) = vm.readCallers();
        if (collateral == address(0)) collateral = address(new DryRunPlaceholderCollateral());
        usdc = new MockUSDC(deployer);
        oracle = new MockZecOracle(deployer, priceCents * 1e32); // cents * 1e32 = USD * 1e34
        marketParams = MarketParams({
            loanToken: address(usdc),
            collateralToken: collateral,
            oracle: address(oracle),
            irm: ADAPTIVE_CURVE_IRM,
            lltv: LLTV
        });
        MORPHO.createMarket(marketParams);
        vm.stopBroadcast();

        id = marketParams.id();
        console.log("dry run (nothing broadcast):", dryRun);
        console.log("deployer / mock owner:", deployer);
        console.log("MockUSDC (MOCK):", address(usdc));
        console.log("MockZecOracle (MOCK):", address(oracle));
        console.log("collateral token:", collateral);
        console.log("oracle price (USD * 1e34):", oracle.price());
        console.log("Morpho market id:");
        console.logBytes32(Id.unwrap(id));
    }
}
