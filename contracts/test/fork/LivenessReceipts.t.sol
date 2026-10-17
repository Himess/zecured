// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {PendingActivation} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {Tier3Base} from "../utils/Tier3Base.sol";

/// @notice R-5 and R-8 end to end, on a bonded deployment (the real Registry, Intake, SignerBondV2, slasher and
///         compensator on a Base Sepolia fork, the MOCK relay and verifier): no liveness without a signer's receipt for
///         the claim's destination, and none for a refund while its vault's activation pends.
contract LivenessReceiptsTest is Tier3Base {
    address internal account = makeAddr("zecured.receipts.account");

    function _release() internal returns (bytes32 vaultId, bytes32 claimId) {
        vaultId = _attest(account, VAULT_005);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
    }

    /// R-5: a release whose claimant never handed the signers its destination stays unpaid by the bonds however long
    /// it waits; once a signer's node has the destination, its receipt arms the claim, which is paid when due, and the
    /// signers then reimburse the bonds.
    function test_R5_aClaimIsPaidFromTheBondsOnlyWithAReceipt() public {
        (bytes32 vaultId, bytes32 claimId) = _release();
        uint256 createdAt = block.timestamp;
        vm.warp(block.timestamp + LIVENESS_PERIOD + COMMIT_GRACE + 30 days);
        (uint64 dueAt, uint256 amount) = bond.livenessDue(claimId);
        assertEq(dueAt, 0, "no receipt: never due");
        assertEq(amount, 0);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotLivenessEligible.selector, claimId));
        bond.slashLiveness(claimId);

        // A node received the destination now (a month late): due a liveness period from now, not from creation.
        uint64 receivedAt = uint64(block.timestamp);
        _armLivenessAt(claimId, receivedAt);
        (dueAt, amount) = bond.livenessDue(claimId);
        assertEq(dueAt, receivedAt + LIVENESS_PERIOD);
        assertGt(dueAt, createdAt + LIVENESS_PERIOD);
        vm.warp(dueAt);
        uint256 before = usdc.balanceOf(account);
        vm.prank(account);
        assertEq(bond.slashLiveness(claimId), amount);
        assertEq(usdc.balanceOf(account), before + amount);
        assertTrue(registry.claimDetails(claimId).bondPaid);

        // The bonds' reimbursement: the signers name its destination (all five sign, which R-6 requires), the payout
        // is committed and confirmed.
        bytes32 dest = keccak256("zecured.receipts.reimburse");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        registry.setReimburseDestination(
            claimId, dest, deadline, _signerSigs(registry.hashReimburseDestination(claimId, dest, deadline), _all5())
        );
        _send(_spendCommit(vaultId, _depositTagFor(account), _one(claimId), "zecured.receipts.reimburse"));
        _confirm(vaultId, _one(claimId));
        (,,,, ClaimStatus status) = registry.claim(claimId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
        assertEq(registry.openVaultCount(), 0);
    }

    /// R-8 (the review's finding): a refund opened while its vault's activation pended accrued liveness, though no
    /// payout could be committed for it (`VaultPending`). Now the refund waits for the vault: no claim exists while it
    /// pends, and once it opens, its clock starts then.
    function test_R8_noRefundAccruesLivenessWhileItsVaultPends() public {
        bytes32 vaultId = _vaultIdFor(account);
        _proveDeposit(vaultId, account, VAULT_001, _depositTagFor(account), keccak256("zecured.receipts.a1"));
        bytes32 second = keccak256("zecured.receipts.second");
        _proveDeposit(vaultId, account, VAULT_005, second, keccak256("zecured.receipts.a2"));
        _readyRefund(second);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.VaultPending.selector, vaultId));
        intake.finalizeRefund(second);
        // A liveness period passes while the activation waits: there is no claim to be overdue.
        vm.warp(block.timestamp + LIVENESS_PERIOD + COMMIT_GRACE);
        assertEq(registry.claimCount(vaultId), 0, "no claim on a pending vault");

        PendingActivation memory p = intake.pendingActivation(vaultId);
        relay.setTipHeight(p.readyHeight);
        intake.finalizeActivation(vaultId);
        bytes32 refundId = intake.finalizeRefund(second);
        uint256 openedAt = block.timestamp;
        vm.prank(account);
        registry.setRefundDestination(refundId, keccak256("zecured.receipts.refundDest"));
        _armLivenessAt(refundId, uint64(openedAt) - 1 days); // a node held the destination before: counts from now
        (uint64 dueAt,) = bond.livenessDue(refundId);
        assertEq(dueAt, openedAt + LIVENESS_PERIOD, "the clock started when the claim opened");
        vm.warp(dueAt - 1);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.LivenessNotDue.selector, refundId, dueAt));
        bond.slashLiveness(refundId);
    }

    /// R-5: a new destination for a refund voids its arming; the old receipt cannot arm the new destination.
    function test_R5_aNewRefundDestinationNeedsANewReceipt() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        bytes32 tag = keccak256("zecured.receipts.refundTag");
        bytes32 refundId = intake.harnessRefund(vaultId, account, VAULT_001, tag);
        vm.prank(account);
        registry.setRefundDestination(refundId, keccak256("zecured.receipts.first"));
        uint64 at = uint64(block.timestamp);
        bytes memory oldReceipt = _receipt(refundId, at, 2);
        bond.armLiveness(refundId, at, oldReceipt);
        vm.warp(block.timestamp + 1 hours);
        vm.prank(account);
        registry.setRefundDestination(refundId, keccak256("zecured.receipts.second"));
        (uint64 dueAt,) = bond.livenessDue(refundId);
        assertEq(dueAt, 0, "armed for the old destination only");
        vm.expectPartialRevert(ISignerBond.NotASigner.selector);
        bond.armLiveness(refundId, at, oldReceipt);
        _armLivenessAt(refundId, uint64(block.timestamp));
        (dueAt,) = bond.livenessDue(refundId);
        assertEq(dueAt, block.timestamp + LIVENESS_PERIOD);
    }
}
