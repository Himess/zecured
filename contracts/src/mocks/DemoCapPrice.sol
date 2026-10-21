// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {IZecuredCapPrice} from "../interfaces/v3/IZecuredPriceFeed.sol";

/// @title DemoCapPrice
/// @notice MOCK / DEMO, testnet deployments only: the price the Intake's coverage cap reads (design note T-01), taken
///         from the market's own oracle and reported as fresh at every block. On testnet the market oracle is itself a
///         MOCK / DEMO (DemoShockOracle), so there is no second source to take the high side of. A mainnet deployment
///         reads a ZecuredPriceFeed instead (T-10): the higher of two independent sources, with a staleness limit.
contract DemoCapPrice is IZecuredCapPrice {
    IOracle public immutable oracle;

    constructor(IOracle oracle_) {
        require(address(oracle_) != address(0), "DemoCapPrice: zero oracle");
        oracle = oracle_;
    }

    function priceHigh() external view returns (uint256 price, uint256 updatedAt) {
        return (oracle.price(), block.timestamp);
    }
}
