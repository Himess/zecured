// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZZEC} from "../IZZEC.sol";
import {IZecuredSignerErrors} from "./IZecuredSignerErrors.sol";

/// @notice Vault status on Base. Values are append-only (off-chain code reads them as numbers).
/// - `None`: unknown to Base.
/// - `Active`: activated by a deposit proof; its zZEC is Morpho collateral.
/// - `Closed`: outstanding reached 0; open while any claim is unpaid.
/// - `Cancelled`: never activated and never will be; its deposits are refunded through Refund claims (v3: there is no
///   `cancelVault` any more, a proof that cannot activate the vault lands as a refund).
/// - `Pending`: a deposit proof reserved the vault, its tag and its account; `ZecuredIntake.finalizeActivation`
///   activates it (K3).
enum VaultStatus {
    None,
    Active,
    Closed,
    Cancelled,
    Pending
}

/// @notice What a claim pays out on Zcash. Append-only.
/// - `Refund`: a deposit that could not activate the vault, paid back to a destination the vault's account gives.
/// - `Fee`: the opening fee, owed to the signers' treasury (`feeCommitment`); paid with the vault's first exit.
/// - `Retired`: bookkeeping only. The outstanding of a compromised vault that the compensator retired (its zZEC burned,
///   its lenders repaid in USDC); created `Compensated`, never paid on Zcash.
enum ClaimKind {
    Release,
    Liquidation,
    CloseUser,
    CloseMarketMaker,
    Refund,
    Fee,
    Retired
}

/// @notice Payout status of a claim. Append-only.
/// - `WrittenOff`: a dust claim of a closed vault (`writeOffDust`, RT-8) or an abandoned refund (`abandonRefund`); its
///   value stays in the vault.
/// - `Compensated`: the claim's vault was compromised (an executed F1 or F2 case); the compensator pays it in USDC. v4:
///   also a claim whose payout an executed F3 case proved short (R-7: the compensator owes the shortfall), a claim
///   confirmed by signatures shortly before its vault froze and never proven (R-3), and an abandoned refund of a
///   compromised vault (R-21).
/// - `Disputed`: its claimant disputes the signers' confirmation that it was paid (`IZecuredDisputes`); unpaid again
///   until the signers prove the payout is in a Zcash block, or `Pending` once they failed to in time.
/// A `Paid` claim with `paidAt != 0` (`IZecuredDisputes.payoutDispute`) was confirmed by three signers and can still be
/// disputed or proven; with `paidAt == 0` its payment is final (proven, or its dispute window over, `IZecuredPayouts`).
enum ClaimStatus {
    None,
    Pending,
    Paid,
    WrittenOff,
    Compensated,
    Disputed
}

/// @notice Everything Base keeps about a claim.
/// @param commitment keccak256(salt32 || receiver43) of the Ironwood recipient; zero for a Refund claim whose account
///        has not given its destination yet, and for a claim the bonds paid (T-05) until all five signers give its
///        Reimburse destination (v4, R-6).
/// @param createdAt When the claim became payable: its creation, or for a Refund claim the moment its destination was
///        set. The liveness penalty counts from it (T-05).
/// @param bondPaid T-05: the claimant was paid from the signers' bonds; the claim's ZEC is now owed to the bonds'
///        Reimburse destination.
/// @param claimant The Base address that receives the claim's USDC from the bonds (T-05) or the compensator (T-04):
///        the liquidator or executor for Liquidation and CloseMarketMaker claims, the vault's account for Release,
///        CloseUser and Refund claims, zero for Fee and Retired claims.
struct ClaimDetails {
    bytes32 vaultId;
    ClaimKind kind;
    ClaimStatus status;
    uint64 amount;
    bytes32 commitment;
    uint40 createdAt;
    bool bondPaid;
    address claimant;
}

/// @notice Everything Base keeps about a vault.
/// @param unpaidClaims Claims not yet paid, written off or compensated (the vault is open while any is left).
struct VaultDetails {
    address account;
    uint64 outstanding;
    VaultStatus status;
    uint64 claimCount;
    uint64 unpaidClaims;
}

