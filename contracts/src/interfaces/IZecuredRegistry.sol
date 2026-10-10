// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZZEC} from "./IZZEC.sol";
import {DepositAttestation} from "../libraries/ZecuredEIP712.sol";

/// @notice Vault status on Base. Vaults are opened off-chain, so Base only knows `Active` (from `attestDeposit`) and
///         `Closed` (outstanding reached 0).
enum VaultStatus {
    None,
    Active,
    Closed
}

/// @notice What a claim pays out on Zcash.
enum ClaimKind {
    Release,
    Liquidation,
    CloseUser,
    CloseMarketMaker
}

/// @notice Payout status of a claim.
enum ClaimStatus {
    None,
    Pending,
    Paid
}

/// @title IZecuredRegistry
/// @notice Vault and claim book-keeping on top of the zZEC / USDC Morpho Blue market (docs/INTERFACES.md § 3.2).
interface IZecuredRegistry {
    // ------------------------------------------------------------------ events (docs/INTERFACES.md § 3.2)

    event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount);
    event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
    event LiquidationRelease(
        bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment
    );
    event CloseRelease(
        bytes32 indexed vaultId,
        bytes32 userClaimId,
        bytes32 mmClaimId,
        uint64 userAmount,
        uint64 mmAmount,
        bytes32 userCommitment,
        bytes32 mmCommitment
    );
    event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);

    // ------------------------------------------------------------------ errors

    /// @notice A constructor argument is the zero address.
    error ZeroAddress();
    /// @notice The zZEC token is not wired to this Registry, the given Adapter and Morpho.
    error ZZECMisconfigured();
    /// @notice The signer set is not 5 strictly increasing, non-zero addresses.
    error InvalidSignerSet();
    /// @notice Fewer signatures than the threshold (I6).
    error InsufficientSignatures(uint256 provided);
    /// @notice A signature recovers to an address outside the signer set.
    error UnknownSigner(address recovered);
    /// @notice The signer recovered at `index` is not strictly greater than the previous one (unsorted or duplicate).
    error SignaturesNotSorted(uint256 index);
    error ZeroVaultId();
    error ZeroAccount();
    error ZeroAmount();
    error ZeroDepositTag();
    error ZeroCommitment();
    /// @notice The vault id was already used (I5).
    error VaultIdUsed(bytes32 vaultId);
    /// @notice The deposit tag was already used (I5).
    error DepositTagUsed(bytes32 depositTag);
    /// @notice The account already has (or had) a vault (I4).
    error AccountHasVault(address account, bytes32 vaultId);
    error VaultNotActive(bytes32 vaultId);
    error NotVaultAccount(bytes32 vaultId, address caller);
    error OnlyAdapter(address caller);
    /// @notice `requestRelease` while the account still has Morpho debt.
    error DebtOutstanding(bytes32 vaultId);
    /// @notice D14: a partial release must be at least `MIN_RELEASE` zatoshi.
    error ReleaseAmountTooSmall(uint64 amount, uint64 minimum);
    /// @notice D14: a partial release must be smaller than the outstanding (use `requestRelease` for all of it).
    error ReleaseAmountNotPartial(uint64 amount, uint64 outstanding);
    /// @notice D14: a partial release must leave at least `MIN_RELEASE` zatoshi in the vault.
    error ReleaseRemainderTooSmall(uint64 remaining, uint64 minimum);
    /// @notice D14: after a partial release the position's LTV would exceed `MAX_WITHDRAW_LTV_BPS`
    ///         (`debt` in loan units rounded up, `collateralValue` in loan units rounded down).
    error ReleaseLtvTooHigh(uint256 debt, uint256 collateralValue);
    /// @notice The account's Morpho collateral differs from the vault's outstanding (should be unreachable).
    error CollateralMismatch(bytes32 vaultId, uint256 morphoCollateral, uint64 outstanding);
    /// @notice Morpho transferred a different zZEC amount than requested (should be unreachable).
    error UnexpectedCollateralReceived(uint256 received, uint256 expected);
    /// @notice A liquidation would seize more than the vault's outstanding (I9).
    error SeizeExceedsOutstanding(bytes32 vaultId, uint64 seized, uint64 outstanding);
    /// @notice `mmAmount + userAmount != outstanding` (I7).
    error CloseAmountsMismatch(bytes32 vaultId, uint64 mmAmount, uint64 userAmount, uint64 outstanding);
    error NoClaims();
    error UnknownClaim(bytes32 claimId);
    /// @notice The claim belongs to another vault.
    error ClaimVaultMismatch(bytes32 claimId, bytes32 vaultId);
    error ClaimAlreadyPaid(bytes32 claimId);

    // ------------------------------------------------------------------ state-changing (docs/INTERFACES.md § 3.2)

    /// @notice Registers the vault (outstanding = amount, Active), mints the zZEC to the Registry and supplies it to
    ///         Morpho on behalf of `att.account`. Anyone may relay; needs >= 3 signer signatures (sorted, distinct).
    function attestDeposit(DepositAttestation calldata att, bytes[] calldata sigs) external;

    /// @notice Vault account only. Withdraws the whole outstanding from Morpho to the Registry (reverts while debt
    ///         remains), burns it, closes the vault and creates a Release claim.
    function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);

    /// @notice D14. Vault account only. Withdraws `amount` (MIN_RELEASE <= amount <= outstanding - MIN_RELEASE) from
    ///         Morpho to the Registry, requires the position's LTV after the withdrawal to be <= MAX_WITHDRAW_LTV_BPS
    ///         (besides Morpho's own LLTV check), burns it and creates a Release claim. The vault stays Active.
    function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment)
        external
        returns (bytes32 claimId);

    /// @notice SettlementAdapter only. Burns `seized` zZEC held by the Adapter and creates a Liquidation claim. The
    ///         vault stays Active while outstanding > 0.
    function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment)
        external
        returns (bytes32 claimId);

    /// @notice SettlementAdapter only. Burns the whole outstanding held by the Adapter, closes the vault and creates
    ///         the user claim, then the market maker claim.
    function recordClose(
        bytes32 vaultId,
        uint64 mmAmount,
        uint64 userAmount,
        bytes32 mmCommitment,
        bytes32 userCommitment
    ) external returns (bytes32 userClaimId, bytes32 mmClaimId);

    /// @notice Marks pending claims of `vaultId` as paid. Anyone may relay; needs >= 3 signer signatures over
    ///         PayoutConfirmation(vaultId, keccak256(abi.encodePacked(claimIds))).
    function confirmPayout(bytes32 vaultId, bytes32[] calldata claimIds, bytes[] calldata sigs) external;

    // ------------------------------------------------------------------ views (docs/INTERFACES.md § 3.2)

    function vaultOf(address account) external view returns (bytes32 vaultId);
    function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status);
    function claim(bytes32 claimId)
        external
        view
        returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status);
    function marketParams() external view returns (MarketParams memory);
    function signers() external view returns (address[5] memory);

    // ------------------------------------------------------------------ additional views (not in INTERFACES.md)

    /// @notice Minimum number of distinct signer signatures (3).
    function THRESHOLD() external view returns (uint256);
    /// @notice D14: smallest partial release, in zatoshi (100,000 = 0.001 ZEC).
    function MIN_RELEASE() external view returns (uint64);
    /// @notice D14: highest LTV a partial release may leave, in basis points (5,000 = 50 %).
    function MAX_WITHDRAW_LTV_BPS() external view returns (uint256);
    function morpho() external view returns (IMorpho);
    function zzec() external view returns (IZZEC);
    function adapter() external view returns (address);
    function marketId() external view returns (Id);
    function depositTagUsed(bytes32 depositTag) external view returns (bool);
    /// @notice Number of claims created for the vault so far (= the sequence number of the next claim).
    function claimCount(bytes32 vaultId) external view returns (uint64);
    /// @notice keccak256(abi.encode(vaultId, sequence)).
    function claimIdOf(bytes32 vaultId, uint64 sequence) external pure returns (bytes32);
    /// @notice The EIP-712 digest (Registry domain) the signers sign for `att`.
    function hashDepositAttestation(DepositAttestation calldata att) external view returns (bytes32);
    /// @notice The EIP-712 digest (Registry domain) the signers sign to confirm `claimIds` of `vaultId`.
    function hashPayoutConfirmation(bytes32 vaultId, bytes32[] calldata claimIds) external view returns (bytes32);
    /// @notice The Registry's EIP-712 domain separator.
    function domainSeparator() external view returns (bytes32);
}
