// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";

/// @title MockZecOracle
/// @notice MOCK. Owner-set ZEC/USD price for the Zecured demo market on Morpho Blue. It is NOT a price feed: the
///         owner (demo control panel) sets the price by hand, e.g. to trigger a liquidation in the demo.
/// @dev Morpho's `IOracle.price()` is the price of 1 collateral asset in loan assets, scaled by
///      1e36 * 10^(loanDecimals - collateralDecimals). Collateral is zZEC (8 decimals, zatoshi) and the loan asset
///      is USDC (6 decimals), so price = USD-per-ZEC * 1e36 * 10^(6 - 8) = USD-per-ZEC * 1e34.
///      Example: ZEC = $40.00 -> price = 40e34; ZEC = $37.25 -> price = 3725e32 (cents * 1e32).
///      Check: 1 ZEC = 1e8 zatoshi -> 1e8 * 40e34 / 1e36 = 40e6 = 40 USDC.
///      Morpho assumes the price cannot drop below old price * LLTV * LIF in one step; a demo price crash can break
///      that assumption and create bad debt in the demo market, which is acceptable for a testnet mock.
contract MockZecOracle is IOracle, Ownable {
    /// @notice Multiply a whole-dollar ZEC/USD price by this to get the Morpho-scaled price.
    uint256 public constant USD_SCALE = 1e34;

    /// @notice Morpho-scaled ZEC/USD price (USD-per-ZEC * 1e34).
    uint256 public price;

    /// @notice Emitted on every price change, including the initial price set in the constructor.
    event PriceSet(uint256 oldPrice, uint256 newPrice);

    /// @notice A zero price would make every position liquidatable and make `liquidate` divide by zero.
    error ZeroPrice();

    constructor(address initialOwner, uint256 initialPrice) Ownable(initialOwner) {
        _setPrice(initialPrice);
    }

    /// @notice MOCK control: sets the Morpho-scaled price (USD-per-ZEC * 1e34). Owner only.
    function setPrice(uint256 newPrice) external onlyOwner {
        _setPrice(newPrice);
    }

    function _setPrice(uint256 newPrice) internal {
        if (newPrice == 0) revert ZeroPrice();
        emit PriceSet(price, newPrice);
        price = newPrice;
    }
}
