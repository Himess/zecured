// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";

/// @notice The conservative (high) side of the ZEC price, for the coverage cap (design notes T-01): the more the
///         collateral is worth, the more the bonds must cover. Morpho scale (USDC units per zatoshi * 1e36).
interface IZecuredCapPrice {
    /// @return price The higher of the sources' fresh prices (the last good one if none is fresh).
    /// @return updatedAt When that price was last confirmed by a fresh source.
    function priceHigh() external view returns (uint256 price, uint256 updatedAt);
}

/// @title IZecuredPriceFeed
/// @notice ZEC/USD for the market (design notes T-10): a primary feed (Chainlink) and a secondary one, each with its own
///         staleness limit (at least its heartbeat plus an hour). Never reverts.
/// @dev `price()` (Morpho's oracle, the collateral side): both fresh and within `maxDeviationBps` -> the primary; both
///      fresh and apart -> the lower; one fresh -> that one; none fresh -> the last good price. `priceHigh()` mirrors it
///      with the higher price and reports when it was last fresh, so that the cap stops minting on stale data while
///      Morpho keeps running. View functions cannot emit events: `poke()` records the state and emits them.
interface IZecuredPriceFeed is IOracle, IZecuredCapPrice {
    enum FeedState {
        BothFresh,
        Diverged,
        PrimaryOnly,
        SecondaryOnly,
        Stale
    }

    event FeedStateChanged(FeedState state, uint256 primaryPrice, uint256 secondaryPrice);

    error ZeroAddress();
    error InvalidConfig();

    /// @notice Records the last good price and emits `FeedStateChanged` when the state changed. Anyone.
    function poke() external;
    function state() external view returns (FeedState);
    function lastGoodPrice() external view returns (uint256 price, uint256 updatedAt);
    function primary() external view returns (address);
    function secondary() external view returns (address);
    function primaryMaxAge() external view returns (uint64);
    function secondaryMaxAge() external view returns (uint64);
    function maxDeviationBps() external view returns (uint16);
}
