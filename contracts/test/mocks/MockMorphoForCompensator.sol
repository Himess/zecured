// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {MarketParams, Id, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {MathLib} from "morpho-blue/src/libraries/MathLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";

/// @title MockMorphoForCompensator
/// @notice MOCK, tests only. One Morpho Blue market, with Morpho's share arithmetic for borrows and repayments
///         (`SharesMathLib`, virtual shares), its authorization rule and its health check, for the compensator's unit
///         tests. Interest is added by the test (`addInterest`, applied at the next accrual) instead of an IRM; there is
///         no supply side beyond a USDC balance, no liquidation, and collateral is a number (no token moves). The fork
///         suites run against the real Morpho Blue.
contract MockMorphoForCompensator {
    using MarketParamsLib for MarketParams;
    using SharesMathLib for uint256;
    using MathLib for uint256;

    Id public immutable marketId;
    IERC20 public immutable loanToken;
    address public immutable oracle;
    uint256 public immutable lltv;

    uint128 public totalBorrowAssets;
    uint128 public totalBorrowShares;
    uint256 public pendingInterest;
    mapping(address account => uint128) public borrowSharesOf;
    mapping(address account => uint128) public collateralOf;
    mapping(address authorizer => mapping(address authorized => bool)) public isAuthorized;

    event Repay(address indexed caller, address indexed onBehalf, uint256 assets, uint256 shares);
    event Borrow(address indexed onBehalf, uint256 assets, uint256 shares);
    event WithdrawCollateral(address indexed caller, address indexed onBehalf, uint256 assets);

    constructor(MarketParams memory params) {
        marketId = params.id();
        loanToken = IERC20(params.loanToken);
        oracle = params.oracle;
        lltv = params.lltv;
    }

    // ------------------------------------------------------------------ test controls

    function setCollateral(address account, uint256 assets) external {
        collateralOf[account] = uint128(assets);
    }

    /// @notice Gives `account` a debt of `assets` without a health check or a transfer (the position's history).
    function seedDebt(address account, uint256 assets) external returns (uint256 shares) {
        shares = assets.toSharesUp(totalBorrowAssets, totalBorrowShares);
        borrowSharesOf[account] += uint128(shares);
        totalBorrowShares += uint128(shares);
        totalBorrowAssets += uint128(assets);
    }

    /// @notice Interest the next accrual adds to every borrower, pro rata to its shares.
    function addInterest(uint256 assets) external {
        pendingInterest += assets;
    }

    function debtOf(address account) external view returns (uint256) {
        return uint256(borrowSharesOf[account]).toAssetsUp(totalBorrowAssets + pendingInterest, totalBorrowShares);
    }

    // ------------------------------------------------------------------ the IMorpho subset

    function setAuthorization(address authorized, bool newIsAuthorized) external {
        isAuthorized[msg.sender][authorized] = newIsAuthorized;
    }

    function accrueInterest(MarketParams memory params) external {
        _checkMarket(params);
        _accrue();
    }

    function market(Id id) external view returns (Market memory m) {
        require(Id.unwrap(id) == Id.unwrap(marketId), "MockMorpho: unknown market");
        m.totalBorrowAssets = totalBorrowAssets;
        m.totalBorrowShares = totalBorrowShares;
        m.totalSupplyAssets = totalBorrowAssets;
        m.lastUpdate = uint128(block.timestamp);
    }

    function position(Id id, address account) external view returns (Position memory p) {
        require(Id.unwrap(id) == Id.unwrap(marketId), "MockMorpho: unknown market");
        p.borrowShares = borrowSharesOf[account];
        p.collateral = collateralOf[account];
    }

    function borrow(MarketParams memory params, uint256 assets, uint256 shares, address onBehalf, address receiver)
        external
        returns (uint256, uint256)
    {
        _checkMarket(params);
        require((assets == 0) != (shares == 0), "MockMorpho: inconsistent input");
        require(msg.sender == onBehalf || isAuthorized[onBehalf][msg.sender], "MockMorpho: unauthorized");
        _accrue();
        if (assets > 0) shares = assets.toSharesUp(totalBorrowAssets, totalBorrowShares);
        else assets = shares.toAssetsDown(totalBorrowAssets, totalBorrowShares);
        borrowSharesOf[onBehalf] += uint128(shares);
        totalBorrowShares += uint128(shares);
        totalBorrowAssets += uint128(assets);
        require(_isHealthy(onBehalf), "MockMorpho: insufficient collateral");
        emit Borrow(onBehalf, assets, shares);
        require(loanToken.transfer(receiver, assets), "MockMorpho: transfer");
        return (assets, shares);
    }

    function repay(MarketParams memory params, uint256 assets, uint256 shares, address onBehalf, bytes calldata data)
        external
        returns (uint256, uint256)
    {
        _checkMarket(params);
        require((assets == 0) != (shares == 0), "MockMorpho: inconsistent input");
        require(data.length == 0, "MockMorpho: no callback");
        _accrue();
        if (assets > 0) shares = assets.toSharesDown(totalBorrowAssets, totalBorrowShares);
        else assets = shares.toAssetsUp(totalBorrowAssets, totalBorrowShares);
        // Reverts on underflow, as Morpho does for a repayment of more shares than borrowed.
        borrowSharesOf[onBehalf] -= uint128(shares);
        totalBorrowShares -= uint128(shares);
        totalBorrowAssets = assets >= totalBorrowAssets ? 0 : totalBorrowAssets - uint128(assets);
        emit Repay(msg.sender, onBehalf, assets, shares);
        require(loanToken.transferFrom(msg.sender, address(this), assets), "MockMorpho: transferFrom");
        return (assets, shares);
    }

    function withdrawCollateral(MarketParams memory params, uint256 assets, address onBehalf, address) external {
        _checkMarket(params);
        require(assets != 0, "MockMorpho: zero assets");
        require(msg.sender == onBehalf || isAuthorized[onBehalf][msg.sender], "MockMorpho: unauthorized");
        _accrue();
        collateralOf[onBehalf] -= uint128(assets);
        require(_isHealthy(onBehalf), "MockMorpho: insufficient collateral");
        emit WithdrawCollateral(msg.sender, onBehalf, assets);
    }

    // ------------------------------------------------------------------ internal

    function _accrue() internal {
        if (pendingInterest == 0) return;
        totalBorrowAssets += uint128(pendingInterest);
        pendingInterest = 0;
    }

    function _isHealthy(address borrower) internal view returns (bool) {
        if (borrowSharesOf[borrower] == 0) return true;
        uint256 borrowed = uint256(borrowSharesOf[borrower]).toAssetsUp(totalBorrowAssets, totalBorrowShares);
        uint256 maxBorrow =
            uint256(collateralOf[borrower]).mulDivDown(IOracle(oracle).price(), ORACLE_PRICE_SCALE).wMulDown(lltv);
        return maxBorrow >= borrowed;
    }

    function _checkMarket(MarketParams memory params) internal view {
        require(Id.unwrap(params.id()) == Id.unwrap(marketId), "MockMorpho: wrong market params");
    }
}
