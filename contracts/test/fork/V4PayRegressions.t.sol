// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Vm.sol";
import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {IZecuredRegistry, VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes, IZecuredDisputesEvents} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredPayoutsEvents} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {IZecuredSlasher, ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredSignerErrors} from "../../src/interfaces/v3/IZecuredSignerErrors.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredCompensator} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {DisputesBase} from "./Disputes.t.sol";

/// Shared helpers of the v4 payout suites (this file and ProvenDelivery.t.sol).
abstract contract V4PayBase is DisputesBase {
    address internal account = makeAddr("v4.account");
    IZecuredDisputes internal disputes;

    function setUp() public virtual override {
        super.setUp();
        disputes = IZecuredDisputes(address(registry));
    }

    function _claimStatus(bytes32 claimId) internal view returns (ClaimStatus) {
        return registry.claimDetails(claimId).status;
    }

    function _paidAt(bytes32 claimId) internal view returns (uint64 paidAt) {
        (paidAt,) = disputes.payoutDispute(claimId);
    }

    /// A vault of 0.05 ZEC whose account authorized the Registry and the Adapter, with a partial release of 0.01 ZEC
    /// committed from its deposit (with change).
    function _partialCommitted() internal returns (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) {
        vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        c = _commitNext(vaultId, _one(claimId));
    }

    /// An F1 case on `noteTag` (a lineage note of its vault), proven now: the vault freezes.
    function _proveTheft(bytes32 noteTag) internal returns (bytes32) {
        return _prove(_journal(ViolationKind.Uncommitted, ANCHOR, keccak256("v4.theft"), noteTag, 0, bytes32(0), 0));
    }

    /// The same, executed after the challenge window: the vault is compromised.
    function _compromise(bytes32 noteTag) internal returns (bytes32 caseId) {
        caseId = _proveTheft(noteTag);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
    }

    function _disputeAs(address claimant, bytes32 claimId) internal {
        vm.prank(claimant);
        disputes.disputePayout(claimId);
    }
}

