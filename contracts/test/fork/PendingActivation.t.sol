// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IZecuredRegistry, VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake, PendingActivation, RefundReason} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {LineageNote} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice K3 (a proven deposit only mints once the relay has moved on with the proof's anchor still on its best chain,
///         and is dropped if the anchor leaves it), E2, and what a halt or a pause does to a pending activation (T-02,
///         T-03), driven through a MOCK relay whose best chain the test sets (real Equihash headers cannot fork in a
///         test; the real relay path is in DepositProofs.t.sol), the MOCK verifier, and a MOCK bond and slasher.
contract PendingActivationTest is ZecuredFixture {
    bytes32 internal constant IMAGE_ID = keccak256("zecured.test.pending.imageId");
    uint256 internal constant TIP_HEIGHT = 4_500_000;

    bytes32 internal constant ANCHOR = keccak256("zecured.test.pending.anchor");
    bytes32 internal constant ANCHOR_2 = keccak256("zecured.test.pending.anchor2");
    bytes32 internal constant VAULT = keccak256("zecured.test.pending.vault");
    bytes32 internal constant TAG = keccak256("zecured.test.pending.tag");
    bytes32 internal constant OPENED = keccak256("zecured.test.pending.vaultOpened");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    MockSignerBond internal mockBond;
    address internal account = makeAddr("zecured.test.pending.account");

    function _proofSetup() internal override returns (ProofSetup memory) {
        relay = new MockHeaderRelay();
        verifier = new MockRiscZeroVerifier();
        return ProofSetup({
            relay: address(relay),
            verifier: address(verifier),
            imageId: IMAGE_ID,
            sealSelector: verifier.SELECTOR(),
            minWork: MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function _mockBondAndSlasher() internal override returns (address) {
        mockBond = new MockSignerBond();
        return address(mockBond);
    }

    // ------------------------------------------------------------------ helpers

    function _journal(bytes32 anchor, bytes32 tag) internal view returns (bytes memory) {
        return abi.encode(anchor, VAULT, account, VAULT_001, tag, OPENED);
    }

    function _all5() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    /// The relay set up for a proof anchored at `anchor`, and the proof (journal, MOCK seal, the five signatures).
    function _proofOf(bytes32 anchor, bytes32 tag)
        internal
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        relay.setBlock(anchor, true, MIN_WORK);
        relay.setTipHeight(TIP_HEIGHT);
        journal = _journal(anchor, tag);
        seal = verifier.mockSeal(IMAGE_ID, sha256(journal));
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), _all5());
    }

    function _proveTag(bytes32 anchor, bytes32 tag) internal {
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(anchor, tag);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function _prove(bytes32 anchor) internal {
        _proveTag(anchor, TAG);
    }

    function _ready() internal {
        relay.setTipHeight(TIP_HEIGHT + FINALIZE_DEPTH);
        vm.warp(block.timestamp + FINALIZE_DELAY);
    }

    function _pendingAccount(bytes32 vaultId) internal view returns (address) {
        return intake.pendingActivation(vaultId).account;
    }

    function _pause(uint64 until) internal {
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        intake.pauseMinting(until, nonce, deadline, _threeSigs(intake.hashPauseMinting(until, nonce, deadline)));
    }

    // ------------------------------------------------------------------ Tier 1 is gone

    /// Signer-attested minting does not exist in v3: neither the Intake nor the Registry has `attestDeposit`.
    function test_noSignerAttestedMinting() public {
        bytes memory call = abi.encodeWithSignature(
            "attestDeposit((bytes32,address,uint64,bytes32),bytes[])", VAULT, account, VAULT_001, TAG, new bytes[](0)
        );
        (bool okIntake,) = address(intake).call(call);
        (bool okRegistry,) = address(registry).call(call);
        assertFalse(okIntake);
        assertFalse(okRegistry);
        assertEq(zzec.totalSupply(), 0);
    }

    // ------------------------------------------------------------------ K3: ready height, ready time, anchor

    function test_finalize_waitsForTheRelayAndTheDelay() public {
        uint256 provedAt = block.timestamp;
        _prove(ANCHOR);
        PendingActivation memory p = intake.pendingActivation(VAULT);
        assertEq(p.amount, VAULT_001);
        assertEq(p.depositTag, TAG);
        assertEq(p.anchor, ANCHOR);
        assertEq(p.readyHeight, TIP_HEIGHT + FINALIZE_DEPTH);
        assertEq(p.readyTime, provedAt + FINALIZE_DELAY);
        bytes memory notReady =
            abi.encodeWithSelector(IZecuredIntake.ActivationNotReady.selector, VAULT, p.readyHeight, p.readyTime);

        // Neither condition.
        vm.expectRevert(notReady);
        intake.finalizeActivation(VAULT);
        // Time without the relay: the feeders may be down.
        vm.warp(p.readyTime);
        relay.setTipHeight(p.readyHeight - 1);
        vm.expectRevert(notReady);
        intake.finalizeActivation(VAULT);
        // The relay without the time: someone may be feeding a branch of their own.
        vm.warp(p.readyTime - 1);
        relay.setTipHeight(p.readyHeight);
        vm.expectRevert(notReady);
        intake.finalizeActivation(VAULT);

        vm.warp(p.readyTime);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT, account, VAULT_001, TAG);
        vm.prank(stranger); // anyone
        intake.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), VAULT_001);
        assertEq(_pendingAccount(VAULT), address(0));

        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.finalizeActivation(VAULT);
    }

    function test_revert_finalizeAfterTheAnchorLeftTheBestChain() public {
        _prove(ANCHOR);
        _ready();
        relay.setBlock(ANCHOR, false, 0); // a heavier branch replaced it
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, ANCHOR, 0));
        intake.finalizeActivation(VAULT);
    }

    function test_revert_finalizeRechecksTheWork() public {
        _prove(ANCHOR);
        _ready();
        relay.setBlock(ANCHOR, true, MIN_WORK - 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, ANCHOR, MIN_WORK - 1));
        intake.finalizeActivation(VAULT);
    }

    // ------------------------------------------------------------------ K3: dropping a pending activation

    function test_drop_onlyOnceTheAnchorLeftTheBestChain_thenTheNoteCanBeProvenAgain() public {
        _prove(ANCHOR);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorStillOnBestChain.selector, VAULT));
        intake.dropPendingActivation(VAULT);

        relay.setBlock(ANCHOR, false, 0);
        vm.expectEmit(address(intake));
        emit IZecuredIntake.PendingActivationDropped(VAULT);
        vm.prank(stranger); // anyone
        intake.dropPendingActivation(VAULT);
        assertEq(_pendingAccount(VAULT), address(0));
        assertFalse(registry.depositTagUsed(TAG), "the tag is free again");
        assertEq(registry.vaultOf(account), bytes32(0), "the account is free again");
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.None));
        assertEq(zzec.totalSupply(), 0, "nothing was ever minted");
        assertEq(intake.pendingTotal(), 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.finalizeActivation(VAULT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.dropPendingActivation(VAULT);

        // The same deposit, anchored on the chain that won.
        _prove(ANCHOR_2);
        _ready();
        intake.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), VAULT_001);
    }

    // ------------------------------------------------------------------ Tier 3

    /// The proven note is a lineage note from the proof on (a spend while the activation is pending is already F1)
    /// and stops being one when the activation is dropped: then it backs nothing.
    function test_lineage_fromTheProofOn_untilTheDrop() public {
        _prove(ANCHOR);
        LineageNote memory l = registry.lineageOf(TAG);
        assertEq(l.vaultId, VAULT);
        assertEq(l.value, VAULT_001);
        assertEq(registry.openVaultCount(), 1);

        relay.setBlock(ANCHOR, false, 0);
        vm.warp(block.timestamp + 1 days);
        intake.dropPendingActivation(VAULT);
        assertEq(registry.lineageOf(TAG).vaultId, bytes32(0));
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lastVaultSettledAt(), block.timestamp);

        // Proven again on the chain that won: lineage again, through the activation.
        _prove(ANCHOR_2);
        _ready();
        intake.finalizeActivation(VAULT);
        assertEq(registry.lineageOf(TAG).value, VAULT_001);
        assertEq(registry.openVaultCount(), 1);
    }

    /// E2: an open violation case about the vault (its deposit spent during the wait) holds its activation; once the
    /// case is gone the activation goes on.
    function test_E2_anOpenCaseHoldsTheActivation() public {
        _prove(ANCHOR);
        _ready();
        mockBond.setOpenCases(VAULT, 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.ViolationCaseOpen.selector, VAULT));
        intake.finalizeActivation(VAULT);
        mockBond.setOpenCases(VAULT, 0);
        intake.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
    }

    /// E3: while its activation is pending, the deposit is not refundable by the signers: the vault is Pending and
    /// the note's tag is used; a refund needs a Refund claim on Base (T-02).
    function test_E3_aPendingDepositIsNotRefundable() public {
        _prove(ANCHOR);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Pending));
        assertTrue(registry.depositTagUsed(TAG));
        assertEq(registry.claimCount(VAULT), 0, "no claim: nothing to pay out");
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotPendingAccount.selector, VAULT, stranger));
        vm.prank(stranger);
        intake.refundPending(VAULT);
    }

    // ------------------------------------------------------------------ halt (T-02)

    /// After a slash (the bond halted), no pending activation is activated: anyone converts it into a refund; and a
    /// new proof lands as a refund at once.
    function test_halt_pendingBecomesARefund_newProofsLandAsRefunds() public {
        _prove(ANCHOR);
        mockBond.setHalted(true);
        _ready();
        vm.expectRevert(IZecuredIntake.MintingHalted.selector);
        intake.finalizeActivation(VAULT);

        bytes32 claimId = registry.claimIdOf(VAULT, 0);
        vm.expectEmit(address(intake));
        emit IZecuredIntake.DepositRefunded(VAULT, claimId, RefundReason.PendingConverted);
        vm.prank(stranger); // anyone, after a halt
        assertEq(intake.refundPending(VAULT), claimId);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Cancelled));
        _assertClaim(claimId, VAULT, ClaimKind.Refund, VAULT_001, bytes32(0), ClaimStatus.Pending);
        assertEq(intake.pendingTotal(), 0);
        assertEq(registry.lineageOf(TAG).value, VAULT_001, "still a lineage note");
        assertEq(registry.openVaultCount(), 1, "open until the refund is paid");

        // A second deposit to the vault: a refund too.
        bytes32 tag2 = keccak256("zecured.test.pending.tag2");
        _proveTag(ANCHOR_2, tag2);
        _assertClaim(registry.claimIdOf(VAULT, 1), VAULT, ClaimKind.Refund, VAULT_001, bytes32(0), ClaimStatus.Pending);
        assertEq(zzec.totalSupply(), 0, "nothing minted after the halt");
    }

    function test_halt_aNewVaultsProofLandsAsARefund() public {
        mockBond.setHalted(true);
        bytes32 claimId = registry.claimIdOf(VAULT, 0);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(ANCHOR, TAG);
        vm.expectEmit(address(intake));
        emit IZecuredIntake.DepositRefunded(VAULT, claimId, RefundReason.Halted);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Cancelled));
        assertEq(registry.vaultOf(account), bytes32(0), "a refund binds no account");
    }

    function test_revert_refundPendingOfAReorgedDeposit() public {
        _prove(ANCHOR);
        mockBond.setHalted(true);
        relay.setBlock(ANCHOR, false, 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotRefundableYet.selector, VAULT));
        intake.refundPending(VAULT);
        intake.dropPendingActivation(VAULT); // the drop is the way out
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.None));
    }

    // ------------------------------------------------------------------ pause (T-03)

    function test_pause_newProofsPend_finalizeWaits_accountRefundsAfterTheDelay() public {
        uint64 until = uint64(block.timestamp + 10 days);
        _pause(until);
        assertTrue(intake.mintingPaused());
        _prove(ANCHOR); // pends: a short pause costs nobody the loan
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Pending));
        _ready();
        vm.expectRevert(IZecuredIntake.MintingIsPaused.selector);
        intake.finalizeActivation(VAULT);

        // Before the delay, not even the account; after it, only the account.
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotRefundableYet.selector, VAULT));
        intake.refundPending(VAULT);
        vm.warp(intake.pausedSince() + PENDING_REFUND_DELAY);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotPendingAccount.selector, VAULT, stranger));
        intake.refundPending(VAULT);
        vm.prank(account);
        intake.refundPending(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Cancelled));
    }

    function test_pause_endsByItself_orBySignatures() public {
        uint64 until = uint64(block.timestamp + 1 days);
        _pause(until);
        _prove(ANCHOR);
        _ready();
        vm.warp(until); // the pause lifts by itself
        assertFalse(intake.mintingPaused());
        intake.finalizeActivation(VAULT);

        _pause(uint64(block.timestamp + 30 days));
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        intake.unpauseMinting(nonce, deadline, _threeSigs(intake.hashUnpauseMinting(nonce, deadline)));
        assertFalse(intake.mintingPaused());
    }

    function test_revert_pauseRules() public {
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        uint64 tooLong = uint64(block.timestamp + MAX_PAUSE + 1);
        bytes[] memory sigs = _threeSigs(intake.hashPauseMinting(tooLong, nonce, deadline));
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredIntake.PauseTooLong.selector, tooLong, uint64(block.timestamp + MAX_PAUSE))
        );
        intake.pauseMinting(tooLong, nonce, deadline, sigs);

        uint64 until = uint64(block.timestamp + 1 days);
        sigs = _signerSigs(intake.hashPauseMinting(until, nonce, deadline), _idx(0, 1));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InsufficientSignatures.selector, 2));
        intake.pauseMinting(until, nonce, deadline, sigs);

        sigs = _threeSigs(intake.hashPauseMinting(until, nonce + 1, deadline));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.BadGuardNonce.selector, nonce));
        intake.pauseMinting(until, nonce + 1, deadline, sigs);

        sigs = _threeSigs(intake.hashPauseMinting(until, nonce, deadline));
        intake.pauseMinting(until, nonce, deadline, sigs);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.BadGuardNonce.selector, nonce + 1));
        intake.pauseMinting(until, nonce, deadline, sigs); // no replay

        vm.warp(deadline + 1);
        uint256 next = intake.guardNonce();
        sigs = _threeSigs(intake.hashUnpauseMinting(next, deadline));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.SignatureExpired.selector, deadline));
        intake.unpauseMinting(next, deadline, sigs);
    }

    /// I22: a pause never stops an exit.
    function test_pause_neverStopsAnExit() public {
        bytes32 vaultId = _attest(stranger, VAULT_001);
        _pause(uint64(block.timestamp + 30 days));
        _authorizeBoth(stranger);
        vm.prank(stranger);
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
    }

    function test_revert_dropOrFinalizeWithoutAPendingActivation() public {
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.finalizeActivation(VAULT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.dropPendingActivation(VAULT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NoPendingActivation.selector, VAULT));
        intake.refundPending(VAULT);
    }
}
