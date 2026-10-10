// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {IMorpho, MarketParams, Id, Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";
import {IZecuredRegistry, VaultStatus, ClaimKind, ClaimStatus} from "./interfaces/IZecuredRegistry.sol";
import {ZecuredEIP712, DepositAttestation} from "./libraries/ZecuredEIP712.sol";

/// @title ZecuredRegistry
/// @notice Book-keeping for Zecured vaults on Base (docs/INTERFACES.md § 3.2):
///         - `attestDeposit`: a 3-of-5 signer attestation activates a vault, mints its zZEC and supplies it straight
///           into the account's Morpho position (I1, I4, I5, I6);
///         - `requestRelease`: the account itself withdraws and burns its whole outstanding once the debt is repaid;
///         - `recordLiquidation` / `recordClose`: the SettlementAdapter reports seized / closed collateral, which the
///           Registry burns (I7, I9);
///         - every burn creates a claim (payout instruction for the signers, I3); `confirmPayout` marks claims paid.
/// @dev Morpho authorization is all-or-nothing: an account that authorizes the Registry lets it do anything on its
///      behalf. This contract therefore calls exactly two Morpho functions: `supplyCollateral` (needs no authorization)
///      and `withdrawCollateral` (only inside the account's own `requestRelease`, to the Registry). It has no code path
///      that borrows or withdraws loan assets, and it is not upgradeable and has no admin.
contract ZecuredRegistry is IZecuredRegistry, EIP712 {
    using MarketParamsLib for MarketParams;

    /// @inheritdoc IZecuredRegistry
    uint256 public constant THRESHOLD = 3;
    /// @notice Size of the signer set.
    uint256 public constant SIGNER_COUNT = 5;

    /// @inheritdoc IZecuredRegistry
    IMorpho public immutable morpho;
    /// @inheritdoc IZecuredRegistry
    IZZEC public immutable zzec;
    /// @inheritdoc IZecuredRegistry
    address public immutable adapter;
    /// @inheritdoc IZecuredRegistry
    Id public immutable marketId;

    // MarketParams cannot be an immutable struct: its fields are stored one by one (collateral token = zzec).
    address private immutable _loanToken;
    address private immutable _oracle;
    address private immutable _irm;
    uint256 private immutable _lltv;

    // The signer set, strictly increasing (fixed at construction).
    address private immutable _signer0;
    address private immutable _signer1;
    address private immutable _signer2;
    address private immutable _signer3;
    address private immutable _signer4;

    struct Vault {
        address account;
        uint64 outstanding; // zatoshi still locked (= zZEC supplied to Morpho for this vault)
        VaultStatus status;
        uint64 claimCount; // sequence number of the next claim
    }

    struct Claim {
        bytes32 vaultId;
        bytes32 commitment; // keccak256(salt32 || receiver43) of the Ironwood recipient
        uint64 amount; // zatoshi
        ClaimKind kind;
        ClaimStatus status;
    }

    mapping(bytes32 vaultId => Vault) private _vaults;
    mapping(address account => bytes32 vaultId) private _vaultOf;
    mapping(bytes32 claimId => Claim) private _claims;
    /// @inheritdoc IZecuredRegistry
    mapping(bytes32 depositTag => bool used) public depositTagUsed;

    modifier onlyAdapter() {
        _checkAdapter();
        _;
    }

    /// @param morpho_ Morpho Blue.
    /// @param zzec_ The zZEC token, already deployed with `registry = address(this)` and `adapter = adapter_`
    ///        (predicted CREATE addresses, see contracts/README.md).
    /// @param adapter_ The SettlementAdapter's (predicted) address; it is deployed right after this contract.
    /// @param loanToken_ Loan token of the market (MockUSDC on Base Sepolia).
    /// @param oracle_ ZEC/USD oracle of the market (MockZecOracle on Base Sepolia).
    /// @param irm_ Interest rate model of the market (must be enabled on Morpho).
    /// @param lltv_ Liquidation LTV of the market (must be enabled on Morpho).
    /// @param signers_ The 5 signer addresses, strictly increasing.
    /// @dev Creates the zZEC market on Morpho unless it already exists (market creation is permissionless, so someone
    ///      may have created it between the zZEC deployment and this one).
    constructor(
        IMorpho morpho_,
        IZZEC zzec_,
        address adapter_,
        address loanToken_,
        address oracle_,
        address irm_,
        uint256 lltv_,
        address[5] memory signers_
    ) EIP712(ZecuredEIP712.REGISTRY_DOMAIN_NAME, ZecuredEIP712.DOMAIN_VERSION) {
        if (
            address(morpho_) == address(0) || address(zzec_) == address(0) || adapter_ == address(0)
                || loanToken_ == address(0) || oracle_ == address(0)
        ) revert ZeroAddress();
        if (zzec_.registry() != address(this) || zzec_.adapter() != adapter_ || zzec_.morpho() != address(morpho_)) {
            revert ZZECMisconfigured();
        }
        address previous = address(0);
        for (uint256 i; i < SIGNER_COUNT; ++i) {
            if (signers_[i] <= previous) revert InvalidSignerSet(); // strictly increasing => distinct and non-zero
            previous = signers_[i];
        }

        morpho = morpho_;
        zzec = zzec_;
        adapter = adapter_;
        _loanToken = loanToken_;
        _oracle = oracle_;
        _irm = irm_;
        _lltv = lltv_;
        _signer0 = signers_[0];
        _signer1 = signers_[1];
        _signer2 = signers_[2];
        _signer3 = signers_[3];
        _signer4 = signers_[4];

        MarketParams memory params = MarketParams({
            loanToken: loanToken_, collateralToken: address(zzec_), oracle: oracle_, irm: irm_, lltv: lltv_
        });
        Id id = params.id();
        marketId = id;
        if (morpho_.market(id).lastUpdate == 0) morpho_.createMarket(params);
    }

    // ------------------------------------------------------------------ deposits

    /// @inheritdoc IZecuredRegistry
    function attestDeposit(DepositAttestation calldata att, bytes[] calldata sigs) external {
        if (att.vaultId == bytes32(0)) revert ZeroVaultId();
        if (att.account == address(0)) revert ZeroAccount();
        if (att.amount == 0) revert ZeroAmount();
        if (att.depositTag == bytes32(0)) revert ZeroDepositTag();
        if (_vaults[att.vaultId].status != VaultStatus.None) revert VaultIdUsed(att.vaultId);
        if (depositTagUsed[att.depositTag]) revert DepositTagUsed(att.depositTag);
        bytes32 existing = _vaultOf[att.account];
        if (existing != bytes32(0)) revert AccountHasVault(att.account, existing);

        _verifySignerSignatures(_hashTypedDataV4(ZecuredEIP712.hashDepositAttestation(att)), sigs);

        depositTagUsed[att.depositTag] = true;
        _vaultOf[att.account] = att.vaultId;
        _vaults[att.vaultId] =
            Vault({account: att.account, outstanding: att.amount, status: VaultStatus.Active, claimCount: 0});
        emit VaultActivated(att.vaultId, att.account, att.amount);

        // The Registry holds the zZEC only within this call: Morpho pulls exactly `amount` from it.
        zzec.mint(address(this), att.amount);
        zzec.approve(address(morpho), att.amount);
        morpho.supplyCollateral(_marketParams(), att.amount, att.account, "");
    }

    // ------------------------------------------------------------------ exits

    /// @inheritdoc IZecuredRegistry
    function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId) {
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active) revert VaultNotActive(vaultId);
        address account = v.account;
        if (msg.sender != account) revert NotVaultAccount(vaultId, msg.sender);
        if (destCommitment == bytes32(0)) revert ZeroCommitment();

        uint64 amount = v.outstanding;
        Position memory p = morpho.position(marketId, account);
        if (p.borrowShares != 0) revert DebtOutstanding(vaultId);
        if (p.collateral != amount) revert CollateralMismatch(vaultId, p.collateral, amount);

        v.outstanding = 0;
        v.status = VaultStatus.Closed;
        claimId = _newClaim(vaultId, v, ClaimKind.Release, amount, destCommitment);
        emit ReleaseRequested(vaultId, claimId, amount, destCommitment);

        // Uses the account's Morpho authorization of the Registry; the zZEC guard admits exactly this transfer.
        uint256 balanceBefore = zzec.balanceOf(address(this));
        zzec.expectFromMorpho(amount);
        morpho.withdrawCollateral(_marketParams(), amount, account, address(this));
        uint256 received = zzec.balanceOf(address(this)) - balanceBefore;
        if (received != amount) revert UnexpectedCollateralReceived(received, amount);
        zzec.burn(address(this), amount);
    }

    /// @inheritdoc IZecuredRegistry
    function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment)
        external
        onlyAdapter
        returns (bytes32 claimId)
    {
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active) revert VaultNotActive(vaultId);
        if (seized == 0) revert ZeroAmount();
        uint64 outstanding = v.outstanding;
        if (seized > outstanding) revert SeizeExceedsOutstanding(vaultId, seized, outstanding);
        if (liquidatorCommitment == bytes32(0)) revert ZeroCommitment();

        uint64 remaining = outstanding - seized;
        v.outstanding = remaining;
        if (remaining == 0) v.status = VaultStatus.Closed;
        claimId = _newClaim(vaultId, v, ClaimKind.Liquidation, seized, liquidatorCommitment);
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
        bytes32 userCommitment
    ) external onlyAdapter returns (bytes32 userClaimId, bytes32 mmClaimId) {
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active) revert VaultNotActive(vaultId);
        uint64 outstanding = v.outstanding;
        if (uint256(mmAmount) + uint256(userAmount) != outstanding) {
            revert CloseAmountsMismatch(vaultId, mmAmount, userAmount, outstanding);
        }
        if (mmCommitment == bytes32(0) || userCommitment == bytes32(0)) revert ZeroCommitment();

        v.outstanding = 0;
        v.status = VaultStatus.Closed;
        // The user claim first, then the market maker claim.
        userClaimId = _newClaim(vaultId, v, ClaimKind.CloseUser, userAmount, userCommitment);
        mmClaimId = _newClaim(vaultId, v, ClaimKind.CloseMarketMaker, mmAmount, mmCommitment);
        emit CloseRelease(vaultId, userClaimId, mmClaimId, userAmount, mmAmount, userCommitment, mmCommitment);

        // Reverts unless the Adapter actually holds the whole outstanding.
        zzec.burn(adapter, outstanding);
    }

    // ------------------------------------------------------------------ payouts

    /// @inheritdoc IZecuredRegistry
    function confirmPayout(bytes32 vaultId, bytes32[] calldata claimIds, bytes[] calldata sigs) external {
        if (claimIds.length == 0) revert NoClaims();
        _verifySignerSignatures(_hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, claimIds)), sigs);

        for (uint256 i; i < claimIds.length; ++i) {
            Claim storage c = _claims[claimIds[i]];
            if (c.status == ClaimStatus.None) revert UnknownClaim(claimIds[i]);
            if (c.vaultId != vaultId) revert ClaimVaultMismatch(claimIds[i], vaultId);
            if (c.status == ClaimStatus.Paid) revert ClaimAlreadyPaid(claimIds[i]); // also rejects duplicate ids
            c.status = ClaimStatus.Paid;
        }
        emit PayoutConfirmed(vaultId, claimIds);
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
    function claim(bytes32 claimId)
        external
        view
        returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status)
    {
        Claim storage c = _claims[claimId];
        return (c.vaultId, c.kind, c.amount, c.commitment, c.status);
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
    function hashDepositAttestation(DepositAttestation calldata att) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashDepositAttestation(att));
    }

    /// @inheritdoc IZecuredRegistry
    function hashPayoutConfirmation(bytes32 vaultId, bytes32[] calldata claimIds) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, claimIds));
    }

    /// @inheritdoc IZecuredRegistry
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ internal

    function _checkAdapter() internal view {
        if (msg.sender != adapter) revert OnlyAdapter(msg.sender);
    }

    function _marketParams() internal view returns (MarketParams memory) {
        return
            MarketParams({
                loanToken: _loanToken, collateralToken: address(zzec), oracle: _oracle, irm: _irm, lltv: _lltv
            });
    }

    function _newClaim(bytes32 vaultId, Vault storage v, ClaimKind kind, uint64 amount, bytes32 commitment)
        internal
        returns (bytes32 claimId)
    {
        uint64 sequence = v.claimCount;
        claimId = claimIdOf(vaultId, sequence);
        v.claimCount = sequence + 1;
        _claims[claimId] =
            Claim({vaultId: vaultId, commitment: commitment, amount: amount, kind: kind, status: ClaimStatus.Pending});
    }

    /// @dev I6: at least THRESHOLD signatures; every one must be a valid, non-malleable ECDSA signature (OpenZeppelin
    ///      ECDSA rejects high-s and bad v) of a signer, and the recovered signers must be strictly increasing, so the
    ///      same signer can never be counted twice.
    function _verifySignerSignatures(bytes32 digest, bytes[] calldata sigs) internal view {
        if (sigs.length < THRESHOLD) revert InsufficientSignatures(sigs.length);
        address previous = address(0);
        for (uint256 i; i < sigs.length; ++i) {
            address recovered = ECDSA.recoverCalldata(digest, sigs[i]);
            if (!_isSigner(recovered)) revert UnknownSigner(recovered);
            if (recovered <= previous) revert SignaturesNotSorted(i);
            previous = recovered;
        }
    }

    function _isSigner(address account) internal view returns (bool) {
        return
            account == _signer0 || account == _signer1 || account == _signer2 || account == _signer3
                || account == _signer4;
    }
}
