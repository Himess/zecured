// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test, console} from "forge-std/Test.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {IZecuredPriceFeed} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";
import {IAggregatorV3} from "../../src/interfaces/IAggregatorV3.sol";
import {ZecuredPriceFeed} from "../../src/ZecuredPriceFeed.sol";

/// @dev What the test reads of API3's reader proxy besides the AggregatorV3 functions.
interface IApi3ReaderProxyV1 {
    function dapiName() external view returns (bytes32);
    function version() external view returns (uint256);
}

/// @notice ZecuredPriceFeed on the real ZEC/USD feeds of Base mainnet, the ones config/price-feed-base.json names:
///         Chainlink's (primary) and API3's (secondary). Read-only: a fork at the latest block, or at
///         ZECURED_BASE_FORK_BLOCK. Skipped, with a log line, unless BASE_MAINNET_RPC_URL is set.
contract PriceFeedBaseForkTest is Test {
    string internal constant CONFIG = "config/price-feed-base.json";
    uint256 internal constant BASE_CHAIN_ID = 8453;
    /// The same Chainlink DualAggregator behind its SVR-Backup proxy, and the MorphoChainlinkOracleV2 of Morpho's
    /// cbZEC/USDC market (62.5 % LLTV) that reads it with a scale factor of 1e16 (docs/ref/FACTS.md).
    address internal constant CHAINLINK_SVR_PROXY = 0xC4a161651546983083AeDDDEc5878A2eAf604890;
    IOracle internal constant MORPHO_CBZEC_USDC_ORACLE = IOracle(0xa3CB2d7204F47a468B82C6fAb1CA9675e9F7A272);

    bool internal forked;
    address internal primary;
    address internal secondary;
    uint64 internal primaryMaxAge;
    uint64 internal secondaryMaxAge;
    uint16 internal maxDeviationBps;
    uint256 internal primaryDecimals;
    uint256 internal secondaryDecimals;

    function setUp() public {
        string memory url = vm.envOr("BASE_MAINNET_RPC_URL", string(""));
        if (bytes(url).length == 0) return;
        uint256 forkBlock = vm.envOr("ZECURED_BASE_FORK_BLOCK", uint256(0));
        if (forkBlock == 0) vm.createSelectFork(url);
        else vm.createSelectFork(url, forkBlock);
        forked = true;

        string memory json = vm.readFile(CONFIG);
        assertEq(vm.parseJsonUint(json, ".chainId"), BASE_CHAIN_ID, "config is not Base mainnet");
        primary = vm.parseJsonAddress(json, ".primary.address");
        secondary = vm.parseJsonAddress(json, ".secondary.address");
        primaryMaxAge = uint64(vm.parseJsonUint(json, ".primary.maxAge"));
        secondaryMaxAge = uint64(vm.parseJsonUint(json, ".secondary.maxAge"));
        maxDeviationBps = uint16(vm.parseJsonUint(json, ".maxDeviationBps"));
        primaryDecimals = vm.parseJsonUint(json, ".primary.decimals");
        secondaryDecimals = vm.parseJsonUint(json, ".secondary.decimals");
    }

    modifier onBase() {
        if (!forked) {
            console.log("PriceFeedBaseFork: skipped, BASE_MAINNET_RPC_URL is not set");
            vm.skip(true, "BASE_MAINNET_RPC_URL is not set");
        }
        _;
    }

    /// @notice The configured sources are what the config says they are, and live.
    function test_theConfiguredSourcesAreLive() public onBase {
        assertEq(block.chainid, BASE_CHAIN_ID, "fork is not Base mainnet");
        console.log("Base block", block.number, "timestamp", block.timestamp);

        assertEq(IAggregatorV3(primary).decimals(), primaryDecimals, "primary decimals");
        assertEq(IAggregatorV3(primary).description(), "ZEC / USD", "primary description");
        (, int256 pAnswer,, uint256 pAt,) = IAggregatorV3(primary).latestRoundData();
        assertGt(pAnswer, 0, "primary answer");
        assertLe(block.timestamp - pAt, primaryMaxAge, "primary older than its max age");

        assertEq(IAggregatorV3(secondary).decimals(), secondaryDecimals, "secondary decimals");
        assertEq(IApi3ReaderProxyV1(secondary).version(), 4913, "secondary is not an Api3ReaderProxyV1");
        assertEq(IApi3ReaderProxyV1(secondary).dapiName(), bytes32("ZEC/USD"), "secondary dAPI");
        (uint80 sRound, int256 sAnswer,, uint256 sAt,) = IAggregatorV3(secondary).latestRoundData();
        assertEq(sRound, 0, "API3's adapter has no rounds");
        assertGt(sAnswer, 0, "secondary answer");
        assertLe(block.timestamp - sAt, secondaryMaxAge, "secondary older than its max age");

        console.log("Chainlink ZEC/USD (18 dec):", uint256(pAnswer), "updated", pAt);
        console.log("API3 ZEC/USD (18 dec):     ", uint256(sAnswer), "updated", sAt);
    }

    /// @notice On the live sources the feed is fresh and scales like Morpho's own Chainlink oracle.
    function test_theFeedOnTheLiveSources() public onBase {
        ZecuredPriceFeed feed = _deploy(secondary, secondaryMaxAge);
        IZecuredPriceFeed.FeedState s = feed.state();
        console.log("state (0 BothFresh, 1 Diverged, 2 PrimaryOnly, 3 SecondaryOnly, 4 Stale):", uint256(s));
        assertTrue(
            s == IZecuredPriceFeed.FeedState.BothFresh || s == IZecuredPriceFeed.FeedState.Diverged,
            "both live sources are fresh"
        );
        (uint256 pPrice, uint256 sPrice) = _checkScaling(feed);

        uint256 price = feed.price();
        (uint256 high, uint256 highAt) = feed.priceHigh();
        console.log("price()     USD/ZEC * 1e34:", price);
        console.log("priceHigh() USD/ZEC * 1e34:", high, "updatedAt", highAt);
        // USD 1 = 1e6 USDC units: one ZEC (1e8 zatoshi) at the feed's price, in USDC units.
        console.log("1 ZEC in USDC units at price():", price * 1e8 / 1e36);
        assertLe(price, high, "price() <= priceHigh()");
        assertLe(highAt, block.timestamp, "priceHigh() is not from the future");
        assertLe(block.timestamp - highAt, feed.maxSourceAge(), "priceHigh() is fresh");
        uint256 higher = pPrice > sPrice ? pPrice : sPrice;
        assertEq(high, higher, "the cap reads the higher fresh price");
        if (s == IZecuredPriceFeed.FeedState.BothFresh) {
            assertEq(price, pPrice, "agreeing sources: the primary's price");
            _compareWithMorpho(pPrice);
        } else {
            assertEq(price, pPrice < sPrice ? pPrice : sPrice, "diverged sources: the lower");
        }
    }

    /// @notice A source read costs well under `SOURCE_GAS` from cold storage.
    function test_aSourceReadIsCheap() public onBase {
        uint256 g = gasleft();
        IAggregatorV3(primary).latestRoundData();
        uint256 primaryGas = g - gasleft();
        g = gasleft();
        IAggregatorV3(secondary).latestRoundData();
        uint256 secondaryGas = g - gasleft();
        ZecuredPriceFeed feed = _deploy(secondary, secondaryMaxAge);
        g = gasleft();
        feed.price();
        uint256 priceGas = g - gasleft();
        console.log("gas, cold: Chainlink read", primaryGas, "API3 read", secondaryGas);
        console.log("gas, warm sources: price()", priceGas);
        assertLt(primaryGas * 3, feed.SOURCE_GAS(), "SOURCE_GAS leaves at least 3x headroom (Chainlink)");
        assertLt(secondaryGas * 3, feed.SOURCE_GAS(), "SOURCE_GAS leaves at least 3x headroom (API3)");
    }

    /// @notice Past the max age with no new report, the feed is Stale: Morpho keeps the last good price and the cap
    ///         sees a stale price.
    function test_staleAfterTheMaxAge() public onBase {
        ZecuredPriceFeed feed = _deploy(secondary, secondaryMaxAge);
        uint256 before = feed.price();
        (uint256 p, uint256 pAt,, uint256 q, uint256 sAt,) = feed.readSources();
        uint256 newest = pAt > sAt ? pAt : sAt;
        vm.warp(newest + feed.maxSourceAge() + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale), "stale");
        assertEq(feed.price(), before, "Morpho keeps the price the feed was deployed with (no newer report)");
        (uint256 high, uint256 highAt) = feed.priceHigh();
        assertEq(high, before);
        assertLe(highAt, block.timestamp - feed.maxSourceAge() - 1, "dated at least maxSourceAge + 1 s ago");
        // The Intake's check with capPriceMaxAge = maxSourceAge(): stale.
        assertGt(block.timestamp, highAt + feed.maxSourceAge(), "the coverage cap sees it stale");
        // The answers are still valid, only old: the event carries both prices.
        vm.expectEmit(address(feed));
        emit IZecuredPriceFeed.FeedStateChanged(IZecuredPriceFeed.FeedState.Stale, p, q);
        feed.poke();
        assertEq(feed.price(), before, "a poke while stale keeps the last good price");
    }

    /// @notice Single-source mode on Chainlink alone.
    function test_singleSourceOnChainlink() public onBase {
        ZecuredPriceFeed feed = _deploy(address(0), 0);
        (, int256 pAnswer,,,) = IAggregatorV3(primary).latestRoundData();
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        assertEq(feed.price(), uint256(pAnswer) * 1e16);
        assertEq(feed.secondaryDecimals(), 0);
        assertEq(feed.maxSourceAge(), primaryMaxAge);
    }

    // ------------------------------------------------------------------ helpers

    function _deploy(address secondary_, uint64 secondaryMaxAge_) internal returns (ZecuredPriceFeed feed) {
        feed = new ZecuredPriceFeed(primary, primaryMaxAge, secondary_, secondaryMaxAge_, maxDeviationBps);
        assertEq(feed.primaryDecimals(), primaryDecimals);
    }

    /// @dev Both sources' answers, scaled by 10^(34 - 18), as the feed reads them.
    function _checkScaling(ZecuredPriceFeed feed) internal view returns (uint256 pPrice, uint256 sPrice) {
        (, int256 pAnswer,, uint256 pAt,) = IAggregatorV3(primary).latestRoundData();
        (, int256 sAnswer,, uint256 sAt,) = IAggregatorV3(secondary).latestRoundData();
        pPrice = uint256(pAnswer) * 1e16;
        sPrice = uint256(sAnswer) * 1e16;
        (uint256 p, uint256 pReadAt, bool pFresh, uint256 q, uint256 qReadAt, bool qFresh) = feed.readSources();
        assertEq(p, pPrice, "primary scaled by 10^(34 - 18)");
        assertEq(q, sPrice, "secondary scaled by 10^(34 - 18)");
        assertEq(pReadAt, pAt);
        assertEq(qReadAt, sAt);
        assertTrue(pFresh && qFresh, "both live sources are fresh");
    }

    /// @dev Morpho's MorphoChainlinkOracleV2 of the cbZEC/USDC market reads the same aggregator through the SVR-Backup
    ///      proxy with a scale factor of 1e16: when that proxy shows the primary's report, both oracles agree exactly.
    function _compareWithMorpho(uint256 pPrice) internal view {
        (, int256 pAnswer,, uint256 pAt,) = IAggregatorV3(primary).latestRoundData();
        (, int256 svrAnswer,, uint256 svrAt,) = IAggregatorV3(CHAINLINK_SVR_PROXY).latestRoundData();
        if (svrAt == pAt && svrAnswer == pAnswer) {
            assertEq(MORPHO_CBZEC_USDC_ORACLE.price(), pPrice, "Morpho's oracle scales the same answer the same way");
            console.log("Morpho's cbZEC/USDC oracle returns the same price");
        } else {
            console.log("the SVR-Backup proxy shows another report: the comparison with Morpho's oracle is skipped");
        }
    }
}