/// @notice Constructor arguments of ZecuredRegistry v3 (a struct: too many for the stack).
/// @param zzec Already deployed with `registry = address(this)` and `adapter = adapter` (predicted CREATE addresses).
/// @param signers The five signer addresses, strictly increasing.
/// @param spendLogic ZecuredSpendLogic, deployed first with this Registry's (predicted) address: `commitSpend` runs its
///        code by delegatecall, on this Registry's storage (T-11: the Registry alone would exceed EIP-170).
/// @param intake ZecuredIntake: the only caller of the deposit hooks (reserve, release, activate, refunds).
/// @param signerBond SignerBond v2: the only caller of `markBondPaid` (T-05). Zero on deployments without bonds.
/// @param slasher ZecuredSlasher: exits refuse a vault it reports frozen (T-04). Zero on deployments without bonds.
/// @param compensator ZecuredCompensator: the only caller of `retireVault` and of the claim hooks of `IZecuredPayouts`
///        (T-04, v4). Zero on deployments without bonds.
/// @param feeBps T-06: opening fee in basis points of the deposit, charged at activation as a Fee claim. Zero: no fee.
/// @param minFeeZat T-06: a fee below this is waived (its own output would cost about as much); must exceed
///        `DUST_LIMIT` and the 5,000-zatoshi output it bears.
/// @param feeCommitment T-06: salted commitment of the signers' treasury Ironwood address (zero iff `feeBps` is zero).
/// @param refundAbandonPeriod T-02: a Refund claim whose account gave no destination for this long can be written off
///        (and revived later by its account).
struct RegistryConfig {
    IMorpho morpho;
    IZZEC zzec;
    address adapter;
    address loanToken;
    address oracle;
    address irm;
    uint256 lltv;
    address[5] signers;
    address spendLogic;
    address intake;
    address signerBond;
    address slasher;
    address compensator;
    uint16 feeBps;
    uint64 minFeeZat;
    bytes32 feeCommitment;
    uint64 refundAbandonPeriod;
}

/// @title IZecuredRegistry (v3)
/// @notice The core ledger on top of the zZEC / USDC Morpho Blue market: vaults, claims, Tier 3 lineage and payout
///         commitments (`IZecuredSpendCommits`), and every exit. Deposits reach it only through `ZecuredIntake`, which
///         verifies deposit proofs and gates minting (cap, pause, halt); the Registry re-checks its own invariants in
///         every hook (I4, I5, unique lineage tags). No admin, no upgrade path.
/// @dev Changes from v2 (design notes T-02 to T-06, T-11):
///      - the deposit path (`attestDepositWithProof`, `finalizeActivation`, `dropPendingActivation`) moved to the
///        Intake; `attestDeposit` (signer-attested minting) and `cancelVault` are gone;
///      - Refund claims: a deposit that cannot activate its vault lands as a Refund claim, whose destination the vault's
///        account gives (`setRefundDestination`); no deposit is ever refunded without a committed payout;
///      - claims keep their claimant's Base address and when they became payable (T-04, T-05);
///      - exits refuse a vault the slasher reports frozen; the compensator retires a compromised vault (T-04);
///      - the bonds can pay an overdue claim, which the signers then point at a Reimburse destination (T-05);
///      - an optional opening fee, owed to the signers' treasury as a Fee claim (T-06).
///      Changes in v4: `confirmPayout` (now naming the payout it confirms, R-1) and the compensator's claim hook moved to
///      `IZecuredPayouts`, served by the fallback like `IZecuredDisputes`; `setReimburseDestination` takes all five
///      signers (R-6); a proven deposit overrides a change record under its tag (R-16, `ChangeTagOverridden`); a
///      compromised vault the compensator settled while it stays active no longer counts as open or locked (R-20).
interface IZecuredRegistry is IZecuredSignerErrors {
    // ------------------------------------------------------------------ events

