// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Tier3Base} from "../utils/Tier3Base.sol";
import {ViolationKind, ViolationJournal, IZecuredSlasher} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredRegistry, VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {SpendCommit, CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {MorphoBalancesLib} from "morpho-blue/src/libraries/periphery/MorphoBalancesLib.sol";
import {ORACLE_PRICE_SCALE} from "morpho-blue/src/libraries/ConstantsLib.sol";
import {IMorpho} from "morpho-blue/src/interfaces/IMorpho.sol";

/// @notice Regressions for the red-team review of 2026-10-14 (docs/DECISIONS.md D38): each test is one of the review's
///         proofs of concept with its outcome reversed.
contract RedTeamRegressionsTest is Tier3Base {
    using MorphoBalancesLib for IMorpho;

    // ================================================================== RT-1: F4, a wrong claim reference

    bytes32 internal constant SALT = keccak256("rt1.salt");
    bytes32 internal constant TXID_X = keccak256("rt1.txid.X");
    bytes32 internal constant PAYOUT_REF_X = keccak256("rt1.payoutRef(X)");
    address internal rt1Account = makeAddr("rt1.account");

    function _receiver() internal pure returns (bytes memory) {
        return abi.encodePacked(bytes32(keccak256("rt1.receiver.a")), bytes11(keccak256("rt1.receiver.b")));
    }

    function _dest() internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(SALT, _receiver()));
    }

    /// What the claimant (or anyone with the vault's viewing key and the salt) computes for X.
    function _trueClaimRef() internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(SALT, TXID_X));
    }

    /// A vault whose whole value is claimed by a release to `_dest()`; the payout X of that claim, committed by three
    /// signers with `claimRef`.
    function _releaseCommitted(bytes32 claimRef) internal returns (bytes32 vaultId, SpendCommit memory c) {
        vaultId = _attest(rt1Account, VAULT_005);
        vm.startPrank(rt1Account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestRelease(vaultId, _dest());
        vm.stopPrank();
        c = SpendCommit({
            vaultId: vaultId,
            inputTag: _depositTagFor(rt1Account),
            claimIds: _one(claimId),
            claimRefs: _one(claimRef),
            claimOutputs: new uint8[](1),
            payoutRef: PAYOUT_REF_X,
            changeTag: bytes32(0),
            changeValue: 0
        });
        _send(c);
    }

    function test_RT1_aWrongClaimRefIsProvenAsF4AndSlashesTheRecordsSigners() public {
        (bytes32 vaultId,) = _releaseCommitted(keccak256("rt1.garbage"));
        assertEq(registry.claimRefOf(PAYOUT_REF_X, _dest()), keccak256("rt1.garbage"));

        bytes memory f4 = _journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, _trueClaimRef(), 0, _dest(), 0);
        bytes32 caseId = _prove(f4);
        assertEq(slasher.caseOf(caseId).vaultId, vaultId);
        assertEq(slasher.caseOf(caseId).signerBitmap, 0x15, "signers 0, 2 and 4 signed the record");

        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(bond.halted());
        assertEq(bond.bondOf(0) + bond.bondOf(2) + bond.bondOf(4), 0, "the record's signers are slashed");
        assertEq(bond.bondOf(1) + bond.bondOf(3), 2 * MIN_BOND);
    }

    function test_RT1_anHonestClaimRefIsNotF4() public {
        _releaseCommitted(_trueClaimRef());
        _proveExpectRevert(
            _journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, _trueClaimRef(), 0, _dest(), 0),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.WrongClaimRef, PAYOUT_REF_X)
        );
    }

    function test_RT1_F4NeedsTheRecordToNameTheClaimAndCleanFields() public {
        _releaseCommitted(keccak256("rt1.garbage"));
        bytes memory err =
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.WrongClaimRef, PAYOUT_REF_X);
        // Another recipient commitment: the record names no such claim.
        _proveExpectRevert(
            _journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, _trueClaimRef(), 0, keccak256("other"), 0), err
        );
        // A payout reference nobody committed.
        bytes memory noRecord =
            _journal(ViolationKind.WrongClaimRef, ANCHOR, keccak256("no record"), _trueClaimRef(), 0, _dest(), 0);
        _proveExpectRevert(
            noRecord,
            abi.encodeWithSelector(
                IZecuredSlasher.NoViolation.selector, ViolationKind.WrongClaimRef, keccak256("no record")
            )
        );
        // The fields F4 does not use must be zero.
        _proveExpectRevert(
            _journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, _trueClaimRef(), 1, _dest(), 0), err
        );
        _proveExpectRevert(
            _journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, _trueClaimRef(), 0, _dest(), 1), err
        );
        _proveExpectRevert(_journal(ViolationKind.WrongClaimRef, ANCHOR, PAYOUT_REF_X, bytes32(0), 0, _dest(), 0), err);
    }

    /// A payout never names two claims of one recipient commitment, so a commitment's claim reference is unique.
    function test_RT1_aPayoutNamesEachCommitmentOnce() public {
        bytes32 vaultId = _attest(rt1Account, VAULT_005);
        vm.startPrank(rt1Account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 a = registry.requestPartialRelease(vaultId, 1_000_000, _dest());
        bytes32 b = registry.requestPartialRelease(vaultId, 1_000_000, _dest());
        vm.stopPrank();
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(rt1Account), _two(a, b), "rt1.twice");
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.DuplicateClaimCommitment.selector, _dest()));
        registry.commitSpend(c, sigs);
    }

    // ================================================================== cross-language vectors (review § 4)

    /// The journals the Rust statement computes for the TEST-ONLY vault 9's deposit standing in for a payout X
    /// (tier2/statement/tests/violations.rs, `journals_are_pinned_across_languages`): the claim's salt is 0x5a * 32,
    /// its receiver the vault's own address, its output action 1.
    bytes32 internal constant X_ANCHOR = 0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000;
    bytes32 internal constant X_CLAIM_REF = 0x62840e8e15455e60905714b51e12fa370ec8cc849cd2cbf30b696ce139950869;
    bytes32 internal constant X_PAYOUT_REF = 0x46b19981da842f7bf81342fa8db82ecd88933c3f9d777816c80bc712ef931496;
    bytes32 internal constant X_DEST = 0x0144dce00498a1cb4e3d04161414ccf1cd42b09f70fe83ddcf745973c522a0a7;

    function _rustJournal(uint8 kind, bytes32 ref, bytes32 noteTag, uint64 value, uint8 output)
        internal
        pure
        returns (bytes memory)
    {
        return bytes.concat(
            bytes32(uint256(kind)),
            X_ANCHOR,
            hex"000000000000000000000000bebebebebebebebebebebebebebebebebebebebe",
            ref,
            noteTag,
            bytes32(uint256(value)),
            X_DEST,
            bytes32(uint256(output))
        );
    }

    /// A vault whose claim of recipient X_DEST is committed in payout `payoutRef` under `claimRef` at output 1.
    function _committedClaim(uint64 claimAmount, bytes32 payoutRef, bytes32 claimRef) internal returns (bytes32) {
        address acct = makeAddr("vectors.account");
        bytes32 vaultId = _attest(acct, VAULT_005);
        vm.startPrank(acct);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestPartialRelease(vaultId, claimAmount, X_DEST);
        vm.stopPrank();
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(acct), _one(claimId), "vectors");
        (c.payoutRef, c.claimRefs[0], c.claimOutputs[0]) = (payoutRef, claimRef, 1);
        _send(c);
        relay.setBlock(X_ANCHOR, true, SLASH_WORK);
        return vaultId;
    }

    function test_vector_theRustStatementsF3Journal() public {
        bytes memory f3 = _rustJournal(3, X_CLAIM_REF, bytes32(0), 1_000_000, 1);
        ViolationJournal memory j = abi.decode(f3, (ViolationJournal));
        assertEq(uint8(j.kind), uint8(ViolationKind.Misdirected));
        assertEq((j.value), 1_000_000);
        assertEq(j.outputIndex, 1);
        uint256 snap = vm.snapshotState();
        // The output paid 1,000,000 zat: a claim expecting more is short (F3) ...
        bytes32 vaultId = _committedClaim(2_000_000, keccak256("vectors.payout"), X_CLAIM_REF);
        assertEq(slasher.caseOf(_prove(f3)).vaultId, vaultId);
        vm.revertToState(snap);
        // ... and one expecting exactly that (1,010,000 less the 10,000 zat fee) was paid.
        _committedClaim(1_010_000, keccak256("vectors.payout"), X_CLAIM_REF);
        _proveExpectRevert(
            f3, abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Misdirected, X_CLAIM_REF)
        );
    }

    function test_vector_theRustStatementsF4Journal() public {
        bytes memory f4 = _rustJournal(4, X_PAYOUT_REF, X_CLAIM_REF, 0, 0);
        assertEq(uint8(abi.decode(f4, (ViolationJournal)).kind), uint8(ViolationKind.WrongClaimRef));
        uint256 snap = vm.snapshotState();
        // The commitment of X names the claim under another reference: F4 ...
        bytes32 vaultId = _committedClaim(1_000_000, X_PAYOUT_REF, keccak256("vectors.wrong"));
        assertEq(slasher.caseOf(_prove(f4)).vaultId, vaultId);
        vm.revertToState(snap);
        // ... under the one the statement computes: nothing to prove.
        _committedClaim(1_000_000, X_PAYOUT_REF, X_CLAIM_REF);
        _proveExpectRevert(
            f4, abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.WrongClaimRef, X_PAYOUT_REF)
        );
    }

    // ================================================================== RT-4: one claim set per input

    function _closedVault(address account, uint256 accountKey)
        internal
        returns (bytes32 vaultId, bytes32 u, bytes32 m)
    {
        vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        uint256 value = uint256(VAULT_001) * PRICE_40 / ORACLE_PRICE_SCALE;
        _borrow(account, value * 4_000 / 10_000);
        uint256 debt = MORPHO.expectedBorrowAssets(params, account);
        _fundAndApproveAdapter(marketMaker, debt);
        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _sign(accountKey, adapter.hashCloseRequest(req));
        vm.prank(marketMaker);
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);
        (u, m) = (registry.claimIdOf(vaultId, 0), registry.claimIdOf(vaultId, 1));
    }

    function test_RT4_anInputKeepsTheClaimSetOfItsFirstCommitment() public {
        (address account, uint256 key) = makeAddrAndKey("rt4.account");
        (bytes32 vaultId, bytes32 u, bytes32 m) = _closedVault(account, key);
        bytes32 t0 = _depositTagFor(account);

        uint256 snap = vm.snapshotState();
        // The pair first: a later commitment of the borrower alone from the same note is refused ...
        _send(_spendCommit(vaultId, t0, _two(u, m), "rt4.S2"));
        SpendCommit memory alone = _spendCommit(vaultId, t0, _one(u), "rt4.S1");
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(alone));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimSetMismatch.selector, t0));
        registry.commitSpend(alone, sigs);
        // ... while a rebuild of the same payout (same claims, in the same order) is accepted.
        _send(_spendCommit(vaultId, t0, _two(u, m), "rt4.S2.rebuild"));
        vm.revertToState(snap);

        // The borrower alone first: the pair from the same note is refused, so the market maker's claim stays free
        // and is paid from the change.
        SpendCommit memory first = _spendCommit(vaultId, t0, _one(u), "rt4.S1");
        _send(first);
        SpendCommit memory pair = _spendCommit(vaultId, t0, _two(u, m), "rt4.S2");
        sigs = _threeSigs(registry.hashSpendCommit(pair));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimSetMismatch.selector, t0));
        registry.commitSpend(pair, sigs);
        _confirm(vaultId, _one(u));
        SpendCommit memory next = _spendCommit(vaultId, first.changeTag, _one(m), "rt4.next");
        _send(next);
        _confirm(vaultId, _one(m));
        assertEq(registry.openVaultCount(), 0, "the vault settles");
    }

    // ================================================================== RT-6: a halt and a pending deposit

    function test_RT6_afterAHaltAPendingDepositIsRefundedAndTheBondsFreed() public {
        // A vault whose payout withheld its change (F2, signed by signers 1, 3 and 4), slashed and halted.
        address thief = makeAddr("rt6.vault");
        bytes32 badVault = _attest(thief, VAULT_005);
        vm.startPrank(thief);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestPartialRelease(badVault, 1_000_000, keccak256("rt6.dest"));
        vm.stopPrank();
        SpendCommit memory c = _spendCommit(badVault, _depositTagFor(thief), _one(claimId), "rt6");
        registry.commitSpend(c, _signerSigs(registry.hashSpendCommit(c), _idx(1, 3, 4)));

        // Meanwhile an innocent deposit proof is pending.
        bytes32 innocentVault = keccak256("rt6.innocent.vault");
        relay.setTipHeight(4_500_000);
        _proveDeposit(innocentVault, makeAddr("rt6.innocent"), VAULT_001, keccak256("rt6.tag"), keccak256("rt6.anchor"));

        bytes32 caseId =
            _prove(_journal(ViolationKind.WithheldChange, ANCHOR, c.payoutRef, c.changeTag, c.changeValue, 0, 0));
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertTrue(bond.halted());

        // T-04: the slash compromised the bad vault. Its signers can no longer confirm or pay its claims, nor can its
        // account leave; the compensator takes it over with the slashed bonds of signers 1, 3 and 4.
        bytes[] memory confirmSigs = _threeSigs(registry.hashPayoutConfirmation(badVault, _one(claimId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultCompromised.selector, badVault));
        registry.confirmPayout(badVault, _one(claimId), confirmSigs);
        vm.prank(thief);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, badVault));
        registry.requestRelease(badVault, keccak256("rt6.dest2"));
        uint256 slashed = 3 * MIN_BOND;
        assertEq(compensator.funds(), slashed - slashed * BOUNTY_BPS / 10_000, "the slashed bonds less the bounty");

        compensator.tallyClaim(claimId); // the claim leaves the Registry's books: the compensator owes it now
        compensator.tallyVault(badVault);
        vm.warp(block.timestamp + INCIDENT_WINDOW);
        compensator.finalizeIncident(compensator.currentIncident());
        compensator.settleVault(badVault); // no debt: retired in full
        assertEq(uint8(_status(badVault)), uint8(VaultStatus.Closed));
        (,,,, ClaimStatus settled) = registry.claim(claimId);
        assertEq(uint8(settled), uint8(ClaimStatus.Compensated));
        assertEq(zzec.balanceOf(address(MORPHO)), 0, "the bad vault's unbacked zZEC is burned");
        // The owner is paid in USDC at the incident's price: its released claim and the vault's equity, here in full.
        compensator.withdrawEntitlement(claimId);
        compensator.withdrawEquity(badVault);
        assertEq(usdc.balanceOf(thief), uint256(VAULT_005) * PRICE_40 / 1e36, "0.05 ZEC at $40");

        // The innocent pending deposit can never finalize: anyone turns it into a refund (T-02), whose destination its
        // account gives, and the signers pay it with a committed payout like any claim.
        assertTrue(relay.isInBestChain(keccak256("rt6.anchor")));
        bytes32 refund = intake.refundPending(innocentVault);
        (,, VaultStatus status) = registry.vault(innocentVault);
        assertEq(uint8(status), uint8(VaultStatus.Cancelled));
        vm.prank(makeAddr("rt6.innocent"));
        registry.setRefundDestination(refund, keccak256("rt6.innocent.dest"));
        _send(_spendCommit(innocentVault, keccak256("rt6.tag"), _one(refund), "rt6.refund"));
        _confirm(innocentVault, _one(refund));

        // Every vault has settled: once the window has passed, the signers the slash did not name take their bonds,
        // and what the compensator did not need goes to the leftover sink, never back to the signers.
        assertEq(registry.openVaultCount(), 0);
        vm.warp(block.timestamp + WINDOW);
        vm.startPrank(signerSet[0]);
        bond.requestUnbond(MIN_BOND);
        vm.warp(block.timestamp + UNBONDING_PERIOD);
        bond.withdrawUnbonded(signerSet[0]);
        vm.stopPrank();
        assertEq(bond.bondOf(0), 0);
        assertEq(usdc.balanceOf(signerSet[0]), MIN_BOND);
        uint256 leftover = compensator.sweepLeftover();
        assertGt(leftover, 0);
        assertEq(usdc.balanceOf(LEFTOVER_SINK), leftover);
        assertEq(usdc.balanceOf(address(compensator)), 0);
    }

    function test_RT6_withoutAHaltADropStillNeedsTheAnchorOffTheBestChain() public {
        bytes32 vaultId = keccak256("rt6.pending");
        relay.setTipHeight(4_500_000);
        _proveDeposit(vaultId, makeAddr("rt6.pending.account"), VAULT_001, keccak256("rt6.tag2"), keccak256("rt6.a2"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorStillOnBestChain.selector, vaultId));
        intake.dropPendingActivation(vaultId);
    }

    // ================================================================== RT-8: dust

    /// `ZecuredRegistry._claims`' storage slot (`forge inspect ZecuredRegistry storageLayout`).
    uint256 internal constant CLAIMS_SLOT = 4;

    /// White box: overwrites a claim's amount (as test/fork/SpendCommits.t.sol does), checked through the view.
    function _setClaimAmount(bytes32 claimId, uint64 amount) internal {
        bytes32 slot = bytes32(uint256(keccak256(abi.encode(claimId, CLAIMS_SLOT))) + 2);
        uint256 word = uint256(vm.load(address(registry), slot));
        word = (word & ~uint256(type(uint64).max)) | amount;
        vm.store(address(registry), slot, bytes32(word));
        (,, uint64 stored,,) = registry.claim(claimId);
        assertEq(stored, amount, "claim layout");
    }

    function _dustAfterTwoLiquidations(address account) internal returns (bytes32 vaultId, bytes32 big, bytes32 dust) {
        vaultId = _attest(account, VAULT_001);
        _authorizeBoth(account);
        uint256 value = uint256(VAULT_001) * PRICE_40 / ORACLE_PRICE_SCALE;
        _borrow(account, value * 4_000 / 10_000);
        oracle.setPrice(1800e32);
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        adapter.liquidate(account, 995_000, 0, LIQUIDATOR_COMMITMENT);
        vm.prank(liquidator);
        adapter.liquidate(account, 5_000, 0, keccak256("rt8.second"));
        (big, dust) = (registry.claimIdOf(vaultId, 0), registry.claimIdOf(vaultId, 1));
    }

    function test_RT8_aDustClaimOfAClosedVaultIsWrittenOffAndTheVaultSettles() public {
        address account = makeAddr("rt8.account");
        (bytes32 vaultId, bytes32 big, bytes32 dust) = _dustAfterTwoLiquidations(account);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));

        SpendCommit memory first = _spendCommit(vaultId, _depositTagFor(account), _one(big), "rt8.first");
        _send(first);
        _confirm(vaultId, _one(big));
        assertEq(registry.openVaultCount(), 1, "the dust claim keeps the vault open");

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.DustWrittenOff(vaultId, dust, 5_000);
        vm.prank(makeAddr("anyone"));
        registry.writeOffDust(dust);
        (,,,, ClaimStatus status) = registry.claim(dust);
        assertEq(uint8(status), uint8(ClaimStatus.WrittenOff));
        assertEq(registry.openVaultCount(), 0);

        vm.warp(block.timestamp + WINDOW);
        vm.prank(signerSet[1]);
        bond.requestUnbond(MIN_BOND); // the full exit: nothing is at stake any more
        (uint256 unbonding,) = bond.unbondingOf(1);
        assertEq(unbonding, MIN_BOND);
    }

    function test_RT8_onlyUncommittedDustOfAClosedVaultIsWrittenOff() public {
        address account = makeAddr("rt8.account2");
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 big = registry.requestPartialRelease(vaultId, 1_000_000, keccak256("rt8.dest"));
        vm.stopPrank();
        _setClaimAmount(big, 10_000);
        // The vault is still active.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotDust.selector, big));
        registry.writeOffDust(big);
        // No such claim, and a claim above the limit.
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotDust.selector, keccak256("none")));
        registry.writeOffDust(keccak256("none"));
        vm.prank(account);
        bytes32 rest = registry.requestRelease(vaultId, keccak256("rt8.dest2"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotDust.selector, rest));
        registry.writeOffDust(rest);
        // Dust at the limit of a closed vault goes.
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        registry.writeOffDust(big);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotDust.selector, big));
        registry.writeOffDust(big);
    }

    // ================================================================== RT-16: one lineage note per tag

    function test_RT16_aDepositTagAlreadyALineageNoteIsRefused() public {
        address account = makeAddr("rt16.account");
        bytes32 vaultId = _attest(account, VAULT_005);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestPartialRelease(vaultId, 1_000_000, keccak256("rt16.dest"));
        vm.stopPrank();
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(account), _one(claimId), "rt16");
        // A change tag that another vault's deposit will (absurdly) carry.
        address other = makeAddr("rt16.other");
        c.changeTag = _depositTagFor(other);
        _send(c);
        // Neither as an activation nor as a refund: the Registry refuses a tag that is already a lineage note.
        bytes32 tag = _depositTagFor(other);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, tag));
        intake.harnessActivate(_vaultIdFor(other), other, VAULT_001, tag);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, tag));
        intake.harnessRefund(_vaultIdFor(other), other, VAULT_001, tag);
    }
}
