// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IMorpho, MarketParams, Id, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails,
    VaultDetails,
    RegistryConfig
} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommits, LineageNote, SpendRecord, ClaimPayment} from "./interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {ZecuredEIP712, SpendCommit} from "./libraries/ZecuredEIP712.sol";
import {ZecuredRegistryStorage} from "./registry/ZecuredRegistryStorage.sol";
import {ZecuredSpendLogic} from "./registry/ZecuredSpendLogic.sol";

/// @title ZecuredRegistry (v3)
/// @notice The core ledger of Zecured on Base (docs/INTERFACES.md § 3.2; design notes T-01 to T-06, T-11):
///         - vaults and their claims; every burn of zZEC creates a claim, a payout instruction for the signers (I3);
///         - Tier 3 (docs/TIER3.md): `commitSpend` records every payout of a lineage note before the signers sign it,
///           so that a theft is provable (`ZecuredSlasher`);
///         - the exits: `requestRelease`, `requestPartialRelease` (D14), and the SettlementAdapter's
///           `recordLiquidation` / `recordClose` (I7, I9), all refused while the slasher reports the vault frozen;
///         - deposits arrive only through `ZecuredIntake` (the hooks below), which verifies deposit proofs and gates
///           minting; every hook re-checks the ledger's own invariants (I4, I5, unique lineage tags);
///         - Refund claims (T-02): a proven deposit that cannot activate its vault is refunded through a claim whose
///           destination the vault's account gives; a Fee claim (T-06); the bonds' and the compensator's hooks (T-04,
///           T-05).
/// @dev Morpho authorization is all-or-nothing: an account that authorizes the Registry lets it do anything on its
///      behalf. This contract therefore calls exactly two Morpho functions: `supplyCollateral` (needs no authorization)
///      and `withdrawCollateral` (only inside the account's own `requestRelease` / `requestPartialRelease`, and in
///      `retireVault` for a vault an executed theft case compromised, down to what its remaining debt needs). It has
///      no code path
///      that borrows or withdraws loan assets, and it is not upgradeable and has no admin. Its storage layout is
///      `ZecuredRegistryStorage`'s; `commitSpend` runs `ZecuredSpendLogic`'s code on it by delegatecall (T-11).
contract ZecuredRegistry is IZecuredRegistry, IZecuredSpendCommits, ZecuredRegistryStorage {
    using MarketParamsLib for MarketParams;
    using SharesMathLib for uint256;

    /// @inheritdoc IZecuredRegistry
    uint256 public constant THRESHOLD = 3;
    /// @notice Size of the signer set.
    uint256 public constant SIGNER_COUNT = 5;
    /// @inheritdoc IZecuredRegistry
    uint64 public constant MIN_RELEASE = 100_000;
    /// @inheritdoc IZecuredRegistry
    uint256 public constant MAX_WITHDRAW_LTV_BPS = 5_000;
    uint256 private constant BPS = 10_000;
    /// @notice Highest opening fee, in basis points (10 %).
    uint16 public constant MAX_FEE_BPS = 1_000;
    /// @inheritdoc IZecuredSpendCommits
    uint256 public constant MAX_CLAIMS_PER_PAYOUT = 8;
    /// @inheritdoc IZecuredSpendCommits
    /// @dev ZIP 317: 5,000 zatoshi per logical action, at least 2 actions; a payout spends one lineage note, so its
    ///      actions are its outputs (claims plus change). The signers' plans charge exactly this
    ///      (`zecured_zcash_ops::spend::zip317_fee`).
    uint64 public constant ZIP317_MARGINAL_FEE = 5_000;
    /// @inheritdoc IZecuredRegistry
    uint64 public constant DUST_LIMIT = 10_000;

    /// @inheritdoc IZecuredRegistry
    IMorpho public immutable morpho;
    /// @inheritdoc IZecuredRegistry
    IZZEC public immutable zzec;
    /// @inheritdoc IZecuredRegistry
    address public immutable adapter;
    /// @inheritdoc IZecuredRegistry
    Id public immutable marketId;
    /// @inheritdoc IZecuredRegistry
    address public immutable spendLogic;
    /// @inheritdoc IZecuredRegistry
    address public immutable intake;
    /// @inheritdoc IZecuredSpendCommits
    address public immutable signerBond;
    /// @inheritdoc IZecuredRegistry
    address public immutable slasher;
    /// @inheritdoc IZecuredRegistry
    address public immutable compensator;
    /// @inheritdoc IZecuredRegistry
    uint16 public immutable feeBps;
    /// @inheritdoc IZecuredRegistry
    uint64 public immutable minFeeZat;
    /// @inheritdoc IZecuredRegistry
    bytes32 public immutable feeCommitment;
    /// @inheritdoc IZecuredRegistry
    uint64 public immutable refundAbandonPeriod;

    // MarketParams cannot be an immutable struct: its fields are stored one by one (collateral token = zzec).
    address private immutable _loanToken;
    address private immutable _oracle;
    address private immutable _irm;
    uint256 private immutable _lltv;

    /// @param c See `RegistryConfig`. Creates the zZEC market on Morpho unless it already exists (market creation is
    ///        permissionless, so someone may have created it between the zZEC deployment and this one).
    constructor(RegistryConfig memory c) ZecuredRegistryStorage(c.signers) {
        if (
            address(c.morpho) == address(0) || address(c.zzec) == address(0) || c.adapter == address(0)
                || c.loanToken == address(0) || c.oracle == address(0) || c.intake == address(0)
                || c.spendLogic == address(0)
        ) revert ZeroAddress();
        if (c.zzec.registry() != address(this) || c.zzec.adapter() != c.adapter || c.zzec.morpho() != address(c.morpho))
        {
            revert ZZECMisconfigured();
        }
        // The spend logic must have been built for this Registry and this signer set.
        address[5] memory logicSigners = ZecuredSpendLogic(c.spendLogic).signers();
        if (ZecuredSpendLogic(c.spendLogic).registry() != address(this)) revert SpendLogicMismatch();
        for (uint256 i; i < SIGNER_COUNT; ++i) {
            if (logicSigners[i] != c.signers[i]) revert SpendLogicMismatch();
        }
        // T-06: a fee needs a treasury and must leave room for its own output (it bears ZIP317_MARGINAL_FEE) above dust.
        if (
            c.feeBps > MAX_FEE_BPS || (c.feeBps == 0) != (c.feeCommitment == bytes32(0))
                || (c.feeBps != 0 && (c.minFeeZat <= DUST_LIMIT || c.minFeeZat <= ZIP317_MARGINAL_FEE))
                || c.refundAbandonPeriod == 0
        ) revert InvalidFeeConfig();

        morpho = c.morpho;
        zzec = c.zzec;
        adapter = c.adapter;
        spendLogic = c.spendLogic;
        intake = c.intake;
        signerBond = c.signerBond;
        slasher = c.slasher;
        compensator = c.compensator;
        feeBps = c.feeBps;
        minFeeZat = c.minFeeZat;
        feeCommitment = c.feeCommitment;
        refundAbandonPeriod = c.refundAbandonPeriod;
        _loanToken = c.loanToken;
        _oracle = c.oracle;
        _irm = c.irm;
        _lltv = c.lltv;

        MarketParams memory params = MarketParams({
            loanToken: c.loanToken, collateralToken: address(c.zzec), oracle: c.oracle, irm: c.irm, lltv: c.lltv
        });
        Id id = params.id();
        marketId = id;
        if (c.morpho.market(id).lastUpdate == 0) c.morpho.createMarket(params);
    }

    // ------------------------------------------------------------------ hooks: ZecuredIntake

    /// @inheritdoc IZecuredRegistry
    function reserveDeposit(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external {
        _only(intake, OnlyIntake.selector);
        if (vaultId == bytes32(0)) revert ZeroVaultId();
        if (account == address(0)) revert ZeroAccount();
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.None) revert VaultIdUsed(vaultId);
        bytes32 existing = _vaultOf[account];
        if (existing != bytes32(0)) revert AccountHasVault(account, existing);
        _addLineage(vaultId, amount, depositTag);
        _vaultOf[account] = vaultId;
        v.account = account;
        v.status = VaultStatus.Pending;
        _openVaults += 1;
        emit VaultReserved(vaultId, account, amount, depositTag);
    }

    /// @inheritdoc IZecuredRegistry
    function releaseDeposit(bytes32 vaultId, address account, bytes32 depositTag) external {
        _only(intake, OnlyIntake.selector);
        _reservation(vaultId, account, depositTag);
        delete _lineage[depositTag];
        _depositTagUsed[depositTag] = false;
        delete _vaultOf[account];
        // A pending vault has no claim (a refund proven meanwhile waits for it, R-8): it is unknown again.
        delete _vaults[vaultId];
        _settle();
        emit VaultReservationReleased(vaultId);
    }

    /// @inheritdoc IZecuredRegistry
    function activate(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external {
        _only(intake, OnlyIntake.selector);
        Vault storage v = _reservation(vaultId, account, depositTag);
        if (_lineage[depositTag].value != amount) revert ReservationMismatch(vaultId);
        // T-06: the opening fee, owed to the signers' treasury, unless too small to be worth its own output.
        uint64 fee = uint64(uint256(amount) * feeBps / BPS);
        if (fee < minFeeZat) fee = 0;
        uint64 minted = amount - fee;
        v.status = VaultStatus.Active;
        v.outstanding = minted;
        emit VaultActivated(vaultId, account, amount, depositTag);
        if (fee != 0) emit FeeCharged(vaultId, _newClaim(vaultId, v, ClaimKind.Fee, fee, feeCommitment), fee);

        // The Registry holds the zZEC only within this call: Morpho pulls exactly `minted` from it (I1).
        zzec.mint(address(this), minted);
        zzec.approve(address(morpho), minted);
        morpho.supplyCollateral(_marketParams(), minted, account, "");
    }

    /// @inheritdoc IZecuredRegistry
    function reserveRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external {
        _only(intake, OnlyIntake.selector);
        if (vaultId == bytes32(0)) revert ZeroVaultId();
        if (account == address(0)) revert ZeroAccount();
        // A compromised vault's claims are the compensator's; a late deposit must not add one it never tallies (and
        // that, unpayable, would keep the vault open for good). `openReservedRefund` checks it again.
        _checkNotCompromised(vaultId);
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.None && v.account != account) revert ReservationMismatch(vaultId);
        _addLineage(vaultId, amount, depositTag);
        _reservedRefund[depositTag] = account;
    }

    /// @inheritdoc IZecuredRegistry
    function convertToRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag)
        external
        returns (bytes32 claimId)
    {
        _only(intake, OnlyIntake.selector);
        Vault storage v = _reservation(vaultId, account, depositTag);
        if (_lineage[depositTag].value != amount) revert ReservationMismatch(vaultId);
        // The claim first, while the vault still counts as open.
        claimId = _newClaim(vaultId, v, ClaimKind.Refund, amount, bytes32(0));
        v.status = VaultStatus.Cancelled;
        emit RefundOpened(vaultId, claimId, account, amount, depositTag);
    }

    // ------------------------------------------------------------------ hooks: compensator (T-04), bond (T-05)

    /// @inheritdoc IZecuredRegistry
    function retireVault(bytes32 vaultId, uint64 amount) external returns (uint64 retired) {
        _only(compensator, OnlyCompensator.selector);
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active || !IZecuredSlasher(slasher).isCompromised(vaultId)) {
            revert NotRetirable(vaultId);
        }
        address account = v.account;
        uint64 outstanding = v.outstanding;
        if (amount == 0) revert ZeroAmount();
        if (amount > outstanding) revert SeizeExceedsOutstanding(vaultId, amount, outstanding);
        Position memory p = morpho.position(marketId, account);
        if (p.collateral != outstanding) revert CollateralMismatch(vaultId, p.collateral, outstanding);
        retired = amount;
        uint64 remaining = outstanding - amount;
        v.outstanding = remaining;
        if (remaining == 0) {
            if (p.borrowShares != 0) revert DebtOutstanding(vaultId);
            v.status = VaultStatus.Closed;
        }
        // Bookkeeping only (I11): the retired zatoshi, compensated in USDC, never paid on Zcash.
        uint64 sequence = v.claimCount;
        bytes32 claimId = claimIdOf(vaultId, sequence);
        v.claimCount = sequence + 1;
        _claims[claimId] = Claim({
            vaultId: vaultId,
            commitment: bytes32(0),
            amount: retired,
            kind: ClaimKind.Retired,
            status: ClaimStatus.Compensated,
            createdAt: uint40(block.timestamp),
            bondPaid: false,
            paidAt: 0,
            disputeDueAt: 0
        });
        emit VaultRetired(vaultId, claimId, retired);
        if (remaining == 0 && v.unpaidClaims == 0) _settle();

        // Morpho refuses a withdrawal that would leave the position unhealthy: with debt left, the compensator burns
        // the collateral down to the LLTV at most.
        _withdrawToRegistry(account, retired);
        zzec.burn(address(this), retired);
    }

    /// @inheritdoc IZecuredRegistry
    function settleCompensatedClaim(bytes32 claimId) external {
        _only(compensator, OnlyCompensator.selector);
        Claim storage c = _claims[claimId];
        if (c.status != ClaimStatus.Pending || !IZecuredSlasher(slasher).isCompromised(c.vaultId)) {
            revert NotCompensable(claimId);
        }
        _closeClaim(c, ClaimStatus.Compensated);
        emit ClaimCompensated(c.vaultId, claimId);
    }

    /// @inheritdoc IZecuredRegistry
    function markBondPaid(bytes32 claimId) external {
        _only(signerBond, OnlyBond.selector);
        Claim storage c = _claims[claimId];
        if (c.status != ClaimStatus.Pending || c.bondPaid) revert ClaimNotPayable(claimId);
        c.bondPaid = true;
        c.commitment = bytes32(0);
        emit ClaimBondPaid(c.vaultId, claimId);
    }

    // ------------------------------------------------------------------ exits

    /// @inheritdoc IZecuredRegistry
    function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId) {
        Vault storage v = _activeVault(vaultId);
        address account = v.account;
        if (msg.sender != account) revert NotVaultAccount(vaultId, msg.sender);
        if (destCommitment == bytes32(0)) revert ZeroCommitment();

        uint64 amount = v.outstanding;
        Position memory p = morpho.position(marketId, account);
        if (p.borrowShares != 0) revert DebtOutstanding(vaultId);
        if (p.collateral != amount) revert CollateralMismatch(vaultId, p.collateral, amount);

        v.outstanding = 0;
        // The claim first, then Closed: a closing exit never counts the vault as settled and reopened.
        claimId = _newClaim(vaultId, v, ClaimKind.Release, amount, destCommitment);
        v.status = VaultStatus.Closed;
        emit ReleaseRequested(vaultId, claimId, amount, destCommitment);

        _withdrawToRegistry(account, amount);
        zzec.burn(address(this), amount);
    }

    /// @inheritdoc IZecuredRegistry
    function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment)
        external
        returns (bytes32 claimId)
    {
        Vault storage v = _activeVault(vaultId);
        address account = v.account;
        if (msg.sender != account) revert NotVaultAccount(vaultId, msg.sender);
        if (destCommitment == bytes32(0)) revert ZeroCommitment();
        if (amount < MIN_RELEASE) revert ReleaseAmountTooSmall(amount, MIN_RELEASE);
        uint64 outstanding = v.outstanding;
        if (amount >= outstanding) revert ReleaseAmountNotPartial(amount, outstanding);

        uint64 remaining = outstanding - amount;
        // The rest must stay payable on its own: a remainder below MIN_RELEASE could not cover its payout fee.
        if (remaining < MIN_RELEASE) revert ReleaseRemainderTooSmall(remaining, MIN_RELEASE);
        v.outstanding = remaining;
        claimId = _newClaim(vaultId, v, ClaimKind.Release, amount, destCommitment);
        emit ReleaseRequested(vaultId, claimId, amount, destCommitment);

        // Morpho accrues interest and enforces its LLTV health check inside withdrawCollateral; the Registry then
        // applies the stricter D14 limit on the resulting position.
        _withdrawToRegistry(account, amount);
        _checkWithdrawLtv(vaultId, account, remaining);
        zzec.burn(address(this), amount);
    }

    /// @inheritdoc IZecuredRegistry
    function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment, address claimant)
        external
        returns (bytes32 claimId)
    {
        _only(adapter, OnlyAdapter.selector);
        Vault storage v = _activeVault(vaultId);
        if (seized == 0) revert ZeroAmount();
        uint64 outstanding = v.outstanding;
        if (seized > outstanding) revert SeizeExceedsOutstanding(vaultId, seized, outstanding);
        if (liquidatorCommitment == bytes32(0)) revert ZeroCommitment();

        uint64 remaining = outstanding - seized;
        v.outstanding = remaining;
        claimId = _newClaim(vaultId, v, ClaimKind.Liquidation, seized, liquidatorCommitment);
        if (remaining == 0) v.status = VaultStatus.Closed;
        _claimantOf[claimId] = claimant;
        emit LiquidationRelease(vaultId, claimId, seized, liquidatorCommitment);

        // Reverts unless the Adapter actually holds the seized zZEC.
        zzec.burn(adapter, seized);
    }

    /// @inheritdoc IZecuredRegistry
    function recordClose(
        bytes32 vaultId,
        uint64 mmAmount,
        uint64 userAmount,
        bytes32 mmCommitment,
        bytes32 userCommitment,
        address mmClaimant
    ) external returns (bytes32 userClaimId, bytes32 mmClaimId) {
        _only(adapter, OnlyAdapter.selector);
        Vault storage v = _activeVault(vaultId);
        uint64 outstanding = v.outstanding;
        if (uint256(mmAmount) + uint256(userAmount) != outstanding) {
            revert CloseAmountsMismatch(vaultId, mmAmount, userAmount, outstanding);
        }
        if (mmCommitment == bytes32(0) || userCommitment == bytes32(0)) revert ZeroCommitment();

        v.outstanding = 0;
        // The user claim first, then the market maker claim, then Closed (see `requestRelease`).
        userClaimId = _newClaim(vaultId, v, ClaimKind.CloseUser, userAmount, userCommitment);
        mmClaimId = _newClaim(vaultId, v, ClaimKind.CloseMarketMaker, mmAmount, mmCommitment);
        v.status = VaultStatus.Closed;
        _claimantOf[mmClaimId] = mmClaimant;
        emit CloseRelease(vaultId, userClaimId, mmClaimId, userAmount, mmAmount, userCommitment, mmCommitment);

        // Reverts unless the Adapter actually holds the whole outstanding.
        zzec.burn(adapter, outstanding);
    }

    // ------------------------------------------------------------------ payouts

    /// @inheritdoc IZecuredRegistry
    function confirmPayout(bytes32 vaultId, bytes32[] calldata claimIds, bytes[] calldata sigs) external {
        if (claimIds.length == 0) revert NoClaims();
        _checkNotCompromised(vaultId);
        _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, claimIds)), sigs, THRESHOLD
        );

        for (uint256 i; i < claimIds.length; ++i) {
            Claim storage c = _claims[claimIds[i]];
            ClaimStatus status = c.status;
            if (status == ClaimStatus.None) revert UnknownClaim(claimIds[i]);
            if (c.vaultId != vaultId) revert ClaimVaultMismatch(claimIds[i], vaultId);
            if (status == ClaimStatus.Paid) revert ClaimAlreadyPaid(claimIds[i]); // also rejects duplicate ids
            if (status != ClaimStatus.Pending) revert ClaimNotPayable(claimIds[i]);
            // Tier 3: a claim is paid by a committed payout only, which its claimant can check (F3).
            if (_claimInput[claimIds[i]] == bytes32(0)) revert ClaimNotCommitted(claimIds[i]);
            c.paidAt = uint40(block.timestamp); // its claimant may dispute this for `disputeWindow`
            _closeClaim(c, ClaimStatus.Paid);
        }
        emit PayoutConfirmed(vaultId, claimIds);
    }

    /// @inheritdoc IZecuredRegistry
    function writeOffDust(bytes32 claimId) external {
        Claim storage c = _claims[claimId];
        Vault storage v = _vaults[c.vaultId];
        if (
            c.status != ClaimStatus.Pending || c.amount > DUST_LIMIT || v.status != VaultStatus.Closed
                || _claimInput[claimId] != bytes32(0)
        ) revert NotDust(claimId);
        _checkNotCompromised(c.vaultId); // the compensator settles a compromised vault's claims
        emit DustWrittenOff(c.vaultId, claimId, c.amount);
        _closeClaim(c, ClaimStatus.WrittenOff);
    }

    // ------------------------------------------------------------------ T-02: refunds

    /// @inheritdoc IZecuredRegistry
    /// @dev Runs `ZecuredSpendLogic.setRefundDestination` on this contract's storage (R-11 made it outgrow the Registry).
    function setRefundDestination(bytes32, bytes32) external {
        _delegate();
    }

    /// @inheritdoc IZecuredRegistry
    /// @dev Runs `ZecuredSpendLogic.abandonRefund` on this contract's storage.
    function abandonRefund(bytes32) external {
        _delegate();
    }

    // ------------------------------------------------------------------ T-05: claims the bonds paid

    /// @inheritdoc IZecuredRegistry
    function setReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline, bytes[] calldata sigs)
        external
    {
        if (block.timestamp > deadline) revert SignatureExpired(deadline);
        Claim storage c = _claims[claimId];
        if (!c.bondPaid || c.status != ClaimStatus.Pending) revert NotBondPaid(claimId);
        if (c.commitment != bytes32(0)) revert DestinationAlreadySet(claimId);
        if (commitment == bytes32(0)) revert ZeroCommitment();
        _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashReimburseDestination(claimId, commitment, deadline)), sigs, THRESHOLD
        );
        c.commitment = commitment;
        emit ReimburseDestinationSet(c.vaultId, claimId, commitment);
    }

    // ------------------------------------------------------------------ Tier 3: spend commitments

    /// @inheritdoc IZecuredSpendCommits
    /// @dev Runs `ZecuredSpendLogic.commitSpend` on this contract's storage (T-11), after refusing a compromised vault.
    function commitSpend(SpendCommit calldata c, bytes[] calldata) external {
        _checkNotCompromised(c.vaultId);
        (bool ok, bytes memory ret) = spendLogic.delegatecall(msg.data);
        if (!ok) {
            assembly ("memory-safe") {
                revert(add(ret, 32), mload(ret))
            }
        }
    }

    /// @notice Every other call, the dispute path (`IZecuredDisputes`) above all, runs `ZecuredSpendLogic`'s code on this
    ///         contract's storage (T-11), which refuses what it does not implement. Not payable.
    fallback() external {
        _delegate();
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredRegistry
    function vaultOf(address account) external view returns (bytes32 vaultId) {
        return _vaultOf[account];
    }

    /// @inheritdoc IZecuredRegistry
    function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status) {
        Vault storage v = _vaults[vaultId];
        return (v.account, v.outstanding, v.status);
    }

    /// @inheritdoc IZecuredRegistry
    function vaultDetails(bytes32 vaultId) external view returns (VaultDetails memory) {
        Vault storage v = _vaults[vaultId];
        return VaultDetails({
            account: v.account,
            outstanding: v.outstanding,
            status: v.status,
            claimCount: v.claimCount,
            unpaidClaims: v.unpaidClaims
        });
    }

    /// @inheritdoc IZecuredRegistry
    function claim(bytes32 claimId)
        external
        view
        returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status)
    {
        Claim storage c = _claims[claimId];
        return (c.vaultId, c.kind, c.amount, c.commitment, c.status);
    }

    /// @inheritdoc IZecuredRegistry
    function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory d) {
        Claim storage c = _claims[claimId];
        d.vaultId = c.vaultId;
        d.kind = c.kind;
        d.status = c.status;
        d.amount = c.amount;
        d.commitment = c.commitment;
        d.createdAt = c.createdAt;
        d.bondPaid = c.bondPaid;
        if (c.kind == ClaimKind.Liquidation || c.kind == ClaimKind.CloseMarketMaker) {
            d.claimant = _claimantOf[claimId];
        } else if (c.kind != ClaimKind.Fee && c.kind != ClaimKind.Retired && c.status != ClaimStatus.None) {
            d.claimant = _vaults[c.vaultId].account;
        }
    }

    /// @inheritdoc IZecuredRegistry
    function lockedZatoshi() external view returns (uint256) {
        return zzec.totalSupply() + _pendingClaimZat;
    }

    /// @inheritdoc IZecuredRegistry
    function claimCount(bytes32 vaultId) external view returns (uint64) {
        return _vaults[vaultId].claimCount;
    }

    /// @inheritdoc IZecuredRegistry
    function claimIdOf(bytes32 vaultId, uint64 sequence) public pure returns (bytes32) {
        return keccak256(abi.encode(vaultId, sequence));
    }

    /// @inheritdoc IZecuredRegistry
    function marketParams() external view returns (MarketParams memory) {
        return _marketParams();
    }

    /// @inheritdoc IZecuredRegistry
    function signers() external view returns (address[5] memory) {
        return [_signer0, _signer1, _signer2, _signer3, _signer4];
    }

    /// @inheritdoc IZecuredRegistry
    function hashPayoutConfirmation(bytes32 vaultId, bytes32[] calldata claimIds) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, claimIds));
    }

    /// @inheritdoc IZecuredRegistry
    function hashReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline)
        external
        view
        returns (bytes32)
    {
        return _hashTypedDataV4(ZecuredEIP712.hashReimburseDestination(claimId, commitment, deadline));
    }

    /// @inheritdoc IZecuredRegistry
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    /// @inheritdoc IZecuredSpendCommits
    function lineageOf(bytes32 tag) external view returns (LineageNote memory) {
        return _lineage[tag];
    }

    /// @inheritdoc IZecuredSpendCommits
    function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory) {
        return _spends[payoutRef];
    }

    /// @inheritdoc IZecuredSpendCommits
    function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory) {
        return _claimPayments[claimRef];
    }

    /// @inheritdoc IZecuredSpendCommits
    function claimRefOf(bytes32 payoutRef, bytes32 commitment) external view returns (bytes32) {
        return _claimRefOf[payoutRef][commitment];
    }

    /// @inheritdoc IZecuredSpendCommits
    function hashSpendCommit(SpendCommit calldata c) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashSpendCommit(c));
    }

    /// @inheritdoc IZecuredRegistry
    function depositTagUsed(bytes32 depositTag) external view returns (bool) {
        return _depositTagUsed[depositTag];
    }

    /// @inheritdoc IZecuredSpendCommits
    function claimInput(bytes32 claimId) external view returns (bytes32) {
        return _claimInput[claimId];
    }

    /// @inheritdoc IZecuredSpendCommits
    function inputClaimSet(bytes32 inputTag) external view returns (bytes32) {
        return _inputClaimSet[inputTag];
    }

    /// @inheritdoc IZecuredSpendCommits
    function lastCommitAt(bytes32 inputTag) external view returns (uint40) {
        return _lastCommitAt[inputTag];
    }

    /// @inheritdoc IZecuredSpendCommits
    function openVaultCount() external view returns (uint256) {
        return _openVaults;
    }

    /// @inheritdoc IZecuredSpendCommits
    function lastVaultSettledAt() external view returns (uint64) {
        return _lastVaultSettledAt;
    }

    // ------------------------------------------------------------------ internal

    /// @dev Runs `ZecuredSpendLogic`'s code for this call on this contract's storage and ends the call with its result
    ///      (T-11). Never returns to Solidity code, so it may use memory from zero.
    function _delegate() internal {
        address logic = spendLogic;
        assembly {
            calldatacopy(0, 0, calldatasize())
            let ok := delegatecall(gas(), logic, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            if iszero(ok) { revert(0, returndatasize()) }
            return(0, returndatasize())
        }
    }

    /// @dev Reverts with `err(msg.sender)` unless the caller is `role`; a role that is not deployed (zero) admits nobody.
    function _only(address role, bytes4 err) internal view {
        if (msg.sender != role || role == address(0)) {
            assembly ("memory-safe") {
                mstore(0, err)
                mstore(4, caller())
                revert(0, 36)
            }
        }
    }

    /// @dev An active vault that the slasher does not report frozen (T-04: an open F1/F2 case on it, or an executed
    ///      one). Every exit goes through here.
    function _activeVault(bytes32 vaultId) internal view returns (Vault storage v) {
        v = _vaults[vaultId];
        if (v.status != VaultStatus.Active) revert VaultNotActive(vaultId);
        if (slasher != address(0) && IZecuredSlasher(slasher).vaultFrozen(vaultId)) revert VaultFrozen(vaultId);
    }

    function _checkNotCompromised(bytes32 vaultId) internal view {
        if (slasher != address(0) && IZecuredSlasher(slasher).isCompromised(vaultId)) revert VaultCompromised(vaultId);
    }

    /// @dev The pending reservation `(account, depositTag)` of `vaultId`.
    function _reservation(bytes32 vaultId, address account, bytes32 depositTag)
        internal
        view
        returns (Vault storage v)
    {
        v = _vaults[vaultId];
        if (v.status != VaultStatus.Pending) revert VaultNotPending(vaultId);
        if (v.account != account || _lineage[depositTag].vaultId != vaultId) revert ReservationMismatch(vaultId);
    }

    /// @dev I5 and RT-16: a deposit's tag marks one note, used once, never an existing lineage tag.
    function _addLineage(bytes32 vaultId, uint64 amount, bytes32 depositTag) internal {
        if (amount == 0) revert ZeroAmount();
        if (depositTag == bytes32(0)) revert ZeroDepositTag();
        if (_depositTagUsed[depositTag] || _lineage[depositTag].vaultId != bytes32(0)) {
            revert DepositTagUsed(depositTag);
        }
        _depositTagUsed[depositTag] = true;
        _lineage[depositTag] = LineageNote({vaultId: vaultId, value: amount});
    }

    /// @dev Withdraws `amount` of `account`'s collateral to the Registry, using the account's Morpho authorization of
    ///      the Registry. Y5: arm the zZEC expectation immediately before the Morpho call (the guard then admits exactly
    ///      this transfer), then settle it, which requires the transfer to have consumed it, clears it whatever
    ///      happened, and reports the amount the guard let through.
    function _withdrawToRegistry(address account, uint64 amount) internal {
        zzec.expectFromMorpho(amount);
        morpho.withdrawCollateral(_marketParams(), amount, account, address(this));
        uint256 received = zzec.settleExpectation();
        if (received != amount) revert UnexpectedCollateralReceived(received, amount);
    }

    /// @dev D14: the position's LTV after a partial release must be <= MAX_WITHDRAW_LTV_BPS.
    ///      Called right after Morpho.withdrawCollateral, which has already accrued interest for this block, so the
    ///      market totals read here are current and `debt` is exactly the amount Morpho's own health check used:
    ///      borrow shares converted to loan units with Morpho's share math, rounded up. The collateral value is
    ///      collateral * price / 1e36, rounded down. A position without debt always passes.
    function _checkWithdrawLtv(bytes32 vaultId, address account, uint64 remaining) internal view {
        Position memory p = morpho.position(marketId, account);
        if (p.collateral != remaining) revert CollateralMismatch(vaultId, p.collateral, remaining);
        if (p.borrowShares == 0) return;
        Market memory m = morpho.market(marketId);
        uint256 debt = uint256(p.borrowShares).toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);
        uint256 collateralValue = Math.mulDiv(p.collateral, IOracle(_oracle).price(), ORACLE_PRICE_SCALE);
        if (debt * BPS > collateralValue * MAX_WITHDRAW_LTV_BPS) revert ReleaseLtvTooHigh(debt, collateralValue);
    }

    function _marketParams() internal view returns (MarketParams memory) {
        return
            MarketParams({
                loanToken: _loanToken, collateralToken: address(zzec), oracle: _oracle, irm: _irm, lltv: _lltv
            });
    }
}
