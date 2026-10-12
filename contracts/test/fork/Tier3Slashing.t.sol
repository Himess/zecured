// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {SignerBond} from "../../src/SignerBond.sol";
import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {
    IZecuredSlasher,
    ViolationKind,
    ViolationJournal,
    ViolationCase
} from "../../src/interfaces/IZecuredSlasher.sol";
import {IZecuredSpendCommits} from "../../src/interfaces/IZecuredSpendCommits.sol";
import {DepositProofConfig, MintMode, ClaimStatus} from "../../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {SpendCommit, DepositAttestation} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Tier 3 end to end on a Base Sepolia fork (docs/TIER3.md §§ 6, 7): the five bonds, each violation kind proven
///         with the MOCK verifier against Registry records and a MOCK relay's anchors, the challenge window, dismissal,
///         the slash, the bounty, the halt and the bond withdrawal rules.
contract Tier3SlashingTest is ZecuredFixture {
    bytes32 internal constant DEPOSIT_IMAGE = keccak256("zecured.test.tier3.depositImage");
    bytes32 internal constant VIOLATION_IMAGE = keccak256("zecured.test.tier3.violationImage");
    uint256 internal constant SLASH_WORK = 38_000_000; // ~100 normal Testnet blocks (S4)
    uint64 internal constant WINDOW = 1 days;
    bytes32 internal constant ANCHOR = keccak256("zecured.test.tier3.anchor");
    bytes32 internal constant THEFT_REF = keccak256("zecured.test.tier3.theftPayoutRef");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    SignerBond internal bond;
    ZecuredSlasher internal slasher;

    address internal account = makeAddr("zecured.tier3.account");
    address internal watcher = makeAddr("zecured.tier3.watcher");
    bytes32 internal vaultId;
    bytes32 internal depositTag;

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        relay = new MockHeaderRelay();
        verifier = new MockRiscZeroVerifier();
        return DepositProofConfig({
            verifier: IRiscZeroVerifier(address(verifier)),
            imageId: DEPOSIT_IMAGE,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: 3_500_000,
            mintMode: MintMode.SignerAttested, // both paths: the tests open vaults by attestation
            sealSelector: verifier.SELECTOR(),
            finalizeDepth: 6,
            finalizeDelay: 150
        });
    }

    /// The bond at the next nonce, then ZZEC, Registry and Adapter (the fixture), then the slasher (setUp).
    function _signerBond() internal override returns (ISignerBond) {
        uint64 n = vm.getNonce(address(this));
        bond = new SignerBond(
            vm.computeCreateAddress(address(this), n + 2), vm.computeCreateAddress(address(this), n + 4), WINDOW
        );
        return ISignerBond(address(bond));
    }

    function setUp() public override {
        super.setUp();
        slasher = new ZecuredSlasher(
            address(registry),
            address(bond),
            address(relay),
            address(verifier),
            VIOLATION_IMAGE,
            verifier.SELECTOR(),
            SLASH_WORK,
            WINDOW
        );
        assertEq(bond.slasher(), address(slasher), "slasher address prediction");
        _bondAll();
        vaultId = _attest(account, VAULT_005);
        depositTag = _depositTagFor(account);
        relay.setBlock(ANCHOR, true, SLASH_WORK);
    }

    // ------------------------------------------------------------------ helpers

    function _bondAll() internal {
        for (uint256 i; i < 5; ++i) {
            vm.deal(signerSet[i], 1 ether);
            vm.prank(signerSet[i]);
            bond.deposit{value: 0.01 ether}();
        }
    }

    function _journal(ViolationKind kind, bytes32 ref, bytes32 noteTag, uint64 value, bytes32 dest)
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
                destCommitment: dest
            })
        );
    }

    function _prove(bytes memory journal) internal returns (bytes32 caseId) {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        return slasher.proveViolation(kind, journal, verifier.mockSeal(VIOLATION_IMAGE, sha256(journal)));
    }

    function _expectProveRevert(bytes memory journal, bytes memory err) internal {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        bytes memory seal = verifier.mockSeal(VIOLATION_IMAGE, sha256(journal));
        vm.expectRevert(err);
        slasher.proveViolation(kind, journal, seal);
    }

    function _partial(uint64 amount) internal returns (bytes32 claimId) {
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        claimId = registry.requestPartialRelease(vaultId, amount, keccak256("zecured.tier3.dest"));
        vm.stopPrank();
    }

    function _one(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _totalBonds() internal view returns (uint256 sum) {
        for (uint8 i; i < 5; ++i) {
            sum += bond.bondOf(i);
        }
    }

    // ================================================================== bonds

    function test_bond_depositRules() public {
        assertTrue(bond.fullyBonded());
        vm.deal(stranger, 1 ether);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NotASigner.selector, stranger));
        bond.deposit{value: 0.01 ether}();
        vm.prank(signerSet[0]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.AlreadyBonded.selector, 0));
        bond.deposit{value: 0.01 ether}();
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
        emit IZecuredSlasher.ViolationExecuted(caseId, 0.05 ether);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0);
        assertTrue(bond.halted());
        assertEq(bond.bountyOf(watcher), 0.005 ether, "10 %, capped at half a bond");
        assertEq(bond.compensationPool(), 0.045 ether);
        assertEq(slasher.openCaseCount(), 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.CaseAlreadyExecuted.selector, caseId));
        slasher.executeSlash(caseId);

        // Minting stopped; the bounty is the watcher's to pull.
        DepositAttestation memory att = _attestation(makeAddr("zecured.tier3.late"), VAULT_001);
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredSpendCommits.MintingHalted.selector);
        registry.attestDeposit(att, sigs);
        vm.prank(watcher);
        bond.claimBounty();
        assertEq(watcher.balance, 0.005 ether);
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
        assertEq(_totalBonds(), 0.05 ether);
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
        assertEq(_totalBonds(), 0.05 ether);
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
            hex"0000000000000000000000000000000000000000000000000000000000000000"
        );
        address vault9Account = makeAddr("zecured.tier3.vault9");
        DepositAttestation memory att = DepositAttestation({
            vaultId: keccak256("zecured.tier3.vault9"), account: vault9Account, amount: VAULT_001, depositTag: tag
        });
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att)));
        relay.setBlock(anchor, true, SLASH_WORK);

        ViolationJournal memory j = abi.decode(journal, (ViolationJournal));
        assertEq(uint8(j.kind), uint8(ViolationKind.Uncommitted));
        assertEq(j.beneficiary, address(0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe));
        bytes32 caseId = _prove(journal);
        assertEq(slasher.caseOf(caseId).vaultId, att.vaultId);
        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0);
        assertEq(bond.bountyOf(address(0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe)), 0.005 ether);
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
        assertEq(bond.bondOf(0), 0.01 ether);
        assertEq(bond.bondOf(2), 0.01 ether);
        assertEq(bond.bondOf(1) + bond.bondOf(3) + bond.bondOf(4), 0);
        assertEq(bond.bountyOf(watcher), 0.003 ether);
        assertEq(bond.compensationPool(), 0.027 ether);
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
        assertEq(bond.bondOf(1) + bond.bondOf(3), 0.02 ether);
        assertTrue(bond.halted());
    }

    // ================================================================== journal checks

    function test_prove_revertsOnABadJournalOrSeal() public {
        bytes memory journal = _journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0));
        bytes memory seal = verifier.mockSeal(VIOLATION_IMAGE, sha256(journal));

        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.InvalidJournalLength.selector, 6 * 32));
        slasher.proveViolation(
            ViolationKind.Uncommitted,
            abi.encodePacked(bytes32(0), bytes32(0), bytes32(0), bytes32(0), bytes32(0), bytes32(0)),
            seal
        );
        vm.expectRevert(abi.encodeWithSelector(IZecuredSlasher.InvalidSealSelector.selector, bytes4(0x73c457ba)));
        slasher.proveViolation(ViolationKind.Uncommitted, journal, abi.encodePacked(bytes4(0x73c457ba), bytes32(0)));
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredSlasher.KindMismatch.selector, ViolationKind.WithheldChange, ViolationKind.Uncommitted
            )
        );
        slasher.proveViolation(ViolationKind.WithheldChange, journal, seal);
        // A seal of another journal (here: another beneficiary) does not verify.
        bytes memory other = abi.encode(
            ViolationJournal({
                kind: ViolationKind.Uncommitted,
                anchor: ANCHOR,
                beneficiary: stranger,
                ref: THEFT_REF,
                noteTag: depositTag,
                value: 0,
                destCommitment: bytes32(0)
            })
        );
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        slasher.proveViolation(ViolationKind.Uncommitted, other, seal);
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
                destCommitment: bytes32(0)
            })
        );
        _expectProveRevert(toVault, abi.encodeWithSelector(IZecuredSlasher.InvalidBeneficiary.selector, account));

        relay.setBlock(ANCHOR, true, SLASH_WORK - 1); // a mint's depth, not a slash's
        _expectProveRevert(
            _journal(ViolationKind.Uncommitted, THEFT_REF, depositTag, 0, bytes32(0)),
            abi.encodeWithSelector(IZecuredSlasher.AnchorNotConfirmed.selector, ANCHOR, SLASH_WORK - 1)
        );
    }

    // ================================================================== bond withdrawal (S5)

    function test_withdraw_onlyOnceNothingIsAtStake() public {
        vm.prank(signerSet[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.WithdrawalLocked.selector, 1, 0, WINDOW));
        bond.withdraw(payable(signerSet[2]));

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
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.WithdrawalLocked.selector, 0, 0, unlocksAt));
        bond.withdraw(payable(signerSet[2]));

        vm.warp(unlocksAt);
        uint256 before = signerSet[2].balance;
        vm.prank(signerSet[2]);
        bond.withdraw(payable(signerSet[2]));
        assertEq(signerSet[2].balance, before + 0.01 ether);
        assertFalse(bond.fullyBonded(), "no new vault opens now");
        DepositAttestation memory att = _attestation(makeAddr("zecured.tier3.next"), VAULT_001);
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(IZecuredSpendCommits.SignersNotBonded.selector);
        registry.attestDeposit(att, sigs);
        vm.prank(signerSet[2]);
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.NoBond.selector, 2));
        bond.withdraw(payable(signerSet[2]));
    }

    function test_withdraw_lockedWhileACaseIsOpen() public {
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
        vm.expectRevert(abi.encodeWithSelector(ISignerBond.WithdrawalLocked.selector, 0, 1, unlocksAt));
        bond.withdraw(payable(signerSet[0]));
        slasher.executeSlash(caseId);
    }

    // ================================================================== wiring

    function test_slasher_constructorChecksTheWiring() public {
        bytes4 selector = verifier.SELECTOR();
        address r = address(registry);
        address b = address(bond);
        address rl = address(relay);
        address v = address(verifier);
        // The bond names another slasher (the one deployed in setUp).
        vm.expectRevert(IZecuredSlasher.InvalidConfig.selector);
        new ZecuredSlasher(r, b, rl, v, VIOLATION_IMAGE, selector, SLASH_WORK, WINDOW);
        vm.expectRevert(IZecuredSlasher.ZeroWindow.selector);
        new ZecuredSlasher(r, b, rl, v, VIOLATION_IMAGE, selector, SLASH_WORK, 0);
        vm.expectRevert(IZecuredSlasher.ZeroAddress.selector);
        new ZecuredSlasher(address(0), b, rl, v, VIOLATION_IMAGE, selector, SLASH_WORK, WINDOW);
        vm.expectRevert(IZecuredSlasher.InvalidConfig.selector);
        new ZecuredSlasher(r, b, rl, v, bytes32(0), selector, SLASH_WORK, WINDOW);
    }
}
