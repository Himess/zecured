// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {IMorpho, MarketParams, Id, Position, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IZZEC} from "./interfaces/IZZEC.sol";
import {IRiscZeroVerifier} from "./interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "./relay/IZcashHeaderRelay.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    DepositProofConfig,
    MintMode
} from "./interfaces/IZecuredRegistry.sol";
import {ZecuredEIP712, DepositAttestation, SpendCommit} from "./libraries/ZecuredEIP712.sol";
import {IZecuredSpendCommits, LineageNote, SpendRecord, ClaimPayment} from "./interfaces/IZecuredSpendCommits.sol";
import {ISignerBond} from "./interfaces/ISignerBond.sol";
import {IZecuredSlasher} from "./interfaces/IZecuredSlasher.sol";

/// @title ZecuredRegistry
/// @notice Book-keeping for Zecured vaults on Base (docs/INTERFACES.md § 3.2):
///         - `attestDeposit`: a 3-of-5 signer attestation activates a vault, mints its zZEC and supplies it straight
///           into the account's Morpho position (I1, I4, I5, I6); disabled in `ProofOnly` mode (K1);
///         - `attestDepositWithProof` (Tier 2): a RISC Zero proof that the deposit is in a Zcash block committed by a
///           relay-verified header, for the vault all five signers described when it opened, makes the activation
///           pending; `finalizeActivation` activates the vault once the relay has moved on and the proof's anchor is
///           still on its best chain, and `dropPendingActivation` discards it otherwise (K3);
///         - `cancelVault` (L-8): 3 of 5 signers retire a vault that was never activated before refunding its deposit;
///         - `requestRelease`: the account itself withdraws and burns its whole outstanding once the debt is repaid;
///         - `requestPartialRelease` (D14): the account withdraws and burns part of its outstanding, debt or not, as
///           long as the position's LTV stays <= 50 % afterwards and at least MIN_RELEASE stays in the vault;
///         - `recordLiquidation` / `recordClose`: the SettlementAdapter reports seized / closed collateral, which the
///           Registry burns (I7, I9);
///         - every burn creates a claim (payout instruction for the signers, I3); `confirmPayout` marks claims paid;
///         - Tier 3 (docs/TIER3.md): `commitSpend` records every payout of a lineage note before the signers sign it,
///           so that a theft is provable (`ZecuredSlasher`); a slash halts minting through the bond contract.
/// @dev Morpho authorization is all-or-nothing: an account that authorizes the Registry lets it do anything on its
///      behalf. This contract therefore calls exactly two Morpho functions: `supplyCollateral` (needs no authorization)
///      and `withdrawCollateral` (only inside the account's own `requestRelease` / `requestPartialRelease`, to the
///      Registry). It has no code path that borrows or withdraws loan assets, and it is not upgradeable and has no admin.
contract ZecuredRegistry is IZecuredRegistry, IZecuredSpendCommits, EIP712 {
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
    /// @inheritdoc IZecuredRegistry
    uint256 public constant JOURNAL_LENGTH = 6 * 32;
    /// @inheritdoc IZecuredSpendCommits
    uint256 public constant MAX_CLAIMS_PER_PAYOUT = 8;
    /// @dev ZIP 317: 5,000 zatoshi per logical action, at least 2 actions; a payout spends one lineage note, so its
    ///      actions are its outputs (claims plus change). The signers' plans charge exactly this
    ///      (`zecured_zcash_ops::spend::zip317_fee`).
    uint64 private constant ZIP317_MARGINAL_FEE = 5_000;
    uint256 private constant ZIP317_GRACE_ACTIONS = 2;
    /// @dev All five signers: an uncommitted spend is signed by three of them, and FROST does not tell which.
    uint8 private constant ALL_SIGNERS = 0x1f;

    /// @inheritdoc IZecuredRegistry
    IMorpho public immutable morpho;
    /// @inheritdoc IZecuredRegistry
    IZZEC public immutable zzec;
    /// @inheritdoc IZecuredRegistry
    address public immutable adapter;
    /// @inheritdoc IZecuredRegistry
    Id public immutable marketId;

    /// @inheritdoc IZecuredRegistry
    IRiscZeroVerifier public immutable depositVerifier;
    /// @inheritdoc IZecuredRegistry
    bytes32 public immutable depositImageId;
    /// @inheritdoc IZecuredRegistry
    IZcashHeaderRelay public immutable relay;
    /// @inheritdoc IZecuredRegistry
    uint256 public immutable minConfirmationWork;
    /// @inheritdoc IZecuredRegistry
    MintMode public immutable mintMode;
    /// @inheritdoc IZecuredRegistry
    bytes4 public immutable depositSealSelector;
    /// @inheritdoc IZecuredRegistry
    uint64 public immutable finalizeDepth;
    /// @inheritdoc IZecuredRegistry
    uint64 public immutable finalizeDelay;
    /// @inheritdoc IZecuredSpendCommits
    ISignerBond public immutable signerBond;

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
        uint64 unpaidClaims; // claims not yet confirmed paid (the vault is open while any is left)
    }

    /// @dev The journal of a deposit proof (tier2/statement `Journal::abi_encode`).
    struct DepositJournal {
        bytes32 tipHash;
        bytes32 vaultId;
        address account;
        uint64 amount;
        bytes32 depositTag;
        bytes32 vaultOpenedHash;
    }

    struct Claim {
        bytes32 vaultId;
        bytes32 commitment; // keccak256(salt32 || receiver43) of the Ironwood recipient
        uint64 amount; // zatoshi
        ClaimKind kind;
        ClaimStatus status;
    }

    /// @dev K3: a deposit proof waiting for the relay to move on. Its vault id, tag and account are reserved.
    struct PendingActivation {
        address account;
        uint64 amount;
        bytes32 depositTag;
        bytes32 anchor; // the journal's tipHash
        uint256 readyHeight;
        uint256 readyTime;
    }

    mapping(bytes32 vaultId => Vault) private _vaults;
    mapping(address account => bytes32 vaultId) private _vaultOf;
    mapping(bytes32 claimId => Claim) private _claims;
    mapping(bytes32 vaultId => PendingActivation) private _pending;
    /// @inheritdoc IZecuredRegistry
    mapping(bytes32 depositTag => bool used) public depositTagUsed;

    // Tier 3 (docs/TIER3.md § 4).
    mapping(bytes32 tag => LineageNote) private _lineage;
    mapping(bytes32 payoutRef => SpendRecord) private _spends;
    mapping(bytes32 claimRef => ClaimPayment) private _claimPayments;
    /// @inheritdoc IZecuredSpendCommits
    mapping(bytes32 claimId => bytes32 inputTag) public claimInput;
    uint256 private _openVaults;
    uint64 private _lastVaultSettledAt;

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
    /// @param proofs_ Tier 2 deposit proofs: verifier, guest image id, header relay, minimum confirmation work, mint mode
    ///        (K1), seal selector (Y1) and finalization depth and delay (K3); all zero (`SignerAttested`) to disable them.
    /// @param signerBond_ Tier 3: the signers' bond contract (its predicted address), whose halt stops minting; zero
    ///        for a deployment without bonds (local fork, tests).
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
        address[5] memory signers_,
        DepositProofConfig memory proofs_,
        ISignerBond signerBond_
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
        _checkDepositProofConfig(proofs_);

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
        depositVerifier = proofs_.verifier;
        depositImageId = proofs_.imageId;
        relay = proofs_.relay;
        minConfirmationWork = proofs_.minConfirmationWork;
        mintMode = proofs_.mintMode;
        depositSealSelector = proofs_.sealSelector;
        finalizeDepth = proofs_.finalizeDepth;
        finalizeDelay = proofs_.finalizeDelay;
        signerBond = signerBond_;

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
        if (mintMode == MintMode.ProofOnly) revert MintRequiresProof();
        _checkNewVault(att.vaultId, att.account, att.amount, att.depositTag);
        _verifySignerSignatures(_hashTypedDataV4(ZecuredEIP712.hashDepositAttestation(att)), sigs, THRESHOLD);
        _open(att.vaultId, att.amount, att.depositTag);
        _activate(att.vaultId, att.account, att.amount, att.depositTag);
    }

    /// @inheritdoc IZecuredRegistry
    /// @dev What the proof itself establishes is in tier2/statement/src/lib.rs. The Registry adds: the seal is for the
    ///      pinned verifier (Y1); the anchor header is on the relay's best chain with enough work on top (so is the
    ///      deposit's block, which precedes it); all five signers described the vault (id, owner, account, amount,
    ///      expiry, address and viewing-key hashes) in the `VaultOpened` whose struct hash the proof computed, which
    ///      binds the proven note to this vault and account without the address hash ever reaching Base; the vault, tag
    ///      and account are unused (I4, I5). The vault is only activated later, by `finalizeActivation` (K3).
    function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes[] calldata openedSigs) external {
        if (address(depositVerifier) == address(0)) revert DepositProofsDisabled();
        if (journal.length != JOURNAL_LENGTH) revert InvalidJournalLength(journal.length);
        _checkSealSelector(seal);
        // A struct of static fields decodes like the flat tuple; abi.decode rejects dirty address / uint64 words.
        DepositJournal memory j = abi.decode(journal, (DepositJournal));
        _checkNewVault(j.vaultId, j.account, j.amount, j.depositTag);
        _checkAnchor(j.tipHash);
        // All five: the DKG needs every signer, so all five always sign, and one honest signer suffices to refuse a
        // vault key it did not help create.
        _verifySignerSignatures(_hashTypedDataV4(j.vaultOpenedHash), openedSigs, SIGNER_COUNT);
        depositVerifier.verify(seal, depositImageId, sha256(journal));

        _pend(j);
    }

    /// @inheritdoc IZecuredRegistry
    function finalizeActivation(bytes32 vaultId) external {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        (, uint256 height,) = relay.bestChainTip();
        if (height < p.readyHeight || block.timestamp < p.readyTime) {
            revert ActivationNotReady(vaultId, p.readyHeight, p.readyTime);
        }
        // Still on the best chain, and confirmation work only grows while it stays there.
        _checkAnchor(p.anchor);
        // E2: not while a violation case about this vault (its deposit spent during the wait) is open.
        _checkNoOpenCase(vaultId);
        delete _pending[vaultId];
        // The vault is still None: cancelVault and both mint paths refuse a vault with a pending activation.
        _activate(vaultId, p.account, p.amount, p.depositTag);
    }

    /// @inheritdoc IZecuredRegistry
    function dropPendingActivation(bytes32 vaultId) external {
        PendingActivation memory p = _pending[vaultId];
        if (p.account == address(0)) revert NoPendingActivation(vaultId);
        if (relay.isInBestChain(p.anchor)) revert AnchorStillOnBestChain(vaultId);
        delete _pending[vaultId];
        // Nothing was minted: the note may be proven again against the chain that won, and it backs nothing.
        depositTagUsed[p.depositTag] = false;
        delete _vaultOf[p.account];
        delete _lineage[p.depositTag];
        _settle();
        emit PendingActivationDropped(vaultId);
    }

    /// @inheritdoc IZecuredRegistry
    function cancelVault(bytes32 vaultId, bytes[] calldata sigs) external {
        if (vaultId == bytes32(0)) revert ZeroVaultId();
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.None) revert VaultIdUsed(vaultId);
        if (_pending[vaultId].account != address(0)) revert ActivationIsPending(vaultId);
        _verifySignerSignatures(_hashTypedDataV4(ZecuredEIP712.hashVaultCancellation(vaultId)), sigs, THRESHOLD);
        v.status = VaultStatus.Cancelled;
        emit VaultCancelled(vaultId);
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

        _withdrawToRegistry(account, amount);
        zzec.burn(address(this), amount);
    }

    /// @inheritdoc IZecuredRegistry
    function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment)
        external
        returns (bytes32 claimId)
    {
        Vault storage v = _vaults[vaultId];
        if (v.status != VaultStatus.Active) revert VaultNotActive(vaultId);
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
        _verifySignerSignatures(
            _hashTypedDataV4(ZecuredEIP712.hashPayoutConfirmation(vaultId, claimIds)), sigs, THRESHOLD
        );

        Vault storage v = _vaults[vaultId];
        for (uint256 i; i < claimIds.length; ++i) {
            Claim storage c = _claims[claimIds[i]];
            if (c.status == ClaimStatus.None) revert UnknownClaim(claimIds[i]);
            if (c.vaultId != vaultId) revert ClaimVaultMismatch(claimIds[i], vaultId);
            if (c.status == ClaimStatus.Paid) revert ClaimAlreadyPaid(claimIds[i]); // also rejects duplicate ids
            // Tier 3: a claim is paid by a committed payout only, which its claimant can check (F3).
            if (claimInput[claimIds[i]] == bytes32(0)) revert ClaimNotCommitted(claimIds[i]);
            c.status = ClaimStatus.Paid;
        }
        v.unpaidClaims -= uint64(claimIds.length);
        emit PayoutConfirmed(vaultId, claimIds);
        if (v.status == VaultStatus.Closed && v.unpaidClaims == 0) _settle();
    }

    // ------------------------------------------------------------------ Tier 3: spend commitments

    /// @inheritdoc IZecuredSpendCommits
    function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external {
        uint64 inputValue = _checkSpendShape(c);
        bytes32 recordHash = ZecuredEIP712.hashSpendCommit(c);
        uint8 signerBitmap = _verifySignerSignatures(_hashTypedDataV4(recordHash), sigs, THRESHOLD);

        uint256 n = c.claimIds.length;
        uint64 fee = ZIP317_MARGINAL_FEE * uint64(Math.max(ZIP317_GRACE_ACTIONS, c.changeValue != 0 ? n + 1 : n));
        uint256 claimed;
        for (uint256 i; i < n; ++i) {
            claimed += _commitClaim(c, i, fee);
        }
        if (claimed + c.changeValue != inputValue) revert ConservationBroken(inputValue, claimed, c.changeValue);
        _recordSpend(c, recordHash, signerBitmap);
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
    function hashVaultCancellation(bytes32 vaultId) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashVaultCancellation(vaultId));
    }

    /// @inheritdoc IZecuredRegistry
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    /// @inheritdoc IZecuredRegistry
    function pendingActivation(bytes32 vaultId)
        external
        view
        returns (
            address account,
            uint64 amount,
            bytes32 depositTag,
            bytes32 anchor,
            uint256 readyHeight,
            uint256 readyTime
        )
    {
        PendingActivation storage p = _pending[vaultId];
        return (p.account, p.amount, p.depositTag, p.anchor, p.readyHeight, p.readyTime);
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
    function hashSpendCommit(SpendCommit calldata c) external view returns (bytes32) {
        return _hashTypedDataV4(ZecuredEIP712.hashSpendCommit(c));
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

    /// @dev Proofs disabled: everything zero and `SignerAttested` (nothing could mint in `ProofOnly`). Enabled: a relay,
    ///      an image id, a seal selector and a finalization depth of at least one header.
    function _checkDepositProofConfig(DepositProofConfig memory c) internal pure {
        if (address(c.verifier) == address(0)) {
            if (
                address(c.relay) != address(0) || c.imageId != bytes32(0) || c.minConfirmationWork != 0
                    || c.mintMode != MintMode.SignerAttested || c.sealSelector != bytes4(0) || c.finalizeDepth != 0
                    || c.finalizeDelay != 0
            ) revert InvalidDepositProofConfig();
        } else if (
            address(c.relay) == address(0) || c.imageId == bytes32(0) || c.sealSelector == bytes4(0)
                || c.finalizeDepth == 0
        ) {
            revert InvalidDepositProofConfig();
        }
    }

    /// @dev Y1: the router dispatches on the seal's first four bytes; requiring the pinned selector fixes the verifier
    ///      (a selector, once used, cannot be pointed at another verifier) while keeping the router's emergency stop.
    function _checkSealSelector(bytes calldata seal) internal view {
        bytes4 selector = seal.length >= 4 ? bytes4(seal[:4]) : bytes4(0);
        if (selector != depositSealSelector) revert UnexpectedSealSelector(selector);
    }

    /// @dev K3: reserves the vault id, tag and account of a proven deposit until `finalizeActivation` or
    ///      `dropPendingActivation`.
    function _pend(DepositJournal memory j) internal {
        _checkNotHalted();
        _open(j.vaultId, j.amount, j.depositTag);
        depositTagUsed[j.depositTag] = true;
        _vaultOf[j.account] = j.vaultId;
        (, uint256 height,) = relay.bestChainTip();
        uint256 readyHeight = height + finalizeDepth;
        uint256 readyTime = block.timestamp + finalizeDelay;
        _pending[j.vaultId] = PendingActivation({
            account: j.account,
            amount: j.amount,
            depositTag: j.depositTag,
            anchor: j.tipHash,
            readyHeight: readyHeight,
            readyTime: readyTime
        });
        emit ActivationPending(j.vaultId, j.account, j.amount, j.depositTag, readyHeight, readyTime);
    }

    /// @dev I4, I5: a new vault id (never activated nor cancelled, no pending proof), an unused tag and an account
    ///      without a vault.
    function _checkNewVault(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) internal view {
        if (vaultId == bytes32(0)) revert ZeroVaultId();
        if (account == address(0)) revert ZeroAccount();
        if (amount == 0) revert ZeroAmount();
        if (depositTag == bytes32(0)) revert ZeroDepositTag();
        if (_vaults[vaultId].status != VaultStatus.None) revert VaultIdUsed(vaultId);
        if (_pending[vaultId].account != address(0)) revert ActivationIsPending(vaultId);
        if (depositTagUsed[depositTag]) revert DepositTagUsed(depositTag);
        bytes32 existing = _vaultOf[account];
        if (existing != bytes32(0)) revert AccountHasVault(account, existing);
    }

    /// @dev The header committing to a proof's deposit must be on the relay's best chain with `minConfirmationWork` on
    ///      top of it (then so is the deposit's block, which precedes it).
    function _checkAnchor(bytes32 tipHash) internal view {
        uint256 work = relay.confirmationWork(tipHash);
        if (!relay.isInBestChain(tipHash) || work < minConfirmationWork) revert AnchorNotConfirmed(tipHash, work);
    }

    /// @dev Registers the vault, mints its zZEC and supplies it to Morpho on behalf of the account (I1).
    function _activate(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) internal {
        _checkNotHalted();
        depositTagUsed[depositTag] = true;
        _vaultOf[account] = vaultId;
        _vaults[vaultId] =
            Vault({account: account, outstanding: amount, status: VaultStatus.Active, claimCount: 0, unpaidClaims: 0});
        emit VaultActivated(vaultId, account, amount, depositTag);

        // The Registry holds the zZEC only within this call: Morpho pulls exactly `amount` from it.
        zzec.mint(address(this), amount);
        zzec.approve(address(morpho), amount);
        morpho.supplyCollateral(_marketParams(), amount, account, "");
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
        v.unpaidClaims += 1;
        _claims[claimId] =
            Claim({vaultId: vaultId, commitment: commitment, amount: amount, kind: kind, status: ClaimStatus.Pending});
    }

    /// @dev I6: at least `required` (>= THRESHOLD) signatures; every one must be a valid, non-malleable ECDSA signature
    ///      (OpenZeppelin ECDSA rejects high-s and bad v) of a signer, and the recovered signers must be strictly
    ///      increasing, so the same signer can never be counted twice. Returns who signed: bit i for signer i.
    function _verifySignerSignatures(bytes32 digest, bytes[] calldata sigs, uint256 required)
        internal
        view
        returns (uint8 signerBitmap)
    {
        if (sigs.length < required) revert InsufficientSignatures(sigs.length);
        address previous = address(0);
        for (uint256 i; i < sigs.length; ++i) {
            address recovered = ECDSA.recoverCalldata(digest, sigs[i]);
            uint256 index = _signerIndex(recovered);
            if (index == SIGNER_COUNT) revert UnknownSigner(recovered);
            if (recovered <= previous) revert SignaturesNotSorted(i);
            previous = recovered;
            // index < SIGNER_COUNT: one of the five low bits, which fit in a uint8.
            // forge-lint: disable-next-line(incorrect-shift,unsafe-typecast)
            signerBitmap |= uint8(1 << index);
        }
    }

    /// @dev The signer's index in the sorted set, or SIGNER_COUNT for any other address.
    function _signerIndex(address account) internal view returns (uint256) {
        if (account == _signer0) return 0;
        if (account == _signer1) return 1;
        if (account == _signer2) return 2;
        if (account == _signer3) return 3;
        if (account == _signer4) return 4;
        return SIGNER_COUNT;
    }

    /// @dev The parts of a commitment that need no claim lookup: a new, non-zero payout ref; an input that is a lineage
    ///      note of the vault; 1 to MAX_CLAIMS_PER_PAYOUT claims with one ref each; a change tag exactly when there is
    ///      change, not already a lineage note. Returns the input's value.
    function _checkSpendShape(SpendCommit calldata c) internal view returns (uint64) {
        if (c.payoutRef == bytes32(0)) revert ZeroPayoutRef();
        if (_spends[c.payoutRef].vaultId != bytes32(0)) revert PayoutAlreadyCommitted(c.payoutRef);
        LineageNote memory input = _lineage[c.inputTag];
        if (c.vaultId == bytes32(0) || input.vaultId != c.vaultId) revert NotALineageNote(c.vaultId, c.inputTag);
        uint256 n = c.claimIds.length;
        if (n == 0 || n > MAX_CLAIMS_PER_PAYOUT || c.claimRefs.length != n) {
            revert InvalidClaimCount(n, c.claimRefs.length);
        }
        bool withChange = c.changeValue != 0;
        if (withChange != (c.changeTag != bytes32(0)) || (withChange && _lineage[c.changeTag].vaultId != bytes32(0))) {
            revert InvalidChange(c.changeTag, c.changeValue);
        }
        return input.value;
    }

    /// @dev Records a checked commitment; its change becomes a lineage note of the vault.
    function _recordSpend(SpendCommit calldata c, bytes32 recordHash, uint8 signerBitmap) internal {
        _spends[c.payoutRef] = SpendRecord({
            recordHash: recordHash,
            vaultId: c.vaultId,
            inputTag: c.inputTag,
            changeTag: c.changeTag,
            changeValue: c.changeValue,
            claimCount: uint8(c.claimIds.length),
            signerBitmap: signerBitmap
        });
        if (c.changeValue != 0) _lineage[c.changeTag] = LineageNote({vaultId: c.vaultId, value: c.changeValue});
        emit SpendCommitted(
            c.vaultId, c.payoutRef, c.inputTag, c.claimIds, c.claimRefs, c.changeTag, c.changeValue, signerBitmap
        );
    }

    /// @dev One claim of a commitment (docs/TIER3.md § 4): distinct, of the vault, pending, bound to this input (the
    ///      first commitment binds it), with an unused claim ref, which records what the claimant must receive.
    ///      `claimIds[0]` bears the fee. Returns the claim's amount.
    function _commitClaim(SpendCommit calldata c, uint256 i, uint64 fee) internal returns (uint64 amount) {
        bytes32 id = c.claimIds[i];
        for (uint256 k; k < i; ++k) {
            if (c.claimIds[k] == id) revert DuplicateClaim(id);
        }
        Claim storage cl = _claims[id];
        if (cl.vaultId != c.vaultId || cl.status != ClaimStatus.Pending) revert ClaimNotPayable(id);
        bytes32 bound = claimInput[id];
        if (bound == bytes32(0)) claimInput[id] = c.inputTag;
        else if (bound != c.inputTag) revert ClaimBoundToAnotherInput(id, bound);

        bytes32 ref = c.claimRefs[i];
        if (ref == bytes32(0) || _claimPayments[ref].claimId != bytes32(0)) revert ClaimRefAlreadyUsed(ref);
        amount = cl.amount;
        uint64 expected = amount;
        if (i == 0) {
            if (amount <= fee) revert FeeBearerIsDust(id, amount, fee);
            expected = amount - fee;
        }
        _claimPayments[ref] = ClaimPayment({claimId: id, payoutRef: c.payoutRef, expected: expected});
    }

    /// @dev A vault's deposit note becomes its first lineage note as soon as the vault is pending or activated: from
    ///      then on, spending it without a commitment is F1. The vault stays open until it is closed and every claim is
    ///      confirmed paid (or its pending activation is dropped).
    function _open(bytes32 vaultId, uint64 amount, bytes32 depositTag) internal {
        _lineage[depositTag] = LineageNote({vaultId: vaultId, value: amount});
        _openVaults += 1;
    }

    function _settle() internal {
        uint256 open = _openVaults - 1;
        _openVaults = open;
        if (open == 0) _lastVaultSettledAt = uint64(block.timestamp);
    }

    /// @dev E2: a vault with an open violation case is not activated; the case's execution halts minting anyway, and
    ///      its dismissal lets the activation go on. Vault-level, so a case cannot hold up any other vault.
    function _checkNoOpenCase(bytes32 vaultId) internal view {
        if (address(signerBond) == address(0)) return;
        if (IZecuredSlasher(signerBond.slasher()).openCasesOf(vaultId) != 0) revert ViolationCaseOpen(vaultId);
    }

    /// @dev Minting needs every signer bonded and no slash yet (S5).
    function _checkNotHalted() internal view {
        if (address(signerBond) == address(0)) return;
        if (signerBond.halted()) revert MintingHalted();
        if (!signerBond.fullyBonded()) revert SignersNotBonded();
    }
}
