// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IMorphoRepayCallback, IMorphoLiquidateCallback} from "morpho-blue/src/interfaces/IMorphoCallbacks.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SpikeRegistry} from "./SpikeRegistry.sol";
import {SpikeCollateral} from "./SpikeCollateral.sol";

/// @title SpikeAdapter
/// @notice MOCK, SPIKE-ONLY (throwaway, test/spike/). Mimics the Morpho-facing half of SettlementAdapter:
///   (a) `closeWithCollateral`: repay ALL borrow shares on behalf of the account; inside onMorphoRepay pull exactly
///       `repaidAssets` USDC from the caller (the market maker) and approve Morpho; then withdraw the account's whole
///       collateral to this adapter (needs the account's Morpho authorization of this adapter).
///   (b) `liquidate`: Morpho.liquidate; inside onMorphoLiquidate pull `repaidAssets` USDC from the caller (liquidator).
///   USDC is always pulled from msg.sender of the adapter call, never from a caller-supplied address.
///   There are NO CloseRequest / ERC-1271 / executor / nonce / deadline checks here (the real adapter adds them, I8):
///   anyone can close anyone's position in this spike.
///   The adapter records what it observes inside the callbacks so the tests can assert Morpho's callback ordering.
contract SpikeAdapter is IMorphoRepayCallback, IMorphoLiquidateCallback {
    using SafeERC20 for IERC20;
    using MarketParamsLib for MarketParams;

    uint8 internal constant OP_NONE = 0;
    uint8 internal constant OP_CLOSE = 1;
    uint8 internal constant OP_LIQUIDATE = 2;

    IMorpho public immutable morpho;
    SpikeRegistry public immutable registry;
    IERC20 public immutable loanToken;
    SpikeCollateral public immutable collateralToken;
    Id public immutable marketId;
    MarketParams internal _market;

    // In-flight context of the current adapter call (EIP-1153 transient storage).
    uint8 internal transient _op;
    address internal transient _payer;
    address internal transient _account;

    // Spike instrumentation: what the adapter saw inside the last Morpho callback.
    uint256 public seenRepaidAssets;
    uint256 public seenAdapterCollateralBalance;
    uint256 public seenAccountCollateral;
    uint256 public seenAccountBorrowShares;

    event Closed(address indexed account, address indexed marketMaker, uint256 repaidAssets, uint256 withdrawn);
    event Liquidated(address indexed borrower, address indexed liquidator, uint256 seizedAssets, uint256 repaidAssets);

    error OnlyMorpho();
    error UnexpectedCallback();
    error NothingToRepay();
    error UnexpectedAmount(uint256 received, uint256 expected);

    constructor(IMorpho morpho_, SpikeRegistry registry_) {
        morpho = morpho_;
        registry = registry_;
        MarketParams memory marketParams_ = registry_.marketParams();
        _market = marketParams_;
        marketId = marketParams_.id();
        loanToken = IERC20(marketParams_.loanToken);
        collateralToken = SpikeCollateral(marketParams_.collateralToken);
    }

    /// @notice Closes `account`'s position with USDC from msg.sender (market maker), collateral goes to this adapter.
    function closeWithCollateral(address account) external returns (uint256 repaidAssets, uint256 withdrawn) {
        Position memory p = morpho.position(marketId, account);
        if (p.borrowShares == 0) revert NothingToRepay();

        // Repay by SHARES read in this same transaction: a front-run partial repay cannot make it underflow, and no
        // dust shares can remain. The exact USDC amount is only known inside the callback.
        _op = OP_CLOSE;
        _payer = msg.sender;
        _account = account;
        (repaidAssets,) = morpho.repay(_market, 0, p.borrowShares, account, abi.encode(OP_CLOSE));
        _clearContext();

        // repay does not touch collateral, so the pre-read value is still exact.
        withdrawn = p.collateral;
        uint256 balanceBefore = collateralToken.balanceOf(address(this));
        collateralToken.expectFromMorpho(withdrawn);
        morpho.withdrawCollateral(_market, withdrawn, account, address(this));
        uint256 received = collateralToken.balanceOf(address(this)) - balanceBefore;
        if (received != withdrawn) revert UnexpectedAmount(received, withdrawn);

        emit Closed(account, msg.sender, repaidAssets, withdrawn);
    }

    /// @notice Liquidates `borrower` with USDC from msg.sender (liquidator); seized collateral goes to this adapter.
    function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares)
        external
        returns (uint256 seized, uint256 repaid)
    {
        Position memory p = morpho.position(marketId, borrower);

        _op = OP_LIQUIDATE;
        _payer = msg.sender;
        _account = borrower;
        uint256 balanceBefore = collateralToken.balanceOf(address(this));
        // Morpho transfers the seized collateral BEFORE onMorphoLiquidate, so the flag must be armed before the call.
        // With the repaidShares path the seized amount is unknown in advance: arm an upper bound (the whole
        // collateral = the vault's outstanding) and check the real amount afterwards.
        collateralToken.expectFromMorpho(p.collateral);
        (seized, repaid) = morpho.liquidate(_market, borrower, seizedAssets, repaidShares, abi.encode(OP_LIQUIDATE));
        _clearContext();

        uint256 received = collateralToken.balanceOf(address(this)) - balanceBefore;
        if (received != seized) revert UnexpectedAmount(received, seized);

        emit Liquidated(borrower, msg.sender, seized, repaid);
    }

    /// @inheritdoc IMorphoRepayCallback
    function onMorphoRepay(uint256 assets, bytes calldata data) external {
        if (msg.sender != address(morpho)) revert OnlyMorpho();
        if (_op != OP_CLOSE || abi.decode(data, (uint8)) != OP_CLOSE) revert UnexpectedCallback();
        _observe(assets);
        loanToken.safeTransferFrom(_payer, address(this), assets);
        loanToken.forceApprove(address(morpho), assets);
    }

    /// @inheritdoc IMorphoLiquidateCallback
    function onMorphoLiquidate(uint256 repaidAssets, bytes calldata data) external {
        if (msg.sender != address(morpho)) revert OnlyMorpho();
        if (_op != OP_LIQUIDATE || abi.decode(data, (uint8)) != OP_LIQUIDATE) revert UnexpectedCallback();
        _observe(repaidAssets);
        loanToken.safeTransferFrom(_payer, address(this), repaidAssets);
        loanToken.forceApprove(address(morpho), repaidAssets);
    }

    /// @notice Hands everything this adapter holds to the registry's burn (stand-in for recordClose/recordLiquidation).
    function burnHeld() external returns (uint256 amount) {
        amount = collateralToken.balanceOf(address(this));
        registry.burnFromAdapter(amount);
    }

    function _observe(uint256 assets) internal {
        Position memory p = morpho.position(marketId, _account);
        seenRepaidAssets = assets;
        seenAdapterCollateralBalance = collateralToken.balanceOf(address(this));
        seenAccountCollateral = p.collateral;
        seenAccountBorrowShares = p.borrowShares;
    }

    function _clearContext() internal {
        _op = OP_NONE;
        _payer = address(0);
        _account = address(0);
    }
}
