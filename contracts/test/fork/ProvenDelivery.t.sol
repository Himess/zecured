// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredDisputes, IZecuredDisputesEvents} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredSlasher, ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredCompensator} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {IZecuredPayoutsEvents} from "../../src/interfaces/v3/IZecuredPayouts.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {V4PayBase} from "./V4PayRegressions.t.sol";

/// @notice Proven delivery (v4): a payout is final on Base once its inclusion in a Zcash block is proven (the violation
///         guest's `Included` kind, MOCK seals here), whatever state its claims are in before the compensator takes them
///         over; the signers' confirmation stays a fast, disputable step. Every transition of the claim state table and
///         every refusal, the batch proof, and the three orderings of a compromise and a proof.
contract ProvenDeliveryTest is V4PayBase {
    // ================================================================== Pending (committed) -> proof -> final

    function test_pendingCommitted_provenIsFinal() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        uint256 locked = registry.lockedZatoshi();
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.PayoutInclusionProven(claimId, c.payoutRef, ANCHOR);
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutProven(vaultId, claimId, c.payoutRef);
        vm.prank(stranger); // anyone submits the proof
        slasher.proveInclusion(journal, seal, violationImage, claimId);

        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        (uint64 paidAt, uint64 dueAt) = disputes.payoutDispute(claimId);
        assertEq(paidAt, 0, "final: never confirmed, never disputable");
        assertEq(dueAt, 0);
        assertEq(registry.lockedZatoshi(), locked - 1_000_000, "off the books");
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        assertEq(_payouts().unprovenConfirmations(vaultId), 0);
        // Nothing more: no confirmation, no dispute, no second proof, no liveness.
        bytes[] memory sigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, c.payoutRef, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredPayoutsEvents.ClaimAlreadyPaid.selector, claimId));
        _payouts().confirmPayout(vaultId, c.payoutRef, _one(claimId), sigs);
    }

    function test_pendingCommitted_provenOnAFrozenVault() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _proveTheft(c.changeTag);
        assertTrue(slasher.vaultFrozen(vaultId));
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(_paidAt(claimId), 0);
    }

    function test_pendingCommitted_provenOnACompromisedVault_beforeTheTally() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        assertTrue(slasher.isCompromised(vaultId));
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
    }

    // ================================================================== Paid (confirmed) -> proof / dispute

    function test_confirmed_isDisputableUntilProven() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        assertEq(_paidAt(claimId), block.timestamp, "confirmed by signatures: disputable");
        assertEq(_payouts().unprovenConfirmations(vaultId), 1);

        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutProven(vaultId, claimId, c.payoutRef);
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(_paidAt(claimId), 0, "proven: the dispute window closed");
        assertEq(_payouts().unprovenConfirmations(vaultId), 0);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotDisputable.selector, claimId));
        disputes.disputePayout(claimId);
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.AlreadyFinal.selector, claimId));
        slasher.proveInclusion(journal, seal, violationImage, claimId);
    }

    function test_confirmed_disputedThenProven() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        _disputeAs(account, claimId);
        assertEq(_payouts().unprovenConfirmations(vaultId), 0, "a dispute counts the claim unpaid instead");
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutUpheld(vaultId, claimId, c.payoutRef);
        _upholdWith(c.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
    }

    // ================================================================== Pending (reopened) -> proof

    function test_reopened_provenByAnyCommitmentOfItsInput() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory first) = _partialCommitted();
        _confirm(vaultId, _one(claimId));
        _disputeAs(account, claimId);
        vm.warp(block.timestamp + INCLUSION_WINDOW + 1);
        disputes.reopenDisputedClaim(claimId);
        // A new commitment of the same input (D57); either payout's proof pays the claim.
        SpendCommit memory again =
            _spendCommit(vaultId, _depositTagFor(account), _one(claimId), keccak256("v4.pd.again"));
        _send(again);
        uint256 snap = vm.snapshotState();
        _upholdWith(again.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
        vm.revertToState(snap);
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutUpheld(vaultId, claimId, first.payoutRef);
        _upholdWith(first.payoutRef, claimId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid));
    }

    // ================================================================== refusals

    function test_aProofOfAnotherPayoutPaysNothing() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        // A payout reference nobody committed.
        bytes32 unknown = keccak256("v4.pd.unknown");
        bytes memory journal = _included(ANCHOR, unknown);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, claimId, unknown));
        slasher.proveInclusion(journal, seal, violationImage, claimId);
        // Another input's commitment, of the same vault.
        vm.prank(account);
        bytes32 other = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        SpendCommit memory fromChange = _commitNext(vaultId, _one(other));
        journal = _included(ANCHOR, fromChange.payoutRef);
        seal = _seal(journal);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, claimId, fromChange.payoutRef)
        );
        slasher.proveInclusion(journal, seal, violationImage, claimId);
        // Another vault's commitment.
        address b = makeAddr("v4.pd.other");
        bytes32 vaultB = _attest(b, VAULT_001);
        _authorizeBoth(b);
        vm.prank(b);
        bytes32 claimB = registry.requestRelease(vaultB, RELEASE_COMMITMENT);
        SpendCommit memory payoutB = _commitNext(vaultB, _one(claimB));
        journal = _included(ANCHOR, payoutB.payoutRef);
        seal = _seal(journal);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, claimId, payoutB.payoutRef)
        );
        slasher.proveInclusion(journal, seal, violationImage, claimId);
        // The right one still works.
        _upholdWith(c.payoutRef, claimId);
    }

    function test_uncommittedOrClosedClaimsAreNotProvable() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.startPrank(account);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        vm.stopPrank();
        SpendCommit memory c = _commitNext(vaultId, _one(a));
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.PayoutNotOfClaim.selector, b, c.payoutRef));
        slasher.proveInclusion(journal, seal, violationImage, b); // never committed
        bytes32 unknown = keccak256("v4.pd.noClaim");
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotUpholdable.selector, unknown));
        slasher.proveInclusion(journal, seal, violationImage, unknown);

        // Compensated: the compensator took the claim over.
        _compromise(c.changeTag);
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        compensator.tallyClaim(a);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotUpholdable.selector, a));
        slasher.proveInclusion(journal, seal, violationImage, a);
    }

    /// The bonds never pay a final claim.
    function test_aFinalClaimHasNoLiveness() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _upholdWith(c.payoutRef, claimId);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(claimId);
        assertEq(dueAt, 0);
        assertEq(amount, 0);
        vm.warp(block.timestamp + LIVENESS_PERIOD + COMMIT_GRACE);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);
    }

    // ================================================================== the batch

    /// One verification for every claim of a payout; a claim someone proved first is skipped, never a revert.
    function test_batch_provesEveryClaimOfAPayout_skippingTheFinalOnes() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.startPrank(account);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        vm.stopPrank();
        SpendCommit memory c = _commitNext(vaultId, _two(a, b));
        _confirm(vaultId, _one(a)); // a is confirmed, b only committed
        // Someone front-runs the batch with a proof of a alone.
        _upholdWith(c.payoutRef, a);

        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        bytes32[] memory ids = new bytes32[](3);
        (ids[0], ids[1], ids[2]) = (a, b, keccak256("v4.pd.noClaim"));
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.PayoutInclusionProven(b, c.payoutRef, ANCHOR);
        assertEq(slasher.proveInclusionBatch(journal, seal, violationImage, ids), 1, "b only");
        assertEq(uint8(_claimStatus(b)), uint8(ClaimStatus.Paid));
        assertEq(_paidAt(b), 0);
        assertEq(slasher.proveInclusionBatch(journal, seal, violationImage, ids), 0, "nothing left");
    }

    /// The batch verifies its proof like a single proof: a bad seal, a weak anchor or another kind reverts it whole.
    function test_batch_verifiesItsProof() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory wrongSeal = _seal(_included(ANCHOR, keccak256("v4.pd.other")));
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        slasher.proveInclusionBatch(journal, wrongSeal, violationImage, _one(claimId));
        bytes32 weak = keccak256("v4.pd.weak");
        relay.setBlock(weak, true, SLASH_WORK - 1);
        bytes memory weakJournal = _included(weak, c.payoutRef);
        bytes memory weakSeal = _seal(weakJournal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.AnchorNotConfirmed.selector, weak, SLASH_WORK - 1));
        slasher.proveInclusionBatch(weakJournal, weakSeal, violationImage, _one(claimId));
        bytes memory notIncluded = _journal(ViolationKind.Uncommitted, ANCHOR, c.payoutRef, c.changeTag, 0, 0, 0);
        bytes memory notIncludedSeal = _seal(notIncluded);
        vm.expectRevert(IZecuredSlasher.NotAnInclusion.selector);
        slasher.proveInclusionBatch(notIncluded, notIncludedSeal, violationImage, _one(claimId));
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Pending));
    }

    // ================================================================== compromise and proof, three orderings (R-2, R-3)

    /// Proof, then compromise: the claim was paid for good before; the compensator owes nothing for it.
    function test_order_proofThenCompromise() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _upholdWith(c.payoutRef, claimId);
        _compromise(c.changeTag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);
        compensator.tallyVault(vaultId);
    }

    /// Compromise, then proof within the proof window: final, no entitlement (the R-2 regression's case).
    function test_order_compromiseThenProof() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        vm.warp(block.timestamp + INCLUSION_WINDOW - 1);
        _upholdWith(c.payoutRef, claimId);
        vm.warp(block.timestamp + 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, claimId));
        compensator.tallyClaim(claimId);
        compensator.tallyVault(vaultId);
        assertEq(compensator.entitlement(claimId).incident, 0);
    }

    /// Compromise, tally after the window, then a proof: too late, the compensator owes the claim (and pays it).
    function test_order_compromiseTallyThenProof() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        compensator.tallyClaim(claimId);
        bytes memory journal = _included(ANCHOR, c.payoutRef);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredDisputesEvents.NotUpholdable.selector, claimId));
        slasher.proveInclusion(journal, seal, violationImage, claimId);
        assertEq(compensator.entitlement(claimId).zat, 1_000_000);
        compensator.tallyVault(vaultId);
    }
}
