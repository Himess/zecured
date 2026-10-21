// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredSignerErrors} from "../../src/interfaces/v3/IZecuredSignerErrors.sol";
import {ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredIntake, PendingActivation} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {ISettlementAdapter, PreLiquidationTerms} from "../../src/interfaces/ISettlementAdapter.sol";
import {CloseRequest, SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {V4PayBase} from "./V4PayRegressions.t.sol";
import {PreLiquidationBase} from "./PreLiquidation.t.sol";

/// Helpers of the v4.1 suites: deposit versions, pending activations, a vault closed from its collateral.
abstract contract V41Base is V4PayBase {
    using MorphoBalancesLib for IMorpho;

    address internal victim = makeAddr("v41.victim");

    /// Five signers schedule a deposit version of `image` (the fixture's MOCK selector); it is active after the delay.
    function _addDepositVersion(bytes32 image) internal returns (bytes32 id) {
        uint32[4] memory none;
        uint256 nonce = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes4 sel = proofs.sealSelector;
        bytes[] memory sigs =
            _signerSigs(versions.hashScheduleVersion(VersionKind.Deposit, sel, image, none, nonce, deadline), _all5());
        id = versions.scheduleVersion(VersionKind.Deposit, sel, image, none, nonce, deadline, sigs);
        vm.warp(block.timestamp + VERSION_DELAY);
    }

    /// Three signers remove a version.
    function _removeVersion(bytes32 id) internal {
        uint256 nonce = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        versions.removeVersion(id, nonce, deadline, _threeSigs(versions.hashRemoveVersion(id, nonce, deadline)));
    }

    function _depositVersionId() internal view returns (bytes32) {
        return versions.versionIdOf(VersionKind.Deposit, proofs.sealSelector, proofs.imageId);
    }

    /// Lets the MOCK relay and time reach a pending activation's readiness (K3).
    function _ready(bytes32 vaultId) internal {
        PendingActivation memory p = intake.pendingActivation(vaultId);
        if (relay.tipHeight() < p.readyHeight) relay.setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
    }

    /// Three signers give the bond-paid claim's reimbursement destination.
    function _reimburse(bytes32 claimId, bytes32 dest) internal {
        uint64 deadline = uint64(block.timestamp + 1 hours);
        registry.setReimburseDestination(
            claimId, dest, deadline, _threeSigs(registry.hashReimburseDestination(claimId, dest, deadline))
        );
    }

    /// The claim's claimant takes it from the bonds once due (armed with signer 0's receipt when it was created).
    function _livenessPaid(bytes32 claimId) internal {
        _armLivenessAt(claimId, uint64(block.timestamp));
        vm.warp(block.timestamp + LIVENESS_PERIOD);
        vm.prank(registry.claimDetails(claimId).claimant);
        bond.slashLiveness(claimId);
    }

    /// A 0.01 ZEC vault with a 40 % loan, closed from its collateral by the market maker: CloseUser `u` and
    /// CloseMarketMaker `m`, both pending.
    function _closedVault(address owner, uint256 ownerKey) internal returns (bytes32 vaultId, bytes32 u, bytes32 m) {
        vaultId = _attest(owner, VAULT_001);
        _authorizeBoth(owner);
        uint256 value = uint256(VAULT_001) * PRICE_40 / ORACLE_PRICE_SCALE;
        _borrow(owner, value * 4_000 / 10_000);
        uint256 debt = MORPHO.expectedBorrowAssets(params, owner);
        _fundAndApproveAdapter(marketMaker, debt);
        CloseRequest memory req = _closeRequest(vaultId, owner, 100, 0, 1);
        bytes memory sig = _sign(ownerKey, adapter.hashCloseRequest(req));
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        (u, m) = (registry.claimIdOf(vaultId, 0), registry.claimIdOf(vaultId, 1));
    }

    /// The F4 journal of the claim of `c` whose recipient is `dest`: its claimant looks for X under `trueRef`, the
    /// keccak256(salt || txid_X) it computes, not under the reference the commitment recorded.
    function _f4(SpendCommit memory c, bytes32 dest, bytes32 trueRef) internal view returns (bytes memory) {
        return _journal(ViolationKind.WrongClaimRef, ANCHOR, c.payoutRef, trueRef, 0, dest, 0);
    }

    function _f3(SpendCommit memory c, uint64 received) internal view returns (bytes memory) {
        return _journal(ViolationKind.Misdirected, ANCHOR, c.claimRefs[0], bytes32(0), received, RELEASE_COMMITMENT, 0);
    }
}

/// @notice The independent v4 review's findings on the contracts (W-1 to W-6, W-13 to W-16), each reproduced as it was
///         found: every test here failed on the v4 code. Real Registry, spend logic, slasher, bond and compensator,
///         MOCK verifier and relay, real Morpho Blue.
contract V41RegressionsTest is V41Base {
    // ================================================================== W-1: the claim an F4 hid

    /// The colluders committed X with the claim under a reference its claimant never computes, then paid its output to
    /// themselves and returned the change: no F1, F2 or F3. F4 slashed them but registered nothing for the claim, and
    /// the routine proof of X then made the claim final: nothing was left for its claimant.
    function test_W1_F4CompensatesAPendingHiddenClaim() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _armLivenessAt(claimId, uint64(block.timestamp));
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        bytes32 caseId = _prove(_f4(c, RELEASE_COMMITMENT, keccak256("w1.trueRef")));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);

        Entitlement memory e = compensator.entitlement(claimId);
        assertEq(e.zat, expected, "the hidden claim is compensated in full");
        assertEq(e.layer, 4, "the owner's own claim");
        assertEq(e.to, account);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated), "closed in the Registry");
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
        vm.warp(block.timestamp + LIVENESS_PERIOD + COMMIT_GRACE);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);
        assertEq(slasher.proveInclusionByCase(caseId, _one(claimId)), 0, "the case no longer proves it paid");
    }

    /// The colluders, who know K, prove X's inclusion first: the claim is final, and the F4 still compensates it.
    function test_W1_F4AfterTheColludersProvedInclusion() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _upholdWith(c.payoutRef, claimId);
        assertEq(_paidAt(claimId), 0, "final");
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        bytes32 caseId = _prove(_f4(c, RELEASE_COMMITMENT, keccak256("w1.trueRef")));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(compensator.entitlement(claimId).zat, expected, "compensated although its payment was final");
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Paid), "nothing more is owed on Zcash");
    }

    /// A closed vault: the colluders take its whole deposit with X (F1), then, within the challenge window, commit X
    /// with references its claimants never compute (a frozen vault still takes commitments). F1 is dismissed (X is
    /// committed now), there is no change (no F2) and no claimant finds X (no F3): only the two F4s compensate.
    function test_W1_closingVaultCommittedDuringTheF1Window() public {
        (address owner, uint256 key) = makeAddrAndKey("w1.owner");
        (bytes32 vaultId, bytes32 u, bytes32 m) = _closedVault(owner, key);
        bytes32 t0 = _depositTagFor(owner);
        bytes32 xRef = keccak256("w1.x");
        bytes32 f1 = _prove(_journal(ViolationKind.Uncommitted, ANCHOR, xRef, t0, 0, bytes32(0), 0));
        assertTrue(slasher.vaultFrozen(vaultId));
        SpendCommit memory x = _spendCommit(vaultId, t0, _two(u, m), "w1.x.commit");
        x.payoutRef = xRef;
        _send(x);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(f1);
        assertEq(uint8(slasher.caseOf(f1).kind), uint8(ViolationKind.None), "F1 dismissed: X is committed now");
        assertFalse(slasher.isCompromised(vaultId));

        bytes32 fu =
            _prove(_journal(ViolationKind.WrongClaimRef, ANCHOR, xRef, keccak256("w1.u.true"), 0, USER_COMMITMENT, 0));
        bytes32 fm =
            _prove(_journal(ViolationKind.WrongClaimRef, ANCHOR, xRef, keccak256("w1.m.true"), 0, MM_COMMITMENT, 0));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(fu);
        slasher.executeSlash(fm);
        Entitlement memory eu = compensator.entitlement(u);
        Entitlement memory em = compensator.entitlement(m);
        assertEq(eu.layer, 4, "CloseUser: the owner's layer");
        assertEq(eu.to, owner);
        assertEq(eu.zat, registry.claimPayment(x.claimRefs[0]).expected);
        assertEq(em.layer, 2, "CloseMarketMaker: the third parties' layer");
        assertEq(em.to, marketMaker);
        assertEq(em.zat, registry.claimPayment(x.claimRefs[1]).expected);
        assertEq(uint8(_claimStatus(u)), uint8(ClaimStatus.Compensated));
        assertEq(uint8(_claimStatus(m)), uint8(ClaimStatus.Compensated));
    }

    /// The bonds paid the hidden claim's claimant first: the F4 registers nothing more for it, and closes it (its ZEC
    /// went with X, so no reimbursement can ever be paid on Zcash).
    function test_W1_aBondPaidHiddenClaimIsNotPaidTwice() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _livenessPaid(claimId);
        assertTrue(registry.claimDetails(claimId).bondPaid);
        bytes32 caseId = _prove(_f4(c, RELEASE_COMMITMENT, keccak256("w1.trueRef")));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(uint8(_claimStatus(claimId)), uint8(ClaimStatus.Compensated), "closed");
        assertEq(compensator.entitlement(claimId).incident, 0, "the bonds paid it: no entitlement as well");
    }

    // ================================================================== W-2: F1 on a usurped tag

    /// The colluders recorded the victim's deposit tag as their own vault's change, then spent the victim's note
    /// without a commitment: F1 on the tag is proven against their vault.
    function _usurped() internal returns (bytes32 va, bytes32 vb, bytes32 tag, bytes32 caseA) {
        va = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(va, 1_000_000, RELEASE_COMMITMENT);
        tag = _depositTagFor(victim);
        vb = _vaultIdFor(victim);
        SpendCommit memory squat = _spendCommit(va, _depositTagFor(account), _one(claimId), "w2.squat");
        squat.changeTag = tag;
        _send(squat);
        caseA = _prove(_journal(ViolationKind.Uncommitted, ANCHOR, keccak256("w2.theft"), tag, 0, bytes32(0), 0));
        assertEq(slasher.caseOf(caseA).vaultId, va);
    }

    /// The victim's deposit proof lands later (pending): it takes the tag over from the false change record.
    function _victimProof(bytes32 vb, bytes32 tag) internal {
        _proveDeposit(vb, victim, VAULT_001, tag, keccak256("w2.victimAnchor"));
        assertEq(registry.lineageOf(tag).vaultId, vb);
    }

    /// The case proven before the override executed on the colluders' vault and compensated it, while the victim's
    /// vault minted against its stolen note. It now moves to the victim's vault, with a new challenge window.
    function test_W2_anF1BeforeTheOverrideMovesToTheNewOwner() public {
        (bytes32 va, bytes32 vb, bytes32 tag, bytes32 caseA) = _usurped();
        _victimProof(vb, tag);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseA);
        assertFalse(slasher.isCompromised(va), "the colluders' vault is not the victim");
        assertFalse(slasher.vaultFrozen(va), "its freeze ended");
        assertTrue(slasher.vaultFrozen(vb), "the victim's vault is frozen by the moved case");
        assertEq(slasher.openCasesOf(vb), 1);
        assertEq(slasher.openF1CasesOf(tag), 1);

        bytes32 caseB = slasher.caseIdOf(ViolationKind.Uncommitted, keccak256("w2.theft"), tag, vb);
        assertEq(slasher.caseOf(caseB).vaultId, vb);
        assertEq(slasher.caseOf(caseB).anchor, ANCHOR, "the proof's own anchor");
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseB);
        assertTrue(slasher.isCompromised(vb));
        assertTrue(slasher.noteSpendProven(tag));
    }

    /// The case executed on the colluders' vault before the victim's proof landed: anyone moves it to the victim's
    /// vault once the tag is the victim's, and the victim's refund is compensated.
    function test_W2_anExecutedF1MovesToALaterOwner() public {
        (bytes32 va, bytes32 vb, bytes32 tag, bytes32 caseA) = _usurped();
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseA);
        assertTrue(slasher.isCompromised(va), "the tag was the colluders' then");
        _victimProof(vb, tag); // the bonds halted: the victim's deposit waits as a refund
        vm.prank(stranger);
        bytes32 caseB = slasher.followNote(caseA);
        assertEq(slasher.caseOf(caseB).vaultId, vb);
        assertTrue(slasher.vaultFrozen(vb));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseB);
        assertTrue(slasher.isCompromised(vb));
        bytes32 refundId = _finalizeRefund(tag);
        compensator.tallyClaim(refundId);
        assertEq(compensator.entitlement(refundId).to, victim);
        assertEq(compensator.entitlement(refundId).zat, VAULT_001);
    }

    /// With an F1 case on its note, the victim's pending activation neither mints nor is dropped while its proof
    /// stands (three signers removing its deposit version cannot clear it); it can still be taken back as a refund.
    function test_W2_noActivationOrDropWhileAnF1IsOnItsTag() public {
        (,, bytes32 tag,) = _usurped();
        bytes32 vb = _vaultIdFor(victim);
        _victimProof(vb, tag);
        _ready(vb);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoteSpendCase.selector, tag));
        intake.finalizeActivation(vb);

        uint256 snap = vm.snapshotState();
        vm.warp(intake.pendingActivation(vb).readyTime + PENDING_REFUND_DELAY);
        vm.prank(victim);
        intake.refundPending(vb);
        vm.revertToState(snap);

        _addDepositVersion(keccak256("w2.secondDepositImage"));
        _removeVersion(_depositVersionId());
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorStillOnBestChain.selector, vb));
        intake.dropPendingActivation(vb);
        relay.setBlock(keccak256("w2.victimAnchor"), false, 0);
        intake.dropPendingActivation(vb);
    }

    /// End to end: the stolen note never backs a mint; the case reaches the victim's vault, and the victim is paid
    /// from the bonds.
    function test_W2_aNoteProvenSpentNeverBacksAMint() public {
        (,, bytes32 tag, bytes32 caseA) = _usurped();
        bytes32 vb = _vaultIdFor(victim);
        _victimProof(vb, tag);
        _ready(vb);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoteSpendCase.selector, tag));
        intake.finalizeActivation(vb);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseA);
        bytes32 caseB = slasher.caseIdOf(ViolationKind.Uncommitted, keccak256("w2.theft"), tag, vb);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseB);
        assertTrue(slasher.noteSpendProven(tag));
        bytes32 refundId = intake.refundPending(vb); // halted: anyone converts it
        assertEq(_outstanding(vb), 0, "never minted");
        compensator.tallyClaim(refundId);
        assertEq(compensator.entitlement(refundId).to, victim);
        assertEq(compensator.entitlement(refundId).zat, VAULT_001);
        compensator.tallyVault(vb);
    }

    // ================================================================== W-3: a compromised vault's pending items

    /// The victim's deposit waits (proven with the first deposit version); a second deposit version works; the
    /// colluders spend the note, remove the first version, and the case executes.
    function _compromisedPending() internal returns (bytes32 vb, bytes32 anchor) {
        vb = _vaultIdFor(victim);
        anchor = keccak256("w3.anchor");
        _proveDeposit(vb, victim, VAULT_001, _depositTagFor(victim), anchor);
        bytes32 firstVersion = _depositVersionId();
        _addDepositVersion(keccak256("w3.secondDepositImage"));
        bytes32 caseId = _prove(
            _journal(ViolationKind.Uncommitted, ANCHOR, keccak256("w3.theft"), _depositTagFor(victim), 0, bytes32(0), 0)
        );
        _removeVersion(firstVersion);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(slasher.isCompromised(vb));
    }

    /// The execution cleared the vault's open cases, and the version was gone: anyone dropped the pending activation,
    /// which deleted the vault, so the compensator found nothing to tally.
    function test_W3_noDropOnACompromisedVaultWhileTheAnchorHolds() public {
        (bytes32 vb, bytes32 anchor) = _compromisedPending();
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorStillOnBestChain.selector, vb));
        intake.dropPendingActivation(vb);
        relay.setBlock(anchor, false, 0); // a reorganization took the deposit's block away: the item is lost
        intake.dropPendingActivation(vb);
    }

    /// The only exit that kept the note, `refundPending`, required the removed version.
    function test_W3_refundPendingAfterTheVersionWasRemoved() public {
        (bytes32 vb,) = _compromisedPending();
        _ready(vb);
        bytes32 refundId = intake.refundPending(vb);
        compensator.tallyClaim(refundId);
        assertEq(compensator.entitlement(refundId).to, victim);
        assertEq(compensator.entitlement(refundId).zat, VAULT_001);
        compensator.tallyVault(vb);
    }

    /// The same for a refund reserved before the compromise.
    function test_W3_aReservedRefundOpensAfterTheVersionWasRemoved() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 tag = keccak256("w3.refund");
        _mockProof(vaultId, account, VAULT_001, tag, keccak256("w3.refundAnchor")); // the vault is not new: a refund
        bytes32 firstVersion = _depositVersionId();
        _addDepositVersion(keccak256("w3.secondDepositImage"));
        _removeVersion(firstVersion);
        _compromise(_depositTagFor(account));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.RefundStillValid.selector, tag));
        intake.dropPendingRefund(tag);
        bytes32 refundId = _finalizeRefund(tag);
        compensator.tallyClaim(refundId);
        assertEq(compensator.entitlement(refundId).zat, VAULT_001);
        assertEq(compensator.entitlement(refundId).to, account);
    }

    // ================================================================== W-4: late arming

    /// The claimant kept an early receipt and armed it only once a late payout was committed: due at once, the bonds
    /// paid 105 % while the payout could still land. A commitment made before the arming is now protected in full.
    function test_W4_lateArmingProtectsAnEarlierCommitment() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        uint64 t0 = uint64(block.timestamp);
        bytes memory receipt = _receipt(claimId, t0, 0); // kept, not armed
        vm.warp(t0 + LIVENESS_PERIOD + COMMIT_GRACE + 1 hours); // an outage: the payout comes late
        _commitNext(vaultId, _one(claimId));
        uint64 committedAt = uint64(block.timestamp);
        bond.armLiveness(claimId, t0, receipt);
        vm.prank(account);
        vm.expectRevert(
            abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, claimId, committedAt + COMMIT_GRACE)
        );
        bond.slashLiveness(claimId);
        assertEq(bond.livenessArmedAt(claimId), committedAt);
        vm.warp(committedAt + COMMIT_GRACE);
        vm.prank(account);
        assertGt(bond.slashLiveness(claimId), 0, "due once that payout can no longer land");
    }

    /// Commitments after the arming are protected only up to the arming plus `commitGrace`.
    function test_W4_recommittingAfterArmingStaysBounded() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        uint64 t0 = uint64(block.timestamp);
        bytes memory receipt = _receipt(claimId, t0, 0);
        vm.warp(t0 + LIVENESS_PERIOD + COMMIT_GRACE + 1 hours);
        _commitNext(vaultId, _one(claimId));
        uint64 armedAt = uint64(block.timestamp);
        bond.armLiveness(claimId, t0, receipt);
        vm.warp(armedAt + COMMIT_GRACE / 2);
        _send(_spendCommit(vaultId, _depositTagFor(account), _one(claimId), "w4.again"));
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, claimId, armedAt + COMMIT_GRACE));
        bond.slashLiveness(claimId);
        vm.warp(armedAt + COMMIT_GRACE);
        vm.prank(account);
        assertGt(bond.slashLiveness(claimId), 0, "not postponed by the commitment after the arming");
    }

    /// The colluders hold the receipt key: re-arming with ever earlier receipts and committing again never pushes the
    /// due date past the first arming's bound, and a commitment made before the old deadline stays protected.
    function test_W4_loweringReArmsCannotPostponeLiveness() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        uint64 t0 = uint64(block.timestamp);
        bytes32 input = _depositTagFor(account);
        _send(_spendCommit(vaultId, input, _one(claimId), "w4.x0"));

        vm.warp(t0 + 10 hours);
        bond.armLiveness(claimId, t0 + 5 hours, _receipt(claimId, t0 + 5 hours, 0)); // deadline t0 + 29 h
        vm.warp(t0 + 20 hours);
        bond.armLiveness(claimId, t0 + 4 hours, _receipt(claimId, t0 + 4 hours, 1)); // deadline t0 + 28 h
        vm.warp(t0 + 28 hours + 30 minutes); // after the new deadline, before the old one
        _send(_spendCommit(vaultId, input, _one(claimId), "w4.x1"));
        vm.warp(t0 + 30 hours);
        vm.prank(account);
        vm.expectRevert(
            abi.encodeWithSelector(
                ISignerBond.LivenessNotDue.selector, claimId, t0 + 28 hours + 30 minutes + COMMIT_GRACE
            )
        );
        bond.slashLiveness(claimId);

        // More of the same: an earlier receipt and new commitments.
        bond.armLiveness(claimId, t0 + 3 hours, _receipt(claimId, t0 + 3 hours, 2));
        _send(_spendCommit(vaultId, input, _one(claimId), "w4.x2"));
        vm.warp(t0 + 30 hours + 45 minutes);
        _send(_spendCommit(vaultId, input, _one(claimId), "w4.x3"));
        vm.warp(t0 + 29 hours + COMMIT_GRACE); // the first arming's bound: its deadline plus the grace
        vm.prank(account);
        assertGt(bond.slashLiveness(claimId), 0, "never later than the first arming's bound");
    }

    /// The planners' deadline ignores what only delays the payment from the bonds (a freeze here).
    function test_W4_theDeadlineIgnoresAFreeze() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        uint64 t0 = uint64(block.timestamp);
        _armLivenessAt(claimId, t0);
        _proveTheft(c.changeTag);
        (uint64 dueAt,) = bond.livenessDue(claimId);
        assertEq(dueAt, 0, "not payable from the bonds while frozen");
        assertEq(bond.livenessDeadline(claimId), t0 + LIVENESS_PERIOD);
    }

    // ================================================================== W-5: liveness and claim cases

    /// With the liveness period equal to the challenge window, the claim fell due while its F3 was open: the bonds paid
    /// 105 % on top of what the claimant had received.
    function test_W5_noLivenessWhileAnF3IsOpen() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        uint64 t0 = uint64(block.timestamp);
        _armLivenessAt(claimId, t0);
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        vm.warp(t0 + 23 hours);
        bytes32 caseId = _prove(_f3(c, expected / 2));
        vm.warp(t0 + LIVENESS_PERIOD);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);
        (uint64 dueAt,) = bond.livenessDue(claimId);
        assertEq(dueAt, 0);
        assertTrue(slasher.claimCaseOpen(claimId));
        vm.warp(t0 + 23 hours + WINDOW);
        slasher.executeSlash(caseId);
        assertFalse(slasher.claimCaseOpen(claimId));
        assertEq(compensator.entitlement(claimId).zat, expected - expected / 2, "the shortfall, once");
    }

    /// The other order: the bonds paid first and cleared the claim's commitment, so the F3 could no longer be proven
    /// and nobody was slashed. F3 now compares the commitment recorded with the payout.
    function test_W5_anF3ProvenAfterTheBondsPaid() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _livenessPaid(claimId);
        uint64 expected = registry.claimPayment(c.claimRefs[0]).expected;
        bytes32 caseId = _prove(_f3(c, expected / 2));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(bond.bondOf(0), 0, "the commitment's signers are slashed");
        assertEq(bond.bondOf(2), 0);
        assertEq(bond.bondOf(4), 0);
        assertEq(compensator.entitlement(claimId).incident, 0, "the bonds paid the claimant already");
    }

    /// The same for an open F4.
    function test_W5_noLivenessWhileAnF4IsOpen() public {
        (, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        uint64 t0 = uint64(block.timestamp);
        _armLivenessAt(claimId, t0);
        vm.warp(t0 + 23 hours);
        _prove(_f4(c, RELEASE_COMMITMENT, keccak256("w5.trueRef")));
        vm.warp(t0 + LIVENESS_PERIOD);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);
        assertTrue(slasher.claimCaseOpen(claimId));
    }

    // ================================================================== W-6: the reimbursement takes three signers

    /// R-6 asked all five: one missing signer then froze every input bound to a bond-paid claim, and with it the rest
    /// of its vault's collateral.
    function test_W6_threeSignaturesSetTheReimbursement() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.prank(address(bond));
        registry.markBondPaid(claimId);
        bytes32 dest = keccak256("w6.reimburse");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes32 digest = registry.hashReimburseDestination(claimId, dest, deadline);
        bytes[] memory two = _signerSigs(digest, _idx(0, 2));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 2));
        registry.setReimburseDestination(claimId, dest, deadline, two);
        registry.setReimburseDestination(claimId, dest, deadline, _threeSigs(digest));
        (,,, bytes32 commitment,) = registry.claim(claimId);
        assertEq(commitment, dest);
    }

    /// A bond-paid claim's input could not be committed until its destination was given; with three signers it is, and
    /// the input's other claims are paid.
    function test_W6_theFrozenInputIsCommittableAgain() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.startPrank(account);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        vm.stopPrank();
        bytes32 t0 = _depositTagFor(account);
        _send(_spendCommit(vaultId, t0, _two(a, b), "w6.first")); // never lands
        _livenessPaid(a);
        SpendCommit memory again = _spendCommit(vaultId, t0, _two(a, b), "w6.again");
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(again));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimDestinationUnset.selector, a));
        registry.commitSpend(again, sigs);
        _reimburse(a, keccak256("w6.treasury.a"));
        _send(again);
        _confirmPayout(vaultId, again.payoutRef, _two(a, b));
        assertEq(uint8(_claimStatus(a)), uint8(ClaimStatus.Paid));
        assertEq(uint8(_claimStatus(b)), uint8(ClaimStatus.Paid));
    }

    /// Two bond-paid claims of one bound set get their own treasury destinations: one payout pays both.
    function test_W6_twoReimbursedClaimsInOneBoundSet() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.startPrank(account);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, USER_COMMITMENT);
        vm.stopPrank();
        bytes32 t0 = _depositTagFor(account);
        _send(_spendCommit(vaultId, t0, _two(a, b), "w6.first"));
        _armLivenessAt(a, uint64(block.timestamp));
        _armLivenessAt(b, uint64(block.timestamp));
        vm.warp(block.timestamp + LIVENESS_PERIOD);
        vm.startPrank(account);
        bond.slashLiveness(a);
        bond.slashLiveness(b);
        vm.stopPrank();
        _reimburse(a, keccak256("w6.treasury.a"));
        _reimburse(b, keccak256("w6.treasury.b"));
        SpendCommit memory again = _spendCommit(vaultId, t0, _two(a, b), "w6.again");
        _send(again);
        _confirmPayout(vaultId, again.payoutRef, _two(a, b));
        assertEq(uint8(_claimStatus(a)), uint8(ClaimStatus.Paid));
        assertEq(uint8(_claimStatus(b)), uint8(ClaimStatus.Paid));
    }

    // ================================================================== W-13: a stopped deposit verifier

    /// RISC Zero stopped the deposit verifier's selector: the version is still active (conversions and refunds keep
    /// working) but proves nothing new, so its pending activations no longer mint.
    function test_W13_aStoppedDepositVerifierNeverMints() public {
        bytes32 vb = _vaultIdFor(victim);
        _proveDeposit(vb, victim, VAULT_001, _depositTagFor(victim), keccak256("w13.anchor"));
        bytes32 id = _depositVersionId();
        verifier.stop();
        assertTrue(versions.isActive(id));
        assertFalse(versions.isWorking(id));
        _ready(vb);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.VersionNotWorking.selector, id));
        intake.finalizeActivation(vb);
        vm.warp(intake.pendingActivation(vb).readyTime + PENDING_REFUND_DELAY);
        vm.prank(victim);
        intake.refundPending(vb);
    }

    // ================================================================== W-14: full exits and pending refunds

    /// A vault that settled; its account proves a second deposit, which waits as a refund (the account has a vault).
    function _settledWithPendingRefund() internal {
        bytes32 vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        SpendCommit memory c = _commitNext(vaultId, _one(claimId));
        _upholdWith(c.payoutRef, claimId);
        assertEq(registry.openVaultCount(), 0);
        _mockProof(keccak256("w14.second"), account, VAULT_001, keccak256("w14.tag"), keccak256("w14.anchor"));
        assertEq(intake.pendingRefundTotal(), VAULT_001);
        vm.warp(block.timestamp + WINDOW);
    }

    /// A signer left in full while a refund waited for its relay: no vault was open, yet the refund's claim would open
    /// with nobody bonded for it.
    function test_W14_noFullExitWhileARefundIsPending() public {
        _settledWithPendingRefund();
        vm.prank(signerSet[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.PendingRefunds.selector, uint256(VAULT_001)));
        bond.requestUnbond(MIN_BOND);
    }

    /// The same for what arrives during the unbonding: a full exit is withdrawn only while no vault is open and no
    /// refund waits.
    function test_W14_aFullWithdrawalWaitsForWhatArrivedMeanwhile() public {
        vm.warp(block.timestamp + WINDOW);
        vm.prank(signerSet[1]);
        bond.requestUnbond(MIN_BOND);
        _mockProof(_vaultIdFor(victim), victim, VAULT_001, _depositTagFor(victim), keccak256("w14.newVault"));
        assertEq(registry.openVaultCount(), 1, "a pending vault counts as open");
        vm.warp(block.timestamp + UNBONDING_PERIOD);
        vm.prank(signerSet[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 1, 0, 0));
        bond.withdrawUnbonded(signerSet[1]);
    }

    // ================================================================== W-15: the emergency stop's shortcut

    /// Removing the stopped violation versions turned `violationStopped()` false: a new version then waited the long
    /// delay although no violation could be proven at all.
    function test_W15_removingStoppedVersionsKeepsTheShortcut() public {
        verifier.stop();
        assertTrue(versions.violationStopped());
        _removeVersion(versions.versionIdOf(VersionKind.Violation, proofs.sealSelector, violationImage));
        assertTrue(versions.violationStopped(), "still no working violation verifier");
    }

    // ================================================================== W-16: a retired victim's equity

    /// R-19 retired the victim at once (no debt); its account revoked the Registry's authorization later. The tally
    /// counted it as unauthorized, so its owner lost its equity, though nothing was left to withdraw.
    function test_W16_aRetiredVictimThatRevokedItsAuthorizationKeepsItsEquity() public {
        (bytes32 vaultId, bytes32 claimId, SpendCommit memory c) = _partialCommitted();
        _compromise(c.changeTag);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed), "retired at the compromise");
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), false);
        vm.warp(slasher.compromisedAt(vaultId) + INCLUSION_WINDOW);
        compensator.tallyClaim(claimId);
        compensator.tallyVault(vaultId);
        assertTrue(compensator.victim(vaultId).authorized, "nothing left to withdraw: counted as authorized");
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        compensator.finalizeIncident(compensator.currentIncident());
        compensator.settleVault(vaultId);
        assertGt(compensator.withdrawEquity(vaultId), 0);
    }
}

