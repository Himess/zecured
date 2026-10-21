// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredCapPrice} from "../../src/interfaces/v3/IZecuredPriceFeed.sol";

/// @notice MOCK (tests only): the high side of the ZEC price that the Intake's coverage cap reads (T-01), set by the
///         test, with the moment it was last confirmed fresh.
contract MockCapPrice is IZecuredCapPrice {
    uint256 public price;
    uint256 public updatedAt;

    constructor(uint256 price_) {
        set(price_);
    }

    /// @notice A fresh price, confirmed now.
    function set(uint256 price_) public {
        price = price_;
        updatedAt = block.timestamp;
    }

    /// @notice A price last confirmed at `updatedAt_` (to make it stale).
    function setAt(uint256 price_, uint256 updatedAt_) external {
        price = price_;
        updatedAt = updatedAt_;
    }

    function priceHigh() external view returns (uint256, uint256) {
        return (price, updatedAt);
    }
}
