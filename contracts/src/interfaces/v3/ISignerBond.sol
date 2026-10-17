// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";

/// @notice Constructor arguments of SignerBond v2.
/// @param registry The Registry, already deployed: the constructor reads its dispute windows (R-18).
/// @param usdc The market's loan token: bonds, bounties, liveness payments and compensation are all in it.
/// @param oracle The market's oracle (Morpho scale): prices a claim's zatoshi for a liveness payment. If it reports a
///        feed state (`IZecuredPriceFeed.state()`, the mainnet `ZecuredPriceFeed`), no liveness is paid while it is
///        `Stale` (R-9); the demo oracles of the testnets report none.
/// @param minBond A signer counts as bonded with at least this much (`fullyBonded`).
/// @param unbondingPeriod Between `requestUnbond` and `withdrawUnbonded`; at least `challengeWindow +
///        livenessPeriod + commitGrace`, so that every claim created before a withdrawal starts is due (at the latest
///        `livenessPeriod + commitGrace` after it) while the withdrawn amount is still slashable; and (R-18) at least
///        the Registry's `disputeWindow + inclusionWindow`, so that a claim confirmed just before a partial unbonding,
///        then disputed and reopened, falls due while that amount is still slashable.
/// @param challengeWindow The slasher's; also the wait after the last vault settled before a full exit.
/// @param bountyBps, bountyCap A slash credits its prover `min(slashed * bountyBps / 10,000, bountyCap)`.
/// @param livenessPeriod T-05: a claim unpaid this long after it became payable can be paid from the bonds.
/// @param commitGrace T-05: a payout committed less than this long ago may still be mined, so its claims wait, but a
///        commitment made after the claim's deadline never extends it. R-10: the deploy script requires at least a
///        payout's lifetime (its 120-block expiry from its target height, 5 blocks of tip skew and the 10-block reorg
///        margin, twice the time those 136 blocks take at the network's target spacing).
/// @param livenessPremiumBps T-05: the claimant gets the claim's value plus this premium.
struct BondConfig {
    address registry;
    address slasher;
    address intake;
    address compensator;
    IERC20 usdc;
    IOracle oracle;
    uint256 minBond;
    uint64 unbondingPeriod;
    uint64 challengeWindow;
    uint256 bountyBps;
    uint256 bountyCap;
    uint64 livenessPeriod;
    uint64 commitGrace;
    uint16 livenessPremiumBps;
}

/// @title ISignerBond (v2)
/// @notice The five signers' bonds, in USDC (design notes T-01, T-05). No owner, no upgrade path.
/// @dev Bonds:
///      - `deposit(amount)`: a signer adds to its bond (any time before a halt).
///      - `effectiveBond() = min(sum of bondOf, 5 * S3 / 3)`, S3 the sum of the three smallest bonds: what the coverage
///        cap measures (three colluders lose at least their own three bonds). Unbonding amounts do not count.
///      - `requestUnbond(amount)`: moves `amount` from `bondOf` to `unbondingOf` (ready after `unbondingPeriod`;
///        a new request adds to a running one and restarts its clock). Partial: the rest stays >= `minBond`, no case
///        is open, and the Intake confirms the coverage still holds without it. Full (rest 0): no case open, no vault
///        open, and `challengeWindow` passed since the last vault settled (v1's rule). Unbonding amounts stay slashable
///        (theft and liveness) until withdrawn.
///      - `withdrawUnbonded(to)`: once ready, while no case is open.
///      Slash (`slash`, the slasher only): takes `bondOf + unbondingOf` of every signer in the bitmap, credits the
///      bounty (pulled with `claimBounty`), sends the rest to the compensator (`onFunds`), and halts minting for good.
///      Liveness receipts (R-5; EIP-712 domain "ZecuredSignerBond" "1"):
///      `DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)`. A signer's node signs one once it
///      holds the preimage of the claim's destination commitment (salt and Ironwood receiver) and has checked it.
///      `armLiveness(claimId, receivedAt, signature)`, anyone: a receipt by any signer for the claim's current
///      commitment, `receivedAt <= now`, arms the claim with `payableAt = max(createdAt, receivedAt)`; arming again for
///      the same commitment keeps the earlier `payableAt`, and a new commitment voids the arming (a new receipt is
///      needed). One honest signer suffices: the signers who stall a payout cannot withhold the receipt of another.
///      Liveness (`slashLiveness(claimId)`, the claimant only; T-05):
///      - the claim is pending, not bond-paid, has a commitment, its kind is Liquidation, CloseMarketMaker, Release,
///        CloseUser or Refund, it is armed (R-5), its vault is not frozen, the compensator owes it nothing (R-7: an F3
///        shortfall is paid there), and the oracle is not stale (R-9). No claim exists on a pending vault (R-8);
///      - with `payableAt` the arming's, at least the claim's `createdAt`: `deadline = payableAt + livenessPeriod`; if
///        the claim is bound to an input, `protectedUntil = min(lastCommitAt(input), deadline) + commitGrace`; it is
///        due at `max(deadline, protectedUntil)`, so the longest wait is `livenessPeriod + commitGrace` however often
///        the signers re-commit;
///      - pays the claimant `amount * price * (10,000 + livenessPremiumBps) / 10,000` (USDC units), taken from every
///        signer's slashable amount pro rata, or reverts `InsufficientBond` (the claimant can wait); then
///        `registry.markBondPaid`.
interface ISignerBond {
    event BondDeposited(uint8 indexed signer, uint256 amount, uint256 bond);
    event UnbondRequested(uint8 indexed signer, uint256 amount, uint64 readyAt);
    event UnbondCancelled(uint8 indexed signer, uint256 amount);
    event BondWithdrawn(uint8 indexed signer, address indexed to, uint256 amount);
    event BondSlashed(bytes32 indexed caseId, uint8 indexed signer, uint256 amount);
    event BountyCredited(bytes32 indexed caseId, address indexed beneficiary, uint256 amount);
    event BountyClaimed(address indexed beneficiary, uint256 amount);
    event Halted(bytes32 indexed caseId);
    event LivenessPaid(bytes32 indexed claimId, address indexed claimant, uint256 amount, uint256 price);
    /// @notice R-5: a signer's receipt armed the claim's liveness for `commitment`, payable from `payableAt`.
    event LivenessArmed(bytes32 indexed claimId, bytes32 commitment, uint64 payableAt);

