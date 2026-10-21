// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {SignerBondV2} from "../../src/SignerBondV2.sol";
import {ClaimDetails, ClaimKind, ClaimStatus, VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {IntakeHarness} from "../utils/IntakeHarness.sol";

/// @notice What the bond handler drives: a bonded deployment (the real SignerBondV2, ZecuredSlasher and
///         ZecuredCompensator) whose vaults the TEST-ONLY Intake harness opens without proofs.
struct BondSystem {
    IMorpho morpho;
    ZecuredRegistry registry;
    IntakeHarness intake;
    SignerBondV2 bond;
    MockUSDC usdc;
    MockZecOracle oracle;
}

/// @notice Invariant-test handler of the bonds (review finding: the main campaign never exercised liveness, unbonding
///         or reimbursement): signers deposit, unbond (in part and in full), cancel and withdraw; vaults open and release,
///         refunds land and get destinations; nodes hand out liveness receipts (R-5), honest or forged; claimants take
///         overdue claims from the bonds; the signers commit and confirm payouts, and reimburse the bonds for the claims
///         they paid; time and the price move. Ghost state mirrors the bond's books; flags record a violation when it
///         happens. Against the real Morpho Blue on a Base Sepolia fork.
contract BondHandler is Test {
    IMorpho internal immutable morpho;
    ZecuredRegistry internal immutable registry;
    IntakeHarness internal immutable intake;
    SignerBondV2 public immutable bond;
    MockUSDC internal immutable usdc;
    MockZecOracle internal immutable oracle;
    uint256[5] internal signerKeys;
    address[5] public signers;

    uint256 internal constant ACCOUNT_COUNT = 6;
    address[ACCOUNT_COUNT] public accounts;
    address public immutable outsider;
    uint256 internal immutable outsiderKey;

    // ------------------------------------------------------------------ ghost state

    bytes32[] public vaultIds;
    mapping(bytes32 vaultId => address) public accountOf;
    /// Every claim the handler created, in order.
    bytes32[] public claimIds;
    /// The lineage note a claim is paid from (its vault's deposit, or its refund's own note).
    mapping(bytes32 claimId => bytes32) public noteOf;
    /// The commitment the claim was armed for (R-5), as the handler saw it.
    mapping(bytes32 claimId => bytes32) public armedFor;
    /// Liveness payments per claim (at most one, I26).
    mapping(bytes32 claimId => uint256) public paidCount;
    uint256 public totalLivenessPaid;
    uint256 public totalDeposited;
    uint256 public totalWithdrawn;
    uint256 internal nonce;

    bool public violated;
    string public violation;
    mapping(string => uint256) public calls;

    constructor(BondSystem memory s, uint256[5] memory keys, address[5] memory signers_) {
        morpho = s.morpho;
        registry = s.registry;
        intake = s.intake;
        bond = s.bond;
        usdc = s.usdc;
        oracle = s.oracle;
        signerKeys = keys;
        signers = signers_;
        for (uint256 i; i < ACCOUNT_COUNT; ++i) {
            accounts[i] = makeAddr(string.concat("zecured.bondinv.account", vm.toString(i)));
        }
        (outsider, outsiderKey) = makeAddrAndKey("zecured.bondinv.outsider");
    }

    /// The bond's USDC the fixture deposited before the campaign started (five minimum bonds).
    function recordInitialDeposits(uint256 amount) external {
        totalDeposited += amount;
    }

    // ------------------------------------------------------------------ views for the invariants

    function vaultCount() external view returns (uint256) {
        return vaultIds.length;
    }

    function claimCount() external view returns (uint256) {
        return claimIds.length;
    }

    // ------------------------------------------------------------------ helpers

    function _mix(uint256 seed) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(seed)));
    }

    function _flag(string memory what) internal {
        if (!violated) violation = what;
        violated = true;
    }

    function _sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _sigs(bytes32 digest, uint256 count) internal view returns (bytes[] memory sigs) {
        sigs = new bytes[](count);
        for (uint256 i; i < count; ++i) {
            sigs[i] = _sign(signerKeys[i], digest);
        }
    }

    function _claim(uint256 seed) internal view returns (bool, bytes32) {
        if (claimIds.length == 0) return (false, bytes32(0));
        return (true, claimIds[seed % claimIds.length]);
    }

    function _signerIndex(uint256 seed) internal pure returns (uint8) {
        // forge-lint: disable-next-line(unsafe-typecast)
        return uint8(seed % 5);
    }

    function _status(bytes32 vaultId) internal view returns (VaultStatus status) {
        (,, status) = registry.vault(vaultId);
    }

    /// Whether the bonds may pay the claim now, as the bond's rules say (computed independently).
    function _livenessExpected(bytes32 claimId, ClaimDetails memory c) internal view returns (bool, uint256) {
        bool kindOk = c.kind == ClaimKind.Release || c.kind == ClaimKind.Liquidation || c.kind == ClaimKind.CloseUser
            || c.kind == ClaimKind.CloseMarketMaker || c.kind == ClaimKind.Refund;
        if (!kindOk || c.status != ClaimStatus.Pending || c.bondPaid || c.commitment == bytes32(0)) return (false, 0);
        (bytes32 commitment, uint64 armedAt) = bond.livenessArming(claimId);
        if (armedAt == 0 || commitment != c.commitment) return (false, 0); // R-5
        uint64 payableAt = armedAt > c.createdAt ? armedAt : uint64(c.createdAt);
        uint64 deadline = payableAt + bond.livenessPeriod();
        uint64 dueAt = deadline;
        bytes32 input = registry.claimInput(claimId);
        if (input != bytes32(0)) {
            uint64 last = registry.lastCommitAt(input);
            uint64 protectedUntil = (last < deadline ? last : deadline) + bond.commitGrace();
            if (protectedUntil > dueAt) dueAt = protectedUntil;
        }
        if (block.timestamp < dueAt) return (false, 0);
        uint256 due =
            oracle.price() * uint256(c.amount) * (10_000 + bond.livenessPremiumBps()) / 10_000 / ORACLE_PRICE_SCALE;
        if (due == 0 || due > bond.totalSlashable()) return (false, 0);
        return (true, due);
    }

    // ------------------------------------------------------------------ vaults and claims

    /// A vault opens for an account without one (the harness stands in for a deposit proof).
    function open(uint256 seed) external {
        seed = _mix(seed);
        address account = accounts[seed % ACCOUNT_COUNT];
        if (registry.vaultOf(account) != bytes32(0)) return;
        bytes32 vaultId = keccak256(abi.encode("zecured.bondinv.vault", nonce++));
        uint64 amount = seed % 2 == 0 ? 1_000_000 : 5_000_000;
        intake.harnessActivate(vaultId, account, amount, keccak256(abi.encode("zecured.bondinv.tag", vaultId)));
        vaultIds.push(vaultId);
        accountOf[vaultId] = account;
        vm.prank(account);
        morpho.setAuthorization(address(registry), true);
        calls["open"]++;
    }

    /// An active vault's account releases all of it: a Release claim to its destination.
    function release(uint256 seed) external {
        if (vaultIds.length == 0) return;
        bytes32 vaultId = vaultIds[seed % vaultIds.length];
        if (_status(vaultId) != VaultStatus.Active) return;
        vm.prank(accountOf[vaultId]);
        bytes32 claimId = registry.requestRelease(vaultId, keccak256(abi.encode("zecured.bondinv.dest", vaultId)));
        claimIds.push(claimId);
        noteOf[claimId] = keccak256(abi.encode("zecured.bondinv.tag", vaultId));
        calls["release"]++;
    }

    /// A second deposit to a known vault lands as a refund (the harness opens it at once), without a destination.
    function refund(uint256 seed) external {
        if (vaultIds.length == 0) return;
        bytes32 vaultId = vaultIds[seed % vaultIds.length];
        if (_status(vaultId) == VaultStatus.Pending) return;
        bytes32 tag = keccak256(abi.encode("zecured.bondinv.refundTag", nonce++));
        bytes32 claimId = intake.harnessRefund(vaultId, accountOf[vaultId], 1_000_000, tag);
        claimIds.push(claimId);
        noteOf[claimId] = tag;
        calls["refund"]++;
    }

    /// A refund's account names (or changes) its destination: the arming of the old one no longer counts (R-5).
    function giveDestination(uint256 seed, uint256 destSeed) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (c.kind != ClaimKind.Refund || c.status != ClaimStatus.Pending || c.bondPaid) return;
        if (registry.claimInput(claimId) != bytes32(0)) return;
        vm.prank(c.claimant);
        registry.setRefundDestination(claimId, keccak256(abi.encode("zecured.bondinv.refundDest", destSeed % 3)));
        calls["refund destination"]++;
    }

    // ------------------------------------------------------------------ receipts and liveness (R-5, T-05)

    /// A signer's node hands out the receipt of a claim's destination, received some time around its creation; anyone
    /// relays it.
    function arm(uint256 seed, uint256 signerSeed, uint256 offsetSeed) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (c.status != ClaimStatus.Pending || c.bondPaid || c.commitment == bytes32(0)) return;
        // From a day before the claim (the node held the destination already) to now.
        uint256 earliest = c.createdAt > 1 days ? c.createdAt - 1 days : 0;
        // forge-lint: disable-next-line(unsafe-typecast)
        uint64 receivedAt = uint64(bound(offsetSeed, earliest, block.timestamp));
        uint256 signer = _signerIndex(signerSeed);
        bytes memory receipt =
            _sign(signerKeys[signer], bond.hashDestinationReceived(claimId, c.commitment, receivedAt));
        (bytes32 oldCommitment, uint64 oldPayableAt) = bond.livenessArming(claimId);
        vm.prank(outsider);
        try bond.armLiveness(claimId, receivedAt, receipt) {
            (bytes32 commitment, uint64 payableAt) = bond.livenessArming(claimId);
            uint64 fresh = receivedAt > c.createdAt ? receivedAt : uint64(c.createdAt);
            uint64 expected =
                oldCommitment == c.commitment && oldPayableAt != 0 && oldPayableAt <= fresh ? oldPayableAt : fresh;
            if (commitment != c.commitment || payableAt != expected) _flag("R-5: an arming not as the rules say");
            armedFor[claimId] = c.commitment;
            calls["arm"]++;
        } catch {
            _flag("R-5: a signer's receipt for a payable claim was refused");
        }
    }

    /// Forged receipts never arm a claim: an outsider's, or a signer's for another destination or a later time.
    function armForged(uint256 seed, uint256 kind) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        (bytes32 beforeCommitment, uint64 beforePayableAt) = bond.livenessArming(claimId);
        uint64 at = uint64(block.timestamp);
        bytes memory receipt;
        if (kind % 3 == 0) {
            receipt = _sign(outsiderKey, bond.hashDestinationReceived(claimId, c.commitment, at));
        } else if (kind % 3 == 1) {
            receipt = _sign(signerKeys[0], bond.hashDestinationReceived(claimId, keccak256("forged"), at));
        } else {
            receipt = _sign(signerKeys[1], bond.hashDestinationReceived(claimId, c.commitment, at + 1));
        }
        try bond.armLiveness(claimId, at, receipt) {
            _flag("R-5: a forged receipt armed a claim");
        } catch {}
        (bytes32 afterCommitment, uint64 afterPayableAt) = bond.livenessArming(claimId);
        if (afterCommitment != beforeCommitment || afterPayableAt != beforePayableAt) {
            _flag("R-5: a refused receipt changed an arming");
        }
        calls["arm (forged)"]++;
    }

    /// The claimant takes an overdue claim from the bonds; it succeeds exactly when the rules say (receipt, kind,
    /// status, due time, the bonds' cover), for exactly the claim's value plus the premium, taken from the bonds.
    function slashLiveness(uint256 seed, bool waitDue) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        if (waitDue) {
            (uint64 dueAt,) = bond.livenessDue(claimId);
            if (dueAt > block.timestamp && dueAt < block.timestamp + 30 days) vm.warp(dueAt);
        }
        ClaimDetails memory c = registry.claimDetails(claimId);
        (bool expected, uint256 due) = _livenessExpected(claimId, c);
        uint256 slashable = bond.totalSlashable();
        uint256 before = usdc.balanceOf(c.claimant);
        vm.prank(c.claimant == address(0) ? outsider : c.claimant);
        try bond.slashLiveness(claimId) returns (uint256 paid) {
            if (!expected) _flag("liveness paid against the rules");
            if (armedFor[claimId] != c.commitment) _flag("R-5: liveness paid without a receipt for its destination");
            if (paid != due || usdc.balanceOf(c.claimant) != before + paid) _flag("liveness paid the wrong amount");
            if (bond.totalSlashable() != slashable - paid) _flag("liveness not taken from the bonds");
            if (!registry.claimDetails(claimId).bondPaid) _flag("a claim the bonds paid is not marked");
            paidCount[claimId] += 1;
            if (paidCount[claimId] > 1) _flag("I26: a claim paid twice from the bonds");
            totalLivenessPaid += paid;
            calls["liveness"]++;
        } catch {
            if (expected) _flag("an overdue armed claim could not be paid from the bonds");
            calls["liveness (refused)"]++;
        }
    }

    // ------------------------------------------------------------------ payouts and reimbursement (T-05)

    /// The signers commit the payout of a claim from its note (the whole note: no change), then maybe confirm it.
    function pay(uint256 seed, bool confirmToo) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (c.status != ClaimStatus.Pending || c.commitment == bytes32(0)) return;
        bytes32 note = noteOf[claimId];
        if (registry.lineageOf(note).value != c.amount) return; // the note already paid something else
        // One payout per claim and note: committed now, or committed earlier (then only confirmed).
        bytes32 payoutRef = keccak256(abi.encode("zecured.bondinv.payout", claimId, note));
        if (registry.spendRecord(payoutRef).vaultId == bytes32(0) && !_commit(c.vaultId, note, claimId, payoutRef)) {
            return;
        }
        if (confirmToo) _confirm(c.vaultId, payoutRef, claimId);
    }

    function _commit(bytes32 vaultId, bytes32 note, bytes32 claimId, bytes32 payoutRef) internal returns (bool) {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = claimId;
        bytes32[] memory refs = new bytes32[](1);
        refs[0] = keccak256(abi.encode("zecured.bondinv.ref", payoutRef));
        SpendCommit memory sc = SpendCommit({
            vaultId: vaultId,
            inputTag: note,
            claimIds: ids,
            claimRefs: refs,
            claimOutputs: new uint8[](1),
            payoutRef: payoutRef,
            changeTag: bytes32(0),
            changeValue: 0
        });
        try registry.commitSpend(sc, _sigs(registry.hashSpendCommit(sc), 3)) {
            calls["commit"]++;
            return true;
        } catch {
            _flag("a valid payout commitment reverted");
            return false;
        }
    }

    /// The signers confirm the committed payout of `claimId` (R-1: a confirmation names its payout; `IZecuredPayouts`,
    /// served at the Registry's address).
    function _confirm(bytes32 vaultId, bytes32 payoutRef, bytes32 claimId) internal {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = claimId;
        IZecuredPayouts payouts = IZecuredPayouts(address(registry));
        bytes[] memory sigs = _sigs(payouts.hashPayoutConfirmation(vaultId, payoutRef, ids), 3);
        try payouts.confirmPayout(vaultId, payoutRef, ids, sigs) {
            calls["confirm"]++;
        } catch {
            _flag("a committed payout could not be confirmed");
        }
    }

    /// The signers point a claim the bonds paid at their reimbursement destination (all five sign: enough before and
    /// after R-6), then pay it.
    function reimburse(uint256 seed) external {
        (bool found, bytes32 claimId) = _claim(seed);
        if (!found) return;
        ClaimDetails memory c = registry.claimDetails(claimId);
        if (!c.bondPaid || c.status != ClaimStatus.Pending || c.commitment != bytes32(0)) return;
        bytes32 dest = keccak256(abi.encode("zecured.bondinv.reimburse", claimId));
        uint64 deadline = uint64(block.timestamp + 1 hours);
        try registry.setReimburseDestination(
            claimId, dest, deadline, _sigs(registry.hashReimburseDestination(claimId, dest, deadline), 5)
        ) {
            calls["reimburse destination"]++;
        } catch {
            _flag("a bond-paid claim's reimbursement destination was refused");
        }
    }

    // ------------------------------------------------------------------ bonds

    function deposit(uint256 signerSeed, uint256 amount) external {
        uint8 i = _signerIndex(signerSeed);
        if (bond.halted()) return;
        amount = bound(amount, 1, 200e6);
        usdc.mint(signers[i], amount);
        vm.startPrank(signers[i]);
        usdc.approve(address(bond), amount);
        bond.deposit(amount);
        vm.stopPrank();
        totalDeposited += amount;
        calls["deposit"]++;
    }

    /// A signer starts to leave: in part (keeping minBond) or in full (only with no vault open, no case, and the
    /// challenge window past since the last vault settled).
    function requestUnbond(uint256 signerSeed, uint256 amountSeed, bool full) external {
        uint8 i = _signerIndex(signerSeed);
        uint256 bonded = bond.bondOf(i);
        if (bonded == 0) return;
        uint256 minBond = bond.minBond();
        if (!full && bonded <= minBond && !bond.halted()) {
            // A partial exit needs something above the minimum: the signer tops up first.
            uint256 topUp = bound(amountSeed, 1e6, 100e6);
            usdc.mint(signers[i], topUp);
            vm.startPrank(signers[i]);
            usdc.approve(address(bond), topUp);
            bond.deposit(topUp);
            vm.stopPrank();
            totalDeposited += topUp;
            bonded += topUp;
        }
        uint256 amount = full ? bonded : (bonded > minBond ? bound(amountSeed, 1, bonded - minBond) : 0);
        if (amount == 0) return;
        bool allowed = !full
            || (registry.openVaultCount() == 0
                && block.timestamp >= registry.lastVaultSettledAt() + bond.challengeWindow());
        (uint256 unbondingBefore,) = bond.unbondingOf(i);
        vm.prank(signers[i]);
        try bond.requestUnbond(amount) {
            if (!allowed) _flag("a full exit while a vault was open");
            (uint256 unbonding, uint64 readyAt) = bond.unbondingOf(i);
            if (unbonding != unbondingBefore + amount || readyAt != block.timestamp + bond.unbondingPeriod()) {
                _flag("an unbonding not as requested");
            }
            calls[full ? "unbond (full)" : "unbond (partial)"]++;
        } catch {
            if (allowed) _flag("an allowed unbonding was refused");
            calls["unbond (refused)"]++;
        }
    }

    function cancelUnbond(uint256 signerSeed) external {
        uint8 i = _signerIndex(signerSeed);
        (uint256 unbonding,) = bond.unbondingOf(i);
        if (unbonding == 0) return;
        uint256 bonded = bond.bondOf(i);
        vm.prank(signers[i]);
        bond.cancelUnbond();
        if (bond.bondOf(i) != bonded + unbonding) _flag("a cancelled unbonding not returned in full");
        calls["unbond (cancelled)"]++;
    }

    /// A ready unbonding is withdrawn; before it is ready, it never is.
    function withdraw(uint256 signerSeed, bool waitReady) external {
        uint8 i = _signerIndex(signerSeed);
        (uint256 unbonding, uint64 readyAt) = bond.unbondingOf(i);
        if (unbonding == 0) return;
        if (waitReady && block.timestamp < readyAt) vm.warp(readyAt);
        bool ready = block.timestamp >= readyAt;
        uint256 before = usdc.balanceOf(signers[i]);
        vm.prank(signers[i]);
        try bond.withdrawUnbonded(signers[i]) {
            if (!ready) _flag("an unbonding withdrawn before it was ready");
            if (usdc.balanceOf(signers[i]) != before + unbonding) _flag("a withdrawal of the wrong amount");
            totalWithdrawn += unbonding;
            calls["withdraw"]++;
        } catch {
            if (ready) _flag("a ready unbonding could not be withdrawn");
            calls["withdraw (not ready)"]++;
        }
    }

    // ------------------------------------------------------------------ time and price

    function warp(uint256 secondsToSkip) external {
        skip(bound(secondsToSkip, 1, 3 days));
        calls["warp"]++;
    }

    function setPrice(uint256 cents) external {
        oracle.setPrice(bound(cents, 2_000, 8_000) * 1e32); // $20 .. $80
        calls["setPrice"]++;
    }
}
