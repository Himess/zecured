// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {IZecuredPriceFeed} from "../src/interfaces/v3/IZecuredPriceFeed.sol";
import {IAggregatorV3} from "../src/interfaces/IAggregatorV3.sol";
import {ZecuredPriceFeed} from "../src/ZecuredPriceFeed.sol";

/// @title DeployPriceFeed
/// @notice Deploys ZecuredPriceFeed (design notes T-10) from a config file, by default config/price-feed-base.json
///         (Base mainnet). Before deploying it checks that the RPC is on the config's chain, that each source has code,
///         the decimals the config states and a positive answer, and that the primary is fresh; it warns when the
///         secondary is not. Afterwards it reads the feed back and writes deployments/price-feed-<network>.json, or
///         deployments/price-feed-<network>.dry-run.json in a simulation.
/// @dev Environment:
///        PRICE_FEED_CONFIG      optional: the config file (default config/price-feed-base.json)
///        DEPLOYER_PRIVATE_KEY   optional: the deployer key (never logged); without it the script runs as forge's
///                               default sender, which is only meaningful for a simulation
///      Simulation (nothing is sent): forge script script/DeployPriceFeed.s.sol --rpc-url <Base mainnet RPC>
///      Deployment: the same with --broadcast and DEPLOYER_PRIVATE_KEY.
///      The Intake that reads this feed (ZecuredIntake's capOracle) should use capPriceMaxAge = maxSourceAge(), which
///      the script prints and writes.
contract DeployPriceFeed is Script {
    string internal constant OUT_DIR = "deployments";

    /// The config file's values.
    struct Config {
        string network;
        uint256 chainId;
        address primary;
        uint64 primaryMaxAge;
        uint256 primaryDecimals;
        address secondary;
        uint64 secondaryMaxAge;
        uint256 secondaryDecimals;
        uint16 maxDeviationBps;
    }

    function run() external returns (ZecuredPriceFeed feed) {
        Config memory c = _readConfig();
        require(block.chainid == c.chainId, "DeployPriceFeed: the RPC's chain is not the config's");
        require(
            _checkSource(c.primary, c.primaryDecimals, c.primaryMaxAge, "primary"), "DeployPriceFeed: stale primary"
        );
        if (c.secondary != address(0)) {
            if (!_checkSource(c.secondary, c.secondaryDecimals, c.secondaryMaxAge, "secondary")) {
                console.log("WARNING: the secondary is stale now; the feed starts PrimaryOnly");
            }
        }
        bool dryRun =
            !vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) && !vm.isContext(VmSafe.ForgeContext.ScriptResume);

        uint256 deployerKey = vm.envOr("DEPLOYER_PRIVATE_KEY", uint256(0)); // never logged
        if (deployerKey == 0) vm.startBroadcast();
        else vm.startBroadcast(deployerKey);
        feed = new ZecuredPriceFeed(c.primary, c.primaryMaxAge, c.secondary, c.secondaryMaxAge, c.maxDeviationBps);
        vm.stopBroadcast();

        _check(c, feed);
        _log(feed, dryRun);
        _writeJson(c, feed, dryRun);
    }

    function _readConfig() internal view returns (Config memory c) {
        string memory path = vm.envOr("PRICE_FEED_CONFIG", string("config/price-feed-base.json"));
        string memory json = vm.readFile(path);
        c.network = vm.parseJsonString(json, ".network");
        c.chainId = vm.parseJsonUint(json, ".chainId");
        c.primary = vm.parseJsonAddress(json, ".primary.address");
        c.primaryMaxAge = uint64(vm.parseJsonUint(json, ".primary.maxAge"));
        c.primaryDecimals = vm.parseJsonUint(json, ".primary.decimals");
        c.secondary = vm.parseJsonAddress(json, ".secondary.address");
        c.secondaryMaxAge = uint64(vm.parseJsonUint(json, ".secondary.maxAge"));
        c.secondaryDecimals = vm.parseJsonUint(json, ".secondary.decimals");
        c.maxDeviationBps = uint16(vm.parseJsonUint(json, ".maxDeviationBps"));
    }

    /// @dev Code, decimals and a positive answer are required; returns whether the answer is fresh.
    function _checkSource(address source, uint256 decimals, uint64 maxAge, string memory role)
        internal
        view
        returns (bool fresh)
    {
        require(source.code.length > 0, string.concat("DeployPriceFeed: the ", role, " has no code"));
        require(
            IAggregatorV3(source).decimals() == decimals,
            string.concat("DeployPriceFeed: the ", role, "'s decimals differ from the config")
        );
        (, int256 answer,, uint256 updatedAt,) = IAggregatorV3(source).latestRoundData();
        require(answer > 0, string.concat("DeployPriceFeed: the ", role, " has no price"));
        fresh = updatedAt <= block.timestamp && block.timestamp - updatedAt <= maxAge;
        console.log(role, source);
        console.log("  answer", uint256(answer), "updatedAt", updatedAt);
    }

    /// @dev The deployed feed is configured as the file says and serves a fresh price.
    function _check(Config memory c, ZecuredPriceFeed feed) internal view {
        require(feed.primary() == c.primary && feed.secondary() == c.secondary, "DeployPriceFeed: sources differ");
        require(feed.primaryDecimals() == c.primaryDecimals, "DeployPriceFeed: primary decimals differ");
        require(feed.state() != IZecuredPriceFeed.FeedState.Stale, "DeployPriceFeed: the new feed is stale");
        require(feed.price() != 0, "DeployPriceFeed: no price");
    }

    function _log(ZecuredPriceFeed feed, bool dryRun) internal view {
        console.log(dryRun ? "DRY RUN (simulation, nothing broadcast)" : "BROADCAST");
        console.log("ZecuredPriceFeed:", address(feed));
        console.log("state (0 BothFresh, 1 Diverged, 2 PrimaryOnly, 3 SecondaryOnly, 4 Stale):", uint256(feed.state()));
        console.log("price() (USD per ZEC * 1e34):", feed.price());
        (uint256 high, uint256 highAt) = feed.priceHigh();
        console.log("priceHigh():", high, "updatedAt", highAt);
        console.log("the Intake's capPriceMaxAge for this feed (maxSourceAge):", feed.maxSourceAge());
    }

    function _writeJson(Config memory c, ZecuredPriceFeed feed, bool dryRun) internal {
        string memory root = "priceFeed";
        vm.serializeString(root, "network", c.network);
        vm.serializeUint(root, "chainId", block.chainid);
        vm.serializeBool(root, "dryRun", dryRun);
        vm.serializeUint(root, "block", block.number);
        vm.serializeUint(root, "timestamp", block.timestamp);
        vm.serializeAddress(root, "ZecuredPriceFeed", address(feed));
        vm.serializeAddress(root, "primary", c.primary);
        vm.serializeUint(root, "primaryMaxAge", c.primaryMaxAge);
        vm.serializeUint(root, "primaryDecimals", feed.primaryDecimals());
        vm.serializeAddress(root, "secondary", c.secondary);
        vm.serializeUint(root, "secondaryMaxAge", c.secondaryMaxAge);
        vm.serializeUint(root, "secondaryDecimals", feed.secondaryDecimals());
        vm.serializeUint(root, "maxDeviationBps", c.maxDeviationBps);
        vm.serializeUint(root, "capPriceMaxAge", feed.maxSourceAge());
        vm.serializeUint(root, "state", uint256(feed.state()));
        string memory json = vm.serializeString(root, "price", vm.toString(feed.price())); // decimal string: > 2^53
        vm.createDir(OUT_DIR, true);
        string memory path = string.concat(OUT_DIR, "/price-feed-", c.network, dryRun ? ".dry-run.json" : ".json");
        vm.writeJson(json, path);
        console.log("written:", path);
    }
}
