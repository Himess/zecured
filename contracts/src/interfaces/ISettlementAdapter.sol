// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorphoLiquidateCallback, IMorphoRepayCallback} from "morpho-blue/src/interfaces/IMorphoCallbacks.sol";
import {CloseRequest} from "../libraries/ZecuredEIP712.sol";

/// @title ISettlementAdapter
/// @notice Liquidation and close-from-collateral for Zecured vault accounts (docs/INTERFACES.md § 3.3).
interface ISettlementAdapter is IMorphoLiquidateCallback, IMorphoRepayCallback {
    // ------------------------------------------------------------------ events (docs/INTERFACES.md § 3.3)

    event NonceInvalidated(address indexed account, uint256 nonce);

    // ------------------------------------------------------------------ errors

    /// @notice The Registry / zZEC wiring does not point at this Adapter.
    error AdapterMisconfigured();
    error ZeroCommitment();
    /// @notice `liquidate` needs exactly one of `seizedAssets` / `repaidShares` to be non-zero.
    error InconsistentInput();
    /// @notice The borrower has no Zecured vault.
    error NotAVaultAccount(address borrower);
    error VaultNotActive(bytes32 vaultId);
    /// @notice The account has no debt (liquidation: nothing to repay; close: use `requestRelease` instead).
    error NoDebt(address account);
    /// @notice D11: a liquidation must seize at least `MIN_SEIZE` zatoshi, or the vault's whole remaining outstanding.
    error SeizeBelowMinimum(uint256 seized, uint256 minimum);
    /// @notice Morpho transferred a different zZEC amount than it reported (should be unreachable).
    error UnexpectedCollateralReceived(uint256 received, uint256 expected);
    error NotExecutor(address caller, address executor);
    error RequestExpired(uint64 deadline);
    /// @notice The nonce was already used by a close or invalidated by the account.
    error NonceAlreadyUsed(address account, uint256 nonce);
    error MarginTooHigh(uint16 marginBps, uint16 maxMarginBps);
    /// @notice `req.vaultId` is not the vault of `req.account`.
    error VaultAccountMismatch(bytes32 vaultId, address account);
    /// @notice The ERC-1271 / ECDSA check of the CloseRequest signature failed.
    error InvalidUserSignature();
    error ZeroPrice();
    /// @notice The market maker's share (debt + margin, in ZEC) exceeds the vault's collateral.
    error CloseExceedsCollateral(uint256 mmAmount, uint64 outstanding);
    error UserAmountBelowMinimum(uint256 userAmount, uint64 minUserAmount);
    /// @notice The debt the executor would repay exceeds the cap it passed to `closeWithCollateral`.
    error RepayAboveMaximum(uint256 repaidAssets, uint256 maxRepayAssets);
    /// @notice Y7: the market maker's share is below the floor it passed to `closeWithCollateral` (the price moved
    ///         against it since it quoted).
    error MarketMakerAmountBelowMinimum(uint64 mmAmount, uint64 minMmAmount);
    error OnlyMorpho();
    /// @notice A Morpho callback arrived outside the matching Adapter operation.
    error UnexpectedCallback();

    // ------------------------------------------------------------------ functions (docs/INTERFACES.md § 3.3)

    /// @notice Liquidates `borrower` through Morpho. Exactly one of `seizedAssets` / `repaidShares` is non-zero;
    ///         `repaidShares` is capped at the borrower's current borrow shares. The seized amount must be at least
    ///         `MIN_SEIZE` or the vault's whole remaining outstanding (D11). USDC is pulled from `msg.sender` inside
    ///         `onMorphoLiquidate`. Seized zZEC is burned through `Registry.recordLiquidation`.
    /// @return seized Collateral seized, in zatoshi.
    /// @return repaid USDC repaid (6-decimal units).
    function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment)
        external
        returns (uint256 seized, uint256 repaid);

    /// @notice Executes a signed CloseRequest: repays all of the account's debt with USDC pulled from the executor
    ///         (`msg.sender`) inside `onMorphoRepay`, withdraws the whole outstanding collateral to this Adapter and
    ///         records the close (market maker claim + borrower claim) in the Registry.
    /// @param maxRepayAssets The most USDC the executor accepts to repay (the borrower may have borrowed more since
    ///        signing).
    /// @param minMmAmount Y7: the fewest zatoshi the executor accepts for that USDC (the price may have risen since it
    ///        quoted).
    function closeWithCollateral(
        CloseRequest calldata req,
        bytes calldata userSig,
        bytes32 mmCommitment,
        uint256 maxRepayAssets,
        uint64 minMmAmount
    ) external;

    /// @notice Cancels a CloseRequest nonce of `msg.sender` (the account). Idempotent: it never reverts, also for a
    ///         nonce that is already used or invalidated, and emits `NonceInvalidated` on every call.
    function invalidateNonce(uint256 nonce) external;

    /// @notice Whether `nonce` of `account` was used by a close or invalidated.
    function nonceUsed(address account, uint256 nonce) external view returns (bool);

    // ------------------------------------------------------------------ additional views (not in INTERFACES.md)

    /// @notice D11 liquidation floor in zatoshi (100,000 = 0.001 ZEC): a liquidation seizes at least this much, or the
    ///         vault's whole remaining outstanding. The remainder is not constrained.
    function MIN_SEIZE() external view returns (uint256);
    /// @notice Maximum market maker margin a CloseRequest may carry (500 bps = 5 %).
    function MAX_MARGIN_BPS() external view returns (uint16);
    /// @notice The EIP-712 digest (Adapter domain) of `req`, i.e. the hash the account must validate via ERC-1271.
    function hashCloseRequest(CloseRequest calldata req) external view returns (bytes32);
    /// @notice The Adapter's EIP-712 domain separator.
    function domainSeparator() external view returns (bytes32);
}