    /// @notice K2-a: `depositTag` names the note that backs the vault (the head of its lineage).
    event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
    /// @notice A deposit proof reserved the vault (K3); the Intake emits the readiness details.
    event VaultReserved(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
    /// @notice K3: the reservation was released (the proof's anchor left the relay's best chain).
    event VaultReservationReleased(bytes32 indexed vaultId);
    /// @notice T-02: a deposit landed as a refund. `destination` is set later by the vault's account. R-12: emitted
    ///         when the Refund claim opens (`ZecuredIntake.finalizeRefund`, or a converted pending activation), not
    ///         when the proof lands (the Intake's `RefundPending`).
    event RefundOpened(
        bytes32 indexed vaultId, bytes32 indexed claimId, address account, uint64 amount, bytes32 depositTag
    );
    event RefundDestinationSet(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 commitment);
    event RefundAbandoned(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice R-11: a Refund claim without a destination for `refundDormancy` no longer counts in `lockedZatoshi` (nor
    ///         in the coverage cap) until its account gives one (`IZecuredRefunds.markRefundDormant`).
    event RefundDormant(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice T-06: the opening fee was charged as a Fee claim.
    event FeeCharged(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 fee);
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
    /// @notice RT-8: a closed vault's claim too small to bear any payout's fee was settled without a payment.
    event DustWrittenOff(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount);
    /// @notice T-05: the bonds paid the claimant; the claim's ZEC is now owed to a Reimburse destination.
    event ClaimBondPaid(bytes32 indexed vaultId, bytes32 indexed claimId);
    event ReimburseDestinationSet(bytes32 indexed vaultId, bytes32 indexed claimId, bytes32 commitment);
    /// @notice T-04: the compensator retired `retired` zatoshi of a compromised vault: its unbacked zZEC was burned.
    event VaultRetired(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 retired);
    /// @notice T-04: a pending claim of a compromised vault is paid by the compensator instead (since v4 emitted by
    ///         `IZecuredPayouts.settleCompensatedClaim` and `closeMisdirectedClaim`, at the Registry's address).
    event ClaimCompensated(bytes32 indexed vaultId, bytes32 indexed claimId);
    /// @notice R-16: a proven deposit took over `tag`, which a commitment of `oldVaultId` had recorded as its change. A
    ///         real change note never carries a deposit's tag: the commitment's signers recorded a false change.
    event ChangeTagOverridden(bytes32 indexed tag, bytes32 indexed oldVaultId);

    // ------------------------------------------------------------------ errors

    error ZeroAddress();
    error ZZECMisconfigured();
    error InvalidFeeConfig();
    /// @notice The spend logic was built for another Registry or signer set.
    error SpendLogicMismatch();
    error SignatureExpired(uint64 deadline);
    error ZeroVaultId();
    error ZeroAccount();
    error ZeroAmount();
    error ZeroDepositTag();
    error ZeroCommitment();
    error VaultIdUsed(bytes32 vaultId);
    error DepositTagUsed(bytes32 depositTag);
    error AccountHasVault(address account, bytes32 vaultId);
    error VaultNotActive(bytes32 vaultId);
    error VaultNotPending(bytes32 vaultId);
    error ReservationMismatch(bytes32 vaultId);
    error NotVaultAccount(bytes32 vaultId, address caller);
    error OnlyAdapter(address caller);
    error OnlyIntake(address caller);
    error OnlyBond(address caller);
    error OnlyCompensator(address caller);
    error DebtOutstanding(bytes32 vaultId);
    error ReleaseAmountTooSmall(uint64 amount, uint64 minimum);
    error ReleaseAmountNotPartial(uint64 amount, uint64 outstanding);
    error ReleaseRemainderTooSmall(uint64 remaining, uint64 minimum);
    error ReleaseLtvTooHigh(uint256 debt, uint256 collateralValue);
    error CollateralMismatch(bytes32 vaultId, uint256 morphoCollateral, uint64 outstanding);
    error UnexpectedCollateralReceived(uint256 received, uint256 expected);
    error SeizeExceedsOutstanding(bytes32 vaultId, uint64 seized, uint64 outstanding);
    error CloseAmountsMismatch(bytes32 vaultId, uint64 mmAmount, uint64 userAmount, uint64 outstanding);
    error NoClaims();
    error UnknownClaim(bytes32 claimId);
    error ClaimVaultMismatch(bytes32 claimId, bytes32 vaultId);
    error ClaimAlreadyPaid(bytes32 claimId);
    error NotDust(bytes32 claimId);
    /// @notice T-04: the slasher has an open F1/F2 case on the vault, or executed one.
    error VaultFrozen(bytes32 vaultId);
    /// @notice T-04: the vault was compromised; its claims are settled by the compensator.
    error VaultCompromised(bytes32 vaultId);
    error NotARefund(bytes32 claimId);
    error RefundBound(bytes32 claimId);
    error NotAbandonable(bytes32 claimId, uint64 at);
    error NotBondPaid(bytes32 claimId);
    error DestinationAlreadySet(bytes32 claimId);
    error NotRetirable(bytes32 vaultId);
    error NotCompensable(bytes32 claimId);

    // ------------------------------------------------------------------ exits (unchanged rules, plus T-04's freeze)

    /// @notice Vault account only. Withdraws the whole outstanding to the Registry (reverts while debt remains), burns
    ///         it, closes the vault and creates a Release claim. Reverts while the vault is frozen.
    function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);

    /// @notice D14. Vault account only. As in v2, plus T-04's freeze.
    function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment)
        external
        returns (bytes32 claimId);

    /// @notice SettlementAdapter only. Burns `seized` zZEC held by the Adapter and creates a Liquidation claim whose
    ///         claimant (T-04, T-05) is `claimant`: the liquidator, or the executor of a deleverage.
    function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment, address claimant)
        external
        returns (bytes32 claimId);

    /// @notice SettlementAdapter only. Burns the whole outstanding held by the Adapter, closes the vault and creates the
    ///         user claim, then the market maker claim, whose claimant is `mmClaimant` (the executor).
    function recordClose(
        bytes32 vaultId,
        uint64 mmAmount,
        uint64 userAmount,
        bytes32 mmCommitment,
        bytes32 userCommitment,
        address mmClaimant
    ) external returns (bytes32 userClaimId, bytes32 mmClaimId);

    // ------------------------------------------------------------------ payouts (v4: confirmations in IZecuredPayouts)

    /// @notice RT-8: as in v2; not on a compromised vault (the compensator settles its claims).
    function writeOffDust(bytes32 claimId) external;

    // ------------------------------------------------------------------ T-02: refunds

    /// @notice The vault's account gives the destination of its Refund claim. Allowed while the claim is pending (or
    ///         written off by `abandonRefund`, which this revives) and not yet bound to a payout input; restarts the
    ///         claim's liveness clock (`createdAt = now`; a new commitment also voids the bond's liveness arming, R-5)
    ///         and counts a dormant claim in `lockedZatoshi` again (R-11).
    function setRefundDestination(bytes32 claimId, bytes32 destCommitment) external;

    /// @notice Anyone. Writes off a Refund claim without a destination once `refundAbandonPeriod` has passed since it
    ///         was created, so that it no longer keeps the vault (and every bond) open. The note stays a lineage note:
    ///         spending it without a commitment is still F1. Not on a compromised vault (the compensator settles its
    ///         claims).
    function abandonRefund(bytes32 claimId) external;

    // ------------------------------------------------------------------ T-05: claims the bonds paid

    /// @notice Anyone may relay; needs the signatures of all five signers (v4, R-6: every signer's bond paid the
    ///         claimant, pro rata, so three could otherwise redirect the reimbursement to themselves) over
    ///         ReimburseDestination(claimId, commitment, deadline) (Registry domain). Points a bond-paid claim, whose
    ///         commitment is zero, at the destination that reimburses the bonds.
    function setReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline, bytes[] calldata sigs)
        external;

