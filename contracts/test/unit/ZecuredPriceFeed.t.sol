// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IZecuredPriceFeed} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";
import {ZecuredPriceFeed} from "../../src/ZecuredPriceFeed.sol";
import {MockAggregator} from "../mocks/MockAggregator.sol";

/// @notice ZecuredPriceFeed (design notes T-10) on MOCK sources: the five states, what each returns, the last good price
///         (never poked, poked long ago, poked while diverged, sources gone), garbage and failing sources, the gas
///         rule, `poke()`'s events, the scaling, and fuzzed properties (never reverts, `price() <= priceHigh()` from
///         fresh sources, `priceHigh()` never claims more freshness than its sources had).
contract ZecuredPriceFeedTest is Test {
    uint256 internal constant T0 = 1_800_000_000;
    uint64 internal constant MAX_AGE = 25 hours;
    uint16 internal constant DEVIATION_BPS = 300;
    /// $1,265.00 with 8 decimals, and its Morpho price (USD per ZEC * 1e34).
    int256 internal constant A1265 = 1265e8;
    uint256 internal constant P1265 = 1265e34;

    MockAggregator internal primary;
    MockAggregator internal secondary;
    ZecuredPriceFeed internal feed;

    function setUp() public {
        vm.warp(T0);
        primary = new MockAggregator(8);
        secondary = new MockAggregator(8);
        primary.set(A1265, T0);
        secondary.set(A1265, T0);
        feed = new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, DEVIATION_BPS);
    }

    // ------------------------------------------------------------------ construction

    function test_constructionStoresTheConfigAndTheFirstRecord() public view {
        assertEq(feed.primary(), address(primary));
        assertEq(feed.secondary(), address(secondary));
        assertEq(feed.primaryMaxAge(), MAX_AGE);
        assertEq(feed.secondaryMaxAge(), MAX_AGE);
        assertEq(feed.maxDeviationBps(), DEVIATION_BPS);
        assertEq(feed.maxSourceAge(), MAX_AGE);
        assertEq(feed.primaryDecimals(), 8);
        assertEq(feed.secondaryDecimals(), 8);
        assertEq(uint256(feed.recordedState()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        (uint256 recordPrice, uint256 updatedAt, uint256 recordedAt, uint256 pSeen, uint256 sSeen) = feed.lastRecord();
        assertEq(recordPrice, P1265);
        assertEq(updatedAt, T0);
        assertEq(recordedAt, T0);
        assertEq(pSeen, T0);
        assertEq(sSeen, T0);
    }

    function test_constructionEmitsTheFirstState() public {
        address next = vm.computeCreateAddress(address(this), vm.getNonce(address(this)));
        vm.expectEmit(next);
        emit IZecuredPriceFeed.FeedStateChanged(IZecuredPriceFeed.FeedState.BothFresh, P1265, P1265);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, DEVIATION_BPS);
    }

    function test_decimalsAreReadOnce() public {
        primary.setDecimals(18, false);
        assertEq(feed.price(), P1265, "still scaled with the 8 decimals read at construction");
        primary.setDecimals(8, true);
        assertEq(feed.price(), P1265, "decimals() is never called again");
    }

    function test_revert_zeroPrimary() public {
        vm.expectRevert(IZecuredPriceFeed.ZeroAddress.selector);
        new ZecuredPriceFeed(address(0), MAX_AGE, address(secondary), MAX_AGE, DEVIATION_BPS);
    }

    function test_revert_invalidConfig() public {
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), 0, address(secondary), MAX_AGE, DEVIATION_BPS);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), 0, DEVIATION_BPS);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(0), MAX_AGE, DEVIATION_BPS);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(primary), MAX_AGE, DEVIATION_BPS);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, 10_001);
        // The bounds themselves are accepted.
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, 10_000);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(0), 0, 0);
    }

    function test_revert_decimalsAbove34OrGarbage() public {
        MockAggregator bad = new MockAggregator(35);
        bad.set(A1265, T0);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(bad), MAX_AGE, address(0), 0, 0);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(bad), MAX_AGE, DEVIATION_BPS);
        bad.setDecimals(256, false); // not a uint8
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(bad), MAX_AGE, address(0), 0, 0);
        bad.setDecimals(8, true); // decimals() reverts
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(bad), MAX_AGE, address(0), 0, 0);
        // A contract without the function (here: the test itself has no decimals()).
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(this), MAX_AGE, address(0), 0, 0);
    }

    function test_revert_noFreshSourceAtConstruction() public {
        vm.warp(T0 + MAX_AGE + 1);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, DEVIATION_BPS);
        primary.setMode(MockAggregator.Mode.Revert);
        vm.warp(T0);
        vm.expectRevert(IZecuredPriceFeed.InvalidConfig.selector);
        new ZecuredPriceFeed(address(primary), MAX_AGE, address(0), 0, 0);
        // One fresh source is enough.
        ZecuredPriceFeed f = new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, DEVIATION_BPS);
        assertEq(uint256(f.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
    }

    // ------------------------------------------------------------------ scaling

    /// @notice The same USD price in 6, 8 and 18 decimals gives the same Morpho price, and 1 ZEC is worth that many USDC.
    function test_scalingFor6And8And18Decimals() public {
        uint256 expected = 118_801e32; // $1,188.01 * 1e34
        assertEq(_singleSource(6, 1_188_010_000).price(), expected);
        assertEq(_singleSource(8, 118_801_000_000).price(), expected);
        assertEq(_singleSource(18, 1_188_010_000_000_000_000_000).price(), expected);
        // 1 ZEC = 1e8 zatoshi; Morpho values collateral as zatoshi * price / 1e36 USDC units.
        assertEq(uint256(1e8) * expected / 1e36, 1_188_010_000, "1 ZEC = 1,188.01 USDC");
    }

    function test_scalingAtTheDecimalBounds() public {
        assertEq(_singleSource(0, 1188).price(), 1188e34, "0 decimals: times 1e34");
        assertEq(_singleSource(34, 1188e34).price(), 1188e34, "34 decimals: as is");
    }

    function test_answersAboveMaxPriceAreGarbage() public {
        uint256 maxAnswer = feed.MAX_PRICE() / 1e26;
        primary.set(int256(maxAnswer), T0);
        secondary.set(int256(maxAnswer), T0);
        assertEq(feed.price(), feed.MAX_PRICE(), "MAX_PRICE itself is a price");
        primary.set(int256(maxAnswer + 1), T0);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        primary.set(type(int256).max, T0);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
    }

    // ------------------------------------------------------------------ the states

    function test_bothFreshAndClose() public {
        // The secondary 2 % higher and older: price() is the primary's, the cap reads the higher with its own date.
        secondary.set(A1265 * 102 / 100, T0 - 1 hours);
        primary.set(A1265, T0 - 2 hours);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        assertEq(feed.price(), P1265);
        _assertHigh(P1265 * 102 / 100, T0 - 1 hours);
        // The secondary 2 % lower: the primary is both.
        secondary.set(A1265 * 98 / 100, T0);
        assertEq(feed.price(), P1265);
        _assertHigh(P1265, T0 - 2 hours);
        (uint256 p, uint256 at) = feed.lastGoodPrice();
        assertEq(p, P1265);
        assertEq(at, T0 - 2 hours);
    }

    function test_deviationBoundIsInclusive() public {
        // 3 % above the lower agrees, one unit more diverges.
        secondary.set(A1265 * 103 / 100, T0);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        secondary.set(A1265 * 103 / 100 + 1, T0);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Diverged));
        // Measured from the lower price, whichever source it is.
        primary.set(A1265 * 103 / 100, T0);
        secondary.set(A1265, T0);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
    }

    function test_divergedWithTheSecondaryLower() public {
        secondary.set(1100e8, T0 - 3 hours);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Diverged));
        assertEq(feed.price(), 1100e34, "the lower");
        _assertHigh(P1265, T0);
        (uint256 p, uint256 at) = feed.lastGoodPrice();
        assertEq(p, 1100e34);
        assertEq(at, T0 - 3 hours);
    }

    function test_divergedWithTheSecondaryHigher() public {
        secondary.set(1400e8, T0 - 3 hours);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Diverged));
        assertEq(feed.price(), P1265, "the lower");
        _assertHigh(1400e34, T0 - 3 hours);
    }

    function test_primaryOnly() public {
        // The secondary stale, reverting, or absent.
        secondary.set(1400e8, T0 - MAX_AGE - 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        assertEq(feed.price(), P1265);
        _assertHigh(P1265, T0);
        secondary.setMode(MockAggregator.Mode.Revert);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        assertEq(feed.price(), P1265);
        ZecuredPriceFeed single = new ZecuredPriceFeed(address(primary), MAX_AGE, address(0), 0, DEVIATION_BPS);
        assertEq(uint256(single.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        assertEq(single.secondaryDecimals(), 0);
        (,,, uint256 q, uint256 qAt, bool qFresh) = single.readSources();
        assertEq(q, 0);
        assertEq(qAt, 0);
        assertFalse(qFresh);
    }

    function test_secondaryOnly() public {
        secondary.set(1300e8, T0 - 1 hours);
        primary.set(A1265, T0 - MAX_AGE - 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        assertEq(feed.price(), 1300e34);
        _assertHigh(1300e34, T0 - 1 hours);
        primary.setMode(MockAggregator.Mode.Revert);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        assertEq(feed.price(), 1300e34);
    }

    function test_freshnessBoundary() public {
        vm.warp(T0 + MAX_AGE);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh), "exactly maxAge old: fresh");
        vm.warp(T0 + MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale), "one second more: stale");
    }

    /// @notice Stale: Morpho keeps the last good price; the cap gets it dated back, so the Intake (capPriceMaxAge =
    ///         maxSourceAge) stops new minting, while it accepted every fresh state before.
    function test_staleKeepsTheLastGoodPriceAndTheCapSeesItStale() public {
        primary.set(1250e8, T0 + 1 hours);
        secondary.set(1260e8, T0 + 2 hours);
        vm.warp(T0 + 2 hours + MAX_AGE);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        assertFalse(_capStale(), "the Intake accepts a fresh state up to maxSourceAge old");
        vm.warp(T0 + 2 hours + MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        // The last good price: the secondary's answer, the newest since the record.
        assertEq(feed.price(), 1260e34);
        (uint256 high, uint256 highAt) = feed.priceHigh();
        assertEq(high, 1260e34);
        assertEq(highAt, T0 + 2 hours, "its own updatedAt, already older than maxSourceAge + 1 s");
        assertTrue(_capStale(), "the Intake stops new minting");
    }

    // ------------------------------------------------------------------ the last good price

    /// @notice Never poked: the newest valid answer published since deployment, or else the deployment's own price.
    function test_neverPokedFallsBackToTheNewestAnswerSinceDeployment() public {
        vm.warp(T0 + 2 * MAX_AGE);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), P1265, "no answer since deployment: the deployment's price");
        (uint256 p, uint256 at) = feed.lastGoodPrice();
        assertEq(p, P1265);
        assertEq(at, T0);

        primary.set(1200e8, T0 + 1 hours);
        secondary.set(1210e8, T0 + 3 hours);
        assertEq(feed.price(), 1210e34, "the newest answer since deployment");
        (p, at) = feed.lastGoodPrice();
        assertEq(at, T0 + 3 hours);
        primary.set(1220e8, T0 + 3 hours);
        assertEq(feed.price(), 1220e34, "a tie between the sources goes to the primary");
    }

    /// @notice Poked long ago: newer answers win over the old record.
    function test_pokedLongAgoNewerAnswersWin() public {
        feed.poke();
        primary.set(1300e8, T0 + 10 days);
        secondary.set(1310e8, T0 + 10 days);
        vm.warp(T0 + 12 days);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), 1300e34, "the newest answers (primary on the tie), not the 12-day-old record");
    }

    /// @notice A record made while the sources diverged keeps the lower price once they go stale. Without that poke the
    ///         feed falls back to the newest answer, which may be the higher one.
    function test_aPokeWhileDivergedKeepsTheLowerPrice() public {
        secondary.set(1100e8, T0 + 1 hours); // lower and older
        primary.set(1300e8, T0 + 2 hours);
        vm.warp(T0 + 2 hours);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Diverged));
        assertEq(feed.price(), 1100e34);
        uint256 snap = vm.snapshotState();

        vm.warp(T0 + 2 hours + MAX_AGE + 1);
        assertEq(feed.price(), 1300e34, "not poked: the newest answer, the higher");

        vm.revertToState(snap);
        feed.poke();
        vm.warp(T0 + 2 hours + MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), 1100e34, "poked: the lower price the market used");
        (uint256 high, uint256 highAt) = feed.priceHigh();
        assertEq(high, 1100e34);
        assertEq(highAt, T0 + 1 hours);
    }

    /// @notice Sources that stop answering right after an update: Morpho keeps the recorded price, and the cap sees it
    ///         stale at once although the record is only minutes old.
    function test_brokenSourcesFallBackToTheRecordDatedBack() public {
        primary.set(1270e8, T0 + 10 minutes);
        secondary.set(1271e8, T0 + 10 minutes);
        vm.warp(T0 + 20 minutes);
        feed.poke();
        primary.setMode(MockAggregator.Mode.Revert);
        secondary.setMode(MockAggregator.Mode.ShortData);
        vm.warp(T0 + 30 minutes);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), 1270e34, "the record");
        (uint256 p, uint256 at) = feed.lastGoodPrice();
        assertEq(p, 1270e34);
        assertEq(at, T0 + 10 minutes, "lastGoodPrice() reports the record's own updatedAt");
        (uint256 high, uint256 highAt) = feed.priceHigh();
        assertEq(high, 1270e34);
        assertEq(highAt, block.timestamp - MAX_AGE - 1, "priceHigh() dates it maxSourceAge + 1 s back");
        assertTrue(_capStale(), "the Intake stops new minting at once");
    }

    /// @notice An answer published after the record but older than the record's price does not replace it.
    function test_anOlderAnswerPublishedAfterTheRecordDoesNotWin() public {
        secondary.set(1250e8, T0 - MAX_AGE - 1); // stale already
        primary.set(1265e8, T0 + 5 hours);
        vm.warp(T0 + 5 hours);
        feed.poke(); // PrimaryOnly: the record is the primary at T0 + 5 h
        secondary.set(1200e8, T0 + 1 hours); // new for the secondary, but older than the record's price
        primary.setMode(MockAggregator.Mode.Revert);
        vm.warp(T0 + 5 hours + MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), P1265, "the record, newer than the late answer");
    }

    /// @notice A new answer exactly as old as the record's price does not replace it: ties go to the record.
    function test_aTieWithTheRecordGoesToTheRecord() public {
        primary.setMode(MockAggregator.Mode.Revert);
        secondary.set(1300e8, T0 + 1 hours);
        vm.warp(T0 + 1 hours);
        feed.poke(); // SecondaryOnly: the record is the secondary's price, the primary unseen
        primary.setMode(MockAggregator.Mode.Normal);
        primary.set(1250e8, T0 + 1 hours); // new for the primary, as old as the record
        vm.warp(T0 + 1 hours + MAX_AGE + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(feed.price(), 1300e34, "the record");
    }

    /// @notice A long-dead secondary never comes back as the last good price once the primary ages out.
    function test_aDeadSecondaryDoesNotReturnWhenThePrimaryAgesOut() public {
        // A feed deployed while its secondary's last answer was already 300 days old.
        secondary.set(500e8, T0 - 300 days);
        ZecuredPriceFeed f = new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), MAX_AGE, 300);
        assertEq(uint256(f.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
        primary.set(1280e8, T0 + 1 days);
        vm.warp(T0 + 1 days + MAX_AGE + 1);
        assertEq(uint256(f.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(f.price(), 1280e34, "the primary's last answer, not the secondary's ancient one");
    }

    function test_lastGoodPriceIsWhatPriceReturns() public {
        // Every state: lastGoodPrice().price == price().
        _assertLastGoodIsPrice();
        secondary.set(1100e8, T0);
        _assertLastGoodIsPrice(); // Diverged
        secondary.set(A1265, T0 - MAX_AGE - 1);
        _assertLastGoodIsPrice(); // PrimaryOnly
        vm.warp(T0 + MAX_AGE + 1);
        _assertLastGoodIsPrice(); // Stale
    }

    // ------------------------------------------------------------------ garbage and failing sources

    function test_zeroAndNegativeAnswersAreNotValid() public {
        primary.set(0, T0);
        _assertSecondaryOnly();
        primary.set(-1, T0);
        _assertSecondaryOnly();
        primary.set(type(int256).min, T0);
        _assertSecondaryOnly();
        (uint256 p, uint256 pAt, bool pFresh,,,) = feed.readSources();
        assertEq(p, 0);
        assertEq(pAt, 0);
        assertFalse(pFresh);
    }

    function test_updatedAtZeroOrInTheFutureIsNotValid() public {
        primary.set(A1265, 0);
        _assertSecondaryOnly();
        primary.set(A1265, T0 + 1);
        _assertSecondaryOnly();
        primary.set(A1265, type(uint256).max);
        _assertSecondaryOnly();
        vm.warp(T0 + 1);
        primary.set(A1265, T0 + 1);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh), "now, not the future");
    }

    function test_revertingShortAndHugeSources() public {
        primary.setMode(MockAggregator.Mode.Revert);
        _assertSecondaryOnly();
        primary.setMode(MockAggregator.Mode.ShortData);
        _assertSecondaryOnly();
        // 64 KB of return data: only the first five words are copied, and they are a normal answer.
        primary.setMode(MockAggregator.Mode.HugeData);
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.BothFresh));
        assertEq(feed.price(), P1265);
    }

    function test_aSourceWithoutCodeIsNotValid() public {
        // The secondary's code disappears (an EOA answers an empty success).
        vm.etch(address(secondary), "");
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));
    }

    /// @notice A source that burns all its gas costs at most SOURCE_GAS and counts as unavailable.
    function test_aGasBurningSourceIsBoundedAndNotValid() public {
        primary.setMode(MockAggregator.Mode.BurnGas);
        uint256 g = gasleft();
        uint256 price = feed.price();
        uint256 used = g - gasleft();
        assertEq(price, P1265, "the secondary's price");
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        assertLt(used, feed.SOURCE_GAS() + 50_000, "bounded by SOURCE_GAS");
        assertGt(used, feed.SOURCE_GAS(), "the source did burn its SOURCE_GAS");
    }

    /// @notice A failing source read with too little gas reverts instead of letting the caller pick the source.
    function test_revert_aStarvedSourceRead() public {
        primary.setMode(MockAggregator.Mode.Revert);
        (bool ok, bytes memory ret) = address(feed).staticcall{gas: 60_000}(abi.encodeCall(feed.price, ()));
        assertFalse(ok, "starved");
        assertEq(ret, abi.encodeWithSelector(ZecuredPriceFeed.InsufficientGasForSource.selector, address(primary)));
        (ok,) = address(feed).staticcall{gas: 300_000}(abi.encodeCall(feed.price, ()));
        assertTrue(ok, "with enough gas the reverting source is simply unavailable");
    }

    /// @notice Healthy sources need no more than their own cost: only a failure under low gas reverts.
    function test_healthySourcesNeedNoGasReserve() public view {
        (bool ok, bytes memory ret) = address(feed).staticcall{gas: 60_000}(abi.encodeCall(feed.price, ()));
        assertTrue(ok, "healthy sources answer with little gas");
        assertEq(abi.decode(ret, (uint256)), P1265);
    }

    // ------------------------------------------------------------------ poke

    function test_pokeEmitsOnlyWhenTheStateChanges() public {
        vm.recordLogs();
        feed.poke();
        assertEq(vm.getRecordedLogs().length, 0, "BothFresh again: no event");

        secondary.set(1250e8, T0 - MAX_AGE - 1);
        vm.expectEmit(address(feed));
        emit IZecuredPriceFeed.FeedStateChanged(IZecuredPriceFeed.FeedState.PrimaryOnly, P1265, 1250e34);
        feed.poke();
        assertEq(uint256(feed.recordedState()), uint256(IZecuredPriceFeed.FeedState.PrimaryOnly));

        vm.recordLogs();
        feed.poke();
        secondary.setMode(MockAggregator.Mode.Revert); // still PrimaryOnly
        feed.poke();
        assertEq(vm.getRecordedLogs().length, 0, "PrimaryOnly again: no event");

        secondary.setMode(MockAggregator.Mode.Normal);
        secondary.set(1100e8, T0);
        vm.expectEmit(address(feed));
        emit IZecuredPriceFeed.FeedStateChanged(IZecuredPriceFeed.FeedState.Diverged, P1265, 1100e34);
        feed.poke();
    }

    function test_pokeRecordsThePriceAndWhatItSaw() public {
        primary.set(1270e8, T0 + 1 hours);
        secondary.setMode(MockAggregator.Mode.Revert);
        vm.warp(T0 + 2 hours);
        vm.prank(makeAddr("anyone"));
        feed.poke();
        (uint256 recordPrice, uint256 updatedAt, uint256 recordedAt, uint256 pSeen, uint256 sSeen) = feed.lastRecord();
        assertEq(recordPrice, 1270e34);
        assertEq(updatedAt, T0 + 1 hours);
        assertEq(recordedAt, T0 + 2 hours);
        assertEq(pSeen, T0 + 1 hours);
        assertEq(sSeen, 0, "an unavailable source is recorded as unseen");
    }

    // ------------------------------------------------------------------ priceHigh().updatedAt

    /// @notice With max ages of 25 h and 2 h, the last good price may come from the 2 h source: its own updatedAt
    ///         would still pass an Intake with capPriceMaxAge = 25 h, the dated-back one does not.
    function test_theDateBackCoversUnequalMaxAges() public {
        ZecuredPriceFeed f = new ZecuredPriceFeed(address(primary), MAX_AGE, address(secondary), 2 hours, 300);
        assertEq(f.maxSourceAge(), MAX_AGE);
        primary.setMode(MockAggregator.Mode.Revert);
        secondary.set(1250e8, T0 + 1 hours);
        vm.warp(T0 + 3 hours + 1);
        assertEq(uint256(f.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        assertEq(f.price(), 1250e34);
        (, uint256 highAt) = f.priceHigh();
        assertEq(highAt, block.timestamp - MAX_AGE - 1);
        assertGt(block.timestamp, highAt + f.maxSourceAge(), "the Intake sees it stale");
    }

    /// @notice So early in a chain's life that `block.timestamp <= maxSourceAge`, a Stale price is dated 0.
    function test_theDateBackFloorsAtZero() public {
        vm.warp(500);
        primary.set(A1265, 400);
        secondary.set(A1265, 400);
        ZecuredPriceFeed f = new ZecuredPriceFeed(address(primary), 1000, address(secondary), 10, 300);
        primary.setMode(MockAggregator.Mode.Revert);
        assertEq(uint256(f.state()), uint256(IZecuredPriceFeed.FeedState.Stale));
        (uint256 high, uint256 highAt) = f.priceHigh();
        assertEq(high, P1265);
        assertEq(highAt, 0);
    }

    // ------------------------------------------------------------------ fuzz

    /// @notice Whatever the sources return, how they fail and when: nothing reverts, price() is never zero and equals
    ///         lastGoodPrice(), and priceHigh() never claims more freshness than its sources had.
    function testFuzz_neverRevertsAndNeverClaimsMoreFreshness(
        uint256 pSeed,
        uint256 sSeed,
        uint8 pMode,
        uint8 sMode,
        uint32 later,
        bool pokeFirst
    ) public {
        if (pokeFirst) feed.poke();
        vm.warp(T0 + later);
        (int256 pAnswer, uint256 pAt) = _fuzzAnswer(pSeed);
        (int256 sAnswer, uint256 sAt) = _fuzzAnswer(sSeed);
        primary.set(pAnswer, pAt);
        secondary.set(sAnswer, sAt);
        primary.setMode(MockAggregator.Mode(bound(pMode, 0, 4)));
        secondary.setMode(MockAggregator.Mode(bound(sMode, 0, 4)));

        IZecuredPriceFeed.FeedState s = feed.state();
        uint256 price = feed.price();
        (uint256 high, uint256 highAt) = feed.priceHigh();
        (uint256 good, uint256 goodAt) = feed.lastGoodPrice();
        feed.readSources();
        assertGt(price, 0, "never zero");
        assertLe(price, feed.MAX_PRICE());
        assertEq(good, price, "lastGoodPrice() is what price() returns");
        assertLe(highAt, block.timestamp, "never from the future");
        assertLe(goodAt, block.timestamp);
        if (s == IZecuredPriceFeed.FeedState.Stale) {
            assertEq(high, price);
            assertLe(highAt, goodAt, "dated back, never forward");
            if (block.timestamp > feed.maxSourceAge()) {
                assertLe(highAt, block.timestamp - feed.maxSourceAge() - 1, "stale for the Intake");
            } else {
                assertEq(highAt, 0);
            }
        } else {
            assertLe(price, high, "price() <= priceHigh() from fresh sources");
            assertLe(block.timestamp - highAt, feed.maxSourceAge(), "a fresh source's own updatedAt");
            (uint256 p, uint256 pReadAt, bool pFresh, uint256 q, uint256 qReadAt, bool qFresh) = feed.readSources();
            assertTrue(
                (pFresh && high == p && highAt == pReadAt) || (qFresh && high == q && highAt == qReadAt),
                "priceHigh() is a fresh source's answer with its own updatedAt"
            );
        }
        feed.poke();
        assertEq(uint256(feed.recordedState()), uint256(s));
    }

    /// @notice Two fresh valid answers: price() <= priceHigh(), price() is the primary's or the lower, and the state
    ///         follows the deviation rule.
    function testFuzz_priceAtMostPriceHighWhenBothFresh(uint256 pAnswer, uint256 sAnswer, uint256 pAge, uint256 sAge)
        public
    {
        uint256 maxAnswer = feed.MAX_PRICE() / 1e26;
        pAnswer = bound(pAnswer, 1, maxAnswer);
        sAnswer = bound(sAnswer, 1, maxAnswer);
        primary.set(int256(pAnswer), T0 - bound(pAge, 0, MAX_AGE));
        secondary.set(int256(sAnswer), T0 - bound(sAge, 0, MAX_AGE));
        uint256 p = pAnswer * 1e26;
        uint256 q = sAnswer * 1e26;
        uint256 lo = p < q ? p : q;
        uint256 hi = p < q ? q : p;
        bool agree = (hi - lo) * 10_000 <= lo * DEVIATION_BPS;

        IZecuredPriceFeed.FeedState s = feed.state();
        uint256 price = feed.price();
        (uint256 high,) = feed.priceHigh();
        assertLe(price, high);
        assertEq(high, hi, "the cap reads the higher");
        if (agree) {
            assertEq(uint256(s), uint256(IZecuredPriceFeed.FeedState.BothFresh));
            assertEq(price, p, "agreeing: the primary's");
        } else {
            assertEq(uint256(s), uint256(IZecuredPriceFeed.FeedState.Diverged));
            assertEq(price, lo, "diverged: the lower");
        }
    }

    // ------------------------------------------------------------------ helpers

    function _singleSource(uint8 decimals, int256 answer) internal returns (ZecuredPriceFeed f) {
        MockAggregator source = new MockAggregator(decimals);
        source.set(answer, T0);
        f = new ZecuredPriceFeed(address(source), MAX_AGE, address(0), 0, 0);
    }

    function _assertHigh(uint256 price, uint256 updatedAt) internal view {
        (uint256 high, uint256 highAt) = feed.priceHigh();
        assertEq(high, price, "priceHigh() price");
        assertEq(highAt, updatedAt, "priceHigh() updatedAt");
    }

    function _assertSecondaryOnly() internal view {
        assertEq(uint256(feed.state()), uint256(IZecuredPriceFeed.FeedState.SecondaryOnly));
        assertEq(feed.price(), P1265);
    }

    function _assertLastGoodIsPrice() internal view {
        (uint256 good,) = feed.lastGoodPrice();
        assertEq(good, feed.price());
    }

    /// @dev The Intake's test (`ZecuredIntake._capRoom`) with capPriceMaxAge = maxSourceAge().
    function _capStale() internal view returns (bool) {
        (, uint256 updatedAt) = feed.priceHigh();
        return block.timestamp > updatedAt + feed.maxSourceAge();
    }

    /// @dev Answers of every kind: zero or negative, too large, normal, or any int256; updated never, in the future,
    ///      fresh, or stale.
    function _fuzzAnswer(uint256 seed) internal view returns (int256 answer, uint256 updatedAt) {
        uint256 kind = seed % 4;
        uint256 x = uint256(keccak256(abi.encode(seed)));
        if (kind == 0) answer = -int256(x % 1e30);
        else if (kind == 1) answer = int256(feed.MAX_PRICE() / 1e26 + 1 + x % 1e30);
        else if (kind == 2) answer = int256(1 + x % (feed.MAX_PRICE() / 1e26));
        else answer = int256(x);
        uint256 when = (seed >> 8) % 4;
        uint256 y = uint256(keccak256(abi.encode(x)));
        if (when == 0) updatedAt = 0;
        else if (when == 1) updatedAt = block.timestamp + 1 + y % 1e9;
        else if (when == 2) updatedAt = block.timestamp - y % (MAX_AGE + 1);
        else updatedAt = block.timestamp - MAX_AGE - 1 - y % (block.timestamp - MAX_AGE - 1);
    }
}