    error ZeroAddress();
    error InvalidConfig();
    error NotASigner(address caller);
    error NotTheSlasher(address caller);
    error ZeroAmount();
    error BondsHalted();
    error BelowMinBond(uint256 remaining, uint256 minBond);
    error CoverageWouldBreak();
    error CaseOpen(uint256 openCases);
    error FullExitLocked(uint256 openVaults, uint256 openCases, uint64 unlocksAt);
    error NothingUnbonding(uint8 signer);
    error UnbondingNotReady(uint64 readyAt);
    error NothingToClaim(address beneficiary);
    error NotTheClaimant(bytes32 claimId, address caller);
    error NotLivenessEligible(bytes32 claimId);
    error LivenessNotDue(bytes32 claimId, uint64 dueAt);
    error InsufficientBond(uint256 due, uint256 slashable);
    /// @notice R-5: a receipt dated after the block it is used in.
    error ReceiptFromTheFuture(uint64 receivedAt);
    /// @notice R-9: the oracle reports a stale price; try again once a source is fresh.
    error PriceStale();

    function deposit(uint256 amount) external;
    function requestUnbond(uint256 amount) external;
    function cancelUnbond() external;
    function withdrawUnbonded(address to) external;
    function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary) external returns (uint256 slashed);
    function claimBounty() external;
    function slashLiveness(bytes32 claimId) external returns (uint256 paid);

    /// @notice R-5, anyone: arms the claim's liveness with a signer's `DestinationReceived` receipt for its current
    ///         commitment (see the interface's notes). Reverts `NotLivenessEligible` for a claim the bonds could never
    ///         pay, `ReceiptFromTheFuture`, or `NotASigner` for a receipt no signer made.
    function armLiveness(bytes32 claimId, uint64 receivedAt, bytes calldata signature) external;

    /// @notice R-5: the EIP-712 digest a signer signs for a receipt.
    function hashDestinationReceived(bytes32 claimId, bytes32 commitment, uint64 receivedAt)
        external
        view
        returns (bytes32);

    /// @notice R-5: the claim's arming: the commitment it was armed for and when it became payable (zeros if never
    ///         armed). It counts only while the claim's commitment is still `commitment`.
    function livenessArming(bytes32 claimId) external view returns (bytes32 commitment, uint64 payableAt);

    /// @notice T-05: when `claimId` can be paid from the bonds and how much it would pay at the current price; `(0, 0)`
    ///         while it cannot be (wrong kind or status, not armed, a frozen vault, a compensator entitlement, a stale
    ///         price).
    function livenessDue(bytes32 claimId) external view returns (uint64 dueAt, uint256 amount);
    /// @notice R-9: whether the oracle reports a feed state, so that liveness waits while it is `Stale`.
    function priceStateChecked() external view returns (bool);
    /// @notice The EIP-712 domain separator of the receipts ("ZecuredSignerBond", "1").
    function domainSeparator() external view returns (bytes32);
    function effectiveBond() external view returns (uint256);
    function fullyBonded() external view returns (bool);
    function halted() external view returns (bool);
    function bondOf(uint8 signer) external view returns (uint256);
    function unbondingOf(uint8 signer) external view returns (uint256 amount, uint64 readyAt);
    /// @notice Sum over the signers of `bondOf + unbondingOf.amount`: what a slash of all five would take.
    function totalSlashable() external view returns (uint256);
    function bountyOf(address beneficiary) external view returns (uint256);

    function registry() external view returns (address);
    function slasher() external view returns (address);
    function intake() external view returns (address);
    function compensator() external view returns (address);
    function usdc() external view returns (IERC20);
    function oracle() external view returns (IOracle);
    function minBond() external view returns (uint256);
    function unbondingPeriod() external view returns (uint64);
    function challengeWindow() external view returns (uint64);
    function bountyBps() external view returns (uint256);
    function bountyCap() external view returns (uint256);
    function livenessPeriod() external view returns (uint64);
    function commitGrace() external view returns (uint64);
    function livenessPremiumBps() external view returns (uint16);
}
