// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IMorpho, MarketParams, Id, Market} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {MarketParamsLib} from "morpho-blue/src/libraries/MarketParamsLib.sol";
import {SharesMathLib} from "morpho-blue/src/libraries/SharesMathLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IZecuredCompensator, Incident, Victim, Entitlement} from "./interfaces/v3/IZecuredCompensator.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails,
    VaultDetails
} from "./interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommits} from "./interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredDisputes} from "./interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "./interfaces/v3/IZecuredPayouts.sol";
import {IZecuredRefunds} from "./interfaces/v3/IZecuredRefunds.sol";
import {IZecuredSlasher, ViolationKind} from "./interfaces/v3/IZecuredSlasher.sol";
import {ISignerBond} from "./interfaces/v3/ISignerBond.sol";
import {CompensationMath} from "./libraries/CompensationMath.sol";

/// @title ZecuredCompensator
/// @notice Pays the victims of a proven theft from the slashed bonds (design notes T-04, rules in
///         `IZecuredCompensator`): third-party claimants, then the lenders (by repaying the victim vault's Morpho debt and
///         retiring its unbacked zZEC), then the owners. No owner, no upgrade path; every step is pulled by anyone, one
///         vault or one claim per call (EIP-7825).
/// @dev How this implementation reads the interface, where its NatSpec leaves room or could not be followed literally:
///      - Claims. `tallyClaim` records a pending claim of a victim vault AND settles it in the Registry
///        (`settleCompensatedClaim`) in the same call, rather than in `settleVault`: the Registry refuses
///        `commitSpend` and `confirmPayout` on a compromised vault, and the Registry's own `unpaidClaims` then counts
///        exactly the claims left to tally, so `tallyVault` requires it to be zero and keeps no count of its own that
///        could drift from it. A committed claim's payout may have landed before or during the compromise (R-2): it is
///        tallied only once `inclusionWindow` has passed since the compromise, and a proof of its payout meanwhile
///        pays it for good instead. Fee and bond-paid claims are settled too, without an entitlement (`ClaimTallied`
///        with layer 0): they are the signers' own. `Victim.pendingClaims` counts the claims tallied with an
///        entitlement, `Victim.talliedClaims` every claim settled. Once a vault is compromised, the Registry lets no
///        claim become pending except through the Intake's `refundPending` of a Pending vault, the end of a dispute
///        opened before (`IZecuredDisputes`: a disputed claim counts as unpaid, and is upheld or reopened within the
///        inclusion window), or the opening of a refund reserved before the compromise (R-12: `finalizeRefund`; while
///        one is reserved, `IZecuredRefunds.reservedRefundCount` holds the tally), all of which `tallyVault` waits for;
///        a claim pending after its incident was finalized could not be tallied any more and would keep its vault open.
///      - R-3: a claim three signers confirmed within the dispute window before the vault froze is tallied like a
///        pending one (the signers' word, given shortly before a proven theft, counts for nothing) unless a proof of its
///        payout comes first; `tallyVault` waits until the vault has no confirmation left that is neither proven,
///        expired (`IZecuredPayouts.expireConfirmation`, for one older than that) nor tallied. R-21: an abandoned
///        refund is tallied on request until the incident is finalized (it does not hold up `tallyVault`: its account
///        gave up on it).
///      - Layer 3 counts each authorized victim's debt at tally time plus a 1 % interest allowance
///        (`CompensationMath.INTEREST_ALLOWANCE_BPS`), and layer 4 its collateral value less that. The allowance only
///        pays interest the account's own equity cannot: the account never keeps more than its equity without it, and
///        what nobody needed goes back to the pool (`CompensationMath.shareOf`). Without it, an underwater vault (or any
///        vault while layer 4 gets nothing) could never be repaid in full after interest accrued, so never retired.
///      - Finalizing prices every victim's equity at P*, which takes one storage read per victim: `finalizeIncident`
///        prices at most `FINALIZE_BATCH` victims per call and completes on the call that prices the last one (P* is
///        fixed by the first call). Incidents are finalized in order, so a later incident never takes the funds first.
///      - F3: a claim's entitlement is the smallest of what is known about its loss: a later shortfall lowers an unpaid
///        entitlement (the claimant did receive the rest), it never raises one, and it never adds a second one.
///      - Partial settlement: a vault whose share does not cover its debt has the share repaid, and the Registry burns
///        the collateral beyond the least that keeps the rest of the debt healthy at the LLTV (`retireVault` with an
///        amount), so the repayment cannot be borrowed again against the vault's unbacked zZEC. The vault keeps that
///        collateral and the rest of its debt, frozen in the Registry (no exit, no liquidation), and stays open, which
///        keeps the leftover locked. While layer 4 is not paid in full, an account below the LLTV can borrow past its
///        share before `settleVault` to reach this case. A vault whose account revoked the Registry's authorization is
///        never retired and stays open as well.
///      - The freeze does not stop anyone repaying the vault's debt directly on Morpho; that only lowers what the vault
///        needs, and what the account repaid itself comes back to it with its equity.
///      - R-19: a vault's collateral is burned down to the LLTV as soon as it is compromised (`burnCollateral`, which
///        the slasher calls), so the account cannot borrow more against unbacked zZEC; `tallyVault` adds what was burned
///        before it back to the outstanding it records, so the vault's equity is that of what it held when compromised,
///        and `settleVault` retires the outstanding left at that time. Between a theft case's proof and its execution
///        (the challenge window) the vault is frozen but Morpho still lends against its collateral: that exposure is
///        bounded by the LLTV and paid like any debt (layer 3).
///      - R-20: a vault still active after `settleVault` (its debt only partly covered, or its account no longer
///        authorizing the Registry) is marked settled in the Registry, which then counts it neither as open nor as
///        locked; anyone burns its collateral again later, down to the LLTV, or retires it once its debt is gone.
contract ZecuredCompensator is IZecuredCompensator, ReentrancyGuardTransient {
    using SafeERC20 for IERC20;
    using SharesMathLib for uint256;
    using MarketParamsLib for MarketParams;

    /// @notice Longest incident window the constructor accepts (the mainnet value is 7 days).
    uint64 public constant MAX_INCIDENT_WINDOW = 365 days;
    /// @notice Most victims one `finalizeIncident` call prices: about 8,500 gas each from cold storage, so a full batch
    ///         takes about 2.2 million gas, far below EIP-7825's 2^24 per transaction.
    uint256 public constant FINALIZE_BATCH = 256;
    /// @notice Gas an F3 or F4 case's execution must leave `onCase` (R-7, W-1): the registration of the shortfall reads
    ///         the claim under a `try`, and the claim is then closed in the Registry, also under a `try` (about 175,000
    ///         gas in all from cold storage); an executor who starved them could leave the claimant without its
    ///         shortfall, or the claim open to the bonds as well, so a starved execution reverts instead.
    uint256 public constant F3_GAS = 300_000;

    /// @inheritdoc IZecuredCompensator
    address public immutable registry;
    /// @inheritdoc IZecuredCompensator
    address public immutable slasher;
    /// @inheritdoc IZecuredCompensator
    address public immutable signerBond;
    /// @inheritdoc IZecuredCompensator
    IERC20 public immutable usdc;
    /// @inheritdoc IZecuredCompensator
    uint64 public immutable incidentWindow;
    /// @inheritdoc IZecuredCompensator
    address public immutable leftoverSink;

    // The market, read from the Registry at construction (its loan token is `usdc`).
    IMorpho private immutable _morpho;
    Id private immutable _marketId;
    address private immutable _collateralToken;
    address private immutable _oracle;
    address private immutable _irm;
    uint256 private immutable _lltv;

    /// @inheritdoc IZecuredCompensator
    uint256 public currentIncident;
    /// @inheritdoc IZecuredCompensator
    uint256 public funds;
    /// @notice USDC set aside for finalized incidents and not yet paid out or given back to `funds` (I19:
    ///         `usdc.balanceOf(this) == funds + reserved`, plus anything sent here directly).
    uint256 public reserved;
    /// @inheritdoc IZecuredCompensator
    mapping(bytes32 vaultId => uint256) public equityOf;

    /// @dev Running totals of an incident, filled by `onCase`, `tallyClaim`, `tallyVault` and `finalizeIncident`.
    struct Totals {
        uint256 thirdPartyZat; // layer 2, in zatoshi
        uint256 ownerZat; // owners' claims (layer 4), in zatoshi
        uint256 lenders; // layer 3: sum of `lendersOf(debt)` over the authorized victims
        uint256 equity; // equity at P* of the victims priced so far (layer 4)
        uint256 priced; // victims priced so far
    }

    uint256 private _openIncidents;
    mapping(uint256 id => Incident) private _incidents;
    mapping(uint256 id => Totals) private _totals;
    mapping(uint256 id => mapping(uint256 index => bytes32 vaultId)) private _victimAt;
    mapping(bytes32 vaultId => Victim) private _victims;
    mapping(bytes32 claimId => Entitlement) private _entitlements;
    /// @dev R-19: zatoshi of a victim's collateral burned before its tally (counted in the outstanding `tallyVault`
    ///      records, so the equity is that of what the vault held when compromised).
    mapping(bytes32 vaultId => uint64) private _burnedBeforeTally;

    /// @notice `finalizeIncident` of an incident that was never opened.
    error UnknownIncident(uint256 incident);
    /// @notice The market oracle returned zero: P* cannot be fixed now.
    error ZeroPrice();

    /// @param registry_ ZecuredRegistry v3; the market (Morpho, oracle, loan token) is read from it.
    /// @param slasher_ ZecuredSlasher v2: the only caller of `onCase`.
    /// @param signerBond_ SignerBond v2: the only caller of `onFunds`.
    /// @param usdc_ The market's loan token, in which the bonds are held.
    /// @param incidentWindow_ How long an incident takes in new cases (testnet 3 days, theft demo 10 minutes, mainnet
    ///        7 days).
    /// @param leftoverSink_ Where `sweepLeftover` sends what is left (testnet: 0x...dEaD, a burn; a public-good address
    ///        is a mainnet option). Never the signers: an F1 does not tell who signed, so a refund could pay the thief.
    /// @dev Deployed after the Registry, the bond and the slasher, which name it at its predicted address: the
    ///      constructor checks that the four contracts name each other.
    constructor(
        address registry_,
        address slasher_,
        address signerBond_,
        IERC20 usdc_,
        uint64 incidentWindow_,
        address leftoverSink_
    ) {
        if (
            registry_ == address(0) || slasher_ == address(0) || signerBond_ == address(0)
                || address(usdc_) == address(0) || leftoverSink_ == address(0)
        ) revert ZeroAddress();
        if (incidentWindow_ == 0 || incidentWindow_ > MAX_INCIDENT_WINDOW) revert InvalidConfig();
        IMorpho morpho_ = IZecuredRegistry(registry_).morpho();
        MarketParams memory params = IZecuredRegistry(registry_).marketParams();
        if (
            address(morpho_) == address(0) || params.loanToken != address(usdc_)
                || IZecuredRegistry(registry_).compensator() != address(this)
                || IZecuredRegistry(registry_).slasher() != slasher_
                || IZecuredSpendCommits(registry_).signerBond() != signerBond_
                || IZecuredSlasher(slasher_).compensator() != address(this)
                || IZecuredSlasher(slasher_).registry() != registry_
                || IZecuredSlasher(slasher_).signerBond() != signerBond_
                || ISignerBond(signerBond_).compensator() != address(this)
                || ISignerBond(signerBond_).registry() != registry_ || ISignerBond(signerBond_).slasher() != slasher_
                || address(ISignerBond(signerBond_).usdc()) != address(usdc_)
        ) revert InvalidConfig();
        registry = registry_;
        slasher = slasher_;
        signerBond = signerBond_;
        usdc = usdc_;
        incidentWindow = incidentWindow_;
        leftoverSink = leftoverSink_;
        _morpho = morpho_;
        _marketId = params.id();
        _collateralToken = params.collateralToken;
        _oracle = params.oracle;
        _irm = params.irm;
        _lltv = params.lltv;
    }

    // ------------------------------------------------------------------ hooks

    /// @inheritdoc IZecuredCompensator
    /// @dev The bond transferred `amount` before the call.
    function onFunds(bytes32 caseId, uint256 amount) external nonReentrant {
        if (msg.sender != signerBond) revert OnlyBond(msg.sender);
        funds += amount;
        emit FundsReceived(caseId, amount);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Called inside `executeSlash`, so it never reverts on what it is told: F1 and F2 register the vault in the
    ///      current incident (once: a vault already registered, in any incident, is left as it is); F3 registers the
    ///      claim's shortfall in the claim's layer (nothing for a Fee, Retired or bond-paid claim, or one without a
    ///      claimant) and closes the claim in the Registry (R-7: its payout landed, short; the shortfall is owed here
    ///      and nowhere else); F4 does the same for the claim its commitment hid, at the claim's whole `expected`
    ///      (W-1: hiding the payout is the violation; the part it may have paid the right recipient cannot be told
    ///      without the claimant's viewing key), also when a proof made the claim final first (the colluders can always
    ///      prove before the case executes): `_registerShortfall` takes any claim that exists, and closing a final
    ///      claim does nothing.
    function onCase(bytes32 caseId, ViolationKind kind, bytes32 vaultId, bytes32 claimId, uint64 shortfallZat)
        external
        nonReentrant
    {
        if (msg.sender != slasher) revert OnlySlasher(msg.sender);
        if (kind == ViolationKind.Uncommitted || kind == ViolationKind.WithheldChange) {
            if (vaultId == bytes32(0) || _victims[vaultId].incident != 0) return;
            uint256 id = _incidentNow();
            _victims[vaultId].incident = id;
            Incident storage inc = _incidents[id];
            uint32 index = inc.victims;
            _victimAt[id][index] = vaultId;
            inc.victims = index + 1;
            emit VictimRegistered(id, vaultId, caseId);
        } else if (kind == ViolationKind.Misdirected || kind == ViolationKind.WrongClaimRef) {
            if (gasleft() < F3_GAS) revert InsufficientGasForCase();
            _registerShortfall(claimId, shortfallZat);
            try IZecuredPayouts(registry).closeMisdirectedClaim(claimId) {} catch {}
        }
    }

    // ------------------------------------------------------------------ tally

    /// @inheritdoc IZecuredCompensator
    /// @dev Also marks the claim Compensated in the Registry (see the contract's notes). A Fee or bond-paid claim, or one
    ///      an F3 shortfall already covers, is settled without a new entitlement (`ClaimTallied(claimId, 0, 0)`). Takes
    ///      a pending claim, a confirmation by signatures made within the dispute window before the vault froze (R-3)
    ///      and an abandoned refund (R-21); a committed claim only once `inclusionWindow` has passed since the vault was
    ///      compromised (R-2: until then a proof of its payout pays it for good instead).
    function tallyClaim(bytes32 claimId) external nonReentrant {
        ClaimDetails memory c = IZecuredRegistry(registry).claimDetails(claimId);
        Victim storage v = _victims[c.vaultId];
        uint256 id = v.incident;
        if (id == 0) revert NotVictimClaim(claimId);
        if (c.status == ClaimStatus.Compensated) revert AlreadyTallied(claimId);
        if (!_tallyable(claimId, c)) revert NotVictimClaim(claimId);
        if (_incidents[id].finalized) revert AlreadyFinalized(id);
        if (IZecuredSpendCommits(registry).claimInput(claimId) != bytes32(0)) {
            uint64 at = IZecuredSlasher(slasher).compromisedAt(c.vaultId) + IZecuredDisputes(registry).inclusionWindow();
            if (block.timestamp < at) revert ProofWindowOpen(claimId, at);
        }

        v.talliedClaims += 1;
        uint8 layer = _layerOf(c.kind);
        if (layer == 0 || c.bondPaid || c.claimant == address(0) || _entitlements[claimId].incident != 0) {
            emit ClaimTallied(claimId, 0, 0);
        } else {
            v.pendingClaims += 1;
            _entitlements[claimId] =
                Entitlement({incident: id, layer: layer, zat: c.amount, to: c.claimant, paid: false});
            _addZat(id, layer, c.amount);
            emit ClaimTallied(claimId, layer, c.amount);
        }
        IZecuredPayouts(registry).settleCompensatedClaim(claimId);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Requires the Registry's `unpaidClaims` of the vault to be zero (every claim went through `tallyClaim`), R-12,
    ///      no refund of it still reserved (each opens a claim to tally first, or is released) and, R-3, no confirmation
    ///      by signatures left open (`unprovenConfirmations`). R-19: the outstanding it records adds back what
    ///      `burnCollateral` burned before, so the equity is that of what the vault held.
    function tallyVault(bytes32 vaultId) external nonReentrant {
        Victim storage v = _victims[vaultId];
        uint256 id = v.incident;
        if (id == 0) revert NotVictim(vaultId);
        if (v.tallied) revert AlreadyTallied(vaultId);
        VaultDetails memory d = IZecuredRegistry(registry).vaultDetails(vaultId);
        if (d.status == VaultStatus.Pending) revert VaultStillPending(vaultId);
        if (d.unpaidClaims != 0) revert ClaimsNotTallied(vaultId, d.unpaidClaims);
        uint256 reservedRefunds = IZecuredRefunds(registry).reservedRefundCount(vaultId);
        if (reservedRefunds != 0) revert RefundsReserved(vaultId, reservedRefunds);
        uint256 unproven = IZecuredPayouts(registry).unprovenConfirmations(vaultId);
        if (unproven != 0) revert ConfirmationsUnsettled(vaultId, unproven);

        (uint256 debt, bool authorized) = _snapshot(d.account);
        // W-16: a vault no longer active (retired at its compromise, R-19, or never activated) has no collateral left to
        // withdraw: its account's authorization does not matter, and revoking it must not cost the owner its equity.
        authorized = authorized || d.status != VaultStatus.Active;
        uint64 outstanding = d.outstanding + _burnedBeforeTally[vaultId];
        v.tallied = true;
        v.authorized = authorized;
        v.outstanding = outstanding;
        v.debt = debt;
        _incidents[id].talliedVictims += 1;
        if (authorized) _totals[id].lenders += CompensationMath.lendersOf(debt);
        emit VaultTallied(vaultId, outstanding, debt, authorized);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Prices at most `FINALIZE_BATCH` victims per call; call again until `incident(id).finalized`. P* is the
    ///      oracle's price at the first call. Requires the previous incident to be finalized.
    function finalizeIncident(uint256 id) external nonReentrant {
        if (id == 0 || id > currentIncident) revert UnknownIncident(id);
        Incident storage inc = _incidents[id];
        if (inc.finalized) revert AlreadyFinalized(id);
        if (block.timestamp < inc.closesAt) revert IncidentStillOpen(id, inc.closesAt);
        if (inc.talliedVictims != inc.victims) revert VictimsNotTallied(id, inc.victims - inc.talliedVictims);
        if (id > 1 && !_incidents[id - 1].finalized) revert NotFinalized(id - 1);

        uint256 price = inc.price;
        if (price == 0) {
            price = IOracle(_oracle).price();
            if (price == 0) revert ZeroPrice();
            inc.price = price;
        }
        Totals storage t = _totals[id];
        uint256 count = inc.victims;
        uint256 i = t.priced;
        uint256 end = Math.min(count, i + FINALIZE_BATCH);
        uint256 equity = t.equity;
        for (; i < end; ++i) {
            Victim storage v = _victims[_victimAt[id][i]];
            if (v.authorized) equity += CompensationMath.equityAt(v.outstanding, v.debt, price);
        }
        t.equity = equity;
        t.priced = end;
        if (end < count) return;

        uint256 layer2 = CompensationMath.valueOf(t.thirdPartyZat, price);
        uint256 layer3 = t.lenders;
        uint256 layer4 = CompensationMath.valueOf(t.ownerZat, price) + equity;
        uint256 available = funds;
        (uint256 r2, uint256 r3, uint256 r4, uint256 used) =
            CompensationMath.allocate(available, layer2, layer3, layer4);
        funds = available - used;
        reserved += used;
        _openIncidents -= 1;
        inc.finalized = true;
        inc.layer2 = layer2;
        inc.layer3 = layer3;
        inc.layer4 = layer4;
        inc.r2 = r2;
        inc.r3 = r3;
        inc.r4 = r4;
        emit IncidentFinalized(id, price, layer2, layer3, layer4, r2, r3, r4);
    }

    // ------------------------------------------------------------------ settle and pay

    /// @inheritdoc IZecuredCompensator
    /// @dev The vault's share is `lendersOf(debt) * r3 + equity * r4` (zero for a vault unauthorized at tally). With
    ///      the debt now `D`:
    ///      - an active vault whose account no longer authorizes the Registry: nothing is repaid, the share goes back to
    ///        `funds`;
    ///      - `D <= share`: the whole debt is repaid (by shares), an active vault is retired, and the account's equity is
    ///        what is left, up to `max(0, value - debt) * r4 + debt - D` (`CompensationMath.shareOf`): interest and new
    ///        borrowing since the tally come out of the equity, debt repaid directly comes back to the account, and the
    ///        rest (the interest allowance nobody needed) goes back to `funds`;
    ///      - `D > share`: the share is repaid and an active vault's collateral is burned down to the LLTV (see the
    ///        contract's notes on partial settlement).
    function settleVault(bytes32 vaultId) external nonReentrant {
        Victim storage v = _victims[vaultId];
        uint256 id = v.incident;
        if (id == 0) revert NotVictim(vaultId);
        if (v.settled) revert AlreadySettled(vaultId);
        Incident storage inc = _incidents[id];
        if (!inc.finalized) revert NotFinalized(id);
        v.settled = true;

        (uint256 repaid, uint64 retired, uint256 equity) = _settle(vaultId, v, inc);
        emit VaultSettled(vaultId, repaid, retired, equity);
        // R-20: a vault left active (partly covered, or unauthorized) no longer holds the bonds in or the cap up.
        (,, VaultStatus status) = IZecuredRegistry(registry).vault(vaultId);
        if (status == VaultStatus.Active) IZecuredPayouts(registry).markVaultCompensated(vaultId);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev R-19 at the compromise (the slasher calls it) and any time after, R-20 after the settlement: with debt, down
    ///      to the LLTV (`_burnToLltv`); without, all of it, which retires the vault.
    function burnCollateral(bytes32 vaultId) external nonReentrant returns (uint64 retired) {
        Victim storage v = _victims[vaultId];
        (address account, uint64 outstanding, VaultStatus status) = IZecuredRegistry(registry).vault(vaultId);
        if (v.incident == 0 || status != VaultStatus.Active) revert NotBurnable(vaultId);
        if (_morpho.position(_marketId, account).borrowShares == 0) {
            retired = IZecuredRegistry(registry).retireVault(vaultId, outstanding);
        } else {
            retired = _burnToLltv(vaultId, account);
        }
        if (retired == 0) return 0;
        if (!v.tallied) _burnedBeforeTally[vaultId] += retired;
        emit CollateralBurned(vaultId, retired);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Pays `valueOf(zat, P*) * r / 1e18` (r of the entitlement's layer) to the claimant recorded at tally.
    function withdrawEntitlement(bytes32 claimId) external nonReentrant returns (uint256 amount) {
        Entitlement storage e = _entitlements[claimId];
        uint256 id = e.incident;
        if (id == 0 || e.paid) revert NothingOwed(claimId);
        Incident storage inc = _incidents[id];
        if (!inc.finalized) revert NotFinalized(id);
        amount = _payout(e.zat, e.layer, inc);
        if (amount == 0) revert NothingOwed(claimId);
        e.paid = true;
        reserved -= amount;
        address to = e.to;
        emit EntitlementPaid(claimId, to, amount);
        usdc.safeTransfer(to, amount);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Paid to the vault's account; `equityOf` drops to zero.
    function withdrawEquity(bytes32 vaultId) external nonReentrant returns (uint256 amount) {
        Victim storage v = _victims[vaultId];
        if (v.incident == 0) revert NotVictim(vaultId);
        if (!v.settled) revert NotSettled(vaultId);
        amount = equityOf[vaultId];
        if (amount == 0) revert NothingOwed(vaultId);
        equityOf[vaultId] = 0;
        reserved -= amount;
        address to = IZecuredRegistry(registry).vaultDetails(vaultId).account;
        emit EntitlementPaid(vaultId, to, amount);
        usdc.safeTransfer(to, amount);
    }

    /// @inheritdoc IZecuredCompensator
    /// @dev Only while no vault is open (`openVaultCount() == 0`), the slasher has no open case and every incident is
    ///      finalized; sends everything not `reserved` (so also USDC sent here directly) and empties `funds`.
    function sweepLeftover() external nonReentrant returns (uint256 amount) {
        if (
            _openIncidents != 0 || IZecuredSpendCommits(registry).openVaultCount() != 0
                || IZecuredSlasher(slasher).openCaseCount() != 0
        ) revert LeftoverLocked();
        amount = usdc.balanceOf(address(this)) - reserved;
        funds = 0;
        if (amount == 0) return 0;
        emit LeftoverSwept(leftoverSink, amount);
        usdc.safeTransfer(leftoverSink, amount);
    }

    // ------------------------------------------------------------------ views

    /// @inheritdoc IZecuredCompensator
    function incident(uint256 id) external view returns (Incident memory) {
        return _incidents[id];
    }

    /// @inheritdoc IZecuredCompensator
    function victim(bytes32 vaultId) external view returns (Victim memory) {
        return _victims[vaultId];
    }

    /// @inheritdoc IZecuredCompensator
    function entitlement(bytes32 claimId) external view returns (Entitlement memory) {
        return _entitlements[claimId];
    }

    // ------------------------------------------------------------------ internal

    /// @dev The incident new cases join: the current one while its window is open, else a new one.
    function _incidentNow() private returns (uint256 id) {
        id = currentIncident;
        if (id != 0) {
            Incident storage current = _incidents[id];
            if (block.timestamp < current.closesAt && !current.finalized) return id;
        }
        id += 1;
        currentIncident = id;
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 openedAt = uint64(block.timestamp); // a uint64 timestamp lasts for billions of years
        uint64 closesAt = openedAt + incidentWindow; // incidentWindow <= MAX_INCIDENT_WINDOW: cannot overflow
        Incident storage inc = _incidents[id];
        inc.openedAt = openedAt;
        inc.closesAt = closesAt;
        _openIncidents += 1;
        emit IncidentOpened(id, closesAt);
    }

    /// @dev F3: the claimant of `claimId` received `zat` less than it was owed.
    function _registerShortfall(bytes32 claimId, uint64 zat) private {
        if (zat == 0) return;
        Entitlement storage e = _entitlements[claimId];
        if (e.incident != 0) {
            if (!e.paid && zat < e.zat) _lower(claimId, e, zat);
            return;
        }
        // A read that fails must not block the slash: the shortfall is then not registered.
        ClaimDetails memory c;
        try IZecuredRegistry(registry).claimDetails(claimId) returns (ClaimDetails memory details) {
            c = details;
        } catch {
            return;
        }
        uint8 layer = _layerOf(c.kind);
        if (layer == 0 || c.status == ClaimStatus.None || c.bondPaid || c.claimant == address(0)) return;
        if (zat > c.amount) zat = c.amount;
        uint256 id = _incidentNow();
        _entitlements[claimId] = Entitlement({incident: id, layer: layer, zat: zat, to: c.claimant, paid: false});
        _addZat(id, layer, zat);
        emit ShortfallRegistered(id, claimId, zat);
    }

    /// @dev Lowers an unpaid entitlement to `zat`: from its incident's totals while it is open, else by giving the
    ///      difference in payout back to `funds`.
    function _lower(bytes32 claimId, Entitlement storage e, uint64 zat) private {
        uint256 id = e.incident;
        Incident storage inc = _incidents[id];
        uint64 old = e.zat;
        e.zat = zat;
        if (!inc.finalized) {
            Totals storage t = _totals[id];
            if (e.layer == 2) t.thirdPartyZat -= old - zat;
            else t.ownerZat -= old - zat;
        } else {
            _release(_payout(old, e.layer, inc) - _payout(zat, e.layer, inc));
        }
        emit ShortfallRegistered(id, claimId, zat);
    }

    function _addZat(uint256 id, uint8 layer, uint64 zat) private {
        if (layer == 2) _totals[id].thirdPartyZat += zat;
        else _totals[id].ownerZat += zat;
    }

    /// @dev Repays, retires and fixes the equity of a victim of a finalized incident. A victim unauthorized at tally
    ///      has no share (layers 3 and 4 skipped it): nothing to do.
    function _settle(bytes32 vaultId, Victim storage v, Incident storage inc)
        private
        returns (uint256 repaid, uint64 retired, uint256 equity)
    {
        if (!v.authorized) return (0, 0, 0);
        (uint256 share, uint256 cap) = CompensationMath.shareOf(v.outstanding, v.debt, inc.price, inc.r3, inc.r4);
        (address account, uint64 outstanding, VaultStatus status) = IZecuredRegistry(registry).vault(vaultId);
        bool active = status == VaultStatus.Active;
        // Without the authorization the Registry cannot withdraw the collateral, so a repayment could be borrowed
        // again and the vault never retired.
        if (active && !_morpho.isAuthorized(account, registry)) {
            _release(share);
            return (0, 0, 0);
        }
        uint256 debtNow;
        (repaid, debtNow) = _repayIfCovered(account, share);
        if (debtNow > share) {
            // Partly covered: the share is repaid, and the collateral the rest of the debt does not need is burned down
            // to the LLTV, so that the repayment cannot be borrowed again against the vault's unbacked zZEC.
            if (active) retired = _burnToLltv(vaultId, account);
            return (repaid, retired, 0);
        }

        equity = _keepEquity(vaultId, share - repaid, Math.saturatingSub(cap, debtNow));
        // Debt is zero now: the Registry withdraws the collateral with the account's authorization and burns it (what is
        // left of it: `burnCollateral` may have burned part already, R-19).
        if (active) retired = IZecuredRegistry(registry).retireVault(vaultId, outstanding);
    }

    /// @dev Retires the collateral beyond the least that keeps `account`'s position healthy at the market price (Morpho:
    ///      collateral * price / 1e36 * lltv / 1e18 >= debt, here rounded up, plus one zatoshi). Nothing when the price
    ///      is zero or the position needs all of it.
    function _burnToLltv(bytes32 vaultId, address account) private returns (uint64 retired) {
        uint256 price = IOracle(_oracle).price();
        if (price == 0) return 0;
        (, uint256 debt) = _debtOf(account);
        uint256 collateral = _morpho.position(_marketId, account).collateral;
        uint256 needed = Math.mulDiv(
            Math.mulDiv(debt, CompensationMath.WAD, _lltv, Math.Rounding.Ceil),
            ORACLE_PRICE_SCALE,
            price,
            Math.Rounding.Ceil
        ) + 1;
        if (needed >= collateral) return 0;
        retired = IZecuredRegistry(registry).retireVault(vaultId, uint64(collateral - needed));
    }

    /// @dev Records `min(left, cap)` as the vault's equity and gives the rest of `left` back to the pool.
    function _keepEquity(bytes32 vaultId, uint256 left, uint256 cap) private returns (uint256 equity) {
        equity = Math.min(left, cap);
        if (equity != 0) equityOf[vaultId] = equity;
        _release(left - equity);
    }

    /// @dev Repays `account`'s whole debt if `share` covers it, else `share` of it; returns what was repaid and the
    ///      debt before the repayment.
    function _repayIfCovered(address account, uint256 share) private returns (uint256 repaid, uint256 debtNow) {
        if (account == address(0)) return (0, 0);
        uint256 shares;
        (shares, debtNow) = _debtOf(account);
        if (debtNow <= share) {
            if (shares != 0) repaid = _repay(account, debtNow, 0, shares);
        } else if (share != 0) {
            // share < debtNow: repaying by assets burns fewer shares than the account holds.
            repaid = _repay(account, share, share, 0);
        }
        reserved -= repaid;
    }

    /// @dev Repays `assets` or `shares` of `account`'s debt with this contract's USDC (Morpho pulls at most
    ///      `allowance`); returns the assets repaid. No callback: `data` is empty.
    function _repay(address account, uint256 allowance, uint256 assets, uint256 shares)
        private
        returns (uint256 repaid)
    {
        usdc.forceApprove(address(_morpho), allowance);
        (repaid,) = _morpho.repay(_marketParams(), assets, shares, account, "");
    }

    /// @dev The tally of a vault's position: its account's debt now, and whether the account lets the Registry withdraw
    ///      its collateral. A vault without an account (never reserved) has neither.
    function _snapshot(address account) private returns (uint256 debt, bool authorized) {
        if (account == address(0)) return (0, false);
        (, debt) = _debtOf(account);
        authorized = _morpho.isAuthorized(account, registry);
    }

    /// @dev `account`'s borrow shares and their value now (interest accrued first; rounded up, as Morpho charges).
    function _debtOf(address account) private returns (uint256 shares, uint256 debt) {
        _morpho.accrueInterest(_marketParams());
        Market memory m = _morpho.market(_marketId);
        shares = _morpho.position(_marketId, account).borrowShares;
        debt = shares.toAssetsUp(m.totalBorrowAssets, m.totalBorrowShares);
    }

    /// @dev Gives set-aside USDC that no victim will get back to the pool.
    function _release(uint256 amount) private {
        if (amount == 0) return;
        reserved -= amount;
        funds += amount;
    }

    function _payout(uint64 zat, uint8 layer, Incident storage inc) private view returns (uint256) {
        return CompensationMath.scale(CompensationMath.valueOf(zat, inc.price), layer == 2 ? inc.r2 : inc.r4);
    }

    function _marketParams() private view returns (MarketParams memory) {
        return MarketParams({
            loanToken: address(usdc), collateralToken: _collateralToken, oracle: _oracle, irm: _irm, lltv: _lltv
        });
    }

    /// @dev The claims of a victim vault `tallyClaim` takes: pending ones; R-21 abandoned refunds; R-3 confirmations by
    ///      signatures made within the dispute window before the vault froze, which no proof made final since.
    function _tallyable(bytes32 claimId, ClaimDetails memory c) private view returns (bool) {
        ClaimStatus status = c.status;
        if (status == ClaimStatus.Pending) return true;
        if (status == ClaimStatus.WrittenOff) return c.kind == ClaimKind.Refund;
        if (status != ClaimStatus.Paid) return false;
        (uint64 paidAt,) = IZecuredDisputes(registry).payoutDispute(claimId);
        return paidAt != 0
            && paidAt + IZecuredDisputes(registry).disputeWindow() > IZecuredSlasher(slasher).frozenSince(c.vaultId);
    }

    /// @dev Layer 2: claims a third party paid for. Layer 4: the owner's own. 0: the signers' own, or bookkeeping.
    function _layerOf(ClaimKind kind) private pure returns (uint8) {
        if (kind == ClaimKind.Liquidation || kind == ClaimKind.CloseMarketMaker) return 2;
        if (kind == ClaimKind.Release || kind == ClaimKind.CloseUser || kind == ClaimKind.Refund) return 4;
        return 0;
    }
}
