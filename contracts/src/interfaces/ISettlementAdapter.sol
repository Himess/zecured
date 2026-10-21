// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorphoLiquidateCallback, IMorphoRepayCallback} from "morpho-blue/src/interfaces/IMorphoCallbacks.sol";
import {CloseRequest, DeleverageRequest} from "../libraries/ZecuredEIP712.sol";

/// @notice Pre-liquidation terms a vault's account opts into (all but `expiry` in WAD, 1e18 = 100 %), after Morpho's
///         PreLiquidation: once the position's LTV is above `preLltv` (and at most the market's LLTV), anyone may repay
///         up to a close factor of its borrow shares and take the repaid value times an incentive factor in collateral,
///         paid in ZEC on Zcash. Both factors rise linearly with the LTV across the band, from `preLcf1` / `preLif1` at
///         `preLltv` to `preLcf2` / `preLif2` at the LLTV.
/// @param preLltv The LTV above which the position can be pre-liquidated; below the LLTV.
/// @param preLcf1 Close factor at `preLltv`: the share of the borrow shares one pre-liquidation may repay (0 < preLcf1).
/// @param preLcf2 Close factor at the LLTV (preLcf1 <= preLcf2 <= 100 %).
/// @param preLif1 Incentive factor at `preLltv` (at least 1: the collateral taken is worth at least the repayment).
/// @param preLif2 Incentive factor at the LLTV (preLif1 <= preLif2 <= the market's liquidation incentive factor).
/// @param expiry Unix time from which the terms no longer apply (later than their setting).
struct PreLiquidationTerms {
    uint256 preLltv;
    uint256 preLcf1;
    uint256 preLcf2;
    uint256 preLif1;
    uint256 preLif2;
    uint64 expiry;
}

/// @title ISettlementAdapter
/// @notice Liquidation, pre-liquidation and close-from-collateral for Zecured vault accounts (docs/INTERFACES.md § 3.3).
interface ISettlementAdapter is IMorphoLiquidateCallback, IMorphoRepayCallback {
    // ------------------------------------------------------------------ events (docs/INTERFACES.md § 3.3)

    event NonceInvalidated(address indexed account, uint256 nonce);
    /// @notice The account opted into pre-liquidation on these terms (replacing any earlier ones).
    event PreLiquidationSet(address indexed account, PreLiquidationTerms terms);
    /// @notice The account withdrew its pre-liquidation terms.
    event PreLiquidationCleared(address indexed account);
    /// @notice `liquidator` repaid `repaidAssets` of the account's debt and took `seized` zatoshi of its vault (a
    ///         Liquidation claim of the liquidator's commitment, recorded by the Registry).
    event PreLiquidated(
        bytes32 indexed vaultId,
        address indexed account,
        address indexed liquidator,
        uint256 repaidAssets,
        uint64 seized
    );

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
    /// @notice S18: the market maker's collateral exceeds what the borrower signed for.
    error DeleverageAboveMaximum(uint256 mmAmount, uint64 maxCollateral);
    /// @notice S18: a deleverage must leave at least MIN_SEIZE in the vault (taking everything is a close).
    error DeleverageRemainderTooSmall(uint256 remaining, uint256 minimum);
    /// @notice S18: nothing to repay.
    error ZeroRepay();
    /// @notice Pre-liquidation terms: `preLltv` must be below the market's LLTV.
    error InvalidPreLltv(uint256 preLltv, uint256 lltv);
    /// @notice Pre-liquidation terms: the close factors must satisfy 0 < preLcf1 <= preLcf2 <= 1 (WAD).
    error InvalidPreLcf(uint256 preLcf1, uint256 preLcf2);
    /// @notice Pre-liquidation terms: the incentive factors must satisfy 1 <= preLif1 <= preLif2 <= `maxLif`, the
    ///         market's liquidation incentive factor (WAD).
    error InvalidPreLif(uint256 preLif1, uint256 preLif2, uint256 maxLif);
    /// @notice Pre-liquidation terms must expire after they are set.
    error InvalidExpiry(uint64 expiry);
    /// @notice The account has no pre-liquidation terms in force (never set, cleared, or expired).
    error PreLiquidationInactive(address account);
    /// @notice T-04: the slasher reports the vault frozen (an open theft case, or compromised).
    error VaultFrozen(bytes32 vaultId);
    /// @notice The position's LTV is not above the account's `preLltv`: `borrowed <= collateral value * preLltv`.
    error NotPreLiquidatable(uint256 borrowed, uint256 preLiquidationBorrow);
    /// @notice The position is past the LLTV (`borrowed > collateral value * LLTV`): it is liquidated through
    ///         `liquidate`, not pre-liquidated.
    error PositionLiquidatable(uint256 borrowed, uint256 maxBorrow);
    /// @notice More borrow shares than the close factor allows at the position's LTV.
    error PreLiquidationTooLarge(uint256 repaidShares, uint256 repayableShares);
    /// @notice A pre-liquidation must leave at least the D11 floor in the vault.
    error PreLiquidationRemainderTooSmall(uint256 remaining, uint256 minimum);
    /// @notice v4.1 (W-17): the repayment buys less collateral than the caller asked for at least.
    error PreLiquidationSlippage(uint64 seized, uint64 minSeizedZat);

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