/// @notice W-6 with the opening fee: the fee claim and a reimbursed exit in one bound set. Reimbursing the exit to the
///         fee's own commitment would make the set impossible to commit (`DuplicateClaimCommitment`); each bond-paid
///         claim gets a treasury destination of its own.
contract V41FeeRegressionsTest is V41Base {
    bytes32 internal constant TREASURY = keccak256("v41.treasury");

    function _feeSetup() internal pure override returns (FeeSetup memory) {
        return FeeSetup({feeBps: 50, minFeeZat: 20_000, feeCommitment: TREASURY});
    }

    function test_W6_aFeeClaimAndAReimbursedClaimInOneBoundSet() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 releaseId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        bytes32 t0 = _depositTagFor(account);
        _send(_spendCommit(vaultId, t0, _two(releaseId, feeId), "w6.fee.first")); // never lands
        _livenessPaid(releaseId);
        _reimburse(releaseId, keccak256("w6.treasury.release"));
        SpendCommit memory again = _spendCommit(vaultId, t0, _two(releaseId, feeId), "w6.fee.again");
        _send(again);
        _confirmPayout(vaultId, again.payoutRef, _two(releaseId, feeId));
        assertEq(uint8(_claimStatus(releaseId)), uint8(ClaimStatus.Paid));
        assertEq(uint8(_claimStatus(feeId)), uint8(ClaimStatus.Paid));
    }
}

