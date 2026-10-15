// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {ISignerBond, BondConfig} from "./interfaces/v3/ISignerBond.sol";
import {IZecuredRegistry, ClaimDetails, ClaimKind, ClaimStatus} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommits} from "./interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredSlasher} from "./interfaces/v3/IZecuredSlasher.sol";
import {IZecuredIntake} from "./interfaces/v3/IZecuredIntake.sol";
import {IZecuredCompensator} from "./interfaces/v3/IZecuredCompensator.sol";

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
///      - Every state-changing function is `nonReentrant`. A deposit pulls the USDC before crediting it, and every payment
///        (unbonding, bounty, slash, liveness) updates the books before the USDC leaves or another contract is called,
///        so whenever another contract runs, the bond's views match its balance.
contract SignerBondV2 is ISignerBond, ReentrancyGuardTransient {
    using SafeERC20 for IERC20;

    uint256 private constant BPS = 10_000;
    uint8 private constant SIGNER_COUNT = 5;

    /// @notice A signer's running unbonding: slashable until withdrawn.
    struct Unbonding {
        uint256 amount;
        uint64 readyAt;
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
    mapping(uint8 signer => uint256) public bondOf;
    /// @inheritdoc ISignerBond
    mapping(uint8 signer => Unbonding) public unbondingOf;
    /// @inheritdoc ISignerBond
    bool public halted;
    /// @inheritdoc ISignerBond
    mapping(address beneficiary => uint256) public bountyOf;

    /// @param cfg See `BondConfig`. The Registry, the slasher, the Intake and the compensator may be predicted addresses:
    ///        nothing is called here.
    constructor(BondConfig memory cfg) {
        if (
            cfg.registry == address(0) || cfg.slasher == address(0) || cfg.intake == address(0)
                || cfg.compensator == address(0) || address(cfg.usdc) == address(0) || address(cfg.oracle) == address(0)
        ) revert ZeroAddress();
        if (
            cfg.minBond == 0 || cfg.challengeWindow == 0 || cfg.livenessPeriod == 0
                || cfg.unbondingPeriod < uint256(cfg.challengeWindow) + cfg.livenessPeriod || cfg.bountyBps > BPS
        ) revert InvalidConfig();
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
    function slashLiveness(bytes32 claimId) external nonReentrant returns (uint256 paid) {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        if (!_livenessEligible(c)) revert NotLivenessEligible(claimId);
        // Also refuses a claim without a claimant: no caller is the zero address.
        if (msg.sender != c.claimant) revert NotTheClaimant(claimId, msg.sender);
        if (IZecuredSlasher(slasher).vaultFrozen(c.vaultId)) revert NotLivenessEligible(claimId);
        uint64 dueAt = _livenessDueAt(claimId, c.createdAt);
        if (block.timestamp < dueAt) revert LivenessNotDue(claimId, dueAt);
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
    /// @dev `(0, 0)` for a claim that is never payable from the bonds (also one without a claimant). Otherwise `dueAt`
    ///      may be in the past, and `amount` is zero at a zero price (`slashLiveness` would revert `ZeroAmount`). It
    ///      does not check that the bonds cover `amount`.
    function livenessDue(bytes32 claimId) external view returns (uint64 dueAt, uint256 amount) {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        if (!_livenessEligible(c) || c.claimant == address(0) || IZecuredSlasher(slasher).vaultFrozen(c.vaultId)) {
            return (0, 0);
        }
        dueAt = _livenessDueAt(claimId, c.createdAt);
        amount = _livenessAmount(c.amount, oracle.price());
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

    /// @dev T-05 with Claude Web's bound (approval § 3): `deadline = createdAt + livenessPeriod`; a claim bound to an
    ///      input waits `commitGrace` after its input's latest commitment, but a commitment made after the deadline
    ///      never extends that: the claim is due at `max(deadline, min(lastCommitAt, deadline) + commitGrace)`, at
    ///      most `livenessPeriod + commitGrace` after it became payable, however often the signers re-commit.
    function _livenessDueAt(bytes32 claimId, uint40 createdAt) internal view returns (uint64 dueAt) {
        uint64 deadline = uint64(createdAt) + livenessPeriod;
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
