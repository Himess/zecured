// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @title IZZEC
/// @notice zZEC: non-transferable receipt for ZEC locked in a Zecured vault (8 decimals, 1 wei = 1 zatoshi).
///         See docs/INTERFACES.md § 3.1. No account ever holds it (I2): it only moves Registry -> Morpho (collateral
///         supply) and Morpho -> Registry / SettlementAdapter while that receiver has armed a one-shot expectation.
interface IZZEC is IERC20 {
    /// @notice A constructor argument is the zero address.
    error ZeroAddress();
    /// @notice `mint` / `burn` called by someone other than the Registry.
    error OnlyRegistry(address caller);
    /// @notice `expectFromMorpho` called by someone other than the Registry or the Adapter.
    error OnlyRegistryOrAdapter(address caller);
    /// @notice zZEC can only be minted to the Registry (which supplies it to Morpho in the same call).
    error MintOnlyToRegistry(address to);
    /// @notice zZEC can only be burned from the Registry or the Adapter.
    error BurnOnlyFromRegistryOrAdapter(address from);
    /// @notice A transfer out of Morpho that was not armed by its recipient, or exceeds the armed maximum.
    ///         Morpho surfaces this as "transfer reverted".
    error UnexpectedTransferFromMorpho(address to, uint256 value);
    /// @notice Any other transfer (zZEC is not transferable).
    error TransferNotAllowed(address from, address to);

    /// @notice The ZecuredRegistry: sole minter and burner.
    function registry() external view returns (address);

    /// @notice The SettlementAdapter: may receive zZEC from Morpho (liquidation, close) after arming an expectation.
    function adapter() external view returns (address);

    /// @notice Morpho Blue.
    function morpho() external view returns (address);

    /// @notice Mints `amount` to `to`. Registry only; `to` must be the Registry.
    function mint(address to, uint256 amount) external;

    /// @notice Burns `amount` from `from`. Registry only; `from` must be the Registry or the Adapter.
    function burn(address from, uint256 amount) external;

    /// @notice Arms a one-shot expectation: the next zZEC transfer out of Morpho may go to `msg.sender`, for at most
    ///         `maxAmount`. Registry and Adapter only. EIP-1153 transient storage: consumed by that transfer and gone
    ///         at the end of the transaction. Arm it right before the Morpho call that moves zZEC.
    function expectFromMorpho(uint256 maxAmount) external;

    /// @notice The currently armed expectation (zero values when none). For monitoring and tests.
    function pendingExpectation() external view returns (address recipient, uint256 maxAmount);
}
