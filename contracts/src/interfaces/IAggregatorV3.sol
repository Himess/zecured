// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title IAggregatorV3
/// @notice The part of Chainlink's `AggregatorV3Interface` that ZecuredPriceFeed reads. Chainlink's proxies implement it,
///         and so do the Chainlink-compatible adapters of other oracles (API3's `Api3ReaderProxyV1`, Pyth's
///         `PythAggregatorV3`, RedStone's price feed adapters).
/// @dev ZecuredPriceFeed calls these functions with a bounded low-level `staticcall` and checks every word it uses, so a
///      source that reverts or returns malformed data is treated as unavailable rather than reverting the feed.
interface IAggregatorV3 {
    /// @return The number of decimals of `answer`.
    function decimals() external view returns (uint8);

    /// @return A human-readable name of the feed ("ZEC / USD" for Chainlink's; API3's adapters return "").
    function description() external view returns (string memory);

    /// @return roundId The round of the answer (0 from API3's adapter; ZecuredPriceFeed ignores it).
    /// @return answer The price, `decimals()` decimals.
    /// @return startedAt When the round started (ignored).
    /// @return updatedAt When the answer was last updated (Chainlink: the block time of the report; API3: the time the
    ///         data was signed).
    /// @return answeredInRound Deprecated by Chainlink (ignored).
    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
}
