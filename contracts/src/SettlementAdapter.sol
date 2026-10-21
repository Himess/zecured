// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IMorpho, MarketParams, Id, Market, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {IMorphoLiquidateCallback, IMorphoRepayCallback} from "morpho-blue/src/interfaces/IMorphoCallbacks.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {MathLib, WAD} from "morpho-blue/src/libraries/MathLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {
    ORACLE_PRICE_SCALE,
    LIQUIDATION_CURSOR,
    MAX_LIQUIDATION_INCENTIVE_FACTOR
} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";
import {IZecuredRegistry, VaultStatus} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {ISettlementAdapter, PreLiquidationTerms} from "./interfaces/ISettlementAdapter.sol";
import {ZecuredEIP712, CloseRequest, DeleverageRequest} from "./libraries/ZecuredEIP712.sol";

/// @title SettlementAdapter
/// @notice The only way, besides the account's own `requestRelease`, for zZEC collateral to leave Morpho
///         (docs/INTERFACES.md § 3.3):
///         - `liquidate`: a permissionless Morpho liquidation routed through this Adapter; the liquidator pays USDC in
///           Morpho's callback and is paid in ZEC on Zcash through a Liquidation claim (dust floor D11);
///         - `preLiquidate`: for an account that opted in (`setPreLiquidation`), anyone repays part of the debt once
///           the LTV is in the account's band below the LLTV, for a smaller incentive than a liquidation's (Morpho's
///           PreLiquidation design, carried into the Adapter because zZEC cannot leave Morpho any other way); paid
///           like a liquidation;
///         - `closeWithCollateral`: a market maker (the request's `executor`) repays the whole debt in USDC and is paid
///           in ZEC from the vault; the borrower's remainder goes to their own commitment (I7, I8);
///         - `deleverageWithCollateral` (S18): the same for part of the debt; the vault stays open with a lower LTV.
/// @dev Morpho authorization is all-or-nothing, so this contract calls only `accrueInterest`, `liquidate`, `repay` and
///      `withdrawCollateral` (the latter only for a valid signed request whose executor is the caller, I8, or within
///      the pre-liquidation terms the account itself set on this contract). It has no code path that borrows or
///      withdraws loan assets, no admin and no upgrade path.
contract SettlementAdapter is ISettlementAdapter, EIP712, ReentrancyGuardTransient {
    using SafeERC20 for IERC20;
    using MarketParamsLib for MarketParams;
    using MathLib for uint256;
    using SharesMathLib for uint256;

    /// @inheritdoc ISettlementAdapter
    uint256 public immutable MIN_SEIZE;
    /// @inheritdoc ISettlementAdapter
    uint16 public immutable minSeizeBps;
    /// @inheritdoc ISettlementAdapter
    uint16 public immutable MAX_MARGIN_BPS;
    uint256 private constant BPS = 10_000;

    // In-flight operation, so that Morpho callbacks are only honored inside the matching Adapter call.
    uint8 private constant OP_NONE = 0;
    uint8 private constant OP_LIQUIDATE = 1;
    uint8 private constant OP_CLOSE = 2;

    /// @notice The ZecuredRegistry.
    IZecuredRegistry public immutable registry;
    /// @notice Morpho Blue.
    IMorpho public immutable morpho;
    /// @notice The zZEC token (collateral of the market).
    IZZEC public immutable zzec;
    /// @notice The market's loan token (MockUSDC on Base Sepolia).
    IERC20 public immutable loanToken;
    /// @notice The market's ZEC/USD oracle.
    IOracle public immutable oracle;
    /// @notice The zZEC market id.
    Id public immutable marketId;
    address private immutable _irm;
    uint256 private immutable _lltv;
    /// @dev Morpho's liquidation incentive factor of the market, min(1.15, 1 / (1 - 0.3 * (1 - LLTV))), in WAD: the
    ///      highest pre-liquidation incentive factor.
    uint256 private immutable _lif;
    /// @dev The Registry's slasher (zero on deployments without bonds): a vault it reports frozen is not pre-liquidated.
    address private immutable _slasher;

    mapping(address account => mapping(uint256 nonce => bool used)) private _nonceUsed;
    mapping(address account => PreLiquidationTerms) private _preLiquidation;

    // EIP-1153: the operation in flight and the address that pays its USDC (liquidator or executor).
    uint8 private transient _op;
    address private transient _payer;

    /// @dev Deployed right after the Registry, at the address the Registry and zZEC were built with.
    /// @param minSeize_ D11 floor in zatoshi (100,000 on the testnet deployments).
    /// @param minSeizeBps_ T-08 (decision 2e): the floor also scales with the vault, in basis points of its outstanding
    ///        (0 on the testnet deployments; at most 50 %).
    /// @param maxMarginBps_ Highest market maker margin a close or deleverage request may quote (500; at most 20 %).
    constructor(IZecuredRegistry registry_, uint256 minSeize_, uint16 minSeizeBps_, uint16 maxMarginBps_)
        EIP712(ZecuredEIP712.ADAPTER_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION)
    {
        if (minSeize_ == 0 || minSeizeBps_ > BPS / 2 || maxMarginBps_ > BPS / 5) revert AdapterMisconfigured();
        IZZEC zzec_ = registry_.zzec();
        if (
            registry_.adapter() != address(this) || zzec_.adapter() != address(this)
                || zzec_.registry() != address(registry_)
        ) revert AdapterMisconfigured();
        MarketParams memory params = registry_.marketParams();

        registry = registry_;
        MIN_SEIZE = minSeize_;
        minSeizeBps = minSeizeBps_;
        MAX_MARGIN_BPS = maxMarginBps_;
        morpho = registry_.morpho();
        zzec = zzec_;
        loanToken = IERC20(params.loanToken);
        oracle = IOracle(params.oracle);
        _irm = params.irm;
        _lltv = params.lltv;
        marketId = params.id();
        // As Morpho.liquidate computes it (Morpho enables only LLTVs below 1).
        _lif = Math.min(
            MAX_LIQUIDATION_INCENTIVE_FACTOR, WAD.wDivDown(WAD - LIQUIDATION_CURSOR.wMulDown(WAD - params.lltv))
        );
        _slasher = registry_.slasher();
    }

    // ------------------------------------------------------------------ liquidation

    /// @inheritdoc ISettlementAdapter
    function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment)
        external
        nonReentrant
        returns (uint256 seized, uint256 repaid)
    {
        if (liquidatorCommitment == bytes32(0)) revert ZeroCommitment();
        if ((seizedAssets == 0) == (repaidShares == 0)) revert InconsistentInput();
        bytes32 vaultId = registry.vaultOf(borrower);
        if (vaultId == bytes32(0)) revert NotAVaultAccount(borrower);
        (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        if (status != VaultStatus.Active) revert VaultNotActive(vaultId);
        if (repaidShares != 0) {
            // Read in this same call: a front-running repay of a few shares cannot make Morpho underflow.
            uint256 borrowShares = morpho.position(marketId, borrower).borrowShares;
            if (borrowShares == 0) revert NoDebt(borrower);
            if (repaidShares > borrowShares) repaidShares = borrowShares;
        }

        _op = OP_LIQUIDATE;
        _payer = msg.sender;
        // Morpho transfers the seized zZEC BEFORE calling onMorphoLiquidate, so the expectation is armed here, with
        // the vault's outstanding as the upper bound (the repaidShares path does not know the seized amount upfront).
        zzec.expectFromMorpho(outstanding);
        (seized, repaid) =
            morpho.liquidate(_marketParams(), borrower, seizedAssets, repaidShares, abi.encode(OP_LIQUIDATE));
        _op = OP_NONE;
        _payer = address(0);

        // Y5: the guard's own receipt of the transfer, never a balance difference; the expectation is cleared.
        uint256 received = zzec.settleExpectation();
        if (received != seized) revert UnexpectedCollateralReceived(received, seized);
        // D11 dust guard: at least MIN_SEIZE, or the vault's whole remaining outstanding. Any remainder is allowed (it
        // stays with the borrower). `seized <= outstanding` is enforced by the zZEC expectation and the Registry (I9).
        // A smaller seizure is also allowed when it clears the borrower's whole debt: otherwise a small position (for
        // example the floor a partial release leaves) could not be liquidated at all between LLTV and the LTV at which
        // repaying everything seizes MIN_SEIZE (contracts review L-1).
        uint256 floor = minSeize(outstanding);
        if (seized < floor && seized != outstanding && morpho.position(marketId, borrower).borrowShares != 0) {
            revert SeizeBelowMinimum(seized, floor);
        }

        // seized <= outstanding (a uint64): the checked cast cannot fail. The liquidator is the claim's claimant
        // (T-04, T-05).
        registry.recordLiquidation(vaultId, SafeCast.toUint64(seized), liquidatorCommitment, msg.sender);
    }

    /// @inheritdoc IMorphoLiquidateCallback
    function onMorphoLiquidate(uint256 repaidAssets, bytes calldata) external {
        if (msg.sender != address(morpho)) revert OnlyMorpho();
        if (_op != OP_LIQUIDATE) revert UnexpectedCallback();
        _op = OP_NONE; // one callback per operation
        loanToken.safeTransferFrom(_payer, address(this), repaidAssets);
        loanToken.forceApprove(address(morpho), repaidAssets);
    }

    // ------------------------------------------------------------------ close from collateral

    /// @inheritdoc ISettlementAdapter
    function closeWithCollateral(
        CloseRequest calldata req,
        bytes calldata userSig,
        bytes32 mmCommitment,
        uint256 maxRepayAssets,
        uint64 minMmAmount
    ) external nonReentrant {
        uint64 outstanding = _checkCloseRequest(req, userSig, mmCommitment);
        _nonceUsed[req.account][req.nonce] = true;

        // 1. Repay all debt with the executor's USDC (at most `maxRepayAssets`: the borrower may have borrowed more
        //    since signing); split the collateral between market maker and borrower.
        (uint64 mmAmount, uint64 userAmount) = _repayAndSplit(req, outstanding, maxRepayAssets);
        if (mmAmount < minMmAmount) revert MarketMakerAmountBelowMinimum(mmAmount, minMmAmount);

        // 2. Withdraw the whole outstanding to this Adapter (account's Morpho authorization; zZEC guard armed).
        _withdrawCollateralToSelf(req.account, outstanding);

        // 3. The Registry burns it and creates the user claim, then the market maker claim (I7).
        registry.recordClose(req.vaultId, mmAmount, userAmount, mmCommitment, req.userCommitment, msg.sender);
    }

    // ------------------------------------------------------------------ self-deleverage (S18)

    /// @inheritdoc ISettlementAdapter
    function deleverageWithCollateral(
        DeleverageRequest calldata req,
        bytes calldata userSig,
        bytes32 mmCommitment,
        uint64 minMmAmount
    ) external nonReentrant returns (uint64 mmAmount) {
        uint64 outstanding = _checkDeleverageRequest(req, userSig, mmCommitment);
        _nonceUsed[req.account][req.nonce] = true;

        // 1. Repay exactly the signed debt with the executor's USDC.
        _op = OP_CLOSE;
        _payer = msg.sender; // the executor (checked == req.executor)
        (uint256 repaidAssets,) = morpho.repay(_marketParams(), req.repayAssets, 0, req.account, abi.encode(OP_CLOSE));
        _op = OP_NONE;
        _payer = address(0);

        // 2. The executor's collateral: the repaid value plus margin, within what the borrower signed and the floors.
        uint256 mm = _marketMakerAmount(repaidAssets, req.marginBps);
        if (mm > req.maxCollateral) revert DeleverageAboveMaximum(mm, req.maxCollateral);
        uint256 floor = minSeize(outstanding);
        if (mm < floor) revert SeizeBelowMinimum(mm, floor);
        if (mm + floor > outstanding) {
            revert DeleverageRemainderTooSmall(outstanding - Math.min(mm, outstanding), floor);
        }
        mmAmount = SafeCast.toUint64(mm); // < outstanding, a uint64
        if (mmAmount < minMmAmount) revert MarketMakerAmountBelowMinimum(mmAmount, minMmAmount);

        // 3. Withdraw it (Morpho checks the position's health after the repayment) and record it: the Registry burns it
        //    and creates the executor's claim (I8, I9).
        _withdrawCollateralToSelf(req.account, mmAmount);
        registry.recordLiquidation(req.vaultId, mmAmount, mmCommitment, msg.sender);
    }

    /// @inheritdoc IMorphoRepayCallback
    function onMorphoRepay(uint256 repaidAssets, bytes calldata) external {
        if (msg.sender != address(morpho)) revert OnlyMorpho();
        if (_op != OP_CLOSE) revert UnexpectedCallback();
        _op = OP_NONE; // one callback per operation
        loanToken.safeTransferFrom(_payer, address(this), repaidAssets);
        loanToken.forceApprove(address(morpho), repaidAssets);
    }

    /// @inheritdoc ISettlementAdapter
    function invalidateNonce(uint256 nonce) external {
        _nonceUsed[msg.sender][nonce] = true; // idempotent: also fine for a nonce already used or invalidated
        emit NonceInvalidated(msg.sender, nonce);
    }

    // ------------------------------------------------------------------ pre-liquidation

    /// @inheritdoc ISettlementAdapter
    /// @dev Only the account of an Active vault: an activated vault stays its account's for good (I4), so the terms
    ///      can never apply to another vault, and nothing about a vault reaches this contract before it is active.
    function setPreLiquidation(PreLiquidationTerms calldata t) external {
        bytes32 vaultId = registry.vaultOf(msg.sender);
        if (vaultId == bytes32(0)) revert NotAVaultAccount(msg.sender);
        (,, VaultStatus status) = registry.vault(vaultId);
        if (status != VaultStatus.Active) revert VaultNotActive(vaultId);
        if (t.preLltv >= _lltv) revert InvalidPreLltv(t.preLltv, _lltv);
        if (t.preLcf1 == 0 || t.preLcf1 > t.preLcf2 || t.preLcf2 > WAD) revert InvalidPreLcf(t.preLcf1, t.preLcf2);
        if (t.preLif1 < WAD || t.preLif1 > t.preLif2 || t.preLif2 > _lif) {
            revert InvalidPreLif(t.preLif1, t.preLif2, _lif);
        }
        if (t.expiry <= block.timestamp) revert InvalidExpiry(t.expiry);
        _preLiquidation[msg.sender] = t;
        emit PreLiquidationSet(msg.sender, t);
    }

    /// @inheritdoc ISettlementAdapter
    function clearPreLiquidation() external {
        delete _preLiquidation[msg.sender]; // idempotent, like invalidateNonce
        emit PreLiquidationCleared(msg.sender);
    }

    /// @inheritdoc ISettlementAdapter
    function preLiquidate(address account, uint256 repaidShares, bytes32 mmCommitment, uint64 minSeizedZat)
        external
        nonReentrant
        returns (uint256 repaidAssets, uint64 seized)
    {
        if (mmCommitment == bytes32(0)) revert ZeroCommitment();
        if (repaidShares == 0) revert ZeroRepay();
        (bytes32 vaultId, uint64 outstanding) = _preLiquidatableVault(account);
        // Steps 2 to 6, before any transfer: the collateral the repayment buys, within the floors.
        seized = _preLiquidationSeize(account, repaidShares, outstanding);
        // W-17: never less than the caller quoted (the price or the debt may have moved since).
        if (seized < minSeizedZat) revert PreLiquidationSlippage(seized, minSeizedZat);

        // The repayment, with the caller's USDC pulled inside onMorphoRepay (the path of a close).
        _op = OP_CLOSE;
        _payer = msg.sender;
        (repaidAssets,) = morpho.repay(_marketParams(), 0, repaidShares, account, abi.encode(OP_CLOSE));
        _op = OP_NONE;
        _payer = address(0);

        // The collateral leaves Morpho to this Adapter only (Morpho checks the position's health after the repayment;
        // its LTV fell) and the Registry burns it into the caller's claim (I2, I9).
        _withdrawCollateralToSelf(account, seized);
        registry.recordLiquidation(vaultId, seized, mmCommitment, msg.sender);
        emit PreLiquidated(vaultId, account, msg.sender, repaidAssets, seized);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc ISettlementAdapter
    function minSeize(uint64 outstanding) public view returns (uint256) {
        return Math.max(MIN_SEIZE, uint256(outstanding) * minSeizeBps / BPS);
    }

    /// @inheritdoc ISettlementAdapter
    function nonceUsed(address account, uint256 nonce) external view returns (bool) {
        return _nonceUsed[account][nonce];
    }

    /// @inheritdoc ISettlementAdapter
    function preLiquidationTerms(address account) external view returns (PreLiquidationTerms memory) {
        return _preLiquidation[account];
    }

    /// @inheritdoc ISettlementAdapter
    function hashCloseRequest(CloseRequest calldata req) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashCloseRequest(req));
    }

    /// @inheritdoc ISettlementAdapter
    function hashDeleverageRequest(DeleverageRequest calldata req) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashDeleverageRequest(req));
    }

    /// @inheritdoc ISettlementAdapter
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ internal

    /// @dev The checks of docs/INTERFACES.md § 3.3, in that order, plus non-zero commitments and the vault binding.
    /// @return outstanding The vault's outstanding zatoshi (the vault is Active).
    function _checkCloseRequest(CloseRequest calldata req, bytes calldata userSig, bytes32 mmCommitment)
        internal
        view
        returns (uint64 outstanding)
    {
        if (msg.sender != req.executor) revert NotExecutor(msg.sender, req.executor);
        if (block.timestamp > req.deadline) revert RequestExpired(req.deadline);
        if (_nonceUsed[req.account][req.nonce]) revert NonceAlreadyUsed(req.account, req.nonce);
        if (req.marginBps > MAX_MARGIN_BPS) revert MarginTooHigh(req.marginBps, MAX_MARGIN_BPS);
        if (mmCommitment == bytes32(0) || req.userCommitment == bytes32(0)) revert ZeroCommitment();
        if (req.vaultId == bytes32(0) || registry.vaultOf(req.account) != req.vaultId) {
            revert VaultAccountMismatch(req.vaultId, req.account);
        }
        VaultStatus status;
        (, outstanding, status) = registry.vault(req.vaultId);
        if (status != VaultStatus.Active) revert VaultNotActive(req.vaultId);
        // ERC-1271 for the (deployed) smart account; plain ECDSA if the account has no code.
        bytes32 digest = _hashTypedDataV4(ZecuredEIP712.hashCloseRequest(req));
        if (!SignatureChecker.isValidSignatureNowCalldata(req.account, digest, userSig)) revert InvalidUserSignature();
    }

    /// @dev The checks of a DeleverageRequest, as `_checkCloseRequest`'s (I8: a valid, unexpired, unused request signed by
    ///      the account, executed by its executor), plus a non-zero repayment.
    function _checkDeleverageRequest(DeleverageRequest calldata req, bytes calldata userSig, bytes32 mmCommitment)
        internal
        view
        returns (uint64 outstanding)
    {
        if (msg.sender != req.executor) revert NotExecutor(msg.sender, req.executor);
        if (block.timestamp > req.deadline) revert RequestExpired(req.deadline);
        if (_nonceUsed[req.account][req.nonce]) revert NonceAlreadyUsed(req.account, req.nonce);
        if (req.marginBps > MAX_MARGIN_BPS) revert MarginTooHigh(req.marginBps, MAX_MARGIN_BPS);
        if (mmCommitment == bytes32(0)) revert ZeroCommitment();
        if (req.repayAssets == 0) revert ZeroRepay();
        if (req.vaultId == bytes32(0) || registry.vaultOf(req.account) != req.vaultId) {
            revert VaultAccountMismatch(req.vaultId, req.account);
        }
        VaultStatus status;
        (, outstanding, status) = registry.vault(req.vaultId);
        if (status != VaultStatus.Active) revert VaultNotActive(req.vaultId);
        bytes32 digest = _hashTypedDataV4(ZecuredEIP712.hashDeleverageRequest(req));
        if (!SignatureChecker.isValidSignatureNowCalldata(req.account, digest, userSig)) revert InvalidUserSignature();
    }

    /// @dev Repays all of the account's borrow shares (read in this call) with USDC pulled from the executor inside
    ///      onMorphoRepay, then splits `outstanding`: the market maker gets the repaid debt plus margin in zatoshi
    ///      (rounded up), the borrower the rest, which must be at least `req.minUserAmount`.
    function _repayAndSplit(CloseRequest calldata req, uint64 outstanding, uint256 maxRepayAssets)
        internal
        returns (uint64 mmAmount, uint64 userAmount)
    {
        uint256 repaidAssets = _repayAllDebt(req.account);
        if (repaidAssets > maxRepayAssets) revert RepayAboveMaximum(repaidAssets, maxRepayAssets);
        uint256 mm = _marketMakerAmount(repaidAssets, req.marginBps);
        if (mm > outstanding) revert CloseExceedsCollateral(mm, outstanding);
        // Both parts are <= outstanding (a uint64): the checked cast cannot fail.
        mmAmount = SafeCast.toUint64(mm);
        userAmount = outstanding - mmAmount;
        if (userAmount < req.minUserAmount) revert UserAmountBelowMinimum(userAmount, req.minUserAmount);
    }

    /// @dev Pre-liquidation step 1: the account's terms are in force, its vault is Active and the slasher does not report
    ///      it frozen (the Registry refuses a frozen vault's exits too; this fails before any transfer).
    function _preLiquidatableVault(address account) internal view returns (bytes32 vaultId, uint64 outstanding) {
        if (block.timestamp >= _preLiquidation[account].expiry) revert PreLiquidationInactive(account);
        vaultId = registry.vaultOf(account);
        if (vaultId == bytes32(0)) revert NotAVaultAccount(account);
        VaultStatus status;
        (, outstanding, status) = registry.vault(vaultId);
        if (status != VaultStatus.Active) revert VaultNotActive(vaultId);
        if (_slasher != address(0) && IZecuredSlasher(_slasher).vaultFrozen(vaultId)) revert VaultFrozen(vaultId);
    }

    /// @dev Pre-liquidation steps 2 to 6 (see `preLiquidate`), with Morpho's PreLiquidation arithmetic: interest
    ///      accrued, the LTV band, the close factor's cap and the collateral the repayment buys, rounded down, within the
    ///      D11 floors. Read before the repayment, on the totals Morpho repays with.
    function _preLiquidationSeize(address account, uint256 repaidShares, uint64 outstanding) internal returns (uint64) {
        morpho.accrueInterest(_marketParams());
        Market memory m = morpho.market(marketId);
        uint256 price = oracle.price();
        (uint256 repayable, uint256 preLif) = _preLiquidationQuote(account, m, price);
        if (repaidShares > repayable) revert PreLiquidationTooLarge(repaidShares, repayable);
        // price > 0 here: the band check needs a non-zero collateral value.
        uint256 s = repaidShares.toAssetsDown(m.totalBorrowAssets, m.totalBorrowShares)
            .wMulDown(preLif)
            .mulDivDown(ORACLE_PRICE_SCALE, price);
        uint256 floor = minSeize(outstanding);
        if (s < floor) revert SeizeBelowMinimum(s, floor);
        if (s + floor > outstanding) {
            revert PreLiquidationRemainderTooSmall(outstanding - Math.min(s, outstanding), floor);
        }
        return SafeCast.toUint64(s); // < outstanding, a uint64
    }

    /// @dev Pre-liquidation steps 3 to 5: `preLltv < ltv <= LLTV` with Morpho's rounding (the upper bound is its health
    ///      check, so a position Morpho can liquidate is never pre-liquidated), then the close and incentive factors,
    ///      linear in `q = (ltv - preLltv) / (LLTV - preLltv)` (ltv rounded up, `q` and the factors rounded down).
    /// @return repayable The most borrow shares this pre-liquidation may repay: borrowShares * preLcf, rounded down.
    /// @return preLif The incentive factor (WAD).
    function _preLiquidationQuote(address account, Market memory m, uint256 price)
        internal
        view
        returns (uint256 repayable, uint256 preLif)
    {
        Position memory p = morpho.position(marketId, account);
        if (p.borrowShares == 0) revert NoDebt(account);
        PreLiquidationTerms storage t = _preLiquidation[account];
        uint256 preLltv = t.preLltv;
        uint256 collateralQuoted = uint256(p.collateral).mulDivDown(price, ORACLE_PRICE_SCALE);
        uint256 borrowed = uint256(p.borrowShares).toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);
        {
            uint256 maxBorrow = collateralQuoted.wMulDown(_lltv);
            if (borrowed > maxBorrow) revert PositionLiquidatable(borrowed, maxBorrow);
            uint256 preLiquidationBorrow = collateralQuoted.wMulDown(preLltv);
            // Both checks together make collateralQuoted non-zero (borrowed > 0: the account has borrow shares).
            if (borrowed <= preLiquidationBorrow) revert NotPreLiquidatable(borrowed, preLiquidationBorrow);
        }
        uint256 q = (borrowed.wDivUp(collateralQuoted) - preLltv).wDivDown(_lltv - preLltv);
        repayable = uint256(p.borrowShares).wMulDown(q.wMulDown(t.preLcf2 - t.preLcf1) + t.preLcf1);
        preLif = q.wMulDown(t.preLif2 - t.preLif1) + t.preLif1;
    }

    /// @dev Withdraws `amount` of `account`'s collateral to this Adapter. Y5: the zZEC expectation is armed immediately
    ///      before the Morpho call and settled right after (consumed, cleared, amount from the guard's receipt).
    function _withdrawCollateralToSelf(address account, uint64 amount) internal {
        zzec.expectFromMorpho(amount);
        morpho.withdrawCollateral(_marketParams(), amount, account, address(this));
        uint256 received = zzec.settleExpectation();
        if (received != amount) revert UnexpectedCollateralReceived(received, amount);
    }

    function _repayAllDebt(address account) internal returns (uint256 repaidAssets) {
        uint256 borrowShares = morpho.position(marketId, account).borrowShares;
        if (borrowShares == 0) revert NoDebt(account);
        _op = OP_CLOSE;
        _payer = msg.sender; // the executor (checked == req.executor)
        // Repaying by shares leaves no dust shares; Morpho reports the exact USDC in the callback.
        (repaidAssets,) = morpho.repay(_marketParams(), 0, borrowShares, account, abi.encode(OP_CLOSE));
        _op = OP_NONE;
        _payer = address(0);
    }

    /// @dev mmAmount = ceil(repaidAssets * (10_000 + marginBps) * 1e36 / (10_000 * price)), in zatoshi.
    ///      `price` is Morpho-scaled (loan units per collateral unit * 1e36), so `assets * 1e36 / price` converts USDC
    ///      units to zatoshi. 512-bit intermediate (OpenZeppelin Math.mulDiv), rounded up in the market maker's favour.
    function _marketMakerAmount(uint256 repaidAssets, uint16 marginBps) internal view returns (uint256) {
        uint256 price = oracle.price();
        if (price == 0) revert ZeroPrice();
        return Math.mulDiv(repaidAssets * (BPS + marginBps), ORACLE_PRICE_SCALE, BPS * price, Math.Rounding.Ceil);
    }

    function _marketParams() internal view returns (MarketParams memory) {
        return MarketParams({
            loanToken: address(loanToken),
            collateralToken: address(zzec),
            oracle: address(oracle),
            irm: _irm,
            lltv: _lltv
        });
    }
}
