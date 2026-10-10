// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice MOCK placeholder collateral (8 decimals). Deployed ONLY in a DeployMocks dry run when COLLATERAL_TOKEN is
///         unset, so market creation can be simulated before zZEC exists. DeployMocks refuses to broadcast it.
contract DryRunPlaceholderCollateral is ERC20 {
    constructor() ERC20("Dry-run placeholder zZEC (MOCK)", "dryZZEC") {}

    function decimals() public pure override returns (uint8) {
        return 8;
    }
}
