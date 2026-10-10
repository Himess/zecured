// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SpikeCollateral} from "./SpikeCollateral.sol";

/// @title SpikeRegistry
/// @notice MOCK, SPIKE-ONLY (throwaway, test/spike/). Mimics only the Morpho-facing parts of ZecuredRegistry:
///         - `depositFor` (stand-in for attestDeposit): mint to itself, then supplyCollateral(onBehalf = account);
///         - `requestRelease` (called by the account itself): withdrawCollateral(onBehalf = account,
///           receiver = registry), then burn.
///         No signer set, vaults, outstanding, claims or EIP-712: those belong to the real contract.
contract SpikeRegistry {
    using MarketParamsLib for MarketParams;

    IMorpho public immutable morpho;
    SpikeCollateral public immutable collateral;
    address public immutable owner;
    address public adapter;
    Id public marketId;
    MarketParams internal _market;
    bool internal _marketSet;

    event DepositedFor(address indexed account, uint256 amount);
    event Released(address indexed account, uint256 amount);
    event BurnedFromAdapter(uint256 amount);

    error OnlyOwner();
    error OnlyAdapter();
    error MarketAlreadySet();
    error WrongCollateral();
    error NothingToRelease();
    error UnexpectedAmount(uint256 received, uint256 expected);

    modifier onlyOwner() {
        if (msg.sender != owner) revert OnlyOwner();
        _;
    }

    constructor(IMorpho morpho_, bool guarded) {
        owner = msg.sender;
        morpho = morpho_;
        collateral = new SpikeCollateral(address(this), address(morpho_), guarded);
    }

    function setMarket(MarketParams calldata marketParams_) external onlyOwner {
        if (_marketSet) revert MarketAlreadySet();
        if (marketParams_.collateralToken != address(collateral)) revert WrongCollateral();
        _market = marketParams_;
        marketId = marketParams_.id();
        _marketSet = true;
    }

    function setAdapter(address adapter_) external onlyOwner {
        adapter = adapter_;
        collateral.setAdapter(adapter_);
    }

    function marketParams() external view returns (MarketParams memory) {
        return _market;
    }

    /// @notice Stand-in for attestDeposit: the registry is the only holder of the collateral token, ever, and only
    ///         for the duration of this call.
    function depositFor(address account, uint256 amount) external onlyOwner {
        collateral.mint(address(this), amount);
        collateral.approve(address(morpho), amount);
        morpho.supplyCollateral(_market, amount, account, "");
        emit DepositedFor(account, amount);
    }

    /// @notice Stand-in for requestRelease: only the account itself can call it (msg.sender is the onBehalf), and it
    ///         only works if the account did Morpho.setAuthorization(registry, true). Morpho reverts if debt remains.
    function requestRelease() external returns (uint256 amount) {
        address account = msg.sender;
        amount = morpho.position(marketId, account).collateral;
        if (amount == 0) revert NothingToRelease();

        uint256 balanceBefore = collateral.balanceOf(address(this));
        collateral.expectFromMorpho(amount);
        morpho.withdrawCollateral(_market, amount, account, address(this));
        uint256 received = collateral.balanceOf(address(this)) - balanceBefore;
        if (received != amount) revert UnexpectedAmount(received, amount);

        collateral.burn(address(this), amount);
        emit Released(account, amount);
    }

    /// @notice Stand-in for the burn inside recordClose / recordLiquidation: burns what Morpho sent to the adapter.
    function burnFromAdapter(uint256 amount) external {
        if (msg.sender != adapter) revert OnlyAdapter();
        collateral.burn(adapter, amount);
        emit BurnedFromAdapter(amount);
    }
}
