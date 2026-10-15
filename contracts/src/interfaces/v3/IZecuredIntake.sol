// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IRiscZeroVerifier} from "../IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../relay/IZcashHeaderRelay.sol";

/// @notice Why a proven deposit cannot be minted right now (`ZecuredIntake.mintBlock`).
/// - `Halted`: a slash halted minting for good; a proof lands as a refund.
/// - `CapExceeded`: the coverage cap (T-01) has no room for it; a proof lands as a refund.
/// - `NotBonded`, `Paused`: temporary; a proof becomes a pending activation that waits (T-03).
enum MintBlock {
    None,
    Halted,
    NotBonded,
    Paused,
    CapExceeded
}

/// @notice Why a proven deposit landed as a refund (T-02).
enum RefundReason {
    /// The proof was made in Refund mode (T-09): any Ironwood note to the vault's address.
    ModeRefund,
    /// The vault is already active, closed or cancelled (a second deposit).
    VaultNotNew,
    /// Another proof already reserved the vault.
    PendingExists,
    /// The account already has another vault (unreachable with an honest `VaultOpened`).
    AccountHasVault,
    /// Minting halted (a slash).
    Halted,
    /// The coverage cap is full.
    CapExceeded,
    /// A pending activation converted by `refundPending` (halt, or a long pause).
    PendingConverted
}

/// @notice Journal of a deposit proof. v1 (today's guest, 6 words): abi.encode(tipHash, vaultId, account, amount,
///         depositTag, vaultOpenedHash), always Activate mode. v2 (T-09, 8 words): abi.encode(mode, branchId, tipHash,
///         vaultId, account, amount, depositTag, vaultOpenedHash).
/// @param mode 0 = Activate (the D5 deposit rule, the vault's amount, at or below its expiry height), 1 = Refund (any
///        Ironwood note to the vault's published receiver).
/// @param branchId The consensus branch id of the epoch whose history tree the proof walks (v2); must be allowed.
/// @param vaultOpenedHash EIP-712 struct hash of the vault's `VaultOpened` (v2 type: `tagKeyHash` binds the tag key),
///        whose five signer signatures the Intake checks under the Registry's domain.
struct DepositJournal {
    uint8 mode;
    uint32 branchId;
    bytes32 tipHash;
    bytes32 vaultId;
    address account;
    uint64 amount;
    bytes32 depositTag;
    bytes32 vaultOpenedHash;
}

/// @notice K3: a deposit proof waiting for the relay to move on. Its vault, tag and account are reserved in the
///         Registry.
struct PendingActivation {
    address account;
    uint64 amount;
    bytes32 depositTag;
    bytes32 anchor;
    uint256 readyHeight;
    uint256 readyTime;
}

/// @notice T-08 (decision 2c): deposits up to `maxAmountZat` need `minWork` on top of their anchor. Tiers ascend; a
///         deposit above the last tier is refused.
struct WorkTier {
    uint64 maxAmountZat;
    uint256 minWork;
}