    /// @notice S18, self-deleverage: executes a signed DeleverageRequest. The executor (`msg.sender`) repays
    ///         `req.repayAssets` of the account's debt with USDC pulled inside `onMorphoRepay`; the Adapter withdraws the
    ///         repaid value plus the quoted margin in collateral (rounded up, at most `req.maxCollateral`, at least
    ///         MIN_SEIZE, leaving at least MIN_SEIZE in the vault) and records it as a Liquidation claim of the
    ///         executor's commitment, paid like any third-party payout. The vault stays Active; its LTV falls whenever
    ///         LTV * (1 + margin) < 1, which holds for every healthy position at margins up to MAX_MARGIN_BPS.
    /// @param minMmAmount The fewest zatoshi the executor accepts (the price may have risen since it quoted).
    /// @return mmAmount The collateral withdrawn for the executor, in zatoshi.
    function deleverageWithCollateral(
        DeleverageRequest calldata req,
        bytes calldata userSig,
        bytes32 mmCommitment,
        uint64 minMmAmount
    ) external returns (uint64 mmAmount);

    /// @notice Cancels a CloseRequest nonce of `msg.sender` (the account). Idempotent: it never reverts, also for a
    ///         nonce that is already used or invalidated, and emits `NonceInvalidated` on every call.
    function invalidateNonce(uint256 nonce) external;

    /// @notice Pre-liquidation: the caller, the account of an Active vault, opts into pre-liquidation on terms `t`,
    ///         replacing any earlier ones: `preLltv < LLTV`, `1 <= preLif1 <= preLif2 <= LIF(LLTV)` (Morpho's
    ///         liquidation incentive factor of the market), `0 < preLcf1 <= preLcf2 <= 1`, all in WAD, and
    ///         `expiry > block.timestamp`.
    function setPreLiquidation(PreLiquidationTerms calldata t) external;

    /// @notice Withdraws the caller's pre-liquidation terms. Idempotent: it never reverts and emits
    ///         `PreLiquidationCleared` on every call.
    function clearPreLiquidation() external;

    /// @notice Anyone. Pre-liquidates `account`, which opted in (`setPreLiquidation`): the caller repays `repaidShares`
    ///         of its borrow shares with its own USDC (pulled inside `onMorphoRepay`) and is paid in the vault's ZEC:
    ///         1. the terms are in force and the vault is Active and not frozen;
    ///         2. Morpho accrues interest; `borrowed` is the debt rounded up, as Morpho's health check counts it;
    ///         3. `collateral value * preLltv < borrowed <= collateral value * LLTV` (past the LLTV, `liquidate`);
    ///         4. `ltv = borrowed / collateral value` (rounded up), `q = (ltv - preLltv) / (LLTV - preLltv)`, and the
    ///            close factor `preLcf = preLcf1 + q * (preLcf2 - preLcf1)` and the incentive factor
    ///            `preLif = preLif1 + q * (preLif2 - preLif1)` (rounded down);
    ///         5. `repaidShares <= borrowShares * preLcf`;
    ///         6. `seized = assets(repaidShares) * preLif / price`, every step rounded down (the borrower's way), at
    ///            least the D11 floor `minSeize(outstanding)` and leaving at least that floor in the vault;
    ///         7. `seized >= minSeizedZat` (v4.1, W-17: the caller's quote, so that the price or the debt moving
    ///            before its transaction lands cannot make it repay for less);
    ///         8. the Adapter withdraws `seized` and the Registry burns it into a Liquidation claim of `mmCommitment`
    ///            whose claimant is the caller (I2, I9).
    ///         Since `preLif <= LIF(LLTV)` and `ltv <= LLTV`, `ltv * preLif < 1`: the position's LTV always falls.
    /// @param mmCommitment Salted commitment of the caller's Ironwood receiver (the claim's destination).
    /// @param minSeizedZat The least collateral the caller accepts for `repaidShares`, in zatoshi.
    /// @return repaidAssets The USDC repaid (6-decimal units), what Morpho pulled.
    /// @return seized The collateral taken, in zatoshi.
    function preLiquidate(address account, uint256 repaidShares, bytes32 mmCommitment, uint64 minSeizedZat)
        external
        returns (uint256 repaidAssets, uint64 seized);

    /// @notice The account's pre-liquidation terms as stored (all zero if never set or cleared; they apply only while
    ///         `block.timestamp < expiry`).
    function preLiquidationTerms(address account) external view returns (PreLiquidationTerms memory);

    /// @notice Whether `nonce` of `account` was used by a close or invalidated.
    function nonceUsed(address account, uint256 nonce) external view returns (bool);

    // ------------------------------------------------------------------ additional views (not in INTERFACES.md)

    /// @notice D11 liquidation floor in zatoshi (100,000 = 0.001 ZEC): a liquidation seizes at least this much, or the
    ///         vault's whole remaining outstanding. The remainder is not constrained.
    function MIN_SEIZE() external view returns (uint256);
    /// @notice T-08 (decision 2e): the floor's share of the vault's outstanding, in basis points (0 on testnet).
    function minSeizeBps() external view returns (uint16);
    /// @notice The D11 floor for a vault with `outstanding` zatoshi: max(MIN_SEIZE, outstanding * minSeizeBps / 10,000).
    function minSeize(uint64 outstanding) external view returns (uint256);
    /// @notice Maximum market maker margin a CloseRequest may carry (500 bps = 5 %).
    function MAX_MARGIN_BPS() external view returns (uint16);
    /// @notice The EIP-712 digest (Adapter domain) of `req`, i.e. the hash the account must validate via ERC-1271.
    function hashCloseRequest(CloseRequest calldata req) external view returns (bytes32);
    /// @notice The EIP-712 digest (Adapter domain) of a DeleverageRequest.
    function hashDeleverageRequest(DeleverageRequest calldata req) external view returns (bytes32);
    /// @notice The Adapter's EIP-712 domain separator.
    function domainSeparator() external view returns (bytes32);
}
