// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes, IZecuredDisputesEvents} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredSlasher, ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {IZecuredCompensator} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredSpendLogic} from "../../src/registry/ZecuredSpendLogic.sol";
import {Tier3Base} from "../utils/Tier3Base.sol";

/// The dispute path (design notes T-05 (a)) on the real Registry, spend logic, slasher, bond and compensator, with the
/// MOCK verifier and the MOCK relay: a claimant disputes the signers' confirmation that its claim was paid; the signers
/// prove that the payout is in a Zcash block (the violation guest's `Included` kind), or the claim is payable again.
contract DisputesTest is Tier3Base {
    uint256 internal constant INCLUDED = 5;

    address internal account = makeAddr("zecured.disputes.account");
    IZecuredDisputes internal disputes;
    bytes32 internal vaultId;
    bytes32 internal claimId;
    bytes32 internal payoutRef;
    uint256 internal paidAt;

    /// A vault released in full; its payout committed and, an hour later, confirmed: the vault has settled.
    function setUp() public override {
        super.setUp();
        disputes = IZecuredDisputes(address(registry));
        vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        payoutRef = _commitNext(vaultId, _one(claimId)).payoutRef;
        vm.warp(block.timestamp + 1 hours);
        _confirm(vaultId, _one(claimId));
        paidAt = block.timestamp;
    }

    // ------------------------------------------------------------------ helpers

    /// The journal of the violation guest's `Included` kind: kind 5, the anchor, X's payoutRef, every other word zero.
    function _included(bytes32 anchor, bytes32 ref) internal pure returns (bytes memory) {
        return abi.encode(INCLUDED, anchor, uint256(0), ref, bytes32(0), uint256(0), bytes32(0), uint256(0));
    }

    function _seal(bytes memory journal) internal view returns (bytes memory) {
        return verifier.mockSeal(VIOLATION_IMAGE, sha256(journal));
    }

    function _proveIncludedExpectRevert(bytes memory journal, bytes32 id, bytes memory err) internal {
        bytes memory seal = _seal(journal);
        vm.expectRevert(err);
        slasher.proveInclusion(journal, seal, id);
    }

    function _dispute() internal {
        vm.prank(account);
        disputes.disputePayout(claimId);
    }

    function _claimStatus(bytes32 id) internal view returns (ClaimStatus) {
        return registry.claimDetails(id).status;
    }

    /// A second vault, released and committed (not confirmed): its claim and payout reference.
    function _otherCommitted() internal returns (bytes32 otherClaim, bytes32 otherRef) {
        address other = makeAddr("zecured.disputes.other");
        bytes32 otherVault = _attest(other, VAULT_001);
        _authorizeBoth(other);
        vm.prank(other);
        otherClaim = registry.requestRelease(otherVault, RELEASE_COMMITMENT);
        otherRef = _commitNext(otherVault, _one(otherClaim)).payoutRef;
    }

    // ------------------------------------------------------------------ disputing

    function test_dispute_makesTheClaimUnpaidAgain() public {
        assertEq(disputes.disputeWindow(), DISPUTE_WINDOW);
        assertEq(disputes.inclusionWindow(), INCLUSION_WINDOW);
        assertEq(registry.openVaultCount(), 0, "the vault settled with its payout");
        assertEq(registry.lastVaultSettledAt(), paidAt);
        uint256 locked = registry.lockedZatoshi();
        (uint64 confirmedAt, uint64 dueAt) = disputes.payoutDispute(claimId);
        assertEq(confirmedAt, paidAt);
        assertEq(dueAt, 0);

        uint64 due = uint64(block.timestamp) + INCLUSION_WINDOW;
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutDisputed(vaultId, claimId, due);
        _dispute();

        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Disputed));
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 1);
        assertEq(registry.openVaultCount(), 1, "the vault is open again");
        assertEq(registry.lockedZatoshi(), locked + VAULT_005, "its claim counts as locked again");
        (confirmedAt, dueAt) = disputes.payoutDispute(claimId);
        assertEq(confirmedAt, 0);
        assertEq(dueAt, due);

        // Nothing confirms or commits it meanwhile, and no bond leaves in full.
        bytes[] memory sigs = _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, claimId));
        registry.confirmPayout(vaultId, _one(claimId), sigs);
        SpendCommit memory c =
            _spendCommit(vaultId, _depositTagFor(account), _one(claimId), keccak256("zecured.disputes.meanwhile"));
        sigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, claimId));
        registry.commitSpend(c, sigs);
        vm.prank(signerSet[0]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 1, 0, uint64(paidAt) + WINDOW));
        bond.requestUnbond(MIN_BOND);
    }

    function test_dispute_reverts() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotClaimant.selector, claimId, stranger));
        disputes.disputePayout(claimId);

        bytes32 unknown = keccak256("zecured.disputes.unknown");
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, unknown));
        disputes.disputePayout(unknown);

        (bytes32 pendingClaim,) = _otherCommitted();
        vm.prank(makeAddr("zecured.disputes.other"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, pendingClaim));
        disputes.disputePayout(pendingClaim);

        uint64 closedAt = uint64(paidAt) + DISPUTE_WINDOW;
        vm.warp(closedAt + 1);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.DisputeWindowClosed.selector, claimId, closedAt));
        disputes.disputePayout(claimId);

        // The window's last second still counts; a claim is disputed once per confirmation.
        vm.warp(closedAt);
        _dispute();
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, claimId));
        disputes.disputePayout(claimId);
    }

    function test_dispute_refusedOnACompromisedVault() public {
        bytes memory theft = _journal(
            ViolationKind.Uncommitted,
            ANCHOR,
            keccak256("zecured.disputes.theft"),
            _depositTagFor(account),
            0,
            bytes32(0),
            0
        );
        bytes32 caseId = _prove(theft);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(slasher.isCompromised(vaultId));

        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.DisputeVaultCompromised.selector, vaultId));
        disputes.disputePayout(claimId);
    }

    function test_spendLogic_refusesDirectCalls_andZeroWindows() public {
        vm.expectRevert(ZecuredSpendLogic.NotDelegated.selector);
        spendLogic.disputePayout(claimId);
        vm.expectRevert(ZecuredSpendLogic.NotDelegated.selector);
        spendLogic.upholdPayout(claimId, payoutRef);
        vm.expectRevert(ZecuredSpendLogic.NotDelegated.selector);
        spendLogic.reopenDisputedClaim(claimId);

        vm.expectRevert(ZecuredSpendLogic.InvalidDisputeWindows.selector);
        new ZecuredSpendLogic(address(registry), signerSet, 0, INCLUSION_WINDOW);
        vm.expectRevert(ZecuredSpendLogic.InvalidDisputeWindows.selector);
        new ZecuredSpendLogic(address(registry), signerSet, DISPUTE_WINDOW, 0);
    }

    function test_registryFallback_refusesUnknownCallsAndEther() public {
        (bool ok, bytes memory ret) = address(registry).call(abi.encodeWithSignature("noSuchFunction()"));
        assertFalse(ok);
        assertEq(ret.length, 0);

        vm.deal(stranger, 1 ether);
        vm.prank(stranger);
        (ok,) = address(registry).call{value: 1}("");
        assertFalse(ok, "the Registry never receives ETH");
        vm.prank(account);
        (ok,) = address(registry).call{value: 0}(abi.encodeCall(IZecuredDisputes.disputePayout, (claimId)));
        assertTrue(ok, "the dispute path is served through the fallback");
    }

    // ------------------------------------------------------------------ upholding

    function test_inclusionProof_upholdsTheConfirmation() public {
        _dispute();
        vm.warp(block.timestamp + 1 hours);
        bytes memory journal = _included(ANCHOR, payoutRef);
        bytes memory seal = _seal(journal);

        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.PayoutInclusionProven(claimId, payoutRef, ANCHOR);
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutUpheld(vaultId, claimId, payoutRef);
        vm.prank(stranger); // anyone submits the proof
        slasher.proveInclusion(journal, seal, claimId);

        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        assertEq(registry.openVaultCount(), 0, "settled again");
        assertEq(registry.lastVaultSettledAt(), block.timestamp);
        (uint64 confirmedAt, uint64 dueAt) = disputes.payoutDispute(claimId);
        assertEq(confirmedAt, 0);
        assertEq(dueAt, 0);

        // Upheld for good: no second dispute, no second proof.
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, claimId));
        disputes.disputePayout(claimId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputed.selector, claimId));
        slasher.proveInclusion(journal, seal, claimId);
    }

    function test_proveInclusion_reverts() public {
        _dispute();
        bytes32 zero;
        // Not an `Included` journal: another kind, a beneficiary, no payout, or any other word set.
        bytes memory notIncluded = abi.encodeWithSelector(IZecuredSlasher.NotAnInclusion.selector);
        _proveIncludedExpectRevert(
            abi.encode(uint256(1), ANCHOR, uint256(0), payoutRef, zero, uint256(0), zero, uint256(0)),
            claimId,
            notIncluded
        );
        _proveIncludedExpectRevert(
            abi.encode(INCLUDED, ANCHOR, uint256(uint160(watcher)), payoutRef, zero, uint256(0), zero, uint256(0)),
            claimId,
            notIncluded
        );
        _proveIncludedExpectRevert(_included(ANCHOR, zero), claimId, notIncluded);
        _proveIncludedExpectRevert(
            abi.encode(INCLUDED, ANCHOR, uint256(0), payoutRef, payoutRef, uint256(0), zero, uint256(0)),
            claimId,
            notIncluded
        );
        _proveIncludedExpectRevert(
            abi.encode(INCLUDED, ANCHOR, uint256(0), payoutRef, zero, uint256(0), zero, uint256(1)),
            claimId,
            notIncluded
        );
        bytes memory short = new bytes(7 * 32);
        _proveIncludedExpectRevert(
            short, claimId, abi.encodeWithSelector(IZecuredSlasher.InvalidJournalLength.selector, 7 * 32)
        );

        // The anchor needs the slash's confirmation work on the relay's best chain.
        bytes32 weak = keccak256("zecured.disputes.weak");
        relay.setBlock(weak, true, SLASH_WORK - 1);
        _proveIncludedExpectRevert(
            _included(weak, payoutRef),
            claimId,
            abi.encodeWithSelector(IZecuredSlasher.AnchorNotConfirmed.selector, weak, SLASH_WORK - 1)
        );
        bytes32 orphan = keccak256("zecured.disputes.orphan");
        relay.setBlock(orphan, false, SLASH_WORK);
        _proveIncludedExpectRevert(
            _included(orphan, payoutRef),
            claimId,
            abi.encodeWithSelector(IZecuredSlasher.AnchorNotConfirmed.selector, orphan, 0) // no work off the best chain
        );

        // The seal: the pinned selector, of this journal, for the violation image.
        bytes memory good = _included(ANCHOR, payoutRef);
        bytes memory badSelector = abi.encodePacked(bytes4(0xdeadbeef), new bytes(32));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.InvalidSealSelector.selector, bytes4(0xdeadbeef)));
        slasher.proveInclusion(good, badSelector, claimId);
        bytes memory depositSeal = verifier.mockSeal(DEPOSIT_IMAGE, sha256(good));
        vm.expectRevert();
        slasher.proveInclusion(good, depositSeal, claimId);

        // A payout that does not pay this claim: never committed, or committed for another input.
        bytes32 uncommitted = keccak256("zecured.disputes.uncommitted");
        _proveIncludedExpectRevert(
            _included(ANCHOR, uncommitted),
            claimId,
            abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, claimId, uncommitted)
        );
        (bytes32 otherClaim, bytes32 otherRef) = _otherCommitted();
        _proveIncludedExpectRevert(
            _included(ANCHOR, otherRef),
            claimId,
            abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, claimId, otherRef)
        );
        // A claim nobody disputes.
        _proveIncludedExpectRevert(
            _included(ANCHOR, otherRef),
            otherClaim,
            abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputed.selector, otherClaim)
        );
        // Only the slasher upholds.
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.OnlySlasher.selector, stranger));
        disputes.upholdPayout(claimId, payoutRef);

        // The right proof still works.
        slasher.proveInclusion(good, _seal(good), claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
    }

    // ------------------------------------------------------------------ reopening

    function test_reopen_afterTheInclusionWindow_thenPaidAgain() public {
        _dispute();
        uint64 dueAt = uint64(block.timestamp) + INCLUSION_WINDOW;
        vm.warp(dueAt);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.InclusionWindowOpen.selector, claimId, dueAt));
        disputes.reopenDisputedClaim(claimId);

        vm.warp(dueAt + 1);
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.DisputedClaimReopened(vaultId, claimId);
        vm.prank(stranger); // anyone
        disputes.reopenDisputedClaim(claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Pending));
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 1, "still unpaid");
        assertEq(registry.openVaultCount(), 1);
        (uint64 confirmedAt, uint64 due) = disputes.payoutDispute(claimId);
        assertEq(confirmedAt, 0);
        assertEq(due, 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputed.selector, claimId));
        disputes.reopenDisputedClaim(claimId);
        // A late proof upholds nothing any more.
        _proveIncludedExpectRevert(
            _included(ANCHOR, payoutRef),
            claimId,
            abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputed.selector, claimId)
        );

        // Payable again: a new payout of the same input and claims, confirmed, which its claimant may dispute again.
        SpendCommit memory again =
            _spendCommit(vaultId, _depositTagFor(account), _one(claimId), keccak256("zecured.disputes.again"));
        _send(again);
        _confirm(vaultId, _one(claimId));
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(registry.openVaultCount(), 0);
        (confirmedAt,) = disputes.payoutDispute(claimId);
        assertEq(confirmedAt, block.timestamp);
        _dispute();
        // Any committed payout of the claim's input upholds it, the first one included.
        bytes memory journal = _included(ANCHOR, payoutRef);
        slasher.proveInclusion(journal, _seal(journal), claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
    }

    function test_reopenedClaim_isPaidByTheBonds_onItsOriginalSchedule() public {
        uint256 createdAt = registry.claimDetails(claimId).createdAt;
        _dispute();
        vm.warp(block.timestamp + INCLUSION_WINDOW + 1);
        disputes.reopenDisputedClaim(claimId);

        (uint64 dueAt, uint256 amount) = bond.livenessDue(claimId);
        assertEq(dueAt, createdAt + LIVENESS_PERIOD, "a false confirmation never restarts the liveness clock");
        assertGt(amount, 0);
        vm.warp(dueAt);
        uint256 before = usdc.balanceOf(account);
        vm.prank(account);
        assertEq(bond.slashLiveness(claimId), amount);
        assertEq(usdc.balanceOf(account), before + amount);
        assertTrue(registry.claimDetails(claimId).bondPaid);

        // Its ZEC now reimburses the bonds; that payout's confirmation is not the claimant's to dispute.
        bytes32 dest = keccak256("zecured.disputes.reimburse");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        registry.setReimburseDestination(
            claimId, dest, deadline, _threeSigs(registry.hashReimburseDestination(claimId, dest, deadline))
        );
        _send(_spendCommit(vaultId, _depositTagFor(account), _one(claimId), keccak256("zecured.disputes.reimburse")));
        _confirm(vaultId, _one(claimId));
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, claimId));
        disputes.disputePayout(claimId);
    }

    /// A dispute open when the vault is compromised: the compensator waits for it, then tallies the reopened claim.
    function test_disputeOpenAtCompromise_isTalliedOnceReopened() public {
        _dispute();
        bytes memory theft = _journal(
            ViolationKind.Uncommitted,
            ANCHOR,
            keccak256("zecured.disputes.theft"),
            _depositTagFor(account),
            0,
            bytes32(0),
            0
        );
        bytes32 caseId = _prove(theft);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(slasher.isCompromised(vaultId));

        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ClaimsNotTallied.selector, vaultId, uint64(1)));
        compensator.tallyVault(vaultId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);

        disputes.reopenDisputedClaim(claimId); // the inclusion window ended during the challenge window
        compensator.tallyClaim(claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated));
        compensator.tallyVault(vaultId);
        assertTrue(compensator.victim(vaultId).tallied);
        assertEq(registry.openVaultCount(), 0);
    }
}
