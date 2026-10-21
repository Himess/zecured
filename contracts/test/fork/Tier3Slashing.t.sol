// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {ISignerBond} from "../../src/interfaces/v3/ISignerBond.sol";
import {
    IZecuredSlasher,
    ViolationKind,
    ViolationJournal,
    ViolationCase
} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {ClaimStatus, VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredVersions, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

/// @notice Tier 3 end to end on a Base Sepolia fork (docs/TIER3.md §§ 6, 7): the five bonds, each violation kind proven
///         with the MOCK verifier against Registry records and a MOCK relay's anchors, the challenge window, dismissal,
///         the slash, the bounty, the halt, the hand-off to the compensator and the bonds' exit rules (SignerBondV2,
///         USDC).
contract Tier3SlashingTest is ZecuredFixture {
    bytes32 internal constant DEPOSIT_IMAGE = keccak256("zecured.test.tier3.depositImage");
    bytes32 internal constant SUITE_VIOLATION_IMAGE = keccak256("zecured.test.tier3.violationImage");
    bytes32 internal constant ANCHOR = keccak256("zecured.test.tier3.anchor");
    bytes32 internal constant THEFT_REF = keccak256("zecured.test.tier3.theftPayoutRef");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;

    address internal account = makeAddr("zecured.tier3.account");
    address internal watcher = makeAddr("zecured.tier3.watcher");
    bytes32 internal vaultId;
    bytes32 internal depositTag;

    function _proofSetup() internal override returns (ProofSetup memory) {
        relay = new MockHeaderRelay();
        verifier = new MockRiscZeroVerifier();
        return ProofSetup({
            relay: address(relay),
            verifier: address(verifier),
            imageId: DEPOSIT_IMAGE,
            sealSelector: verifier.SELECTOR(),
            minWork: MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function _bonded() internal pure override returns (bool) {
        return true;
    }

    function _violationImageId() internal pure override returns (bytes32) {
        return SUITE_VIOLATION_IMAGE;
    }

    function setUp() public override {
        super.setUp();
        _bondAll();
        vaultId = _attest(account, VAULT_005);
        depositTag = _depositTagFor(account);
        relay.setBlock(ANCHOR, true, SLASH_WORK);
    }

    // ------------------------------------------------------------------ helpers

    function _journal(ViolationKind kind, bytes32 ref, bytes32 noteTag, uint64 value, bytes32 dest)
        internal
        view
        returns (bytes memory)
    {
        return _journalAt(kind, ref, noteTag, value, dest, 0);
    }

    function _journalAt(ViolationKind kind, bytes32 ref, bytes32 noteTag, uint64 value, bytes32 dest, uint8 output)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(
            ViolationJournal({
                kind: kind,
                anchor: ANCHOR,
                beneficiary: watcher,
                ref: ref,
                noteTag: noteTag,
                value: value,
                destCommitment: dest,
                outputIndex: output
            })
        );
    }

    function _prove(bytes memory journal) internal returns (bytes32 caseId) {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        return slasher.proveViolation(
            kind, journal, verifier.mockSeal(SUITE_VIOLATION_IMAGE, sha256(journal)), violationImage
        );
    }

    function _expectProveRevert(bytes memory journal, bytes memory err) internal {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        bytes memory seal = verifier.mockSeal(SUITE_VIOLATION_IMAGE, sha256(journal));
        vm.expectRevert(err);
        slasher.proveViolation(kind, journal, seal, violationImage);
    }

    function _partial(uint64 amount) internal returns (bytes32 claimId) {
        return _partialTo(amount, keccak256("zecured.tier3.dest"));
    }

    function _partialTo(uint64 amount, bytes32 dest) internal returns (bytes32 claimId) {
        vm.startPrank(account);
        if (!MORPHO.isAuthorized(account, address(registry))) MORPHO.setAuthorization(address(registry), true);
        claimId = registry.requestPartialRelease(vaultId, amount, dest);
        vm.stopPrank();
    }

    function _one(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _two(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](2);
        (r[0], r[1]) = (a, b);
    }

    function _totalBonds() internal view returns (uint256 sum) {
        for (uint8 i; i < 5; ++i) {
            sum += bond.bondOf(i);
        }
    }

    // ================================================================== bonds

    function test_bond_depositRules() public {
        assertTrue(bond.fullyBonded());
        assertEq(bond.effectiveBond(), 5 * MIN_BOND);
        usdc.mint(stranger, MIN_BOND);
        vm.startPrank(stranger);
        usdc.approve(address(bond), MIN_BOND);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.deposit(MIN_BOND);
        vm.stopPrank();
        // A signer may add to its bond (until a halt).
        usdc.mint(signerSet[0], MIN_BOND);
        vm.startPrank(signerSet[0]);
        usdc.approve(address(bond), MIN_BOND);
        bond.deposit(MIN_BOND);
        vm.stopPrank();
        assertEq(bond.bondOf(0), 2 * MIN_BOND);
        assertEq(bond.effectiveBond(), 5 * MIN_BOND, "capped by 5/3 of the three smallest");
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotTheSlasher.selector, stranger));
        bond.slash(bytes32(0), 0x1f, stranger);
    }

    // ================================================================== F1: an uncommitted spend

    function test_F1_slashesAllFiveAfterTheWindow_andHaltsMinting() public {
        bytes memory journal = _journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0));
        bytes32 caseId = slasher.caseIdOf(ViolationKind.Uncommitted, THEFT_REF, depositTag);
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.ViolationProven(
            caseId, ViolationKind.Uncommitted, vaultId, 0x1f, watcher, uint64(block.timestamp) + WINDOW
        );
        vm.prank(stranger); // anyone can submit a proof; the bounty is bound to the journal's beneficiary
        assertEq(_prove(journal), caseId);
        assertEq(slasher.openCaseCount(), 1);
        ViolationCase memory c = slasher.caseOf(caseId);
        assertEq(c.vaultId, vaultId);
        assertEq(c.signerBitmap, 0x1f);

        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredSlasher.ChallengeWindowOpen.selector, caseId, uint64(block.timestamp) + WINDOW
            )
        );
        slasher.executeSlash(caseId);
        _expectProveRevert(journal, abi.encodeWithSelector(IZecuredSlasher.CaseAlreadyOpen.selector, caseId));

        vm.warp(block.timestamp + WINDOW);
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.ViolationExecuted(caseId, 5 * MIN_BOND);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0);
        assertTrue(bond.halted());
        assertEq(bond.bountyOf(watcher), BOUNTY_CAP, "10 %, capped at half a bond");
        assertEq(compensator.funds(), 5 * MIN_BOND - BOUNTY_CAP, "the rest goes to the compensator");
        assertEq(usdc.balanceOf(address(compensator)), 5 * MIN_BOND - BOUNTY_CAP);
        assertEq(compensator.victim(vaultId).incident, compensator.currentIncident(), "the vault is a victim");
        assertEq(slasher.openCaseCount(), 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.CaseAlreadyExecuted.selector, caseId));
        slasher.executeSlash(caseId);

        // Minting stopped: a new deposit lands as a refund (T-02); the bounty is the watcher's to pull.
        address late = makeAddr("zecured.tier3.late");
        uint256 supply = zzec.totalSupply();
        _mockProof(_vaultIdFor(late), late, VAULT_001, _depositTagFor(late), keccak256("zecured.tier3.lateAnchor"));
        assertEq(zzec.totalSupply(), supply, "nothing minted after the slash");
        assertEq(uint8(_status(_vaultIdFor(late))), uint8(VaultStatus.Cancelled));
        vm.prank(watcher);
        bond.claimBounty();
        assertEq(usdc.balanceOf(watcher), BOUNTY_CAP);
        vm.prank(watcher);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NothingToClaim.selector, watcher));
        bond.claimBounty();
    }

    /// An honest payout whose commitment a Base reorg dropped is resubmitted within the window: the case is dismissed.
    function test_F1_dismissedIfTheCommitmentAppearsInTheWindow() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "resubmitted");
        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, c.payoutRef, depositTag, 0, bytes32(0)));

        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c))); // an honest node resubmits
        vm.warp(block.timestamp + WINDOW);
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.ViolationDismissed(caseId);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 5 * MIN_BOND);
        assertFalse(bond.halted());
        assertEq(slasher.openCaseCount(), 0);
        assertEq(uint8(slasher.caseOf(caseId).kind), uint8(ViolationKind.None), "deleted");
    }

    function test_F1_notProvableForACommittedSpend() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "committed");
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        _expectProveRevert(
            _journal(ViolationKind.Uncommitted, c.payoutRef, depositTag, 0, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Uncommitted, c.payoutRef)
        );
        // Nor for a note that is no lineage note (a refund of a non-conforming note needs no commitment).
        bytes32 foreign = keccak256("zecured.test.tier3.nonConforming");
        _expectProveRevert(
            _journal(ViolationKind.Uncommitted, THEFT_REF, foreign, 0, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Uncommitted, THEFT_REF)
        );
    }

    function test_F1_dismissedIfTheAnchorLeftTheBestChain() public {
        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0)));
        relay.setBlock(ANCHOR, false, 0); // the "theft" was in a block the honest chain dropped
        vm.warp(block.timestamp + WINDOW);
        vm.expectEmit(address(slasher));
        emit IZecuredSlasher.ViolationDismissed(caseId);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 5 * MIN_BOND);
    }

    /// Cross-language vector: the journal the Rust statement (tier2/statement, violations.rs) computes for a real
    /// Testnet payout of the TEST-ONLY vault 9, which spends that vault's deposit note. Activated here under the same
    /// deposit tag, the vault is slashable with exactly those bytes.
    function test_F1_theRustStatementsJournalOfARealSpend() public {
        bytes32 tag = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
        bytes32 anchor = 0xe47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000;
        bytes memory journal = bytes.concat(
            hex"0000000000000000000000000000000000000000000000000000000000000001",
            anchor,
            hex"000000000000000000000000bebebebebebebebebebebebebebebebebebebebe",
            hex"0a20962f2e701d49033b677adf918a7f8a38e3115e52f52ea3567140930e25ba",
            tag,
            hex"0000000000000000000000000000000000000000000000000000000000000000",
            hex"0000000000000000000000000000000000000000000000000000000000000000",
            hex"0000000000000000000000000000000000000000000000000000000000000000"
        );
        bytes32 vault9 = keccak256("zecured.tier3.vault9");
        intake.harnessActivate(vault9, makeAddr("zecured.tier3.vault9"), VAULT_001, tag);
        relay.setBlock(anchor, true, SLASH_WORK);

        ViolationJournal memory j = abi.decode(journal, (ViolationJournal));
        assertEq(uint8(j.kind), uint8(ViolationKind.Uncommitted));
        assertEq(j.beneficiary, address(0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe));
        bytes32 caseId = _prove(journal);
        assertEq(slasher.caseOf(caseId).vaultId, vault9);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0);
        assertEq(bond.bountyOf(address(0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe)), BOUNTY_CAP);
    }

    /// E2 with the real slasher: a deposit proven but not yet activated is spent (F1); the vault is not activated while
    /// the case is open. Dismissed (the theft's block left the best chain), the activation goes on.
    function test_E2_aCaseOnAPendingDepositHoldsItsActivation() public {
        bytes32 pVault = keccak256("zecured.tier3.pendingVault");
        address pAccount = makeAddr("zecured.tier3.pendingAccount");
        bytes32 pTag = keccak256("zecured.tier3.pendingTag");
        bytes32 opened = keccak256("zecured.tier3.opened");
        bytes32 depositAnchor = keccak256("zecured.tier3.depositAnchor");
        relay.setBlock(depositAnchor, true, 3_500_000);
        relay.setTipHeight(1_000);
        bytes memory journal = abi.encode(depositAnchor, pVault, pAccount, VAULT_001, pTag, opened);
        uint256[] memory all5 = new uint256[](5);
        (all5[0], all5[1], all5[2], all5[3], all5[4]) = (0, 1, 2, 3, 4);
        bytes[] memory openedSigs =
            _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), all5);
        intake.attestDepositWithProof(
            journal, verifier.mockSeal(DEPOSIT_IMAGE, sha256(journal)), proofs.imageId, openedSigs
        );

        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, THEFT_REF, pTag, 0, bytes32(0)));
        assertEq(slasher.openCasesOf(pVault), 1);
        relay.setTipHeight(1_006);
        vm.warp(block.timestamp + 150);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.ViolationCaseOpen.selector, pVault));
        intake.finalizeActivation(pVault);

        relay.setBlock(ANCHOR, false, 0);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId); // dismissed
        assertEq(slasher.openCasesOf(pVault), 0);
        intake.finalizeActivation(pVault);
        (address activated,,) = registry.vault(pVault);
        assertEq(activated, pAccount);
    }

    // ================================================================== F2: withheld change

    function test_F2_slashesOnlyTheRecordsSigners() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "withheld");
        registry.commitSpend(c, _signerSigs(registry.hashSpendCommit(c), _idx(1, 3, 4)));

        _expectProveRevert(
            _journal(ViolationKind.WithheldChange, c.payoutRef, c.changeTag, c.changeValue - 1, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.WithheldChange, c.payoutRef)
        );
        bytes32 caseId =
            _prove(_journal(ViolationKind.WithheldChange, c.payoutRef, c.changeTag, c.changeValue, bytes32(0)));
        assertEq(slasher.caseOf(caseId).signerBitmap, 0x1a, "signers 1, 3 and 4");
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(bond.bondOf(0), MIN_BOND);
        assertEq(bond.bondOf(2), MIN_BOND);
        assertEq(bond.bondOf(1) + bond.bondOf(3) + bond.bondOf(4), 0);
        uint256 bounty = 3 * MIN_BOND * BOUNTY_BPS / 10_000;
        assertEq(bond.bountyOf(watcher), bounty, "10 % of three bonds, under the cap");
        assertEq(compensator.funds(), 3 * MIN_BOND - bounty);
        assertTrue(bond.halted());
    }

    // ================================================================== F3: misdirected payment

    function test_F3_aClaimantProvesItWasNotPaid() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "misdirected");
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        bytes32 claimRef = c.claimRefs[0];
        bytes32 dest = keccak256("zecured.tier3.dest");
        uint64 expected = 1_000_000 - 10_000;

        // Paid in full: nothing to prove.
        _expectProveRevert(
            _journal(ViolationKind.Misdirected, claimRef, bytes32(0), expected, dest),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Misdirected, claimRef)
        );
        // Another recipient's commitment: not this claim's.
        _expectProveRevert(
            _journal(ViolationKind.Misdirected, claimRef, bytes32(0), 0, keccak256("other")),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Misdirected, claimRef)
        );
        // A note tag in an F3 journal (a second journal shape for the same violation) is refused.
        _expectProveRevert(
            _journal(ViolationKind.Misdirected, claimRef, depositTag, 0, dest),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Misdirected, claimRef)
        );

        bytes32 caseId = _prove(_journal(ViolationKind.Misdirected, claimRef, bytes32(0), expected - 1, dest));
        assertEq(slasher.caseOf(caseId).vaultId, vaultId);
        assertEq(slasher.caseOf(caseId).signerBitmap, 0x15, "the record's signers 0, 2, 4");
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(bond.bondOf(1) + bond.bondOf(3), 2 * MIN_BOND);
        assertTrue(bond.halted());
    }

    /// E1: two claims of the same borrower in one payout, to the same receiver. Colluders pay only the first; the
    /// second claim's committed output does not pay it, and its own output is the only one that counts: the other
    /// claim's payment cannot mask the theft.
    function test_F3_aSecondClaimToTheSameReceiverIsNotMasked() public {
        // Two destinations of the same receiver (two salts, so two commitments).
        bytes32 first = _partialTo(1_000_000, keccak256("zecured.tier3.dest.salt1"));
        bytes32 second = _partialTo(500_000, keccak256("zecured.tier3.dest.salt2"));
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _two(first, second), "same receiver");
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        bytes32 dest = keccak256("zecured.tier3.dest.salt2");
        uint8 secondOutput = c.claimOutputs[1];

        // The first claim's payment, 985,000 zat at output 0, is not the second claim's output.
        _expectProveRevert(
            _journalAt(ViolationKind.Misdirected, c.claimRefs[1], bytes32(0), 985_000, dest, c.claimOutputs[0]),
            abi.encodeWithSelector(IZecuredSlasher.NoViolation.selector, ViolationKind.Misdirected, c.claimRefs[1])
        );
        // The second claim's own output pays nothing to its receiver.
        bytes32 caseId =
            _prove(_journalAt(ViolationKind.Misdirected, c.claimRefs[1], bytes32(0), 0, dest, secondOutput));
        assertEq(slasher.caseOf(caseId).signerBitmap, 0x15);
    }

    // ================================================================== journal checks

    function test_prove_revertsOnABadJournalOrSeal() public {
        bytes memory journal = _journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0));
        bytes memory seal = verifier.mockSeal(SUITE_VIOLATION_IMAGE, sha256(journal));

        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.InvalidJournalLength.selector, 6 * 32));
        slasher.proveViolation(
            ViolationKind.Uncommitted,
            abi.encodePacked(bytes32(0), bytes32(0), bytes32(0), bytes32(0), bytes32(0), bytes32(0)),
            seal,
            violationImage
        );
        // A seal for a verifier no version names (T-07a): the suite's versions use the MOCK selector.
        bytes32 groth16Version = versions.versionIdOf(VersionKind.Violation, bytes4(0x73c457ba), violationImage);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, groth16Version));
        slasher.proveViolation(
            ViolationKind.Uncommitted, journal, abi.encodePacked(bytes4(0x73c457ba), bytes32(0)), violationImage
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredSlasher.KindMismatch.selector, ViolationKind.WithheldChange, ViolationKind.Uncommitted
            )
        );
        slasher.proveViolation(ViolationKind.WithheldChange, journal, seal, violationImage);
        // A seal of another journal (here: another beneficiary) does not verify.
        bytes memory other = abi.encode(
            ViolationJournal({
                kind: ViolationKind.Uncommitted,
                anchor: ANCHOR,
                beneficiary: stranger,
                ref: THEFT_REF,
                noteTag: depositTag,
                value: 0,
                destCommitment: bytes32(0),
                outputIndex: 0
            })
        );
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        slasher.proveViolation(ViolationKind.Uncommitted, other, seal, violationImage);
        // Kind None.
        _expectProveRevert(
            _journal(ViolationKind.None, THEFT_REF, depositTag, 0, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.UnknownKind.selector)
        );
    }

    function test_prove_revertsOnABadBeneficiaryOrAnchor() public {
        bytes memory toVault = abi.encode(
            ViolationJournal({
                kind: ViolationKind.Uncommitted,
                anchor: ANCHOR,
                beneficiary: account, // a vault's smart account never receives ETH
                ref: THEFT_REF,
                noteTag: depositTag,
                value: 0,
                destCommitment: bytes32(0),
                outputIndex: 0
            })
        );
        _expectProveRevert(toVault, abi.encodeWithSelector(IZecuredSlasher.InvalidBeneficiary.selector, account));

        relay.setBlock(ANCHOR, true, SLASH_WORK - 1); // a mint's depth, not a slash's
        _expectProveRevert(
            _journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.AnchorNotConfirmed.selector, ANCHOR, SLASH_WORK - 1)
        );
    }

    // ================================================================== the bonds' exit (S5, T-01)

    function test_fullExit_onlyOnceNothingIsAtStake() public {
        vm.prank(signerSet[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 1, 0, WINDOW));
        bond.requestUnbond(MIN_BOND);

        // The vault closes and its last claim is paid.
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
        _commitNext(vaultId, _one(claimId));
        registry.confirmPayout(
            vaultId, _one(claimId), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)))
        );
        (,,,, ClaimStatus status) = registry.claim(claimId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
        uint64 unlocksAt = uint64(block.timestamp) + WINDOW;

        vm.prank(signerSet[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 0, 0, unlocksAt));
        bond.requestUnbond(MIN_BOND);

        vm.warp(unlocksAt);
        vm.prank(signerSet[2]);
        bond.requestUnbond(MIN_BOND);
        (uint256 unbonding, uint64 readyAt) = bond.unbondingOf(2);
        assertEq(unbonding, MIN_BOND, "still slashable until withdrawn");
        assertEq(readyAt, unlocksAt + UNBONDING_PERIOD);
        assertEq(bond.bondOf(2), 0);
        assertFalse(bond.fullyBonded(), "no new vault activates now");
        address next = makeAddr("zecured.tier3.next");
        relay.setTipHeight(1_000);
        _mockProof(_vaultIdFor(next), next, VAULT_001, _depositTagFor(next), keccak256("zecured.tier3.nextAnchor"));
        relay.setTipHeight(1_000 + FINALIZE_DEPTH);
        vm.warp(block.timestamp + FINALIZE_DELAY);
        vm.expectRevert(IZecuredIntake.SignersNotBonded.selector);
        intake.finalizeActivation(_vaultIdFor(next));

        vm.startPrank(signerSet[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.UnbondingNotReady.selector, readyAt));
        bond.withdrawUnbonded(signerSet[2]);
        vm.warp(readyAt);
        bond.withdrawUnbonded(signerSet[2]);
        assertEq(usdc.balanceOf(signerSet[2]), MIN_BOND);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NothingUnbonding.selector, uint8(2)));
        bond.withdrawUnbonded(signerSet[2]);
        vm.stopPrank();
    }

    /// A partial exit keeps at least `minBond`, needs no open case, and asks the Intake whether the coverage still holds
    /// (here the coverage cap is off); vaults may stay open.
    function test_partialExit_keepsTheMinimum() public {
        usdc.mint(signerSet[1], MIN_BOND);
        vm.startPrank(signerSet[1]);
        usdc.approve(address(bond), MIN_BOND);
        bond.deposit(MIN_BOND);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.BelowMinBond.selector, MIN_BOND / 2, MIN_BOND));
        bond.requestUnbond(MIN_BOND + MIN_BOND / 2);
        bond.requestUnbond(MIN_BOND); // the vault is still open
        vm.stopPrank();
        assertEq(bond.bondOf(1), MIN_BOND);
        assertTrue(bond.fullyBonded());

        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0)));
        usdc.mint(signerSet[3], MIN_BOND);
        vm.startPrank(signerSet[3]);
        usdc.approve(address(bond), MIN_BOND);
        bond.deposit(MIN_BOND);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.CaseOpen.selector, uint256(1)));
        bond.requestUnbond(MIN_BOND);
        vm.stopPrank();
        vm.warp(block.timestamp + UNBONDING_PERIOD);
        vm.prank(signerSet[1]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.CaseOpen.selector, uint256(1)));
        bond.withdrawUnbonded(signerSet[1]); // no withdrawal while a case is open: the unbonding is still slashable
        slasher.executeSlash(caseId);
        (uint256 unbonding,) = bond.unbondingOf(1);
        assertEq(unbonding, 0, "slashed with the bond");
    }

    function test_fullExit_lockedWhileACaseIsOpen() public {
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
        _commitNext(vaultId, _one(claimId));
        registry.confirmPayout(
            vaultId, _one(claimId), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)))
        );
        // The vault's deposit tag is still a lineage note: a (false or true) F1 case can be opened.
        bytes32 caseId = _prove(_journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0)));
        vm.warp(block.timestamp + WINDOW);
        uint64 unlocksAt = registry.lastVaultSettledAt() + WINDOW;
        vm.prank(signerSet[0]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.FullExitLocked.selector, 0, 1, unlocksAt));
        bond.requestUnbond(MIN_BOND);
        slasher.executeSlash(caseId);
    }

    // ================================================================== wiring

    function test_slasher_constructorChecksTheWiring() public {
        address r = address(registry);
        address b = address(bond);
        address rl = address(relay);
        address v = address(verifier);
        address cmp = address(compensator);
        address ver = address(versions);
        // The bond and the Registry name another slasher (the one the fixture deployed).
        vm.expectRevert(IZecuredSlasher.InvalidConfig.selector);
        new ZecuredSlasher(r, b, cmp, rl, v, ver, SLASH_WORK, WINDOW);
        vm.expectRevert(IZecuredSlasher.ZeroWindow.selector);
        new ZecuredSlasher(r, b, cmp, rl, v, ver, SLASH_WORK, 0);
        vm.expectRevert(IZecuredSlasher.ZeroAddress.selector);
        new ZecuredSlasher(address(0), b, cmp, rl, v, ver, SLASH_WORK, WINDOW);
        vm.expectRevert(IZecuredSlasher.ZeroAddress.selector);
        new ZecuredSlasher(r, b, cmp, rl, v, address(0), SLASH_WORK, WINDOW);
        vm.expectRevert(IZecuredSlasher.InvalidConfig.selector);
        new ZecuredSlasher(r, b, cmp, rl, v, ver, 0, WINDOW);
    }
}