/// @notice Constructor arguments of ZecuredIntake.
/// @param signerBond Its `halted`, `fullyBonded` and `effectiveBond` gate minting (zero: no bond, tests only).
/// @param slasher E2 and the automatic pause read its open cases (zero: none, tests only).
/// @param journalV2 False: the v1 journal of today's guest; true: the T-09 guest's v2 journal.
/// @param allowedBranchIds Journal v2 only: the branch ids a deposit's epoch may have (zero entries are unused).
/// @param coverageBps T-01: k in basis points; zero disables the coverage cap (tests only, with `signerBond` zero).
/// @param maxTotalZat T-01: absolute cap on locked plus pending zatoshi.
/// @param capPriceMaxAge T-01: oldest `priceHigh` the cap accepts; an older one makes an activatable proof revert
///        (`CapPriceStale`), never a refund: a refund never reads the price.
/// @param capOracle T-01: `IZecuredCapPrice` (the market oracle's conservative high side).
/// @param maxPause T-03: longest pause one set of signatures sets (30 days).
/// @param pendingRefundDelay T-03: a pause this long lets a pending activation's account convert it into a refund.
/// @param autoPauseOnCase T-03: an open F1 or F2 case pauses minting (mainnet true; testnet false, review I-03).
/// @param minRefundZat T-02: the smallest refund a proof can open (smaller notes could never bear a payout's fee).
struct IntakeConfig {
    address registry;
    address signerBond;
    address slasher;
    IRiscZeroVerifier verifier;
    bytes32 imageId;
    bytes4 sealSelector;
    bool journalV2;
    uint32[4] allowedBranchIds;
    IZcashHeaderRelay relay;
    WorkTier[] workTiers;
    uint64 finalizeDepth;
    uint64 finalizeDelay;
    uint16 coverageBps;
    uint64 maxTotalZat;
    uint64 capPriceMaxAge;
    address capOracle;
    uint64 maxPause;
    uint64 pendingRefundDelay;
    bool autoPauseOnCase;
    uint64 minRefundZat;
}

/// @title IZecuredIntake
/// @notice Everything before a vault is active (design notes T-01, T-02, T-03, T-08, T-09): deposit proofs, pending
///         activations (K3), and the gates of minting: the halt, the bonds, the coverage cap and the pause. Minting
///         needs no signer: a proof lands without one. Exits never pass through this contract.
/// @dev `attestDepositWithProof(journal, seal, openedSigs)`, anyone:
///      1. the seal starts with `sealSelector` (Y1) and verifies under `imageId`; the journal has the configured length,
///         a valid mode and (v2) an allowed branch id;
///      2. the anchor (`tipHash`) is on the relay's best chain with `minConfirmationWorkFor(amount)` on top;
///      3. all five signers signed the journal's `vaultOpenedHash` (Registry domain): the note belongs to this vault,
///         owner and account, and (VaultOpened v2) its tag was derived with the vault's own tag key;
///      4. the tag is unused (else `DepositTagUsed`: one note never lands twice);
///      5. then: Refund mode, a vault that is not `None`, an account bound elsewhere, a halt or a full cap -> the
///         Registry opens a Refund claim (`DepositRefunded`); otherwise the vault is reserved and the activation
///         becomes pending (`ActivationPending`), also while paused or not fully bonded (it then waits).
///      `finalizeActivation(vaultId)`, anyone: the relay reached `readyHeight`, `readyTime` passed, the anchor is still
///      on the best chain with enough work, no open case on the vault (E2), not halted, not paused, fully bonded.
///      `dropPendingActivation(vaultId)`, anyone: only once the anchor left the best chain; frees everything.
///      `refundPending(vaultId)`: after a halt (anyone), or while a pause has lasted `pendingRefundDelay` (the vault's
///      account): converts the pending activation into a refund.
interface IZecuredIntake {
    event ActivationPending(
        bytes32 indexed vaultId,
        address indexed account,
        uint64 amount,
        bytes32 depositTag,
        uint256 readyHeight,
        uint256 readyTime
    );
    event PendingActivationDropped(bytes32 indexed vaultId);
    event DepositRefunded(bytes32 indexed vaultId, bytes32 indexed claimId, RefundReason reason);
    event MintingPaused(uint64 until, uint8 signerBitmap, uint256 nonce);
    event MintingUnpaused(uint8 signerBitmap, uint256 nonce);

