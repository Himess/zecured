// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

/// @title MockUSDC
/// @notice MOCK. Testnet stand-in for the loan asset of the Zecured Morpho market. It is NOT Circle's USDC,
///         has no value, and only the owner (the demo deployer) can mint it.
/// @dev 6 decimals, like USDC. Plain OpenZeppelin ERC-20 otherwise: no fees, hooks, pausing or blocklist, so it
///      satisfies every token assumption listed in Morpho Blue's `createMarket` NatSpec.
contract MockUSDC is ERC20, Ownable {
    constructor(address initialOwner) ERC20("Zecured Mock USDC", "mUSDC") Ownable(initialOwner) {}

    /// @inheritdoc ERC20
    function decimals() public pure override returns (uint8) {
        return 6;
    }

    /// @notice MOCK faucet: owner-only mint.
    function mint(address to, uint256 amount) external onlyOwner {
        _mint(to, amount);
    }
}