/// @notice W-17: the pre-liquidator names the least collateral it accepts, so that the price or the debt moving
///         between its quote and its transaction cannot make it repay for less.
contract V41PreLiquidationTest is PreLiquidationBase {
    function test_W17_aPreLiquidationBelowTheCallersMinimumReverts() public {
        _openLoan(account, VAULT_005, 1_000_000);
        PreLiquidationTerms memory t = _preset();
        _setTerms(account, t);
        oracle.setPrice(_priceForLtv(account, 0.6e18));
        Quote memory r = _quote(account, t);
        uint64 expectedSeized = uint64(_seizedFor(r, r.repayable));
        _fundAndApproveAdapter(preLiquidator, _toAssetsUp(r.repayable, r.totalBorrowAssets, r.totalBorrowShares));
        vm.prank(preLiquidator);
        vm.expectRevert(
            abi.encodeWithSelector(
                ISettlementAdapter.PreLiquidationSlippage.selector, expectedSeized, expectedSeized + 1
            )
        );
        adapter.preLiquidate(account, r.repayable, PRE_COMMITMENT, expectedSeized + 1);
        vm.prank(preLiquidator);
        (, uint64 seized) = adapter.preLiquidate(account, r.repayable, PRE_COMMITMENT, expectedSeized);
        assertEq(seized, expectedSeized);
    }
}