    error ZeroAddress();
    error InvalidConfig();
    error InvalidJournalLength(uint256 length);
    error InvalidMode(uint8 mode);
    error BranchNotAllowed(uint32 branchId);
    error UnexpectedSealSelector(bytes4 selector);
    error AnchorNotConfirmed(bytes32 tipHash, uint256 confirmationWork);
    error AmountAboveLargestTier(uint64 amount);
    error DepositTagUsed(bytes32 depositTag);
    error RefundTooSmall(uint64 amount, uint64 minimum);
    error CapPriceStale(uint256 updatedAt);
    error NoPendingActivation(bytes32 vaultId);
    error ActivationNotReady(bytes32 vaultId, uint256 readyHeight, uint256 readyTime);
    error AnchorStillOnBestChain(bytes32 vaultId);
    error ViolationCaseOpen(bytes32 vaultId);
    error MintingHalted();
    error SignersNotBonded();
    error MintingIsPaused();
    error NotRefundableYet(bytes32 vaultId);
    error NotPendingAccount(bytes32 vaultId, address caller);
    error BadGuardNonce(uint256 expected);
    error SignatureExpired(uint64 deadline);
    error PauseTooLong(uint64 until, uint64 maxUntil);
    error InsufficientSignatures(uint256 provided);
    error UnknownSigner(address recovered);
    error SignaturesNotSorted(uint256 index);

    function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes[] calldata openedSigs) external;
    function finalizeActivation(bytes32 vaultId) external;
    function dropPendingActivation(bytes32 vaultId) external;
    function refundPending(bytes32 vaultId) external returns (bytes32 claimId);

    /// @notice T-03: needs >= 3 signer signatures over PauseMinting(until, nonce, deadline) (Intake domain);
    ///         `nonce == guardNonce`, `now <= deadline`, `now < until <= now + maxPause`. Renewable: three signers can
    ///         sign again before it ends (a power one signer already has, by refusing `VaultOpened`).
    function pauseMinting(uint64 until, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;
    /// @notice T-03: needs >= 3 signer signatures over UnpauseMinting(nonce, deadline) (Intake domain).
    function unpauseMinting(uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;

    /// @notice T-03: paused by signatures until `pausedUntil`, or by an open F1/F2 case when `autoPauseOnCase`.
    function mintingPaused() external view returns (bool);
    /// @notice What would stop a proven deposit of `amount` from being minted now (reads the cap price; reverts
    ///         `CapPriceStale` if it is too old).
    function mintBlock(uint64 amount) external view returns (MintBlock);
    /// @notice T-01: zatoshi the coverage cap and the absolute cap still admit at the current high price.
    function capRoom() external view returns (uint256 zatoshi);
    /// @notice T-01: whether locked plus pending value stays covered with `effectiveBond` (the Bond's partial
    ///         unbonding check).
    function coverageHoldsWith(uint256 effectiveBond) external view returns (bool);
    function minConfirmationWorkFor(uint64 amount) external view returns (uint256);
    function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory);
    function pendingTotal() external view returns (uint64);
    function pausedUntil() external view returns (uint64);
    function pausedSince() external view returns (uint64);
    function guardNonce() external view returns (uint256);
    function hashPauseMinting(uint64 until, uint256 nonce, uint64 deadline) external view returns (bytes32);
    function hashUnpauseMinting(uint256 nonce, uint64 deadline) external view returns (bytes32);
    function domainSeparator() external view returns (bytes32);

    function registry() external view returns (address);
    function signerBond() external view returns (address);
    function slasher() external view returns (address);
    function verifier() external view returns (IRiscZeroVerifier);
    function imageId() external view returns (bytes32);
    function sealSelector() external view returns (bytes4);
    function journalV2() external view returns (bool);
    function relay() external view returns (IZcashHeaderRelay);
    function finalizeDepth() external view returns (uint64);
    function finalizeDelay() external view returns (uint64);
    function coverageBps() external view returns (uint16);
    function maxTotalZat() external view returns (uint64);
    function capPriceMaxAge() external view returns (uint64);
    function capOracle() external view returns (address);
    function maxPause() external view returns (uint64);
    function pendingRefundDelay() external view returns (uint64);
    function autoPauseOnCase() external view returns (bool);
    function minRefundZat() external view returns (uint64);
}
