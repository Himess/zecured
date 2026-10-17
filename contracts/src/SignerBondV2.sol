// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {ISignerBond, BondConfig} from "./interfaces/v3/ISignerBond.sol";
import {IZecuredRegistry, ClaimDetails, ClaimKind, ClaimStatus} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommits} from "./interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredDisputes} from "./interfaces/v3/IZecuredDisputes.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {IZecuredIntake} from "./interfaces/v3/IZecuredIntake.sol";
import {IZecuredCompensator} from "./interfaces/v3/IZecuredCompensator.sol";
import {IZecuredPriceFeed} from "./interfaces/v3/IZecuredPriceFeed.sol";

/// @title SignerBondV2
/// @notice The five signers' bonds in USDC (design notes T-01, T-05; rules in the v3 `ISignerBond`). It has no owner and
///         no upgrade path: the slasher takes bonds only for a proven theft, a claimant takes from them only for its own
///         overdue claim, and a signer takes its own bond back only through a slashable unbonding.
/// @dev Implementation notes (where the interface leaves a choice):
///      - A signer's index is its position in the Registry's sorted signer set (`signers()`).
///      - `requestUnbond` of more than the bond reverts `InsufficientBond(amount, bond)`.
///      - `slashLiveness` reverts `ZeroAmount` when the claim is worth nothing at the oracle's price (a zero price, or a
///        claim worth less than one USDC unit): paying nothing would hand the claim's ZEC to the bonds for free.
///      - A liveness payment is shared pro rata to each signer's slashable amount (`bondOf + unbondingOf.amount`), taken
///        from `bondOf` first. Signers pay in index order `floor(left * own / pool)`, where `left` is what is still due
///        and `pool` the slashable amount of this signer and the ones after it. `left <= pool` holds at every step, so
///        nobody pays more than it has, the last signer with a slashable amount pays exactly what is left (the rounding
///        remainder, a few units at most), and the total is exactly the amount due. It emits only `LivenessPaid`.
///      - R-5: without a receipt there is no liveness. A destination nobody can deliver to (a liquidator, a market
///        maker or a borrower that never hands the signers its salt and receiver) would otherwise turn into 105 % of
///        the claim's value from the bonds, at a time the claimant picks; and three colluders could empty the bonds
///        with dummy claims of their own vaults. The effective `payableAt` is never before the claim's current
///        `createdAt`, which a refund's new destination restarts.
///      - R-9: whether the oracle reports a feed state is read once, at construction (a `ZecuredPriceFeed` does, the
///        demo oracles do not); then `state()` is called directly, so a call that fails for lack of gas reverts the
///        payment rather than skipping the check.
///      - Every state-changing function is `nonReentrant`. A deposit pulls the USDC before crediting it, and every payment
///        (unbonding, bounty, slash, liveness) updates the books before the USDC leaves or another contract is called,
///        so whenever another contract runs, the bond's views match its balance.
contract SignerBondV2 is ISignerBond, ReentrancyGuardTransient, EIP712 {
    using SafeERC20 for IERC20;

    uint256 private constant BPS = 10_000;
    uint8 private constant SIGNER_COUNT = 5;
    /// @dev R-5's receipt type (EIP-712 domain "ZecuredSignerBond" "1").
    bytes32 private constant DESTINATION_RECEIVED_TYPEHASH =
        keccak256("DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)");

    /// @notice A signer's running unbonding: slashable until withdrawn.
    struct Unbonding {
        uint256 amount;
        uint64 readyAt;
    }

    /// @notice R-5: what a receipt armed: the claim's commitment it was for, and when the claim became payable.
    struct Arming {
        bytes32 commitment;
        uint64 payableAt;
    }

    /// @inheritdoc ISignerBond
    address public immutable registry;
    /// @inheritdoc ISignerBond
    address public immutable slasher;
    /// @inheritdoc ISignerBond
    address public immutable intake;
    /// @inheritdoc ISignerBond
    address public immutable compensator;
    /// @inheritdoc ISignerBond
    IERC20 public immutable usdc;
    /// @inheritdoc ISignerBond
    IOracle public immutable oracle;
    /// @inheritdoc ISignerBond
    uint256 public immutable minBond;
    /// @inheritdoc ISignerBond
    uint64 public immutable unbondingPeriod;
    /// @inheritdoc ISignerBond
    uint64 public immutable challengeWindow;
    /// @inheritdoc ISignerBond
    uint256 public immutable bountyBps;
    /// @inheritdoc ISignerBond
    uint256 public immutable bountyCap;
    /// @inheritdoc ISignerBond
    uint64 public immutable livenessPeriod;
    /// @inheritdoc ISignerBond
    uint64 public immutable commitGrace;
    /// @inheritdoc ISignerBond
    uint16 public immutable livenessPremiumBps;
    /// @inheritdoc ISignerBond
    bool public immutable priceStateChecked;

    /// @inheritdoc ISignerBond
    mapping(uint8 signer => uint256) public bondOf;
    /// @inheritdoc ISignerBond
    mapping(uint8 signer => Unbonding) public unbondingOf;
    /// @inheritdoc ISignerBond
    bool public halted;
    /// @inheritdoc ISignerBond
    mapping(address beneficiary => uint256) public bountyOf;
    mapping(bytes32 claimId => Arming) internal _armings;

    /// @param cfg See `BondConfig`. The Registry must be deployed (its dispute windows are read, R-18) and so must the
    ///        oracle (whether it reports a feed state is read, R-9); the slasher, the Intake and the compensator may be
    ///        predicted addresses: nothing is called on them here.
    constructor(BondConfig memory cfg) EIP712("ZecuredSignerBond", "1") {
        if (
            cfg.registry == address(0) || cfg.slasher == address(0) || cfg.intake == address(0)
                || cfg.compensator == address(0) || address(cfg.usdc) == address(0) || address(cfg.oracle) == address(0)
        ) revert ZeroAddress();
        if (
            cfg.minBond == 0 || cfg.challengeWindow == 0 || cfg.livenessPeriod == 0
                || cfg.unbondingPeriod < uint256(cfg.challengeWindow) + cfg.livenessPeriod + cfg.commitGrace
                || cfg.bountyBps > BPS
        ) revert InvalidConfig();
        // R-18: a claim confirmed just before a partial unbonding starts can be disputed, then reopened once the
        // inclusion window passes, and be due at once (its liveness clock never restarted): the unbonding amount must
        // still be slashable then.
        if (
            uint256(IZecuredDisputes(cfg.registry).disputeWindow()) + IZecuredDisputes(cfg.registry).inclusionWindow()
                > cfg.unbondingPeriod
        ) revert InvalidConfig();
        priceStateChecked = _reportsState(address(cfg.oracle));
        registry = cfg.registry;
        slasher = cfg.slasher;
        intake = cfg.intake;
        compensator = cfg.compensator;
        usdc = cfg.usdc;
        oracle = cfg.oracle;
        minBond = cfg.minBond;
        unbondingPeriod = cfg.unbondingPeriod;
        challengeWindow = cfg.challengeWindow;
        bountyBps = cfg.bountyBps;
        bountyCap = cfg.bountyCap;
        livenessPeriod = cfg.livenessPeriod;
        commitGrace = cfg.commitGrace;
        livenessPremiumBps = cfg.livenessPremiumBps;
    }

    // ------------------------------------------------------------------ bonds

    /// @inheritdoc ISignerBond
    function deposit(uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (halted) revert BondsHalted();
        uint8 signer = _signerIndex(msg.sender);
        usdc.safeTransferFrom(msg.sender, address(this), amount);
        uint256 bond = bondOf[signer] + amount;
        bondOf[signer] = bond;
        emit BondDeposited(signer, amount, bond);
    }

    /// @inheritdoc ISignerBond
    function requestUnbond(uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        uint8 signer = _signerIndex(msg.sender);
        uint256 bond = bondOf[signer];
        if (amount > bond) revert InsufficientBond(amount, bond);
        uint256 rest = bond - amount;
        uint256 openCases = IZecuredSlasher(slasher).openCaseCount();
        if (rest == 0) {
            // Full exit (v1's rule): the same five signers hold every vault, so "its vaults" are all vaults.
            uint256 openVaults = IZecuredSpendCommits(registry).openVaultCount();
            uint64 unlocksAt = IZecuredSpendCommits(registry).lastVaultSettledAt() + challengeWindow;
            if (openVaults != 0 || openCases != 0 || block.timestamp < unlocksAt) {
                revert FullExitLocked(openVaults, openCases, unlocksAt);
            }
        } else {
            if (rest < minBond) revert BelowMinBond(rest, minBond);
            if (openCases != 0) revert CaseOpen(openCases);
            uint256[5] memory bonds = _bonds();
            bonds[signer] = rest;
            if (!IZecuredIntake(intake).coverageHoldsWith(_effectiveBond(bonds))) revert CoverageWouldBreak();
        }
        bondOf[signer] = rest;
        Unbonding storage u = unbondingOf[signer];
        u.amount += amount;
        // A uint64 of seconds outlasts the chain.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 readyAt = uint64(block.timestamp) + unbondingPeriod;
        u.readyAt = readyAt;
        emit UnbondRequested(signer, amount, readyAt);
    }

    /// @inheritdoc ISignerBond
    function cancelUnbond() external nonReentrant {
        uint8 signer = _signerIndex(msg.sender);
        uint256 amount = unbondingOf[signer].amount;
        if (amount == 0) revert NothingUnbonding(signer);
        delete unbondingOf[signer];
        bondOf[signer] += amount;
        emit UnbondCancelled(signer, amount);
    }

    /// @inheritdoc ISignerBond
    function withdrawUnbonded(address to) external nonReentrant {
        if (to == address(0)) revert ZeroAddress();
        uint8 signer = _signerIndex(msg.sender);
        Unbonding memory u = unbondingOf[signer];
        if (u.amount == 0) revert NothingUnbonding(signer);
        if (block.timestamp < u.readyAt) revert UnbondingNotReady(u.readyAt);
        uint256 openCases = IZecuredSlasher(slasher).openCaseCount();
        if (openCases != 0) revert CaseOpen(openCases);
        delete unbondingOf[signer];
        emit BondWithdrawn(signer, to, u.amount);
        usdc.safeTransfer(to, u.amount);
    }

    // ------------------------------------------------------------------ theft (the slasher)

    /// @inheritdoc ISignerBond
    function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary)
        external
        nonReentrant
        returns (uint256 slashed)
    {
        if (msg.sender != slasher) revert NotTheSlasher(msg.sender);
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if ((signerBitmap >> i) & 1 == 0) continue;
            uint256 amount = bondOf[i] + unbondingOf[i].amount;
            if (amount == 0) continue;
            bondOf[i] = 0;
            delete unbondingOf[i];
            slashed += amount;
            emit BondSlashed(caseId, i, amount);
        }
        uint256 bounty = Math.min(slashed * bountyBps / BPS, bountyCap);
        if (bounty != 0) {
            bountyOf[beneficiary] += bounty;
            emit BountyCredited(caseId, beneficiary, bounty);
        }
        if (!halted) {
            halted = true;
            emit Halted(caseId);
        }
        uint256 rest = slashed - bounty;
        if (rest != 0) {
            usdc.safeTransfer(compensator, rest);
            IZecuredCompensator(compensator).onFunds(caseId, rest);
        }
    }

    /// @inheritdoc ISignerBond
    function claimBounty() external nonReentrant {
        uint256 amount = bountyOf[msg.sender];
        if (amount == 0) revert NothingToClaim(msg.sender);
        bountyOf[msg.sender] = 0;
        emit BountyClaimed(msg.sender, amount);
        usdc.safeTransfer(msg.sender, amount);
    }

    // ------------------------------------------------------------------ liveness (the claimant, T-05)

    /// @inheritdoc ISignerBond
    function armLiveness(bytes32 claimId, uint64 receivedAt, bytes calldata signature) external nonReentrant {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        if (!_livenessEligible(c)) revert NotLivenessEligible(claimId);
        if (receivedAt > block.timestamp) revert ReceiptFromTheFuture(receivedAt);
        // The receipt is for the claim's current commitment: one for an earlier destination recovers to no signer.
        _signerIndex(ECDSA.recoverCalldata(_hashDestinationReceived(claimId, c.commitment, receivedAt), signature));
        uint64 payableAt = receivedAt > c.createdAt ? receivedAt : uint64(c.createdAt);
        Arming storage a = _armings[claimId];
        if (a.commitment == c.commitment && a.payableAt != 0 && a.payableAt <= payableAt) {
            payableAt = a.payableAt; // the earlier receipt stands
        } else {
            a.commitment = c.commitment;
            a.payableAt = payableAt;
        }
        emit LivenessArmed(claimId, c.commitment, payableAt);
    }

    /// @inheritdoc ISignerBond
    function slashLiveness(bytes32 claimId) external nonReentrant returns (uint256 paid) {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        uint64 payableAt = _payableAt(claimId, c);
        // R-5: no receipt, no liveness.
        if (!_livenessEligible(c) || payableAt == 0) revert NotLivenessEligible(claimId);
        // Also refuses a claim without a claimant: no caller is the zero address.
        if (msg.sender != c.claimant) revert NotTheClaimant(claimId, msg.sender);
        if (IZecuredSlasher(slasher).vaultFrozen(c.vaultId)) revert NotLivenessEligible(claimId);
        // R-7: a claim the compensator owes (an F3 shortfall) is paid there, never twice.
        if (_entitled(claimId)) revert NotLivenessEligible(claimId);
        uint64 dueAt = _livenessDueAt(claimId, payableAt);
        if (block.timestamp < dueAt) revert LivenessNotDue(claimId, dueAt);
        // R-9: never at a price no source confirms any more.
        if (_priceStale()) revert PriceStale();
        uint256 price = oracle.price();
        paid = _livenessAmount(c.amount, price);
        if (paid == 0) revert ZeroAmount();
        uint256 slashable = totalSlashable();
        if (slashable < paid) revert InsufficientBond(paid, slashable);
        _takeProRata(paid, slashable);
        IZecuredRegistry(registry).markBondPaid(claimId);
        emit LivenessPaid(claimId, msg.sender, paid, price);
        usdc.safeTransfer(msg.sender, paid);
    }

    /// @inheritdoc ISignerBond
    /// @dev `(0, 0)` for a claim that is not payable from the bonds now (also one without a claimant). Otherwise `dueAt`
    ///      may be in the past, and `amount` is zero at a zero price (`slashLiveness` would revert `ZeroAmount`). It
    ///      does not check that the bonds cover `amount`.
    function livenessDue(bytes32 claimId) external view returns (uint64 dueAt, uint256 amount) {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        uint64 payableAt = _payableAt(claimId, c);
        if (
            !_livenessEligible(c) || payableAt == 0 || c.claimant == address(0)
                || IZecuredSlasher(slasher).vaultFrozen(c.vaultId) || _entitled(claimId) || _priceStale()
        ) {
            return (0, 0);
        }
        dueAt = _livenessDueAt(claimId, payableAt);
        amount = _livenessAmount(c.amount, oracle.price());
    }

    /// @inheritdoc ISignerBond
    function livenessArming(bytes32 claimId) external view returns (bytes32 commitment, uint64 payableAt) {
        Arming storage a = _armings[claimId];
        return (a.commitment, a.payableAt);
    }

    /// @inheritdoc ISignerBond
    function hashDestinationReceived(bytes32 claimId, bytes32 commitment, uint64 receivedAt)
        external
        view
        returns (bytes32)
    {
        return _hashDestinationReceived(claimId, commitment, receivedAt);
    }

    /// @inheritdoc ISignerBond
    function domainSeparator() external view returns (bytes32) {
        return _domainSeparatorV4();
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc ISignerBond
    function effectiveBond() external view returns (uint256) {
        return _effectiveBond(_bonds());
    }

    /// @inheritdoc ISignerBond
    function fullyBonded() external view returns (bool) {
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if (bondOf[i] < minBond) return false;
        }
        return true;
    }

    /// @inheritdoc ISignerBond
    function totalSlashable() public view returns (uint256 total) {
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            total += bondOf[i] + unbondingOf[i].amount;
        }
    }

    // ------------------------------------------------------------------ internals

    /// @dev The caller's index in the Registry's sorted signer set; reverts for anyone else.
    function _signerIndex(address account) internal view returns (uint8) {
        address[5] memory signers = IZecuredRegistry(registry).signers();
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            if (signers[i] == account) return i;
        }
        revert NotASigner(account);
    }

    function _bonds() internal view returns (uint256[5] memory bonds) {
        for (uint8 i; i < SIGNER_COUNT; ++i) {
            bonds[i] = bondOf[i];
        }
    }

    /// @dev `min(sum, 5 * S3 / 3)`, S3 the sum of the three smallest bonds (the sum less the two largest), rounded down.
    ///      Three colluders lose at least 3/5 of it. The mean of the three smallest never exceeds the mean of all five,
    ///      so the second term is the smaller one; the `min` states the bound. Every bond is held here, so the sum fits,
    ///      and `5 * S3 <= 3 * sum` could only overflow for a token supply above 2^256 / 3.
    function _effectiveBond(uint256[5] memory bonds) internal pure returns (uint256) {
        (uint256 sum, uint256 cheapestThree) = _sumAndCheapestThree(bonds);
        uint256 capped = cheapestThree * 5 / 3;
        // A plain comparison rather than Math.min's branch-free product, so that the symbolic checks stay linear.
        return capped < sum ? capped : sum;
    }

    /// @dev The sum of the bonds, and the sum of the three smallest (the sum less the two largest).
    function _sumAndCheapestThree(uint256[5] memory bonds) internal pure returns (uint256 sum, uint256 cheapestThree) {
        uint256 largest;
        uint256 second;
        for (uint256 i; i < SIGNER_COUNT; ++i) {
            uint256 b = bonds[i];
            sum += b;
            if (b > largest) {
                second = largest;
                largest = b;
            } else if (b > second) {
                second = b;
            }
        }
        cheapestThree = sum - largest - second;
    }

    /// @dev Kind Release, Liquidation, CloseUser, CloseMarketMaker or Refund (not Fee, not Retired); pending; not paid
    ///      from the bonds yet; with a destination.
    function _livenessEligible(ClaimDetails memory c) internal pure returns (bool) {
        ClaimKind k = c.kind;
        bool kindOk = k == ClaimKind.Release || k == ClaimKind.Liquidation || k == ClaimKind.CloseUser
            || k == ClaimKind.CloseMarketMaker || k == ClaimKind.Refund;
        return kindOk && c.status == ClaimStatus.Pending && !c.bondPaid && c.commitment != bytes32(0);
    }

    /// @dev R-5: when the claim became payable: its arming's `payableAt`, but never before its current `createdAt` (a
    ///      refund's clock restarts with each destination its account gives); zero if it was never armed, or armed for
    ///      another commitment.
    function _payableAt(bytes32 claimId, ClaimDetails memory c) internal view returns (uint64) {
        Arming storage a = _armings[claimId];
        uint64 payableAt = a.payableAt;
        if (payableAt == 0 || a.commitment != c.commitment) return 0;
        return payableAt > c.createdAt ? payableAt : uint64(c.createdAt);
    }

    /// @dev R-7: the compensator registered an entitlement for the claim (an F3 shortfall, or its tally).
    function _entitled(bytes32 claimId) internal view returns (bool) {
        return IZecuredCompensator(compensator).entitlement(claimId).incident != 0;
    }

    /// @dev R-9: the oracle reports a feed state and it is `Stale`.
    function _priceStale() internal view returns (bool) {
        return priceStateChecked && IZecuredPriceFeed(address(oracle)).state() == IZecuredPriceFeed.FeedState.Stale;
    }

    /// @dev R-9, at construction: `oracle_` answers `state()` with a feed state.
    function _reportsState(address oracle_) internal view returns (bool) {
        (bool ok, bytes memory ret) = oracle_.staticcall(abi.encodeCall(IZecuredPriceFeed.state, ()));
        return ok && ret.length == 32 && uint256(bytes32(ret)) <= uint256(type(IZecuredPriceFeed.FeedState).max);
    }

    function _hashDestinationReceived(bytes32 claimId, bytes32 commitment, uint64 receivedAt)
        internal
        view
        returns (bytes32)
    {
        return _hashTypedDataV4(keccak256(abi.encode(DESTINATION_RECEIVED_TYPEHASH, claimId, commitment, receivedAt)));
    }

    /// @dev T-05, deferrals bounded (design review): `deadline = payableAt + livenessPeriod`; a claim bound to an input
    ///      waits `commitGrace` after its input's latest commitment, but a commitment made after the deadline never
    ///      extends that: the claim is due at `max(deadline, min(lastCommitAt, deadline) + commitGrace)`, at most
    ///      `livenessPeriod + commitGrace` after it became payable, however often the signers re-commit.
    function _livenessDueAt(bytes32 claimId, uint64 payableAt) internal view returns (uint64 dueAt) {
        uint64 deadline = payableAt + livenessPeriod;
        dueAt = deadline;
        bytes32 input = IZecuredSpendCommits(registry).claimInput(claimId);
        if (input != bytes32(0)) {
            uint64 lastCommit = IZecuredSpendCommits(registry).lastCommitAt(input);
            uint64 protectedUntil = (lastCommit < deadline ? lastCommit : deadline) + commitGrace;
            if (protectedUntil > dueAt) dueAt = protectedUntil;
        }
    }

    /// @dev `zat * price * (10,000 + livenessPremiumBps) / 10,000 / 1e36` in USDC units, rounded down once (the price is
    ///      Morpho-scaled, so this is the claim's value at the oracle's price plus the premium).
    function _livenessAmount(uint64 zat, uint256 price) internal view returns (uint256) {
        return Math.mulDiv(price, uint256(zat) * (BPS + livenessPremiumBps), BPS * ORACLE_PRICE_SCALE);
    }

    /// @dev Takes `amount` (at most `slashable`, the total) from the signers pro rata; see the contract's notes.
    function _takeProRata(uint256 amount, uint256 slashable) internal {
        uint256 left = amount;
        uint256 pool = slashable;
        for (uint8 i; i < SIGNER_COUNT && left != 0; ++i) {
            uint256 bond = bondOf[i];
            uint256 unbonding = unbondingOf[i].amount;
            uint256 own = bond + unbonding;
            if (own == 0) continue;
            // left <= pool, so take <= own; for the last signer with a slashable amount, pool == own and take == left.
            uint256 take = Math.mulDiv(left, own, pool);
            left -= take;
            pool -= own;
            if (take <= bond) {
                bondOf[i] = bond - take;
            } else {
                bondOf[i] = 0;
                uint256 unbondingLeft = unbonding - (take - bond);
                if (unbondingLeft == 0) delete unbondingOf[i];
                else unbondingOf[i].amount = unbondingLeft;
            }
        }
    }
}
