// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZZEC} from "./IZZEC.sol";
import {IRiscZeroVerifier} from "./IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../relay/IZcashHeaderRelay.sol";
import {DepositAttestation} from "../libraries/ZecuredEIP712.sol";

/// @notice Vault status on Base. Vaults are opened off-chain, so Base only knows `Active` (from `attestDeposit` or
///         `attestDepositWithProof`), `Closed` (outstanding reached 0) and `Cancelled` (never activated, and now never
///         will be: the signers may refund its deposit, L-8).
enum VaultStatus {
    None,
    Active,
    Closed,
    Cancelled
}

/// @notice K1: which path may activate vaults. Fixed at deployment.
/// - `SignerAttested`: a 3-of-5 signer attestation (`attestDeposit`), and deposit proofs if a verifier is configured.
/// - `ProofOnly`: deposit proofs only; `attestDeposit` reverts, so no set of signers can mint unbacked zZEC.
enum MintMode {
    SignerAttested,
    ProofOnly
}

/// @notice Tier 2 deposit proofs (docs/TIER2.md). A zero `verifier` disables `attestDepositWithProof`.
/// @param verifier RISC Zero verifier (the router on Base Sepolia; a MOCK on a local fork).
/// @param imageId Image id of the deposit guest (tier2/methods/guest): which program the proof is about.
/// @param relay Zcash header relay; a proof's anchor header must be on its best chain.
/// @param minConfirmationWork Work the relay must have on top of the anchor header (work, not blocks: Testnet
///        minimum-difficulty blocks are nearly free).
/// @param mintMode K1: `ProofOnly` needs a verifier.
/// @param sealSelector Y1: the first four bytes every seal must start with, which pins the router's verifier (the
///        Groth16 selector on Base Sepolia; the MOCK's on a local fork). The router's owner can add verifiers under
///        new selectors but cannot change the verifier behind an existing one.
/// @param finalizeDepth K3: headers the relay's best chain must gain after a proof before its vault can be activated.
/// @param finalizeDelay K3: seconds that must pass after a proof before its vault can be activated, so that honest
///        feeders can replace a branch fed while they lagged.
struct DepositProofConfig {
    IRiscZeroVerifier verifier;
    bytes32 imageId;
    IZcashHeaderRelay relay;
    uint256 minConfirmationWork;
    MintMode mintMode;
    bytes4 sealSelector;
    uint64 finalizeDepth;
    uint64 finalizeDelay;
}

/// @notice What a claim pays out on Zcash.
enum ClaimKind {
    Release,
    Liquidation,
    CloseUser,
    CloseMarketMaker
}

/// @notice Payout status of a claim. `WrittenOff`: a dust claim of a closed vault, too small to ever be paid
///         (`writeOffDust`, review RT-8); its value stays in the vault.
enum ClaimStatus {
    None,
    Pending,
    Paid,
    WrittenOff
}

/// @title IZecuredRegistry
/// @notice Vault and claim book-keeping on top of the zZEC / USDC Morpho Blue market (docs/INTERFACES.md § 3.2).
interface IZecuredRegistry {
    // ------------------------------------------------------------------ events (docs/INTERFACES.md § 3.2)

    /// @notice K2-a: `depositTag` names the note that backs the vault (the head of its lineage), so that no signer
    ///         ever refunds it.
    event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
    /// @notice K3: a deposit proof was accepted; the vault can be activated by `finalizeActivation` once the relay has
    ///         reached `readyHeight`, `readyTime` has passed, and the proof's anchor is still on the best chain.
    event ActivationPending(
        bytes32 indexed vaultId,
        address indexed account,
        uint64 amount,
        bytes32 depositTag,
        uint256 readyHeight,
        uint256 readyTime
    );
    /// @notice K3: the anchor of a pending activation left the relay's best chain; the vault, tag and account are free
    ///         again.
    event PendingActivationDropped(bytes32 indexed vaultId);
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
    /// @notice RT-8: a closed vault's claim too small to bear any payout's fee was settled without a payment.
    event DustWrittenOff(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount);
    /// @notice The signers cancelled a vault that was never activated (L-8).
    event VaultCancelled(bytes32 indexed vaultId);

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
    /// @notice The deposit proof configuration is inconsistent (a verifier without relay or image id, or the reverse).
    error InvalidDepositProofConfig();
    /// @notice This deployment has no deposit proof verifier.
    error DepositProofsDisabled();
    /// @notice A deposit proof journal must be exactly `JOURNAL_LENGTH` bytes.
    error InvalidJournalLength(uint256 length);
    /// @notice The proof's anchor header is not on the relay's best chain, or has less than `minConfirmationWork`
    ///         on top of it.
    error AnchorNotConfirmed(bytes32 tipHash, uint256 confirmationWork);
    /// @notice K1: this deployment mints on deposit proofs only.
    error MintRequiresProof();
    /// @notice Y1: the seal does not start with `depositSealSelector`.
    error UnexpectedSealSelector(bytes4 selector);
    /// @notice K3: a proof for this vault is waiting for `finalizeActivation`.
    error ActivationIsPending(bytes32 vaultId);
    /// @notice K3: no proof for this vault is waiting.
    error NoPendingActivation(bytes32 vaultId);
    /// @notice K3: the relay has not reached `readyHeight`, or `readyTime` has not passed yet.
    error ActivationNotReady(bytes32 vaultId, uint256 readyHeight, uint256 readyTime);
    /// @notice K3: only a pending activation whose anchor left the best chain (or any, once minting has halted) can be
    ///         dropped.
    error AnchorStillOnBestChain(bytes32 vaultId);
    /// @notice RT-8: only a claim of a closed vault, not larger than `DUST_LIMIT` and never committed, is dust.
    error NotDust(bytes32 claimId);