    // ------------------------------------------------------------------ hooks: ZecuredIntake only

    /// @notice Reserves a vault for a proven deposit (K3): requires a `None` vault, an unused tag and lineage tag, and
    ///         an account without a vault (I4, I5). Marks the tag used, binds the account, makes the deposit note a
    ///         lineage note (from now on, spending it without a commitment is F1) and opens the vault.
    function reserveDeposit(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external;

    /// @notice K3: releases a reservation whose anchor left the relay's best chain (or whose verifier version was
    ///         removed, R-14): vault `None` again, tag unused, account unbound, lineage note removed, vault settled. A
    ///         pending vault never has a claim (a refund proven meanwhile waits for it, R-8).
    function releaseDeposit(bytes32 vaultId, address account, bytes32 depositTag) external;

    /// @notice Pending -> Active for the reservation `(account, amount, depositTag)`; charges the fee (T-06), mints the
    ///         zZEC and supplies it to Morpho on behalf of the account (I1).
    function activate(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external;

    /// @notice T-02, R-12: a proven deposit that cannot activate the vault (or a Refund-mode proof) waits like an
    ///         activation (K3) before its Refund claim opens. This reserves it: the tag is used, the note is a lineage
    ///         note of the vault (spending it without a commitment is F1), but no payout may spend it and no claim
    ///         exists until `IZecuredRefunds.openReservedRefund`; `IZecuredRefunds.releaseRefund` frees both if the
    ///         proof is dropped. The vault itself is untouched. An existing vault must have this account and must not
    ///         be compromised (`VaultCompromised`: its claims are the compensator's, and an unpayable late refund would
    ///         keep it open for good). `convertToRefund` has no such rule: the compensator needs a compromised pending
    ///         activation converted before it can tally the vault.
    function reserveRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag) external;

    /// @notice T-02, T-03: Pending -> Cancelled for the reservation `(account, amount, depositTag)`, whose lineage note
    ///         stays; creates its Refund claim.
    function convertToRefund(bytes32 vaultId, address account, uint64 amount, bytes32 depositTag)
        external
        returns (bytes32 claimId);

    // ------------------------------------------------------------------ hooks: compensator only (T-04)

    /// @notice An active compromised vault: withdraws `amount` of its collateral (1 to its outstanding) to the Registry
    ///         with the account's Morpho authorization, burns it and records a Retired claim of `amount` (status
    ///         Compensated). All of it needs the debt repaid and closes the vault; a part is what the compensator burns
    ///         down to the LLTV at most (Morpho refuses an unhealthy withdrawal), as soon as the vault is compromised
    ///         (R-19) and whenever its debt is only partly repaid, so that nothing more can be borrowed against the
    ///         unbacked zZEC. A vault the compensator marked settled (R-20) does not count as open again when it closes.
    function retireVault(bytes32 vaultId, uint64 amount) external returns (uint64 retired);

    // ------------------------------------------------------------------ hooks: SignerBond only (T-05)

    /// @notice The bonds paid this pending claim's claimant: `bondPaid = true`, commitment zero until all five signers
    ///         give its Reimburse destination (R-6).
    function markBondPaid(bytes32 claimId) external;

    // ------------------------------------------------------------------ views

    function vaultOf(address account) external view returns (bytes32 vaultId);
    function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status);
    function vaultDetails(bytes32 vaultId) external view returns (VaultDetails memory);
    function claim(bytes32 claimId)
        external
        view
        returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status);
    function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory);
    /// @notice T-01: zatoshi the vaults hold for Base: the zZEC supply (every active vault's outstanding, I1) plus every
    ///         pending or disputed claim but a dormant refund (R-11), less the outstanding of the compromised vaults the
    ///         compensator settled while they stayed active (R-20: their ZEC was taken and they are compensated as far
    ///         as the bonds went). The Intake adds its pending activations.
    function lockedZatoshi() external view returns (uint256);
    function marketParams() external view returns (MarketParams memory);
    function signers() external view returns (address[5] memory);
    function THRESHOLD() external view returns (uint256);
    function MIN_RELEASE() external view returns (uint64);
    function MAX_WITHDRAW_LTV_BPS() external view returns (uint256);
    function DUST_LIMIT() external view returns (uint64);
    function morpho() external view returns (IMorpho);
    function zzec() external view returns (IZZEC);
    function adapter() external view returns (address);
    function intake() external view returns (address);
    function spendLogic() external view returns (address);
    function compensator() external view returns (address);
    function slasher() external view returns (address);
    function marketId() external view returns (Id);
    function depositTagUsed(bytes32 depositTag) external view returns (bool);
    function feeBps() external view returns (uint16);
    function minFeeZat() external view returns (uint64);
    function feeCommitment() external view returns (bytes32);
    function refundAbandonPeriod() external view returns (uint64);
    function claimCount(bytes32 vaultId) external view returns (uint64);
    function claimIdOf(bytes32 vaultId, uint64 sequence) external pure returns (bytes32);
    function hashReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline)
        external
        view
        returns (bytes32);
    function domainSeparator() external view returns (bytes32);
}
