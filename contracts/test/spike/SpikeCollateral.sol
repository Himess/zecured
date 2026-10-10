// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @title SpikeCollateral
/// @notice MOCK, SPIKE-ONLY (throwaway, lives in test/spike/, never deployed). Stand-in for zZEC in the Spike C
///         fork tests: 8 decimals (zatoshi); only the SpikeRegistry that deployed it can mint and burn.
/// @dev With `guarded = true` it also prototypes the zZEC transfer guard from docs/INTERFACES.md § 3.1, so the spike can
///      check that the guard is compatible with every real Morpho Blue code path:
///      - in:  Registry -> Morpho only (Morpho pulls it with transferFrom inside supplyCollateral);
///      - out: Morpho -> X only while X (Registry or Adapter) has armed a one-shot "expected transfer from Morpho"
///             flag in transient storage (EIP-1153) for at most `maxAmount`; the transfer consumes the flag;
///      - every other transfer reverts. Mint and burn are Registry-only.
///      One (recipient, max) pair is enough because Registry/Adapter make at most one Morpho call that moves
///      collateral at a time.
contract SpikeCollateral is ERC20 {
    address public immutable registry;
    address public immutable morpho;
    bool public immutable guarded;
    address public adapter;

    address internal transient _expectedRecipient;
    uint256 internal transient _expectedMax;

    error OnlyRegistry();
    error NotTrusted(address caller);
    error AdapterAlreadySet();
    error UnexpectedTransferFromMorpho(address to, uint256 value);
    error TransferNotAllowed(address from, address to);

    modifier onlyRegistry() {
        if (msg.sender != registry) revert OnlyRegistry();
        _;
    }

    constructor(address registry_, address morpho_, bool guarded_) ERC20("Spike zZEC (MOCK)", "szZEC") {
        registry = registry_;
        morpho = morpho_;
        guarded = guarded_;
    }

    function decimals() public pure override returns (uint8) {
        return 8;
    }

    function setAdapter(address adapter_) external onlyRegistry {
        if (adapter != address(0)) revert AdapterAlreadySet();
        adapter = adapter_;
    }

    function mint(address to, uint256 amount) external onlyRegistry {
        _mint(to, amount);
    }

    /// @dev Registry-only burn from any holder: the real Registry burns zZEC that Morpho sent to the Adapter.
    function burn(address from, uint256 amount) external onlyRegistry {
        _burn(from, amount);
    }

    /// @notice Arms the one-shot flag: the next transfer out of Morpho may go to `msg.sender`, for at most
    ///         `maxAmount`. Only Registry and Adapter may arm it, and only for themselves.
    function expectFromMorpho(uint256 maxAmount) external {
        if (msg.sender != registry && msg.sender != adapter) revert NotTrusted(msg.sender);
        _expectedRecipient = msg.sender;
        _expectedMax = maxAmount;
    }

    /// @notice Clears the caller's unused flag (the flag also dies at the end of the transaction).
    function clearExpectation() external {
        if (msg.sender == _expectedRecipient) {
            _expectedRecipient = address(0);
            _expectedMax = 0;
        }
    }

    function pendingExpectation() external view returns (address recipient, uint256 maxAmount) {
        return (_expectedRecipient, _expectedMax);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (guarded && from != address(0) && to != address(0)) {
            if (from == morpho) {
                if (to != _expectedRecipient || value > _expectedMax) revert UnexpectedTransferFromMorpho(to, value);
                _expectedRecipient = address(0);
                _expectedMax = 0;
            } else if (!(from == registry && to == morpho)) {
                revert TransferNotAllowed(from, to);
            }
        }
        super._update(from, to, value);
    }
}
