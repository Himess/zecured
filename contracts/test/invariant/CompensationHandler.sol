// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho, MarketParams, Id} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {ZecuredCompensator} from "../../src/ZecuredCompensator.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {ViolationKind, ViolationJournal} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredCompensator, Incident, Victim, Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {IntakeHarness} from "../utils/IntakeHarness.sol";

/// @notice What the compensation handler drives: the real Registry, spend logic, slasher, bond and compensator (MOCK
///         verifier and relay, MOCK USDC and price), on the real Morpho Blue.
struct CompensationSystem {
    IMorpho morpho;
    ZecuredRegistry registry;
    IntakeHarness intake;
    ZecuredSlasher slasher;
    ZecuredCompensator compensator;
    MockUSDC usdc;
    MockZecOracle oracle;
    MockHeaderRelay relay;
    MockRiscZeroVerifier verifier;
    bytes32 violationImage;
    bytes32 anchor;
}

/// @notice Invariant-test handler (v4) for what the main campaign's MOCK slasher cannot reach: proofs of payouts through
///         the real slasher (one and batched), confirmations, disputes, expiries, thefts (F1, F2) and misdirected
///         payouts (F3) proven and executed, and the whole compensation (tally, the R-2 proof window, the R-3
///         confirmations, finalization, settlement with its burns and the R-20 marking, withdrawals). Vaults open
///         through the TEST-ONLY Intake harness. Flags record a violation when it happens.
contract CompensationHandler is Test {
    using MorphoBalancesLib for IMorpho;

    IMorpho internal immutable morpho;
    ZecuredRegistry internal immutable registry;
    IntakeHarness internal immutable intake;
    ZecuredSlasher internal immutable slasher;
    ZecuredCompensator internal immutable compensator;
    MockUSDC internal immutable usdc;
    MockZecOracle internal immutable oracle;
    MockHeaderRelay internal immutable relay;
    MockRiscZeroVerifier internal immutable verifier;
    bytes32 internal immutable violationImage;
    bytes32 internal immutable anchor;
    MarketParams internal params;
    Id internal marketId;
    uint256[5] internal signerKeys;

    uint256 internal constant ACCOUNT_COUNT = 8;
    address[ACCOUNT_COUNT] public accounts;
    address public immutable watcher = makeAddr("zecured.comp.inv.watcher");
    address public immutable stranger = makeAddr("zecured.comp.inv.stranger");

    // ------------------------------------------------------------------ ghost state

    struct Payout {
        bytes32 vaultId;
        bytes32 ref;
        bytes32 input;
        bytes32 claimId;
        bytes32 claimRef;
        bytes32 commitment;
        bytes32 changeTag;
        uint64 changeValue;
    }

    bytes32[] public vaultIds;
    mapping(bytes32 vaultId => uint256) public accountIndexOf;
    /// The vault's unspent lineage note (its deposit, then each payout's change; zero once a payout emptied it).
    mapping(bytes32 vaultId => bytes32) public head;
    Payout[] internal payouts;
    bytes32[] internal openCases;
    mapping(bytes32 claimId => bool) public reopened;
    uint256 internal nonce;
    /// R-12: the deposit tags of the refunds reserved and neither opened nor released yet, the vault of each, and how
    /// many each vault has (the ghost of `reservedRefundCount`).
    bytes32[] internal reservedTags;
    mapping(bytes32 tag => bytes32 vaultId) public reservedVault;
    mapping(bytes32 vaultId => uint256) public reservedCountOf;
    /// R-12: the Refund claims opened on a vault already compromised (reserved before its compromise).
    bytes32[] public lateRefunds;

    bool public violated;
    string public violation;
    mapping(string => uint256) public calls;

    constructor(CompensationSystem memory s, uint256[5] memory signerKeys_) {
        morpho = s.morpho;
        registry = s.registry;
        intake = s.intake;
        slasher = s.slasher;
        compensator = s.compensator;
        usdc = s.usdc;
        oracle = s.oracle;
        relay = s.relay;
        verifier = s.verifier;
        violationImage = s.violationImage;
        anchor = s.anchor;
        params = s.registry.marketParams();
        marketId = s.registry.marketId();
        signerKeys = signerKeys_;
        for (uint256 i; i < ACCOUNT_COUNT; ++i) {
            accounts[i] = makeAddr(string.concat("zecured.comp.inv.account", vm.toString(i)));
        }
    }

    // ------------------------------------------------------------------ views for the invariants

    function vaultCount() external view returns (uint256) {
        return vaultIds.length;
    }

    function payoutCount() external view returns (uint256) {
        return payouts.length;
    }

    function lateRefundCount() external view returns (uint256) {
        return lateRefunds.length;
    }

    function reservedCount() external view returns (uint256) {
        return reservedTags.length;
    }

    // ------------------------------------------------------------------ helpers

    function _mix(uint256 seed) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(seed)));
    }

    function _flag(string memory what) internal {
        if (!violated) violation = what;
        violated = true;
    }

    function _threeSigs(bytes32 digest, uint256 seed) internal view returns (bytes[] memory sigs) {
        uint256 first = seed % 3; // signers (0|1|2), then two above it
        sigs = new bytes[](3);
        uint256[3] memory pick = [first, first + 1, first + 2];
        for (uint256 i; i < 3; ++i) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKeys[pick[i]], digest);
            sigs[i] = abi.encodePacked(r, s, v);
        }
    }

    function _payouts() internal view returns (IZecuredPayouts) {
        return IZecuredPayouts(address(registry));
    }

    function _disputes() internal view returns (IZecuredDisputes) {
        return IZecuredDisputes(address(registry));
    }

    function _status(bytes32 vaultId) internal view returns (VaultStatus status) {
        (,, status) = registry.vault(vaultId);
    }

    function _claimStatus(bytes32 claimId) internal view returns (ClaimStatus status) {
        (,,,, status) = registry.claim(claimId);
    }

    function _paidAt(bytes32 claimId) internal view returns (uint64 paidAt) {
        (paidAt,) = _disputes().payoutDispute(claimId);
    }

    function _accountOf(bytes32 vaultId) internal view returns (address) {
        return accounts[accountIndexOf[vaultId]];
    }

    function _vault(uint256 seed) internal view returns (bool, bytes32) {
        if (vaultIds.length == 0) return (false, bytes32(0));
        return (true, vaultIds[seed % vaultIds.length]);
    }

    function _upTo(uint256 at) internal {
        if (block.timestamp < at) vm.warp(at);
    }

    // ------------------------------------------------------------------ vaults

    /// A new vault of an account that never had one (0.01, 0.05 or 1 ZEC); one account in four leaves the Registry
    /// unauthorized on Morpho (it can then neither exit nor be retired: R-20's case).
    function open(uint256 seed) external {
        seed = _mix(seed);
        uint256 index = seed % ACCOUNT_COUNT;
        address account = accounts[index];
        if (registry.vaultOf(account) != bytes32(0)) return;
        bytes32 vaultId = keccak256(abi.encode("zecured.comp.inv.vault", nonce++));
        bytes32 tag = keccak256(abi.encode("zecured.comp.inv.tag", vaultId));
        uint64[3] memory amounts = [uint64(1_000_000), 5_000_000, 100_000_000];
        intake.harnessActivate(vaultId, account, amounts[(seed >> 8) % 3], tag);
        vaultIds.push(vaultId);
        accountIndexOf[vaultId] = index;
        head[vaultId] = tag;
        if ((seed >> 16) % 4 != 0) {
            vm.prank(account);
            morpho.setAuthorization(address(registry), true);
        }
        calls["open"]++;
    }

    function borrow(uint256 vaultSeed, uint256 ltvBps) external {
        (bool found, bytes32 vaultId) = _vault(vaultSeed);
        if (!found || _status(vaultId) != VaultStatus.Active) return;
        address account = _accountOf(vaultId);
        (, uint64 outstanding,) = registry.vault(vaultId);
        uint256 target = uint256(outstanding) * oracle.price() / ORACLE_PRICE_SCALE * bound(ltvBps, 500, 5_500) / 10_000;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        if (target <= debt + 1) return;
        vm.prank(account);
        try morpho.borrow(params, target - debt, 0, account, stranger) {
            calls["borrow"]++;
        } catch {}
    }

    function repay(uint256 vaultSeed) external {
        (bool found, bytes32 vaultId) = _vault(vaultSeed);
        if (!found) return;
        address account = _accountOf(vaultId);
        uint256 shares = morpho.position(marketId, account).borrowShares;
        if (shares == 0) return;
        uint256 debt = morpho.expectedBorrowAssets(params, account);
        usdc.mint(account, debt);
        vm.startPrank(account);
        usdc.approve(address(morpho), debt);
        morpho.repay(params, 0, shares, account, "");
        vm.stopPrank();
        calls["repay"]++;
    }

    /// A partial release, or (`full`) the whole outstanding of a vault without debt.
    function release(uint256 vaultSeed, uint256 amountSeed, bool full) external {
        (bool found, bytes32 vaultId) = _vault(vaultSeed);
        if (!found || _status(vaultId) != VaultStatus.Active) return;
        address account = _accountOf(vaultId);
        (, uint64 outstanding,) = registry.vault(vaultId);
        bytes32 dest = keccak256(abi.encode("zecured.comp.inv.dest", vaultId, registry.claimCount(vaultId)));
        uint64 minRelease = registry.MIN_RELEASE(); // before the prank: it reads the Registry
        bool whole = full || outstanding < 4 * minRelease;
        uint64 amount = whole ? 0 : uint64(bound(amountSeed, minRelease, outstanding / 2));
        vm.prank(account);
        if (whole) {
            try registry.requestRelease(vaultId, dest) {
                calls["release"]++;
            } catch {}
        } else {
            try registry.requestPartialRelease(vaultId, amount, dest) {
                calls["partial release"]++;
            } catch {}
        }
    }

    function setPrice(uint256 cents) external {
        oracle.setPrice(bound(cents, 2_500, 6_000) * 1e32); // $25 .. $60
        calls["setPrice"]++;
    }

    function warp(uint256 secondsToSkip) external {
        skip(bound(secondsToSkip, 1, 2 days));
        calls["warp"]++;
    }

    // ------------------------------------------------------------------ payouts

    /// The signers commit the payout of a vault's first payable claim from its lineage note, with the rest as change.
    /// Goal-directed: the first vault (from the seed) with a payable claim; else the vault's account makes one.
    function commit(uint256 vaultSeed, uint256 signerSeed) external {
        if (vaultIds.length == 0) return;
        bytes32 vaultId;
        bytes32 claimId;
        for (uint256 k; k < vaultIds.length && claimId == bytes32(0); ++k) {
            bytes32 v = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            if (slasher.isCompromised(v) || head[v] == bytes32(0)) continue;
            (bool ok, bytes32 id) = _payableClaim(v);
            if (ok) (vaultId, claimId) = (v, id);
        }
        if (claimId == bytes32(0)) {
            vaultId = vaultIds[vaultSeed % vaultIds.length];
            if (slasher.isCompromised(vaultId) || head[vaultId] == bytes32(0)) return;
            _makeClaim(vaultId, signerSeed);
            (bool ok, bytes32 id) = _payableClaim(vaultId);
            if (!ok) return;
            claimId = id;
        }
        Payout memory p = _payout(vaultId, head[vaultId], claimId);
        if (p.vaultId == bytes32(0)) return;
        SpendCommit memory c = _spendCommit(p);
        try registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c), signerSeed)) {
            head[vaultId] = p.changeTag;
            payouts.push(p);
            calls["commit"]++;
        } catch {
            _flag("a valid commitment reverted");
        }
    }

    /// The vault's account releases part of its collateral (or all of it, without debt) if it can.
    function _makeClaim(bytes32 vaultId, uint256 seed) internal {
        if (_status(vaultId) != VaultStatus.Active) return;
        address account = _accountOf(vaultId);
        (, uint64 outstanding,) = registry.vault(vaultId);
        bytes32 dest = keccak256(abi.encode("zecured.comp.inv.dest", vaultId, registry.claimCount(vaultId)));
        uint64 minRelease = registry.MIN_RELEASE(); // before the prank: it reads the Registry
        bool whole = outstanding < 4 * minRelease;
        uint64 amount = whole ? 0 : uint64(bound(seed, minRelease, outstanding / 2));
        vm.prank(account);
        if (whole) {
            try registry.requestRelease(vaultId, dest) {
                calls["release"]++;
            } catch {}
        } else {
            try registry.requestPartialRelease(vaultId, amount, dest) {
                calls["partial release"]++;
            } catch {}
        }
    }

    function _payableClaim(bytes32 vaultId) internal view returns (bool, bytes32) {
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (,, uint64 amount, bytes32 commitment, ClaimStatus status) = registry.claim(claimId);
            if (status != ClaimStatus.Pending || commitment == bytes32(0) || amount <= 10_000) continue;
            if (registry.claimInput(claimId) == bytes32(0)) return (true, claimId);
        }
        return (false, bytes32(0));
    }

    function _payout(bytes32 vaultId, bytes32 input, bytes32 claimId) internal returns (Payout memory p) {
        (,, uint64 amount, bytes32 commitment,) = registry.claim(claimId);
        uint64 value = registry.lineageOf(input).value;
        if (value < amount) return p;
        uint256 n = nonce++;
        p.vaultId = vaultId;
        p.ref = keccak256(abi.encode("zecured.comp.inv.payout", n));
        p.input = input;
        p.claimId = claimId;
        p.claimRef = keccak256(abi.encode("zecured.comp.inv.claimRef", n));
        p.commitment = commitment;
        p.changeValue = value - amount;
        p.changeTag = p.changeValue == 0 ? bytes32(0) : keccak256(abi.encode("zecured.comp.inv.change", n));
    }

    function _spendCommit(Payout memory p) internal pure returns (SpendCommit memory c) {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = p.claimId;
        bytes32[] memory refs = new bytes32[](1);
        refs[0] = p.claimRef;
        c = SpendCommit({
            vaultId: p.vaultId,
            inputTag: p.input,
            claimIds: ids,
            claimRefs: refs,
            claimOutputs: new uint8[](1),
            payoutRef: p.ref,
            changeTag: p.changeTag,
            changeValue: p.changeValue
        });
    }

    /// Three signers confirm a committed payout whose claim is still pending (never one reopened): refused only while
    /// the vault is frozen (R-3), and once per payout (R-1).
    function confirm(uint256 payoutSeed, uint256 signerSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        if (_claimStatus(p.claimId) != ClaimStatus.Pending || reopened[p.claimId]) return;
        if (_payouts().payoutConfirmed(p.ref)) return;
        bool frozen = slasher.vaultFrozen(p.vaultId);
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = p.claimId;
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(p.vaultId, p.ref, ids), signerSeed);
        try _payouts().confirmPayout(p.vaultId, p.ref, ids, sigs) {
            if (frozen) _flag("R-3: a frozen vault's payout was confirmed");
            calls["confirm"]++;
        } catch {
            if (!frozen) _flag("a payout could not be confirmed");
            calls["confirm (frozen)"]++;
        }
    }

    /// Anyone proves a committed payout's inclusion through the real slasher (one proof, or the batch): its claim is
    /// paid for good unless the compensator took it over first.
    function prove(uint256 payoutSeed, bool batch) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        ClaimStatus before = _claimStatus(p.claimId);
        bool upholdable = before == ClaimStatus.Pending || before == ClaimStatus.Disputed
            || (before == ClaimStatus.Paid && _paidAt(p.claimId) != 0);
        bytes memory journal =
            abi.encode(uint256(5), anchor, uint256(0), p.ref, bytes32(0), uint256(0), bytes32(0), uint256(0));
        bytes memory seal = verifier.mockSeal(violationImage, sha256(journal));
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = p.claimId;
        if (batch) {
            try slasher.proveInclusionBatch(journal, seal, violationImage, ids) returns (uint256 upheld) {
                if ((upheld == 1) != upholdable) {
                    _flag("the batch upheld what it should not, or skipped what it should");
                }
            } catch {
                _flag("a batch proof reverted");
            }
        } else {
            try slasher.proveInclusion(journal, seal, violationImage, p.claimId) {
                if (!upholdable) _flag("a proof upheld a claim it should not");
            } catch {
                if (upholdable) _flag("a proof of a committed payout was refused");
            }
        }
        if (upholdable && (_claimStatus(p.claimId) != ClaimStatus.Paid || _paidAt(p.claimId) != 0)) {
            _flag("a proven claim is not paid for good");
        }
        if (upholdable) calls[batch ? "prove (batch)" : "prove"]++;
    }

    function dispute(uint256 payoutSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        ClaimDetails memory c = registry.claimDetails(p.claimId);
        uint64 paidAt = _paidAt(p.claimId);
        if (c.status != ClaimStatus.Paid || paidAt == 0 || c.bondPaid || c.claimant == address(0)) return;
        if (block.timestamp > uint256(paidAt) + _disputes().disputeWindow()) return;
        bool compromised = slasher.isCompromised(p.vaultId);
        vm.prank(c.claimant);
        try _disputes().disputePayout(p.claimId) {
            if (compromised) _flag("a compromised vault's claim was disputed");
            calls["dispute"]++;
        } catch {
            if (!compromised) _flag("a claimant could not dispute within its window");
        }
    }

    function reopen(uint256 payoutSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        if (_claimStatus(p.claimId) != ClaimStatus.Disputed) return;
        (, uint64 dueAt) = _disputes().payoutDispute(p.claimId);
        _upTo(uint256(dueAt) + 1);
        _disputes().reopenDisputedClaim(p.claimId);
        reopened[p.claimId] = true;
        calls["reopen"]++;
    }

    /// R-3: a confirmation nobody disputed is final once its window ends, unless the vault froze within it.
    function expire(uint256 payoutSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        uint64 paidAt = _paidAt(p.claimId);
        if (_claimStatus(p.claimId) != ClaimStatus.Paid || paidAt == 0) return;
        uint64 window = _disputes().disputeWindow();
        _upTo(uint256(paidAt) + window + 1);
        uint64 frozenSince = slasher.frozenSince(p.vaultId);
        bool suspect = frozenSince != 0 && paidAt + window > frozenSince;
        try _payouts().expireConfirmation(p.claimId) {
            if (suspect) _flag("R-3: a confirmation made just before a freeze expired");
            calls["expire"]++;
        } catch {
            if (!suspect) _flag("an old confirmation could not expire");
        }
    }

    // ------------------------------------------------------------------ violations (F1, F2, F3) and their execution

    function _journal(ViolationKind kind, bytes32 ref, bytes32 noteTag, uint64 value, bytes32 dest)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(
            ViolationJournal({
                kind: kind,
                anchor: anchor,
                beneficiary: watcher,
                ref: ref,
                noteTag: noteTag,
                value: value,
                destCommitment: dest,
                outputIndex: 0
            })
        );
    }

    function _open(bytes memory journal) internal returns (bool) {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        bytes memory seal = verifier.mockSeal(violationImage, sha256(journal));
        try slasher.proveViolation(kind, journal, seal, violationImage) returns (bytes32 caseId) {
            openCases.push(caseId);
            return true;
        } catch {
            return false; // the case is open already, or its condition no longer holds
        }
    }

    /// Three colluders spend a vault's lineage note without a commitment: F1 (the first vault from the seed that has
    /// one and is not compromised yet), sometimes while a refund of the vault waits for its claim (R-12).
    function theft(uint256 vaultSeed) external {
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            if (head[vaultId] == bytes32(0) || slasher.isCompromised(vaultId) || slasher.theftCasesOf(vaultId) != 0) {
                continue;
            }
            // R-12: half the time the account's refund of another note is in flight when the theft is proven.
            if (_mix(vaultSeed) % 2 == 0) _reserve(vaultId, 200_000);
            bytes32 ref = keccak256(abi.encode("zecured.comp.inv.theft", nonce++));
            if (_open(_journal(ViolationKind.Uncommitted, ref, head[vaultId], 0, bytes32(0)))) calls["F1"]++;
            return;
        }
    }

    /// A committed payout withheld its change: F2.
    function withheld(uint256 payoutSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        if (p.changeValue == 0) return;
        if (_open(_journal(ViolationKind.WithheldChange, p.ref, p.changeTag, p.changeValue, bytes32(0)))) {
            calls["F2"]++;
        }
    }

    /// A committed payout paid its claimant short: F3.
    function misdirect(uint256 payoutSeed, uint256 valueSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        uint64 expected = registry.claimPayment(p.claimRef).expected;
        uint64 value = uint64(bound(valueSeed, 0, expected - 1));
        if (_open(_journal(ViolationKind.Misdirected, p.claimRef, bytes32(0), value, p.commitment))) calls["F3"]++;
    }

    /// W-1: a committed payout named its claim under a reference its claimant never computes: F4, proven by a holder of
    /// K and the claim's salt. The open case counts against the claim (W-5); its execution makes the compensator owe the
    /// claim, final or not.
    function wrongRef(uint256 payoutSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        bytes32 honest = keccak256(abi.encode("zecured.comp.inv.honestRef", p.claimRef));
        if (_open(_journal(ViolationKind.WrongClaimRef, p.ref, honest, 0, p.commitment))) {
            if (!slasher.claimCaseOpen(p.claimId)) _flag("W-5: an open F4 case does not count against its claim");
            calls["F4"]++;
        }
    }

    /// W-5: while an F3 or F4 case about a claim is open, the bonds never pay its liveness: its claimant arms it with the
    /// earliest receipt and asks once the deadline has passed.
    function livenessDuringCase(uint256 payoutSeed, uint256 signerSeed) external {
        if (payouts.length == 0) return;
        Payout memory p = payouts[payoutSeed % payouts.length];
        if (!slasher.claimCaseOpen(p.claimId)) return;
        ISignerBond bond = ISignerBond(slasher.signerBond());
        ClaimDetails memory c = registry.claimDetails(p.claimId);
        if (c.status != ClaimStatus.Pending || c.bondPaid || c.claimant == address(0)) return;
        uint64 receivedAt = uint64(c.createdAt);
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(signerKeys[signerSeed % 5], bond.hashDestinationReceived(p.claimId, c.commitment, receivedAt));
        try bond.armLiveness(p.claimId, receivedAt, abi.encodePacked(r, s, v)) {}
        catch {
            return;
        }
        uint64 deadline = bond.livenessDeadline(p.claimId);
        if (deadline == 0 || deadline > block.timestamp + 30 days) return;
        _upTo(deadline);
        vm.prank(c.claimant);
        try bond.slashLiveness(p.claimId) {
            _flag("W-5: the bonds paid a claim while an F3 or F4 case about it was open");
        } catch {
            calls["liveness (case open)"]++;
        }
    }

    /// After the challenge window anyone executes an open case; it never reverts with enough gas.
    function execute(uint256 caseSeed) external {
        if (openCases.length == 0) return;
        uint256 i = caseSeed % openCases.length;
        bytes32 caseId = openCases[i];
        openCases[i] = openCases[openCases.length - 1];
        openCases.pop();
        _upTo(slasher.caseOf(caseId).executableAt);
        try slasher.executeSlash(caseId) {
            calls["execute"]++;
        } catch {
            _flag("a case could not be executed");
        }
    }

    // ------------------------------------------------------------------ refunds (R-12)

    /// The account sends its vault another note, proven as a refund: reserved until its claim opens (R-12). Refused once
    /// the vault is compromised (the compensator tallies it once).
    function reserveRefund(uint256 vaultSeed, uint256 amountSeed) external {
        (bool found, bytes32 vaultId) = _vault(vaultSeed);
        if (!found) return;
        _reserve(vaultId, uint64(bound(amountSeed, 100_000, 2_000_000)));
    }

    function _reserve(bytes32 vaultId, uint64 amount) internal {
        bytes32 tag = keccak256(abi.encode("zecured.comp.inv.refund", nonce++));
        bool compromised = slasher.isCompromised(vaultId);
        try intake.harnessReserveRefund(vaultId, _accountOf(vaultId), amount, tag) {
            if (compromised) _flag("R-12: a refund reserved on a compromised vault");
            reservedTags.push(tag);
            reservedVault[tag] = vaultId;
            reservedCountOf[vaultId] += 1;
            calls["reserve refund"]++;
        } catch (bytes memory reason) {
            if (compromised && bytes4(reason) == IZecuredRegistry.VaultCompromised.selector) {
                calls["reserve refund (compromised)"]++;
            } else {
                _flag("a refund could not be reserved");
            }
        }
    }

    /// A reserved refund is ready: anyone opens its claim (`finalizeRefund`), on a compromised vault too, which then
    /// was not tallied yet (its tally waits for every reserved refund).
    function openRefund(uint256 seed) external {
        if (reservedTags.length == 0) return;
        uint256 i = seed % reservedTags.length;
        bytes32 tag = reservedTags[i];
        bytes32 vaultId = reservedVault[tag];
        if (compensator.victim(vaultId).tallied) _flag("R-12: a victim was tallied while a refund of it was reserved");
        _openRefund(i, tag, vaultId);
    }

    /// A reserved refund's proof fell off the best chain: it is released (`dropPendingRefund`), its note freed.
    function dropRefund(uint256 seed) external {
        if (reservedTags.length == 0) return;
        uint256 i = seed % reservedTags.length;
        bytes32 tag = reservedTags[i];
        bytes32 vaultId = reservedVault[tag];
        try intake.harnessReleaseRefund(vaultId, tag) {
            _removeReserved(i, vaultId);
            calls["drop refund"]++;
        } catch {
            _flag("a reserved refund could not be released");
        }
    }

    function _openRefund(uint256 i, bytes32 tag, bytes32 vaultId) internal {
        bool compromised = slasher.isCompromised(vaultId);
        try intake.harnessOpenRefund(vaultId, tag) returns (bytes32 claimId) {
            _removeReserved(i, vaultId);
            if (compromised) lateRefunds.push(claimId);
            calls[compromised ? "open refund (compromised)" : "open refund"]++;
        } catch {
            _flag("a reserved refund could not be opened");
        }
    }

    function _removeReserved(uint256 i, bytes32 vaultId) internal {
        reservedTags[i] = reservedTags[reservedTags.length - 1];
        reservedTags.pop();
        reservedCountOf[vaultId] -= 1;
    }

    // ------------------------------------------------------------------ compensation

    /// Anyone tallies a victim vault: every claim the compensator takes (after the R-2 proof window for committed ones),
    /// the old confirmations expired (R-3), a dispute still open reopened once due; then the vault. A keeper usually
    /// opens the vault's reserved refunds first (R-12); while one is still reserved, the vault's tally waits.
    function tally(uint256 vaultSeed) external {
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            Victim memory v = compensator.victim(vaultId);
            if (v.incident == 0 || v.tallied || compensator.incident(v.incident).finalized) continue;
            if (_mix(vaultSeed) % 4 != 0) {
                for (uint256 i = reservedTags.length; i > 0; --i) {
                    if (reservedVault[reservedTags[i - 1]] == vaultId) {
                        _openRefund(i - 1, reservedTags[i - 1], vaultId);
                    }
                }
            }
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                _tallyOne(vaultId, registry.claimIdOf(vaultId, j));
            }
            uint256 reserved = reservedCountOf[vaultId];
            try compensator.tallyVault(vaultId) {
                if (reserved != 0) _flag("R-12: a victim was tallied while a refund of it was reserved");
                calls["tally vault"]++;
            } catch (bytes memory reason) {
                if (reserved != 0 && bytes4(reason) == IZecuredCompensator.RefundsReserved.selector) {
                    calls["tally vault (refunds reserved)"]++;
                } else {
                    _flag("a victim vault could not be tallied");
                }
            }
            return;
        }
    }

    function _tallyOne(bytes32 vaultId, bytes32 claimId) internal {
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (c.status == ClaimStatus.Disputed) {
            (, uint64 dueAt) = _disputes().payoutDispute(claimId);
            _upTo(uint256(dueAt) + 1);
            _disputes().reopenDisputedClaim(claimId);
            reopened[claimId] = true;
            c.status = ClaimStatus.Pending;
        }
        uint64 paidAt = _paidAt(claimId);
        if (c.status == ClaimStatus.Paid && paidAt != 0) {
            uint64 window = _disputes().disputeWindow();
            if (paidAt + window <= slasher.frozenSince(vaultId)) {
                _upTo(uint256(paidAt) + window + 1);
                _payouts().expireConfirmation(claimId);
                calls["expire (tally)"]++;
                return;
            }
        } else if (
            c.status != ClaimStatus.Pending && !(c.status == ClaimStatus.WrittenOff && c.kind == ClaimKind.Refund)
        ) {
            return;
        }
        if (registry.claimInput(claimId) != bytes32(0)) {
            _upTo(uint256(slasher.compromisedAt(vaultId)) + _disputes().inclusionWindow());
        }
        try compensator.tallyClaim(claimId) {
            calls[c.status == ClaimStatus.Paid ? "tally (R-3 confirmation)" : "tally claim"]++;
        } catch {
            _flag("a victim's claim could not be tallied");
        }
    }

    /// Anyone finalizes the oldest incident once every victim of it is tallied and its window is over.
    function finalize(uint256) external {
        uint256 current = compensator.currentIncident();
        for (uint256 id = 1; id <= current; ++id) {
            Incident memory inc = compensator.incident(id);
            if (inc.finalized) continue;
            if (inc.talliedVictims != inc.victims) return;
            _upTo(inc.closesAt);
            while (!compensator.incident(id).finalized) {
                try compensator.finalizeIncident(id) {}
                catch {
                    _flag("an incident could not be finalized");
                    return;
                }
            }
            calls["finalize"]++;
            return;
        }
    }

    /// Anyone settles a tallied victim of a finalized incident; a vault left active is marked in the Registry (R-20).
    function settle(uint256 vaultSeed) external {
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            Victim memory v = compensator.victim(vaultId);
            if (v.incident == 0 || !v.tallied || v.settled || !compensator.incident(v.incident).finalized) continue;
            try compensator.settleVault(vaultId) {
                if (_status(vaultId) == VaultStatus.Active && !_payouts().vaultCompensated(vaultId)) {
                    _flag("R-20: a settled vault left active was not marked");
                }
                calls["settle"]++;
            } catch {
                _flag("a victim could not be settled");
            }
            return;
        }
    }

    /// Anyone burns an active victim's collateral down to the LLTV, or retires it without debt (R-19, R-20).
    function burn(uint256 vaultSeed) external {
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[(vaultSeed % vaultIds.length + k) % vaultIds.length];
            if (compensator.victim(vaultId).incident == 0 || _status(vaultId) != VaultStatus.Active) continue;
            try compensator.burnCollateral(vaultId) returns (uint64 retired) {
                if (retired != 0) calls["burn"]++;
            } catch {} // an account that revoked the Registry's authorization
            return;
        }
    }

    /// Claimants and owners withdraw what they are owed.
    function withdraw(uint256) external {
        for (uint256 k; k < vaultIds.length; ++k) {
            bytes32 vaultId = vaultIds[k];
            uint64 count = registry.claimCount(vaultId);
            for (uint64 j; j < count; ++j) {
                bytes32 claimId = registry.claimIdOf(vaultId, j);
                Entitlement memory e = compensator.entitlement(claimId);
                if (e.incident == 0 || e.paid || !compensator.incident(e.incident).finalized) continue;
                try compensator.withdrawEntitlement(claimId) {
                    calls["withdraw entitlement"]++;
                } catch {} // nothing owed at a ratio of zero
            }
            if (compensator.victim(vaultId).settled && compensator.equityOf(vaultId) != 0) {
                compensator.withdrawEquity(vaultId);
                calls["withdraw equity"]++;
            }
        }
    }
}