/// @notice The v4 review's findings on the payout, dispute and compensation paths, each reproduced as it was found
///         (every test here failed on the v3 code) and now refused: R-1, R-2, R-3, R-6, R-7, R-16, R-19, R-20, R-21.
///         Real Registry, spend logic, slasher, bond and compensator, MOCK verifier and relay, real Morpho Blue.
contract V4PayRegressionsTest is V4PayBase {
    using MorphoBalancesLib for IMorpho;

    // ================================================================== R-1: replayed confirmations

    /// The review's attack: reopen a disputed claim and confirm it again with the confirmation's old signatures, in one
    /// transaction; the claimant never sees it pending (no bond pays it) and must dispute in every window.
    function test_R1_reopenThenReplayTheOldSignatures_reverts() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        bytes32 payoutRef = _commitNext(vaultId, _one(claimId)).payoutRef;
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, payoutRef, _one(claimId)));
        _payouts().confirmPayout(vaultId, payoutRef, _one(claimId), sigs);
        _disputeAs(account, claimId);
        vm.warp(block.timestamp + INCLUSION_WINDOW + 1);

        disputes.reopenDisputedClaim(claimId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.PayoutAlreadyConfirmed.selector, payoutRef));
        _payouts().confirmPayout(vaultId, payoutRef, _one(claimId), sigs);

        // Nor does a fresh confirmation of a new payout of the same input confirm a reopened claim (proof only).
        SpendCommit memory again =
            _spendCommit(vaultId, _depositTagFor(account), _one(claimId), keccak256("v4.r1.again"));
        _send(again);
        bytes[] memory fresh = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, again.payoutRef, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.ClaimReopened.selector, claimId));
        _payouts().confirmPayout(vaultId, again.payoutRef, _one(claimId), fresh);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Pending), "pending: the bonds or a proof pay it");
    }

    /// A confirmation names its payout: it binds the claims to that payout's input and confirms the payout once.
    function test_R1_aConfirmationIsBoundToItsPayout() public {
        (bytes32 vaultId, bytes32 a, SpendCommit memory first) = _partialCommitted();
        vm.prank(account);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        SpendCommit memory second = _commitNext(vaultId, _one(b));

        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, first.payoutRef, _one(b)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, b, first.payoutRef));
        _payouts().confirmPayout(vaultId, first.payoutRef, _one(b), sigs);
        bytes32 unknown = keccak256("v4.r1.unknownPayout");
        sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, unknown, _one(a)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.PayoutNotOfVault.selector, vaultId, unknown));
        _payouts().confirmPayout(vaultId, unknown, _one(a), sigs);

        _confirmPayout(vaultId, first.payoutRef, _one(a));
        _confirmPayout(vaultId, second.payoutRef, _one(b));
        assertTrue(_payouts().payoutConfirmed(first.payoutRef));
        assertTrue(_payouts().payoutConfirmed(second.payoutRef));
        assertEq(_payouts().unprovenConfirmations(vaultId), 2);
    }

    // ================================================================== R-2: payouts in flight at the compromise

    /// A payout committed before the compromise may have landed: the compensator waits the inclusion window from the
    /// compromise before it owes the claim, and a proof meanwhile pays the claim for good with no entitlement.
    function test_R2_aPayoutInFlightAtTheCompromiseIsProvenNotCompensatedTwice() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag); // three colluders take the change without a commitment
        uint64 at = slasher.compromisedAt(vaultId) + INCLUSION_WINDOW;
        assertEq(slasher.compromisedAt(vaultId), block.timestamp);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ProofWindowOpen.selector, claimId, at));
        compensator.tallyClaim(claimId);

        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutProven(vaultId, claimId, c.payoutRef);
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(_paidAt(claimId), 0, "final");
        vm.warp(at);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);
        compensator.tallyVault(vaultId);
        assertEq(compensator.entitlement(claimId).incident, 0, "no entitlement for a claim that was paid");
    }

    /// Without a proof in that window, the claim is compensated as before.
    function test_R2_withoutAProofTheClaimIsCompensated() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        compensator.tallyClaim(claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
        assertEq(compensator.entitlement(claimId).zat, 1_000_000);
        // Once tallied, a late proof no longer changes anything: the compensator owes the claim.
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotUpholdable.selector, claimId));
        slasher.proveInclusion(journal, seal, violationImage, claimId);
    }

    /// An uncommitted claim has no payout that could have landed: it is tallied at once.
    function test_R2_anUncommittedClaimIsTalliedAtOnce() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        _compromise(_depositTagFor(account));
        compensator.tallyClaim(claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
    }

    /// The optional closure: an executed F2 case proved its payout in a block, so it proves the payment of that
    /// payout's claims like an `Included` proof (and so does F4); a proof of a case that is not executed, or of
    /// another kind, is refused.
    function test_R2_anExecutedF2CaseProvesItsPayoutsClaims() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        bytes32 caseId =
            _prove(_journal(ViolationKind.WithheldChange, ANCHOR, c.payoutRef, c.changeTag, c.changeValue, 0, 0));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.NotAnInclusionCase.selector, caseId));
        slasher.proveInclusionByCase(caseId, _one(claimId));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(slasher.isCompromised(vaultId));

        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.PayoutInclusionProven(claimId, c.payoutRef, ANCHOR);
        assertEq(slasher.proveInclusionByCase(caseId, _one(claimId)), 1);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(slasher.proveInclusionByCase(caseId, _one(claimId)), 0, "already final: skipped");

        // An F1 case never is: its spend was never committed.
        bytes32 f1 = _proveTheft(c.changeTag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.NotAnInclusionCase.selector, f1));
        slasher.proveInclusionByCase(f1, _one(claimId));
    }

    // ================================================================== R-3: confirmations around a freeze

    /// Freeze, then confirm: the signers' word is refused on a frozen vault (an open theft case), and on a compromised
    /// one.
    function test_R3_noConfirmationOnAFrozenVault() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _proveTheft(c.changeTag);
        assertTrue(slasher.vaultFrozen(vaultId));
        assertEq(slasher.frozenSince(vaultId), block.timestamp);
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, c.payoutRef, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        _payouts().confirmPayout(vaultId, c.payoutRef, _one(claimId), sigs);
        // A proof still pays it (proven delivery works on a frozen vault).
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
    }

    /// Confirm, then freeze within the dispute window, then compromise: the claimant can no longer dispute, so the claim
    /// counts as unpaid unless proven; the compensator tallies it once the proof window is over, and tallies the vault
    /// only once no such confirmation is left.
    function test_R3_aConfirmationJustBeforeTheFreezeIsCompensatedUnlessProven() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        vm.warp(block.timestamp + 1 hours);
        _compromise(c.changeTag);
        // Its dispute window ran out during the challenge window (equal to it here): too late to dispute now.
        uint64 closedAt = _paidAt(claimId) + DISPUTE_WINDOW;
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.DisputeWindowClosed.selector, claimId, closedAt));
        disputes.disputePayout(claimId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ConfirmationsUnsettled.selector, vaultId, 1));
        compensator.tallyVault(vaultId);
        // Nobody can wave it through as an old confirmation either.
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.ConfirmationSuspect.selector, claimId));
        _payouts().expireConfirmation(claimId);

        uint64 at = slasher.compromisedAt(vaultId) + INCLUSION_WINDOW;
        if (block.timestamp < at) {
            vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ProofWindowOpen.selector, claimId, at));
            compensator.tallyClaim(claimId);
            vm.warp(at);
        }
        compensator.tallyClaim(claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
        assertEq(compensator.entitlement(claimId).zat, 1_000_000);
        assertEq(_payouts().unprovenConfirmations(vaultId), 0);
        compensator.tallyVault(vaultId);
    }

    /// The same, proven before the tally: the claim is paid for good, and the compensator owes nothing for it.
    function test_R3_aConfirmationJustBeforeTheFreeze_provenInTime() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        _compromise(c.changeTag);
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutProven(vaultId, claimId, c.payoutRef);
        _upholdWith(c.payoutRef, claimId);
        assertEq(_paidAt(claimId), 0);
        assertEq(_payouts().unprovenConfirmations(vaultId), 0);
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);
        compensator.tallyVault(vaultId);
        assertEq(compensator.entitlement(claimId).incident, 0);
    }

    /// Confirmation and freeze in the same block: confirmed first, the claim is one of those counted unpaid unless
    /// proven; frozen first, the confirmation is refused.
    function test_R3_sameBlock() public {
        uint256 snap = vm.snapshotState();
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        _proveTheft(c.changeTag); // same block
        assertEq(slasher.frozenSince(vaultId), _paidAt(claimId));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(slasher.caseIdOf(ViolationKind.Uncommitted, keccak256("v4.theft"), c.changeTag));
        vm.warp(block.timestamp + INCLUSION_WINDOW);
        compensator.tallyClaim(claimId);
        assertEq(compensator.entitlement(claimId).zat, 1_000_000, "counted unpaid");

        vm.revertToState(snap);
        (vaultId, claimId, c) = _partialCommitted();
        _proveTheft(c.changeTag);
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, c.payoutRef, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        _payouts().confirmPayout(vaultId, c.payoutRef, _one(claimId), sigs); // same block, after the freeze
    }

    /// A confirmation older than the dispute window when the vault froze stands: anyone makes it final, the compensator
    /// does not tally it, and it never holds up the vault's tally.
    function test_R3_anOldConfirmationStands() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        uint64 closesAt = _paidAt(claimId) + DISPUTE_WINDOW;
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.DisputeWindowOpen.selector, claimId, closesAt));
        _payouts().expireConfirmation(claimId);
        vm.warp(closesAt + 1);
        _compromise(c.changeTag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ConfirmationsUnsettled.selector, vaultId, 1));
        compensator.tallyVault(vaultId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);

        vm.expectEmit(address(registry));
        emit IZecuredPayoutsEvents.ConfirmationExpired(vaultId, claimId);
        vm.prank(stranger);
        _payouts().expireConfirmation(claimId);
        assertEq(_paidAt(claimId), 0);
        compensator.tallyVault(vaultId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.NotUnproven.selector, claimId));
        _payouts().expireConfirmation(claimId);
    }

    /// A freeze whose case is dismissed ends: a confirmation from before it can expire again once its window is over.
    function test_R3_aDismissedFreezeEnds() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        bytes32 caseId = _proveTheft(c.changeTag);
        assertTrue(slasher.frozenSince(vaultId) != 0);
        relay.setBlock(ANCHOR, false, 0); // the theft's block left the best chain
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId); // dismissed
        assertEq(slasher.frozenSince(vaultId), 0);
        vm.warp(_paidAt(claimId) + DISPUTE_WINDOW + 1);
        _payouts().expireConfirmation(claimId);
        assertEq(_paidAt(claimId), 0);
    }

    // ================================================================== R-6: the Reimburse destination

    /// Three colluders pointed a bond-paid claim's reimbursement at themselves although all five bonds paid it.
    function test_R6_reimburseDestinationNeedsAllFive() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(address(bond));
        registry.markBondPaid(claimId);
        bytes32 dest = keccak256("v4.reimburse");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes32 digest = registry.hashReimburseDestination(claimId, dest, deadline);
        bytes[] memory three = _threeSigs(digest);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 3));
        registry.setReimburseDestination(claimId, dest, deadline, three);
        registry.setReimburseDestination(claimId, dest, deadline, _fiveSigs(digest));
        (,,, bytes32 commitment,) = registry.claim(claimId);
        assertEq(commitment, dest);
        vaultId;
    }

    // ================================================================== R-7: F3 and liveness

    /// After an F3 slash the claim stayed pending, so its claimant could also take it from the bonds: about twice.
    /// The F3 execution now closes it; the bonds see no pending claim, and the compensator owes the shortfall.
    function test_R7_anF3SlashClosesTheClaim() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        bytes32 caseId = _prove(
            _journal(ViolationKind.Misdirected, ANCHOR, c.claimRefs[0], bytes32(0), expected / 2, RELEASE_COMMITMENT, 0)
        );
        uint256 locked = registry.lockedZatoshi();
        vm.warp(block.timestamp + WINDOW);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ClaimCompensated(vaultId, claimId);
        slasher.executeSlash(caseId);
        assertEq(compensator.entitlement(claimId).zat, expected - expected / 2, "the shortfall is the compensator's");
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated), "closed in the Registry");
        assertEq(registry.lockedZatoshi(), locked - 1_000_000);
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(claimId);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
        vm.warp(block.timestamp + LIVENESS_PERIOD + COMMIT_GRACE);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);
    }

    /// The same for a claim its claimant disputed (and so counted unpaid again): the F3 slash settles it.
    function test_R7_anF3SlashClosesADisputedClaim() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        _disputeAs(account, claimId);
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 1);
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        bytes32 caseId =
            _prove(_journal(ViolationKind.Misdirected, ANCHOR, c.claimRefs[0], bytes32(0), 0, RELEASE_COMMITMENT, 0));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        assertEq(compensator.entitlement(claimId).zat, expected);
        (, uint64 dueAfter) = disputes.payoutDispute(claimId);
        assertEq(dueAfter, 0);
    }

    /// The claim's closure and the shortfall's registration run under `try` (the slash must stand whatever they do), so
    /// an executor starving them could leave the claimant without its shortfall or the claim open to the bonds: a
    /// starved execution reverts instead.
    function test_R7_aStarvedF3ExecutionReverts() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        bytes32 caseId =
            _prove(_journal(ViolationKind.Misdirected, ANCHOR, c.claimRefs[0], bytes32(0), 0, RELEASE_COMMITMENT, 0));
        vm.warp(block.timestamp + WINDOW);
        uint256 snap = vm.snapshotState();
        slasher.executeSlash(caseId);
        Vm.Gas memory gas = vm.lastCallGas();
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
        vm.revertToState(snap);
        uint256 consumed = uint256(gas.gasTotalUsed) + (gas.gasRefunded > 0 ? uint256(uint64(gas.gasRefunded)) : 0);
        vm.expectRevert(IZecuredCompensator.InsufficientGasForCase.selector);
        slasher.executeSlash{gas: consumed}(caseId);
    }

    // ================================================================== R-16: tag squatting

    /// Three colluders recorded a victim's deposit tag, seen on Zcash, as another vault's change: the victim's proof
    /// could never land and no violation was provable. The deposit now takes the tag over (no real change note can carry
    /// it), and the record's input state goes with it.
    function test_R16_aDepositWinsOverAChangeTag() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        address victim = makeAddr("v4.victim");
        bytes32 tag = _depositTagFor(victim);
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(account), _one(claimId), "v4.squat");
        c.changeTag = tag;
        _send(c);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ChangeTagOverridden(tag, vaultId);
        intake.harnessActivate(_vaultIdFor(victim), victim, VAULT_001, tag);
        assertEq(registry.lineageOf(tag).vaultId, _vaultIdFor(victim));
        assertEq(registry.lineageOf(tag).value, VAULT_001);
    }

    /// The squatters went further: they committed a payout of their vault from the squatted tag, under the payout
    /// reference of the transaction they then use to take the victim's note. The override resets the tag's claim set,
    /// so the victim's vault pays its own claims from its note; the squatting vault's claim bound to that tag is bound
    /// again to a real note of its vault; and F1 still catches the theft of the victim's note, which no commitment of
    /// the victim's vault covers.
    function test_R16_aSquattedTagThatWasAlsoSpent() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.startPrank(account);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        vm.stopPrank();
        address victim = makeAddr("v4.victim");
        bytes32 tag = _depositTagFor(victim);
        bytes32 theftRef = keccak256("v4.r16.theftRef");
        SpendCommit memory squat = _spendCommit(vaultId, _depositTagFor(account), _one(a), "v4.r16.squat");
        squat.changeTag = tag; // value 4,000,000
        _send(squat);
        SpendCommit memory fromTag = _spendCommit(vaultId, tag, _one(b), "v4.r16.fromTag");
        fromTag.payoutRef = theftRef;
        _send(fromTag);
        assertEq(registry.claimInput(b), tag);

        intake.harnessActivate(_vaultIdFor(victim), victim, VAULT_005, tag);
        assertEq(registry.inputClaimSet(tag), bytes32(0), "the false record's claim set is gone");
        assertEq(registry.lastCommitAt(tag), 0);

        // The victim's vault pays its own claim from its note.
        vm.startPrank(victim);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 victimClaim = registry.requestPartialRelease(_vaultIdFor(victim), 1_000_000, RELEASE_COMMITMENT);
        vm.stopPrank();
        SpendCommit memory victimPayout = _spendCommit(_vaultIdFor(victim), tag, _one(victimClaim), "v4.r16.victim");
        _send(victimPayout);
        assertEq(registry.claimInput(victimClaim), tag);

        // The squatting vault's claim b is bound again: its deposit (never really spent) pays `a` again with a real
        // change, from which `b` is paid.
        SpendCommit memory redo = _spendCommit(vaultId, _depositTagFor(account), _one(a), "v4.r16.redo");
        _send(redo);
        SpendCommit memory payB = _spendCommit(vaultId, redo.changeTag, _one(b), "v4.r16.payB");
        _send(payB);
        assertEq(registry.claimInput(b), redo.changeTag, "bound again to a note of its own vault");

        // The theft of the victim's note under `theftRef`: the record of another vault under that reference does not
        // cover it.
        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, ANCHOR, theftRef, tag, 0, bytes32(0), 0));
        assertEq(slasher.caseOf(caseId).vaultId, _vaultIdFor(victim));
        // A proof of the squatting payout never pays the victim's claim either.
        bytes memory journal = _included(ANCHOR, theftRef);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, victimClaim, theftRef));
        slasher.proveInclusion(journal, seal, violationImage, victimClaim);
    }

    // ================================================================== R-19: borrowing against stolen collateral

    /// The owner of a compromised vault borrowed against its unbacked zZEC until the settlement, moving value from the
    /// other owners and the lenders to itself. The collateral is now burned down to the LLTV when the case executes; the
    /// vault's equity is still that of what it held.
    function test_R19_noBorrowingAgainstStolenCollateral() public {
        (bytes32 vaultId,, SpendCommit memory c) = _partialCommitted();
        _borrow(account, 400_000); // 4,000,000 zat at $40 = 1.6 USDC: 25 %
        uint64 before = _outstanding(vaultId);
        vm.recordLogs();
        _compromise(c.changeTag);
        uint64 burned = _burnedIn(vm.getRecordedLogs(), vaultId);
        assertGt(burned, 0, "burned at the compromise");
        assertEq(_outstanding(vaultId), before - burned);
        assertEq(_position(account).collateral, before - burned);
        vm.prank(account);
        vm.expectRevert(bytes("insufficient collateral"));
        MORPHO.borrow(params, 1, 0, account, borrowReceiver);

        // The tally counts what the vault held when compromised.
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        compensator.tallyClaim(registry.claimIdOf(vaultId, 0));
        compensator.tallyVault(vaultId);
        assertEq(compensator.victim(vaultId).outstanding, before);
    }

    /// A vault without debt is retired at once.
    function test_R19_aVaultWithoutDebtIsRetiredAtTheCompromise() public {
        (bytes32 vaultId,, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(_outstanding(vaultId), 0);
        assertEq(zzec.totalSupply(), 0);
    }

    /// An account that revoked the Registry's authorization cannot be burned: the slash still executes, and anyone burns
    /// once it authorizes again (or after a price move), never twice what the LLTV allows.
    function test_R19_theBurnNeverBlocksTheSlash_andAnyoneRetries() public {
        (bytes32 vaultId,, SpendCommit memory c) = _partialCommitted();
        _borrow(account, 400_000);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), false);
        uint64 before = _outstanding(vaultId);
        _compromise(c.changeTag);
        assertTrue(slasher.isCompromised(vaultId));
        assertEq(_outstanding(vaultId), before, "nothing burned without the authorization");
        vm.expectRevert(bytes("unauthorized"));
        compensator.burnCollateral(vaultId);

        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
        vm.prank(stranger);
        uint64 retired = compensator.burnCollateral(vaultId);
        Vm.Gas memory gas = vm.lastCallGas();
        assertLt(gas.gasTotalUsed, slasher.BURN_GAS(), "the burn fits the gas the slasher hands it");
        assertGt(retired, 0);
        assertEq(compensator.burnCollateral(vaultId), 0, "nothing more to burn at the same price");
        oracle.setPrice(PRICE_40 * 2);
        assertGt(compensator.burnCollateral(vaultId), 0, "a higher price frees more to burn");
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotBurnable.selector, keccak256("v4.nothing")));
        compensator.burnCollateral(keccak256("v4.nothing"));
    }

    /// The slasher hands the burn exactly `BURN_GAS`; an executor that starves it cannot skip it.
    function test_R19_aStarvedExecutionReverts() public {
        (bytes32 vaultId,, SpendCommit memory c) = _partialCommitted();
        _borrow(account, 400_000);
        bytes32 caseId = _proveTheft(c.changeTag);
        vm.warp(block.timestamp + WINDOW);
        uint256 snap = vm.snapshotState();
        slasher.executeSlash(caseId);
        Vm.Gas memory gas = vm.lastCallGas();
        assertLt(_outstanding(vaultId), 4_000_000, "burned");
        vm.revertToState(snap);
        // The gas the execution consumed is not enough to execute it: the burn needs `BURN_GAS` of headroom, so a
        // starved execution is refused rather than executed without the burn.
        uint256 consumed = uint256(gas.gasTotalUsed) + (gas.gasRefunded > 0 ? uint256(uint64(gas.gasRefunded)) : 0);
        vm.expectRevert(IZecuredSlasher.InsufficientGasForBurn.selector);
        slasher.executeSlash{gas: consumed}(caseId);
    }

    function _burnedIn(Vm.Log[] memory logs, bytes32 vaultId) internal view returns (uint64 burned) {
        for (uint256 i; i < logs.length; ++i) {
            if (
                logs[i].emitter == address(compensator)
                    && logs[i].topics[0] == IZecuredCompensator.CollateralBurned.selector
                    && logs[i].topics[1] == vaultId
            ) burned += abi.decode(logs[i].data, (uint64));
        }
    }

    // ================================================================== R-20: victims left active

    /// A victim whose account revoked the Registry's authorization is never retired: it kept every bond in and the
    /// leftover locked. Settled, it no longer counts as open, nor its outstanding as locked.
    function test_R20_anUnauthorizedVictimNoLongerKeepsTheBondsIn() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), false);
        // F2: the record's signers 0, 2 and 4 are slashed; signers 1 and 3 keep their bonds.
        bytes32 caseId =
            _prove(_journal(ViolationKind.WithheldChange, ANCHOR, c.payoutRef, c.changeTag, c.changeValue, 0, 0));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        slasher.proveInclusionByCase(caseId, _one(claimId)); // the payout landed (F2 shows its block)
        compensator.tallyVault(vaultId);
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        compensator.finalizeIncident(compensator.currentIncident());
        uint64 outstanding = _outstanding(vaultId);
        uint256 locked = registry.lockedZatoshi();

        vm.expectEmit(address(registry));
        emit IZecuredPayoutsEvents.VaultCompensated(vaultId, outstanding);
        compensator.settleVault(vaultId);
        assertEq(uint8(registry.vaultDetails(vaultId).status), uint8(VaultStatus.Active));
        assertTrue(_payouts().vaultCompensated(vaultId));
        assertEq(registry.openVaultCount(), 0, "settled: no longer open");
        assertEq(registry.lockedZatoshi(), locked - outstanding, "nor locked");
        assertEq(registry.lastVaultSettledAt(), block.timestamp);

        // The signers the slash did not name leave in full, and the leftover goes to the sink.
        vm.warp(block.timestamp + WINDOW);
        vm.prank(signerSet[1]);
        bond.requestUnbond(MIN_BOND);
        assertGt(compensator.sweepLeftover(), 0);
    }

    /// A victim whose debt the bonds covered only in part stays active with the rest of its debt; once that is repaid,
    /// anyone retires it, and nothing about it counts as open or locked on the way.
    function test_R20_aPartlyCoveredVictimIsRetiredOnceItsDebtIsGone() public {
        address big = makeAddr("v4.big");
        bytes32 vaultId = _attest(big, 100e8); // 100 ZEC: $4,000
        _authorizeBoth(big);
        _borrow(big, 2_000 * USDC); // 50 %, beyond what the 450 USDC of slashed bonds cover
        _compromise(_depositTagFor(big));
        assertLt(_outstanding(vaultId), 100e8, "burned to the LLTV at the compromise");
        compensator.tallyVault(vaultId);
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        compensator.finalizeIncident(compensator.currentIncident());
        compensator.settleVault(vaultId);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "partly covered: still active");
        assertTrue(_payouts().vaultCompensated(vaultId));
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lockedZatoshi(), 0);

        _repayAll(big);
        vm.prank(stranger);
        uint64 retired = compensator.burnCollateral(vaultId);
        assertGt(retired, 0);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed), "retired");
        assertEq(zzec.totalSupply(), 0);
        assertEq(registry.openVaultCount(), 0, "not counted twice");
        assertEq(registry.lockedZatoshi(), 0);
    }

    // ================================================================== R-21: abandoned refunds

    /// An abandoned refund of a compromised vault could never be compensated (nor revived there): it is tallied on
    /// request until the incident is finalized, without holding up the vault's tally.
    function test_R21_anAbandonedRefundIsTallied() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 refundId = intake.harnessRefund(vaultId, account, VAULT_001, keccak256("v4.refund"));
        vm.warp(block.timestamp + ABANDON_PERIOD);
        registry.abandonRefund(refundId);
        _compromise(_depositTagFor(account));
        compensator.tallyClaim(refundId);
        assertEq(uint8(_claimStatus(refundId)), uint8(ClaimStatus.Compensated));
        assertEq(compensator.entitlement(refundId).zat, VAULT_001);
        assertEq(compensator.entitlement(refundId).to, account);
        compensator.tallyVault(vaultId);
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        compensator.finalizeIncident(compensator.currentIncident());
        assertEq(compensator.withdrawEntitlement(refundId), uint256(VAULT_001) * PRICE_40 / 1e36);
    }

    function test_R21_onlyUntilTheIncidentIsFinalized() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 refundId = intake.harnessRefund(vaultId, account, VAULT_001, keccak256("v4.refund"));
        vm.warp(block.timestamp + ABANDON_PERIOD);
        registry.abandonRefund(refundId);
        _compromise(_depositTagFor(account));
        compensator.tallyVault(vaultId); // does not wait for it
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        uint256 id = compensator.currentIncident();
        compensator.finalizeIncident(id);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadyFinalized.selector, id));
        compensator.tallyClaim(refundId);
    }
}
