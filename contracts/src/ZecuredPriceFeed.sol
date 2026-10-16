// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {IZecuredCapPrice, IZecuredPriceFeed} from "./interfaces/v3/IZecuredPriceFeed.sol";
import {IAggregatorV3} from "./interfaces/IAggregatorV3.sol";

/// @title ZecuredPriceFeed
/// @notice ZEC/USD for the Zecured market on Morpho Blue and for the Intake's coverage cap (design notes T-10): a primary
///         source (Chainlink) and an optional secondary one (an independent oracle's Chainlink-compatible adapter), both
///         read through the AggregatorV3 interface, each with its own staleness limit. It never reverts on anything a
///         source returns. No owner, no upgrade path, no admin.
///
///         Each source separately:
///         - its answer is valid when the call succeeds within `SOURCE_GAS` and returns at least five ABI words,
///           `answer > 0`, the answer's Morpho-scaled price is at most `MAX_PRICE`, `updatedAt != 0` and
///           `updatedAt <= block.timestamp`;
///         - a valid answer is fresh when `block.timestamp - updatedAt <= maxAge` of that source. A secondary at address
///           zero is never fresh (single-source mode).
///         Two fresh prices agree when the higher is at most `maxDeviationBps` above the lower.
///
///         state         | price()                | priceHigh()
///         BothFresh     | the primary's          | the higher of the two (the primary's on a tie)
///         Diverged      | the lower              | the higher
///         PrimaryOnly   | the primary's          | the primary's
///         SecondaryOnly | the secondary's        | the secondary's
///         Stale         | the last good price    | the last good price, dated at least `maxSourceAge + 1` s ago
///
///         In the fresh states `priceHigh()` reports the `updatedAt` of the answer its price comes from. So
///         `price() <= priceHigh()` whenever a source is fresh (they are equal when none is), and `priceHigh()` never
///         claims more freshness than its source had.
///
///         The last good price is what `price()` returns in the Stale state: the newest, by `updatedAt`, of the record
///         and of each source's valid answer published after the record (an `updatedAt` later than that source's when
///         the record was made); on a tie the record, then the primary. The constructor makes the first record and every
///         `poke()` a new one: the price `price()` returned then, that price's own `updatedAt`, and each source's
///         `updatedAt` at that moment. Hence a feed never poked falls back to the newest answer published since its
///         deployment, or else to the price it was deployed with; a feed poked long ago falls back to the newer answers,
///         never to its older record; and a record made while the sources diverged keeps the lower price once they go
///         stale (keepers call `poke()` whenever `state()` differs from `recordedState()`). The constructor requires a
///         fresh source, so the last good price, and `price()`, are never zero.
///
///         Why `priceHigh()` is dated back in the Stale state: a consumer that accepts prices up to `maxSourceAge` old
///         (the Intake's `capPriceMaxAge`) then sees every Stale price as stale and stops new minting, also when the last
///         good price came from a source that stopped answering soon after its last update. The date is never later than
///         the answer's own `updatedAt`: it only ever claims less freshness than the source had.
/// @dev Scale: Morpho prices zZEC (8 decimals) in USDC (6 decimals) as USDC units per zatoshi * 1e36, which is USD per
///      ZEC * 1e34. An answer with `d` decimals is therefore multiplied by 10^(34 - d): 1e16 for the 18-decimal ZEC/USD
///      feeds of Chainlink and API3 on Base, 1e26 for an 8-decimal feed. Sources with more than 34 decimals are refused.
///      Gas: a source read gets at most `SOURCE_GAS` and copies at most five words of return data. If a source fails
///      while less gas was left than it takes to give it `SOURCE_GAS`, the call reverts (`InsufficientGasForSource`)
///      instead of counting the source as unavailable: nobody can choose which source counts by choosing the gas of a
///      transaction (a liquidation, say). Round ids are ignored (API3's adapter returns 0).
contract ZecuredPriceFeed is IZecuredPriceFeed {
    /// @notice Decimals of the feed's price: USD per ZEC * 1e34 (Morpho's scale for zZEC / USDC).
    uint256 public constant PRICE_DECIMALS = 34;
    /// @notice A higher price (USD 10^16 per ZEC) is garbage, not a price. The bound keeps every product that Morpho, the
    ///         Intake and the bonds compute with the price far from overflow.
    uint256 public constant MAX_PRICE = 1e50;
    /// @notice Most gas a source read may use. Chainlink's ZEC/USD proxy and API3's ZEC/USD adapter on Base use about
    ///         17,000 and 25,000 from cold storage (test/fork/PriceFeedBaseFork.t.sol).
    uint256 public constant SOURCE_GAS = 100_000;
    uint256 private constant BPS = 10_000;
    /// @dev Five ABI words: roundId, answer, startedAt, updatedAt, answeredInRound.
    uint256 private constant ROUND_DATA_SIZE = 160;
    /// @dev With at least this much gas left before a source call, the call gets the whole `SOURCE_GAS` (EIP-150 keeps
    ///      1/64 back; the margin covers the cold account access and the call's own cost).
    uint256 private constant MIN_GAS_FOR_SOURCE = SOURCE_GAS * 64 / 63 + 5000;

    /// @inheritdoc IZecuredPriceFeed
    address public immutable primary;
    /// @inheritdoc IZecuredPriceFeed
    address public immutable secondary;
    /// @inheritdoc IZecuredPriceFeed
    uint64 public immutable primaryMaxAge;
    /// @inheritdoc IZecuredPriceFeed
    uint64 public immutable secondaryMaxAge;
    /// @inheritdoc IZecuredPriceFeed
    uint16 public immutable maxDeviationBps;
    /// @notice The larger of the two max ages: the oldest price the feed reports from a fresh source, and the
    ///         `capPriceMaxAge` an Intake reading this feed should use (it then accepts every fresh state and no Stale
    ///         one).
    uint64 public immutable maxSourceAge;
    /// @notice `decimals()` of the primary, read once at construction.
    uint8 public immutable primaryDecimals;
    /// @notice `decimals()` of the secondary, read once at construction (0 without a secondary).
    uint8 public immutable secondaryDecimals;

    uint256 private immutable _primaryScale;
    uint256 private immutable _secondaryScale;
    uint256 private immutable _primaryMaxAnswer;
    uint256 private immutable _secondaryMaxAnswer;

    /// @dev The record (see the contract's NatSpec). Slot 0: the price and its `updatedAt`.
    uint192 private _recordPrice;
    uint64 private _recordUpdatedAt;
    /// @dev Slot 1: when the record was made, each source's `updatedAt` then (0 if its answer was not valid), and the
    ///      state the last `poke()` (or the constructor) saw.
    uint64 private _recordedAt;
    uint64 private _primarySeenAt;
    uint64 private _secondarySeenAt;
    FeedState private _recordedState;

    /// @dev What the feed reads, from immutables at run time and from the constructor's arguments at construction
    ///      (immutables cannot be read in functions called during construction).
    struct Config {
        address primary;
        address secondary;
        uint256 primaryScale;
        uint256 secondaryScale;
        uint256 primaryMaxAnswer;
        uint256 secondaryMaxAnswer;
        uint256 primaryMaxAge;
        uint256 secondaryMaxAge;
        uint256 maxDeviationBps;
        uint256 maxSourceAge;
    }

    /// @dev One source's answer: its Morpho-scaled price and `updatedAt` when valid, else zeros.
    struct Answer {
        uint256 price;
        uint256 updatedAt;
        bool valid;
        bool fresh;
    }

    /// @dev A price and the time its source confirmed it.
    struct Quote {
        uint256 price;
        uint256 updatedAt;
    }

    /// @notice A source read failed while less than `SOURCE_GAS` could be given to it: send the call with more gas.
    error InsufficientGasForSource(address source);

    /// @param primary_ The primary source (Chainlink's ZEC/USD proxy), AggregatorV3.
    /// @param primaryMaxAge_ Oldest primary answer still fresh, in seconds (at least its heartbeat plus an hour).
    /// @param secondary_ The secondary source (an independent oracle's AggregatorV3 adapter), or zero for none.
    /// @param secondaryMaxAge_ Oldest secondary answer still fresh, in seconds; zero exactly when there is no secondary.
    /// @param maxDeviationBps_ How far apart two fresh prices may be and still agree (at most 10,000).
    /// @dev Reverts `ZeroAddress` without a primary, `InvalidConfig` for a zero max age, a max age without a secondary,
    ///      the same source twice, a deviation above 10,000 bps, a source whose `decimals()` fails or exceeds 34, and
    ///      when no source is fresh now (the first record needs a price). Emits `FeedStateChanged` with the first state.
    constructor(
        address primary_,
        uint64 primaryMaxAge_,
        // forge-lint: disable-next-line(missing-zero-check)
        address secondary_, // zero is single-source mode
        uint64 secondaryMaxAge_,
        uint16 maxDeviationBps_
    ) {
        if (primary_ == address(0)) revert ZeroAddress();
        if (
            primaryMaxAge_ == 0 || (secondary_ == address(0)) != (secondaryMaxAge_ == 0) || primary_ == secondary_
                || maxDeviationBps_ > BPS
        ) revert InvalidConfig();

        Config memory c;
        c.primary = primary_;
        c.secondary = secondary_;
        c.primaryMaxAge = primaryMaxAge_;
        c.secondaryMaxAge = secondaryMaxAge_;
        c.maxDeviationBps = maxDeviationBps_;
        c.maxSourceAge = primaryMaxAge_ > secondaryMaxAge_ ? primaryMaxAge_ : secondaryMaxAge_;
        uint8 pd = _readDecimals(primary_);
        c.primaryScale = 10 ** (PRICE_DECIMALS - pd);
        c.primaryMaxAnswer = MAX_PRICE / c.primaryScale;
        uint8 sd = secondary_ == address(0) ? 0 : _readDecimals(secondary_);
        if (secondary_ != address(0)) {
            c.secondaryScale = 10 ** (PRICE_DECIMALS - sd);
            c.secondaryMaxAnswer = MAX_PRICE / c.secondaryScale;
        }

        primary = primary_;
        secondary = secondary_;
        primaryMaxAge = primaryMaxAge_;
        secondaryMaxAge = secondaryMaxAge_;
        maxDeviationBps = maxDeviationBps_;
        // forge-lint: disable-next-line(unsafe-typecast)
        maxSourceAge = uint64(c.maxSourceAge); // the larger of two uint64
        primaryDecimals = pd;
        secondaryDecimals = sd;
        _primaryScale = c.primaryScale;
        _secondaryScale = c.secondaryScale;
        _primaryMaxAnswer = c.primaryMaxAnswer;
        _secondaryMaxAnswer = c.secondaryMaxAnswer;

        (FeedState s, Answer memory p, Answer memory q, Quote memory low,) = _evaluate(c);
        if (s == FeedState.Stale) revert InvalidConfig();
        _record(p, q, low);
        _recordedState = s;
        // forge-lint: disable-next-line(reentrancy-events)
        emit FeedStateChanged(s, p.price, q.price); // the sources are only ever staticcalled
    }

    // ------------------------------------------------------------------ IOracle, IZecuredCapPrice

    /// @inheritdoc IOracle
    /// @dev The primary's price when the sources agree, the lower one when they diverge, the only fresh one, or the last
    ///      good price when none is fresh. Never zero.
    function price() external view returns (uint256) {
        (,,, Quote memory low,) = _evaluate(_config());
        return low.price;
    }

    /// @inheritdoc IZecuredCapPrice
    /// @dev The higher fresh price with its own `updatedAt`; in the Stale state the last good price, dated no later than
    ///      `block.timestamp - maxSourceAge - 1` (0 that early in a chain's life).
    function priceHigh() external view returns (uint256, uint256) {
        (,,,, Quote memory high) = _evaluate(_config());
        return (high.price, high.updatedAt);
    }

    // ------------------------------------------------------------------ IZecuredPriceFeed

    /// @inheritdoc IZecuredPriceFeed
    /// @dev Writes the record (the price `price()` returns now, its `updatedAt`, and each source's `updatedAt`) and
    ///      emits `FeedStateChanged` with each source's valid price (0 if its answer is not valid) when the state differs
    ///      from the one last recorded.
    function poke() external {
        (FeedState s, Answer memory p, Answer memory q, Quote memory low,) = _evaluate(_config());
        _record(p, q, low);
        if (s != _recordedState) {
            _recordedState = s;
            // forge-lint: disable-next-line(reentrancy-events)
            emit FeedStateChanged(s, p.price, q.price); // the sources are only ever staticcalled
        }
    }

    /// @inheritdoc IZecuredPriceFeed
    function state() external view returns (FeedState s) {
        (s,,,,) = _evaluate(_config());
    }

    /// @inheritdoc IZecuredPriceFeed
    /// @dev The price `price()` returns and the `updatedAt` of the answer it comes from: the current price in a fresh
    ///      state, the last good price (see the contract's NatSpec) in the Stale state.
    function lastGoodPrice() external view returns (uint256, uint256) {
        (,,, Quote memory low,) = _evaluate(_config());
        return (low.price, low.updatedAt);
    }

    // ------------------------------------------------------------------ more views

    /// @notice The state the last `poke()` (or the constructor) recorded: a keeper pokes when `state()` differs.
    function recordedState() external view returns (FeedState) {
        return _recordedState;
    }

    /// @notice The record: its price and that price's `updatedAt`, when it was made, and each source's `updatedAt` then
    ///         (0 for a source whose answer was not valid).
    function lastRecord()
        external
        view
        returns (
            uint256 recordPrice,
            uint256 updatedAt,
            uint256 recordedAt,
            uint256 primarySeenAt,
            uint256 secondarySeenAt
        )
    {
        return (_recordPrice, _recordUpdatedAt, _recordedAt, _primarySeenAt, _secondarySeenAt);
    }

    /// @notice Each source's answer as the feed reads it now: its Morpho-scaled price and `updatedAt` (zeros if the
    ///         answer is not valid) and whether it is fresh.
    function readSources()
        external
        view
        returns (
            uint256 primaryPrice,
            uint256 primaryUpdatedAt,
            bool primaryFresh,
            uint256 secondaryPrice,
            uint256 secondaryUpdatedAt,
            bool secondaryFresh
        )
    {
        Config memory c = _config();
        Answer memory p = _read(c.primary, c.primaryScale, c.primaryMaxAnswer, c.primaryMaxAge);
        Answer memory q = _read(c.secondary, c.secondaryScale, c.secondaryMaxAnswer, c.secondaryMaxAge);
        return (p.price, p.updatedAt, p.fresh, q.price, q.updatedAt, q.fresh);
    }

    // ------------------------------------------------------------------ internal

    function _config() internal view returns (Config memory) {
        return Config({
            primary: primary,
            secondary: secondary,
            primaryScale: _primaryScale,
            secondaryScale: _secondaryScale,
            primaryMaxAnswer: _primaryMaxAnswer,
            secondaryMaxAnswer: _secondaryMaxAnswer,
            primaryMaxAge: primaryMaxAge,
            secondaryMaxAge: secondaryMaxAge,
            maxDeviationBps: maxDeviationBps,
            maxSourceAge: maxSourceAge
        });
    }

    /// @dev The state, both answers, and the quotes `price()` (low) and `priceHigh()` (high) return.
    function _evaluate(Config memory c)
        internal
        view
        returns (FeedState s, Answer memory p, Answer memory q, Quote memory low, Quote memory high)
    {
        p = _read(c.primary, c.primaryScale, c.primaryMaxAnswer, c.primaryMaxAge);
        q = _read(c.secondary, c.secondaryScale, c.secondaryMaxAnswer, c.secondaryMaxAge);
        if (p.fresh && q.fresh) {
            // On a tie both are the primary.
            Answer memory lo = q.price < p.price ? q : p;
            Answer memory hi = q.price > p.price ? q : p;
            if ((hi.price - lo.price) * BPS <= lo.price * c.maxDeviationBps) {
                s = FeedState.BothFresh;
                low = Quote(p.price, p.updatedAt);
            } else {
                s = FeedState.Diverged;
                low = Quote(lo.price, lo.updatedAt);
            }
            high = Quote(hi.price, hi.updatedAt);
        } else if (p.fresh) {
            s = FeedState.PrimaryOnly;
            low = Quote(p.price, p.updatedAt);
            high = Quote(p.price, p.updatedAt);
        } else if (q.fresh) {
            s = FeedState.SecondaryOnly;
            low = Quote(q.price, q.updatedAt);
            high = Quote(q.price, q.updatedAt);
        } else {
            s = FeedState.Stale;
            low = _lastGood(p, q);
            // Staleness is measured in hours: the seconds a sequencer can shift block.timestamp do not matter here, nor
            // in `_read`.
            // forge-lint: disable-next-line(block-timestamp)
            uint256 staleBefore = block.timestamp > c.maxSourceAge ? block.timestamp - c.maxSourceAge - 1 : 0;
            high = Quote(low.price, Math.min(low.updatedAt, staleBefore));
        }
    }

    /// @dev The newest of the record and of the valid answers published after it (ties: record, primary, secondary).
    function _lastGood(Answer memory p, Answer memory q) internal view returns (Quote memory g) {
        g = Quote(_recordPrice, _recordUpdatedAt);
        if (p.valid && p.updatedAt > _primarySeenAt && p.updatedAt > g.updatedAt) g = Quote(p.price, p.updatedAt);
        if (q.valid && q.updatedAt > _secondarySeenAt && q.updatedAt > g.updatedAt) g = Quote(q.price, q.updatedAt);
    }

    function _record(Answer memory p, Answer memory q, Quote memory low) internal {
        // Casts are exact: a valid price is at most MAX_PRICE < 2^192 and every `updatedAt` kept is at most
        // block.timestamp.
        // forge-lint: disable-next-line(unsafe-typecast)
        _recordPrice = uint192(low.price);
        // forge-lint: disable-next-line(unsafe-typecast)
        _recordUpdatedAt = uint64(low.updatedAt);
        // forge-lint: disable-next-line(unsafe-typecast)
        _recordedAt = uint64(block.timestamp);
        // forge-lint: disable-next-line(unsafe-typecast)
        _primarySeenAt = uint64(p.updatedAt);
        // forge-lint: disable-next-line(unsafe-typecast)
        _secondarySeenAt = uint64(q.updatedAt);
    }

    /// @dev One source's `latestRoundData()`: valid and fresh as defined in the contract's NatSpec. Never reverts on what
    ///      the source returns; reverts `InsufficientGasForSource` if the source fails while it could not get
    ///      `SOURCE_GAS`.
    function _read(address source, uint256 scale, uint256 maxAnswer, uint256 maxAge)
        internal
        view
        returns (Answer memory a)
    {
        if (source == address(0)) return a;
        bytes4 selector = IAggregatorV3.latestRoundData.selector;
        bool enoughGas = gasleft() >= MIN_GAS_FOR_SOURCE;
        bool ok;
        uint256 answer;
        uint256 updatedAt;
        assembly ("memory-safe") {
            // Temporary memory past the free memory pointer; at most ROUND_DATA_SIZE bytes are copied back.
            let ptr := mload(0x40)
            mstore(ptr, selector)
            ok := staticcall(SOURCE_GAS, source, ptr, 4, ptr, ROUND_DATA_SIZE)
            if lt(returndatasize(), ROUND_DATA_SIZE) { ok := 0 }
            answer := mload(add(ptr, 0x20))
            updatedAt := mload(add(ptr, 0x60))
        }
        if (!ok) {
            if (!enoughGas) revert InsufficientGasForSource(source);
            return a;
        }
        // Read as uint256, a negative int256 answer exceeds maxAnswer (< 2^255).
        // forge-lint: disable-next-line(block-timestamp)
        if (answer == 0 || answer > maxAnswer || updatedAt == 0 || updatedAt > block.timestamp) return a;
        a.price = answer * scale;
        a.updatedAt = updatedAt;
        a.valid = true;
        // forge-lint: disable-next-line(block-timestamp)
        a.fresh = block.timestamp - updatedAt <= maxAge;
    }

    /// @dev A source's `decimals()` at construction: `InvalidConfig` if the call fails, returns less than a word, or the
    ///      value exceeds `PRICE_DECIMALS`.
    function _readDecimals(address source) internal view returns (uint8) {
        bytes4 selector = IAggregatorV3.decimals.selector;
        bool ok;
        uint256 d;
        assembly ("memory-safe") {
            mstore(0, selector)
            ok := staticcall(SOURCE_GAS, source, 0, 4, 0, 0x20)
            if lt(returndatasize(), 0x20) { ok := 0 }
            d := mload(0)
        }
        if (!ok || d > PRICE_DECIMALS) revert InvalidConfig();
        // forge-lint: disable-next-line(unsafe-typecast)
        return uint8(d); // at most 34
    }
}
