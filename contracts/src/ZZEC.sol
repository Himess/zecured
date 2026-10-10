// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";

/// @title ZZEC
/// @notice Receipt for ZEC locked in a Zecured vault on Zcash (8 decimals: 1 zZEC wei = 1 zatoshi).
///         It exists only as Morpho collateral of the vault's smart account and is not transferable
///         (docs/INTERFACES.md § 3.1, invariant I2):
///         - mint: by the Registry, to the Registry (it supplies the zZEC to Morpho in the same call);
///         - burn: by the Registry, from the Registry or the SettlementAdapter;
///         - Registry -> Morpho: only when Morpho pulls it (`supplyCollateral`);
///         - Morpho -> Registry / Adapter: only while that receiver armed a one-shot expectation with
///           `expectFromMorpho(maxAmount)` (EIP-1153 transient storage, consumed by the transfer);
///         - everything else reverts. Morpho reports a refused transfer as "transfer reverted", which is what blocks
///           an account withdrawing its own collateral, a direct `Morpho.liquidate` and a zZEC flash loan.
/// @dev Wiring: Registry and Adapter addresses are immutable and are predicted before deployment (CREATE addresses
///      of the deployer's next nonces); the Registry and Adapter constructors verify the wiring. See contracts/README.md.
contract ZZEC is ERC20, IZZEC {
    /// @inheritdoc IZZEC
    address public immutable registry;
    /// @inheritdoc IZZEC
    address public immutable adapter;
    /// @inheritdoc IZZEC
    address public immutable morpho;

    /// @dev One (recipient, max) pair suffices: the Registry and the Adapter never have two zZEC-moving Morpho calls in
    ///      flight at once, and each arms immediately before its Morpho call.
    address private transient _expectedRecipient;
    uint256 private transient _expectedMax;

    constructor(address registry_, address adapter_, address morpho_) ERC20("Zecured ZEC Receipt", "zZEC") {
        if (registry_ == address(0) || adapter_ == address(0) || morpho_ == address(0)) revert ZeroAddress();
        registry = registry_;
        adapter = adapter_;
        morpho = morpho_;
    }

    /// @notice 8 decimals: amounts are zatoshi.
    function decimals() public pure override returns (uint8) {
        return 8;
    }

    /// @inheritdoc IZZEC
    function mint(address to, uint256 amount) external {
        if (msg.sender != registry) revert OnlyRegistry(msg.sender);
        _mint(to, amount);
    }

    /// @inheritdoc IZZEC
    function burn(address from, uint256 amount) external {
        if (msg.sender != registry) revert OnlyRegistry(msg.sender);
        _burn(from, amount);
    }

    /// @inheritdoc IZZEC
    function expectFromMorpho(uint256 maxAmount) external {
        if (msg.sender != registry && msg.sender != adapter) revert OnlyRegistryOrAdapter(msg.sender);
        _expectedRecipient = msg.sender;
        _expectedMax = maxAmount;
    }

    /// @inheritdoc IZZEC
    function pendingExpectation() external view returns (address recipient, uint256 maxAmount) {
        return (_expectedRecipient, _expectedMax);
    }

    /// @dev The transfer guard. Every balance change (mint, burn, transfer, transferFrom) passes through here.
    function _update(address from, address to, uint256 value) internal override {
        if (from == address(0)) {
            // Mint (the caller is already checked to be the Registry).
            if (to != registry) revert MintOnlyToRegistry(to);
        } else if (to == address(0)) {
            // Burn (the caller is already checked to be the Registry).
            if (from != registry && from != adapter) revert BurnOnlyFromRegistryOrAdapter(from);
        } else if (from == morpho) {
            // Out of Morpho: only to the receiver that armed the expectation, at most the armed amount, once.
            // `to` is never zero here, so an unarmed (zero) recipient never matches.
            if (to != _expectedRecipient || value > _expectedMax) revert UnexpectedTransferFromMorpho(to, value);
            _expectedRecipient = address(0);
            _expectedMax = 0;
        } else if (from != registry || to != morpho || msg.sender != morpho) {
            // Into Morpho: only the Registry's zZEC, only pulled by Morpho itself (supplyCollateral).
            revert TransferNotAllowed(from, to);
        }
        super._update(from, to, value);
    }
}
