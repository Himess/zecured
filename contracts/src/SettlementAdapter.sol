// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {IMorphoLiquidateCallback, IMorphoRepayCallback} from "morpho-blue/src/interfaces/IMorphoCallbacks.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";
import {IZecuredRegistry, VaultStatus} from "./interfaces/IZecuredRegistry.sol";
import {ISettlementAdapter} from "./interfaces/ISettlementAdapter.sol";
import {ZecuredEIP712, CloseRequest} from "./libraries/ZecuredEIP712.sol";

/// @title SettlementAdapter
/// @notice The only way, besides the account's own `requestRelease`, for zZEC collateral to leave Morpho
///         (docs/INTERFACES.md § 3.3):
///         - `liquidate`: a permissionless Morpho liquidation routed through this Adapter; the liquidator pays USDC in
///           Morpho's callback and is paid in ZEC on Zcash through a Liquidation claim (dust floor D11);
///         - `closeWithCollateral`: a market maker (the request's `executor`) repays the whole debt in USDC and is paid
///           in ZEC from the vault; the borrower's remainder goes to their own commitment (I7, I8).
/// @dev Morpho authorization is all-or-nothing, so this contract calls only `liquidate`, `repay` and
///      `withdrawCollateral` (the latter only for a valid signed CloseRequest whose executor is the caller, I8). It has no
///      code path that borrows or withdraws loan assets, no admin and no upgrade path.
contract SettlementAdapter is ISettlementAdapter, EIP712, ReentrancyGuardTransient {
    using SafeERC20 for IERC20;
    using MarketParamsLib for MarketParams;

    /// @inheritdoc ISettlementAdapter
    uint256 public constant MIN_SEIZE = 100_000;
    /// @inheritdoc ISettlementAdapter
    uint16 public constant MAX_MARGIN_BPS = 500;
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

    mapping(address account => mapping(uint256 nonce => bool used)) private _nonceUsed;

    // EIP-1153: the operation in flight and the address that pays its USDC (liquidator or executor).
    uint8 private transient _op;
    address private transient _payer;

    /// @dev Deployed right after the Registry, at the address the Registry and zZEC were built with.
    constructor(IZecuredRegistry registry_) EIP712(ZecuredEIP712.ADAPTER_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION) {
        IZZEC zzec_ = registry_.zzec();
        if (
            registry_.adapter() != address(this) || zzec_.adapter() != address(this)
                || zzec_.registry() != address(registry_)
        ) revert AdapterMisconfigured();
        MarketParams memory params = registry_.marketParams();

        registry = registry_;
        morpho = registry_.morpho();
        zzec = zzec_;
        loanToken = IERC20(params.loanToken);
        oracle = IOracle(params.oracle);
        _irm = params.irm;
        _lltv = params.lltv;
        marketId = params.id();
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
        if (seized < MIN_SEIZE && seized != outstanding && morpho.position(marketId, borrower).borrowShares != 0) {
            revert SeizeBelowMinimum(seized, MIN_SEIZE);
        }

        // seized <= outstanding (a uint64): the checked cast cannot fail.
        registry.recordLiquidation(vaultId, SafeCast.toUint64(seized), liquidatorCommitment);
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
        registry.recordClose(req.vaultId, mmAmount, userAmount, mmCommitment, req.userCommitment);
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

    // ------------------------------------------------------------------ views

    /// @inheritdoc ISettlementAdapter
    function nonceUsed(address account, uint256 nonce) external view returns (bool) {
        return _nonceUsed[account][nonce];
    }

    /// @inheritdoc ISettlementAdapter
    function hashCloseRequest(CloseRequest calldata req) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashCloseRequest(req));
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