    // ------------------------------------------------------------------ state-changing (docs/INTERFACES.md § 3.2)

    /// @notice Registers the vault (outstanding = amount, Active), mints the zZEC to the Registry and supplies it to
    ///         Morpho on behalf of `att.account`. Anyone may relay; needs >= 3 signer signatures (sorted, distinct).
    ///         Reverts with `MintRequiresProof` in `ProofOnly` mode (K1).
    function attestDeposit(DepositAttestation calldata att, bytes[] calldata sigs) external;

    /// @notice Tier 2: accepts a zero-knowledge proof of a vault's deposit instead of a signer attestation. The vault,
    ///         tag and account are reserved at once; the vault is activated (same effects as `attestDeposit`) by
    ///         `finalizeActivation` once the relay has gained `finalizeDepth` headers and `finalizeDelay` seconds have
    ///         passed (K3). Anyone may relay.
    /// @param journal abi.encode(bytes32 tipHash, bytes32 vaultId, address account, uint64 amount, bytes32 depositTag,
    ///        bytes32 vaultOpenedHash): the deposit's block is committed by header `tipHash`, which must be on the
    ///        relay's best chain with `minConfirmationWork` on top; the note pays `amount` to the vault described by
    ///        the `VaultOpened` whose struct hash is `vaultOpenedHash`, in an Ironwood-only transaction at or below
    ///        that vault's expiry height; `depositTag` is the note's tag (docs/TIER2.md, tier2/statement).
    /// @param seal RISC Zero proof that the deposit guest (`depositImageId`) produced `journal`; it must start with
    ///        `depositSealSelector` (Y1).
    /// @param openedSigs Signatures of all five signers over that `VaultOpened` (Registry domain), sorted by signer.
    function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes[] calldata openedSigs) external;

    /// @notice K3: activates a vault whose deposit proof is pending, once the relay's best chain has reached the
    ///         pending activation's `readyHeight`, its `readyTime` has passed, and the proof's anchor is still on the
    ///         best chain with `minConfirmationWork` on top. Anyone may call.
    function finalizeActivation(bytes32 vaultId) external;

    /// @notice K3: drops a pending activation whose anchor is no longer on the relay's best chain (a proof against a
    ///         branch that lost), freeing the vault, its tag and its account. Anyone may call. Once a slash has
    ///         halted minting (the activation can never finalize), any pending activation can be dropped, so that
    ///         the signers can cancel the vault and refund its deposit (review RT-6).
    function dropPendingActivation(bytes32 vaultId) external;

    /// @notice L-8: marks a vault that was never activated as `Cancelled`, so that neither an attestation nor a proof
    ///         can activate it any more; only then may the signers refund its deposit. Anyone may relay; needs >= 3
    ///         signer signatures over VaultCancellation(vaultId). Reverts while a proof for the vault is pending.
    function cancelVault(bytes32 vaultId, bytes[] calldata sigs) external;

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

    /// @notice RT-8: settles a dust claim without a payment. A claim of at most `DUST_LIMIT` zatoshi can never bear a
    ///         payout's fee (the smallest payout costs 10,000 zat under ZIP 317), so once its vault is closed no
    ///         payout will ever carry it, and it would keep the vault open (and every bond locked) for good. Anyone
    ///         may call; the claim must never have been committed. Its value stays in the vault.
    function writeOffDust(bytes32 claimId) external;

    /// @notice The largest claim `writeOffDust` accepts: the ZIP 317 fee of a one-claim payout (10,000 zat).
    function DUST_LIMIT() external view returns (uint64);

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
    /// @notice The EIP-712 digest (Registry domain) the signers sign to cancel `vaultId`.
    function hashVaultCancellation(bytes32 vaultId) external view returns (bytes32);
    /// @notice The Registry's EIP-712 domain separator.
    function domainSeparator() external view returns (bytes32);
    /// @notice Length of a deposit proof journal (six ABI words).
    function JOURNAL_LENGTH() external view returns (uint256);
    /// @notice Tier 2 configuration (zero verifier: proofs disabled).
    function depositVerifier() external view returns (IRiscZeroVerifier);
    function depositImageId() external view returns (bytes32);
    function relay() external view returns (IZcashHeaderRelay);
    function minConfirmationWork() external view returns (uint256);
    function mintMode() external view returns (MintMode);
    function depositSealSelector() external view returns (bytes4);
    function finalizeDepth() external view returns (uint64);
    function finalizeDelay() external view returns (uint64);
    /// @notice K3: the pending activation of `vaultId` (zero account: none). `anchor` is the proof's anchor header,
    ///         a later block than the deposit's.
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
        );
}
