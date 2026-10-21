// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {console} from "forge-std/console.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayouts} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {Victim, Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {IZecuredRefunds} from "../../src/interfaces/v3/IZecuredRefunds.sol";
import {Tier3Base} from "../utils/Tier3Base.sol";
import {CompensationHandler, CompensationSystem} from "./CompensationHandler.sol";

/// @notice Handler-based invariant tests of the v4 payout and compensation paths on the real Registry, spend logic,
///         slasher, bond and compensator (MOCK verifier, relay, USDC and price) against the real Morpho Blue on a Base
///         Sepolia fork: proofs of payouts, confirmations, disputes and expiries, F1, F2 and F3 cases executed, and the
///         compensation from the tally to the withdrawals, burns and the R-20 marking included. The main campaign
///         (ZecuredInvariants.t.sol) drives deposits and exits with a MOCK slasher; this one drives what that MOCK cannot.
contract CompensationInvariantsTest is Tier3Base {
    CompensationHandler internal handler;

    function setUp() public override {
        super.setUp();
        handler = new CompensationHandler(
            CompensationSystem({
                morpho: MORPHO,
                registry: registry,
                intake: intake,
                slasher: slasher,
                compensator: compensator,
                usdc: usdc,
                oracle: oracle,
                relay: MockHeaderRelay(address(headerRelay)),
                verifier: verifier,
                violationImage: violationImage,
                anchor: ANCHOR
            }),
            signerKeys
        );
        usdc.transferOwnership(address(handler)); // MOCK mint for repayments
        oracle.transferOwnership(address(handler)); // MOCK price moves

        bytes4[] memory selectors = new bytes4[](33);
        selectors[0] = CompensationHandler.open.selector;
        selectors[1] = CompensationHandler.open.selector;
        selectors[2] = CompensationHandler.borrow.selector;
        selectors[3] = CompensationHandler.repay.selector;
        selectors[4] = CompensationHandler.release.selector;
        selectors[5] = CompensationHandler.release.selector;
        selectors[6] = CompensationHandler.setPrice.selector;
        selectors[7] = CompensationHandler.warp.selector;
        selectors[8] = CompensationHandler.commit.selector;
        selectors[9] = CompensationHandler.commit.selector;
        selectors[10] = CompensationHandler.confirm.selector;
        selectors[11] = CompensationHandler.prove.selector;
        selectors[12] = CompensationHandler.prove.selector;
        selectors[13] = CompensationHandler.dispute.selector;
        selectors[14] = CompensationHandler.reopen.selector;
        selectors[15] = CompensationHandler.expire.selector;
        selectors[16] = CompensationHandler.theft.selector;
        selectors[17] = CompensationHandler.withheld.selector;
        selectors[18] = CompensationHandler.misdirect.selector;
        selectors[19] = CompensationHandler.execute.selector;
        selectors[20] = CompensationHandler.execute.selector;
        selectors[21] = CompensationHandler.tally.selector;
        selectors[22] = CompensationHandler.tally.selector;
        selectors[23] = CompensationHandler.finalize.selector;
        selectors[24] = CompensationHandler.settle.selector;
        selectors[25] = CompensationHandler.burn.selector;
        selectors[26] = CompensationHandler.withdraw.selector;
        // R-12: refunds reserved before a compromise, opened or dropped.
        selectors[27] = CompensationHandler.reserveRefund.selector;
        selectors[28] = CompensationHandler.reserveRefund.selector;
        selectors[29] = CompensationHandler.openRefund.selector;
        selectors[30] = CompensationHandler.dropRefund.selector;
        // W-1, W-5: F4 cases, and liveness asked while a case about the claim is open.
        selectors[31] = CompensationHandler.wrongRef.selector;
        selectors[32] = CompensationHandler.livenessDuringCase.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
        targetSender(makeAddr("zecured.comp.inv.caller"));
    }

    /// I1: the zZEC supply is the outstanding of the active vaults.
    function invariant_I1_supplyEqualsActiveOutstanding() public view {
        uint256 sum;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            (, uint64 outstanding, VaultStatus status) = registry.vault(handler.vaultIds(i));
            if (status == VaultStatus.Active) sum += outstanding;
            else assertEq(outstanding, 0, "a vault that is not active has outstanding");
        }
        assertEq(zzec.totalSupply(), sum, "I1");
    }

    /// The bookkeeping the bonds and the cap wait on (I17, I31, R-20): a vault is open while active (unless the
    /// compensator settled it), or while it has a claim pending or disputed; `lockedZatoshi` is the supply plus every
    /// pending or disputed claim, less the outstanding of the settled active victims; `unprovenConfirmations` counts
    /// the confirmations still open.
    function invariant_bookkeeping() public view {
        uint256 open;
        uint256 unpaid;
        uint256 compensatedOutstanding;
        for (uint256 i; i < handler.vaultCount(); ++i) {
            (bool isOpen, uint256 vaultUnpaid, uint256 settledOutstanding) = _vaultBooks(handler.vaultIds(i));
            if (isOpen) ++open;
            unpaid += vaultUnpaid;
            compensatedOutstanding += settledOutstanding;
        }
        assertEq(registry.openVaultCount(), open, "I17: open vaults");
        assertEq(registry.lockedZatoshi(), zzec.totalSupply() + unpaid - compensatedOutstanding, "lockedZatoshi");
    }

    function _vaultBooks(bytes32 vaultId)
        internal
        view
        returns (bool isOpen, uint256 unpaid, uint256 settledOutstanding)
    {
        (, uint64 outstanding, VaultStatus status) = registry.vault(vaultId);
        bool compensated = _payouts().vaultCompensated(vaultId);
        uint256 unproven;
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            (,, uint64 amount,, ClaimStatus s) = registry.claim(claimId);
            (uint64 paidAt,) = IZecuredDisputes(address(registry)).payoutDispute(claimId);
            if (s == ClaimStatus.Pending || s == ClaimStatus.Disputed) unpaid += amount;
            if (s == ClaimStatus.Paid && paidAt != 0) ++unproven;
        }
        assertEq(_payouts().unprovenConfirmations(vaultId), unproven, "unproven confirmations");
        isOpen = (status == VaultStatus.Active && !compensated) || unpaid != 0;
        if (compensated && status == VaultStatus.Active) settledOutstanding = outstanding;
    }

    /// R-12: the Registry counts each vault's reserved refunds as the handler does, and a victim is tallied only once
    /// none of its refunds is reserved. A refund opened on a compromised vault is never the signers' to pay: pending until
    /// tallied, then compensated with an entitlement of its amount for its account, exactly once.
    function invariant_R12_reservedRefunds() public view {
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            uint256 reserved = refunds.reservedRefundCount(vaultId);
            assertEq(reserved, handler.reservedCountOf(vaultId), "R-12: reserved refunds");
            if (compensator.victim(vaultId).tallied) assertEq(reserved, 0, "R-12: tallied with a refund reserved");
        }
        for (uint256 i; i < handler.lateRefundCount(); ++i) {
            bytes32 claimId = handler.lateRefunds(i);
            (bytes32 vaultId,, uint64 amount,, ClaimStatus s) = registry.claim(claimId);
            Entitlement memory e = compensator.entitlement(claimId);
            if (s == ClaimStatus.Compensated) {
                assertTrue(e.incident != 0, "R-12: a late refund compensated without an entitlement");
                assertEq(e.zat, amount, "R-12: the entitlement is the refund");
                assertEq(e.to, registry.vaultDetails(vaultId).account, "R-12: owed to the vault's account");
            } else {
                assertEq(uint8(s), uint8(ClaimStatus.Pending), "R-12: a late refund neither pending nor compensated");
                assertEq(e.incident, 0, "R-12: entitled while still pending");
                assertFalse(compensator.victim(vaultId).tallied, "R-12: pending on a tallied victim");
            }
        }
    }

    /// I19: the compensator holds exactly its unassigned funds and what it set aside.
    function invariant_I19_compensatorBooks() public view {
        assertEq(usdc.balanceOf(address(compensator)), compensator.funds() + compensator.reserved(), "I19");
    }

    /// R-7 and T-04: a claim with an entitlement is never also payable on Zcash or by the bonds (pending, disputed, or
    /// confirmed but still open); the compensator's victims are compromised vaults, frozen since before their
    /// compromise; a tallied victim has no claim left to tally; a vault the compensator marked was settled.
    function invariant_R7_R20_compensation() public view {
        for (uint256 i; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            _checkEntitlements(vaultId);
            _checkVictim(vaultId);
        }
    }

    function _checkEntitlements(bytes32 vaultId) internal view {
        uint64 count = registry.claimCount(vaultId);
        for (uint64 j; j < count; ++j) {
            bytes32 claimId = registry.claimIdOf(vaultId, j);
            if (compensator.entitlement(claimId).incident == 0) continue;
            (,,,, ClaimStatus s) = registry.claim(claimId);
            (uint64 paidAt,) = IZecuredDisputes(address(registry)).payoutDispute(claimId);
            assertTrue(s != ClaimStatus.Pending && s != ClaimStatus.Disputed, "R-7: an entitled claim is payable");
            assertFalse(s == ClaimStatus.Paid && paidAt != 0, "R-7: an entitled claim is still disputable");
        }
    }

    function _checkVictim(bytes32 vaultId) internal view {
        Victim memory v = compensator.victim(vaultId);
        if (v.incident != 0) {
            assertTrue(slasher.isCompromised(vaultId), "a victim is compromised");
            uint64 compromisedAt = slasher.compromisedAt(vaultId);
            uint64 frozenSince = slasher.frozenSince(vaultId);
            assertTrue(compromisedAt != 0 && frozenSince != 0 && frozenSince <= compromisedAt, "freeze times");
            if (v.tallied) {
                assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0, "a tallied victim has unpaid claims");
                assertEq(_payouts().unprovenConfirmations(vaultId), 0, "a tallied victim has open confirmations");
            }
        } else {
            assertEq(slasher.compromisedAt(vaultId) == 0, !slasher.isCompromised(vaultId), "compromisedAt");
        }
        if (_payouts().vaultCompensated(vaultId)) assertTrue(v.settled, "R-20: marked without a settlement");
    }

    /// The handler's paths do reach their ends: one deterministic sequence through it, from a confirmation made just
    /// before a theft (R-3) to its tally, the incident's finalization, the settlement and the withdrawals, then an F3
    /// case that closes its claim (R-7).
    function test_handler_exercisesTheCompensationPaths() public {
        for (uint256 seed; handler.vaultCount() < 8 && seed < 64; ++seed) {
            handler.open(seed);
        }
        (uint256 a, uint256 b) = (_authorizedVault(0), 0);
        b = _authorizedVault(a + 1);
        handler.commit(a, 1);
        handler.confirm(0, 1);
        handler.theft(_theftSeed(a));
        handler.execute(0);
        handler.tally(a);
        handler.finalize(0);
        handler.settle(a);
        handler.withdraw(0);
        assertEq(handler.calls("confirm"), 1, "confirmed");
        assertEq(handler.calls("F1"), 1, "stolen");
        assertEq(handler.calls("tally (R-3 confirmation)"), 1, "the confirmation made just before the freeze tallied");
        assertEq(handler.calls("tally vault"), 1);
        assertEq(handler.calls("finalize"), 1);
        assertEq(handler.calls("settle"), 1);
        assertGt(handler.calls("withdraw entitlement"), 0, "paid");

        handler.commit(b, 2);
        handler.misdirect(1, 0);
        handler.execute(0);
        assertEq(handler.calls("F3"), 1);
        assertEq(handler.calls("execute"), 2);
        assertFalse(handler.violated(), handler.violation());
        invariant_R7_R20_compensation();
        invariant_bookkeeping();
    }

    /// The R-12 paths through the handler: refunds reserved before a theft hold the victim's tally until they are
    /// opened on the compromised vault, where the compensator tallies them; a refund reserved elsewhere is dropped.
    function test_handler_reachesTheR12Paths() public {
        for (uint256 seed; handler.vaultCount() < 8 && seed < 64; ++seed) {
            handler.open(seed);
        }
        uint256 a = _authorizedVault(0);
        handler.reserveRefund(a, 500_000);
        handler.reserveRefund(a, 700_000);
        handler.reserveRefund(a + 1, 300_000);
        assertEq(handler.calls("reserve refund"), 3);
        handler.dropRefund(2); // the third refund, of vault a + 1: its proof fell
        assertEq(handler.calls("drop refund"), 1);
        handler.theft(_theftSeed(a));
        handler.execute(0);
        handler.reserveRefund(a, 400_000); // after the compromise: refused
        assertEq(handler.calls("reserve refund (compromised)"), 1);

        handler.tally(_seedWithMix(a, 0)); // nobody opened the refunds: the vault's tally waits
        assertEq(handler.calls("tally vault (refunds reserved)"), 1);
        handler.openRefund(0); // one refund opened on the compromised vault
        handler.tally(_seedWithMix(a, 1)); // a keeper opens the other first, then everything is tallied
        assertEq(handler.calls("open refund (compromised)"), 2);
        assertEq(handler.calls("tally vault"), 1);
        handler.finalize(0);
        handler.withdraw(0);
        assertEq(handler.lateRefundCount(), 2);
        assertGe(handler.calls("withdraw entitlement"), 2, "both refunds paid by the compensator");
        assertFalse(handler.violated(), handler.violation());
        invariant_R12_reservedRefunds();
        invariant_bookkeeping();
    }

    /// W-1, W-5: an F4 case on a committed payout counts against its claim, the bonds refuse that claim's liveness while
    /// the case is open, and its execution makes the compensator owe the claim.
    function test_handler_reachesTheW1W5Paths() public {
        for (uint256 seed; handler.vaultCount() < 8 && seed < 64; ++seed) {
            handler.open(seed);
        }
        handler.commit(_authorizedVault(0), 1);
        handler.wrongRef(0);
        assertEq(handler.calls("F4"), 1, "an F4 case opened");
        handler.livenessDuringCase(0, 0);
        assertEq(handler.calls("liveness (case open)"), 1, "the bonds refused the claim's liveness");
        handler.execute(0);
        assertEq(handler.calls("execute"), 1);
        assertFalse(handler.violated(), handler.violation());
        invariant_R7_R20_compensation();
        invariant_bookkeeping();
    }

    /// A seed that makes `theft` pick the vault at `index` without a refund in flight (the handler's `_mix` odd).
    function _theftSeed(uint256 index) internal view returns (uint256 seed) {
        uint256 n = handler.vaultCount();
        for (seed = index;; seed += n) {
            if (uint256(keccak256(abi.encode(seed))) % 2 == 1) return seed;
        }
    }

    /// The first seed from `from` that selects the same vault in a handler scan (`seed % vaultCount`) and whose hash
    /// (the handler's `_mix`) is a multiple of four (`zero == 0`: refunds left reserved) or not (`zero == 1`: opened).
    function _seedWithMix(uint256 from, uint256 zero) internal view returns (uint256 seed) {
        uint256 n = handler.vaultCount();
        for (seed = from;; seed += n) {
            bool multiple = uint256(keccak256(abi.encode(seed))) % 4 == 0;
            if (multiple == (zero == 0)) return seed;
        }
    }

    /// The index of the first vault from `from` whose account authorized the Registry.
    function _authorizedVault(uint256 from) internal view returns (uint256 i) {
        for (i = from; i < handler.vaultCount(); ++i) {
            bytes32 vaultId = handler.vaultIds(i);
            if (MORPHO.isAuthorized(handler.accounts(handler.accountIndexOf(vaultId)), address(registry))) return i;
        }
        revert("no authorized vault");
    }

    function invariant_noViolationWhileActing() public view {
        assertFalse(handler.violated(), handler.violation());
    }

    function afterInvariant() external view {
        string[36] memory names = [
            "open",
            "borrow",
            "repay",
            "release",
            "partial release",
            "commit",
            "confirm",
            "confirm (frozen)",
            "prove",
            "prove (batch)",
            "dispute",
            "reopen",
            "expire",
            "F1",
            "F2",
            "F3",
            "execute",
            "tally claim",
            "tally (R-3 confirmation)",
            "expire (tally)",
            "tally vault",
            "finalize",
            "settle",
            "burn",
            "withdraw entitlement",
            "withdraw equity",
            "setPrice",
            "warp",
            "reserve refund",
            "reserve refund (compromised)",
            "open refund",
            "open refund (compromised)",
            "drop refund",
            "tally vault (refunds reserved)",
            "F4",
            "liveness (case open)"
        ];
        for (uint256 i; i < names.length; ++i) {
            console.log(names[i], handler.calls(names[i]));
        }
    }
}
