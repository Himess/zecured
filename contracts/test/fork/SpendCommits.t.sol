// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IZecuredSignerErrors} from "../../src/interfaces/v3/IZecuredSignerErrors.sol";
import {
    IZecuredSpendCommitsEvents,
    LineageNote,
    SpendRecord,
    ClaimPayment
} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredRegistry, ClaimKind, ClaimStatus, VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ZecuredEIP712, SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";

/// @notice Tier 3 commitments (docs/TIER3.md § 4) against the real Morpho Blue on a Base Sepolia fork: what
///         `commitSpend` records, every rule it enforces, the claim binding that stops a claim from being paid twice,
///         `confirmPayout`'s new requirement, the open-vault count the bonds wait on, and the halt.
contract SpendCommitsTest is ZecuredFixture {
    bytes32 internal constant PARTIAL_COMMITMENT = keccak256("zecured.test.commitment.partial");
    uint256 internal partials;
    /// Signers 0, 2 and 4 of the sorted set (`_threeSigs`).
    uint8 internal constant BITMAP_0_2_4 = 0x15;
    /// Storage slot of the Registry's `_claims` mapping (`forge inspect ZecuredRegistry storageLayout`).
    uint256 internal constant CLAIMS_SLOT = 4;

    address internal account;
    bytes32 internal vaultId;
    bytes32 internal depositTag;
    MockSignerBond internal mockBond;

    function _mockBondAndSlasher() internal override returns (address) {
        mockBond = new MockSignerBond();
        return address(mockBond);
    }

    function setUp() public override {
        super.setUp();
        account = makeAddr("zecured.commits.account");
        vaultId = _attest(account, VAULT_005);
        depositTag = _depositTagFor(account);
        vm.prank(account);
        MORPHO.setAuthorization(address(registry), true);
    }

    /// Each partial release gets its own destination commitment (a fresh salt, as the app and the CLI do): a payout
    /// never names two claims of one commitment (RT-1).
    function _partial(uint64 amount) internal returns (bytes32 claimId) {
        vm.prank(account);
        claimId = registry.requestPartialRelease(vaultId, amount, keccak256(abi.encode(PARTIAL_COMMITMENT, partials++)));
    }

    function _one(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _two(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](2);
        (r[0], r[1]) = (a, b);
    }

    function _send(SpendCommit memory c) internal {
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
    }

    function _expectRevertOn(SpendCommit memory c, bytes memory err) internal {
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(err);
        registry.commitSpend(c, sigs);
    }

    /// White box: overwrites a claim's amount (the low 8 bytes of the struct's third slot), to make a dust claim that
    /// the public flows only reach through a liquidation remainder. Checked through the view right after.
    function _setClaimAmount(bytes32 claimId, uint64 amount) internal {
        bytes32 slot = bytes32(uint256(keccak256(abi.encode(claimId, CLAIMS_SLOT))) + 2);
        uint256 word = uint256(vm.load(address(registry), slot));
        word = (word & ~uint256(type(uint64).max)) | amount;
        vm.store(address(registry), slot, bytes32(word));
        (,, uint64 stored,,) = registry.claim(claimId);
        assertEq(stored, amount, "claim layout");
    }

    // ================================================================== what a commitment records

    function test_commitSpend_recordsThePayout() public {
        bytes32 claimId = _partial(1_000_000);
        assertEq(registry.lineageOf(depositTag).value, VAULT_005, "the deposit is the lineage until a payout");
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "recorded");
        assertEq(c.changeValue, VAULT_005 - 1_000_000);

        vm.expectEmit(address(registry));
        emit IZecuredSpendCommitsEvents.SpendCommitted(
            vaultId, c.payoutRef, depositTag, c.claimIds, c.claimRefs, c.changeTag, c.changeValue, BITMAP_0_2_4
        );
        vm.prank(stranger); // anyone may relay it
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));

        SpendRecord memory r = registry.spendRecord(c.payoutRef);
        assertEq(r.recordHash, ZecuredEIP712.hashSpendCommit(c), "what a node compares with the record it signed");
        assertEq(r.vaultId, vaultId);
        assertEq(r.inputTag, depositTag);
        assertEq(r.changeTag, c.changeTag);
        assertEq(r.changeValue, VAULT_005 - 1_000_000);
        assertEq(r.claimCount, 1);
        assertEq(r.signerBitmap, BITMAP_0_2_4);

        LineageNote memory change = registry.lineageOf(c.changeTag);
        assertEq(change.vaultId, vaultId, "the change is the vault's next lineage note");
        assertEq(change.value, VAULT_005 - 1_000_000);
        ClaimPayment memory p = registry.claimPayment(c.claimRefs[0]);
        assertEq(p.claimId, claimId);
        assertEq(p.payoutRef, c.payoutRef);
        assertEq(p.expected, 1_000_000 - 10_000, "one claim plus change: 2 actions, 10,000 zat");
        assertEq(registry.claimInput(claimId), depositTag);

        // The payout lands; the claim is confirmed paid.
        registry.confirmPayout(
            vaultId, _one(claimId), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)))
        );
        (,,,, ClaimStatus status) = registry.claim(claimId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
    }

    /// The fee is ZIP 317's for the committed shape: 5,000 zat per output (claims plus change), at least two; only
    /// `claimIds[0]` bears it.
    function test_commitSpend_feeFollowsTheShape() public {
        bytes32 a = _partial(1_000_000);
        bytes32 b = _partial(1_500_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _two(a, b), "two claims");
        _send(c);
        assertEq(registry.claimPayment(c.claimRefs[0]).expected, 1_000_000 - 15_000, "3 outputs");
        assertEq(registry.claimPayment(c.claimRefs[1]).expected, 1_500_000, "only the first claim bears the fee");
        assertEq(registry.lineageOf(c.changeTag).value, VAULT_005 - 2_500_000);

        // A payout that empties the vault: one output, still two actions' fee, and no change note.
        bytes32[] memory both = _two(a, b);
        registry.confirmPayout(vaultId, both, _threeSigs(registry.hashPayoutConfirmation(vaultId, both)));
        vm.prank(account);
        bytes32 rest = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        SpendCommit memory last = _spendCommit(vaultId, c.changeTag, _one(rest), "last");
        assertEq(last.changeValue, 0);
        assertEq(last.changeTag, bytes32(0));
        _send(last);
        assertEq(registry.claimPayment(last.claimRefs[0]).expected, VAULT_005 - 2_500_000 - 10_000);
        assertEq(registry.spendRecord(last.payoutRef).changeTag, bytes32(0));
    }

    // ================================================================== claim binding (the claim-reuse attack)

    /// Colluding signers commit and mine a payout of C from t0, never confirm C, then commit C again from the change
    /// t1 to pay it twice. The first commitment binds C to t0.
    function test_commitSpend_aClaimCannotBePaidFromTwoLineageNotes() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory first = _spendCommit(vaultId, depositTag, _one(claimId), "first");
        _send(first);
        SpendCommit memory again = _spendCommit(vaultId, first.changeTag, _one(claimId), "again");
        _expectRevertOn(
            again,
            abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimBoundToAnotherInput.selector, claimId, depositTag)
        );

        // A rebuild of the same payout from the same note (the first transaction expired) stays possible.
        SpendCommit memory rebuilt = _spendCommit(vaultId, depositTag, _one(claimId), "rebuilt");
        _send(rebuilt);
        assertEq(registry.claimInput(claimId), depositTag);
        assertEq(registry.spendRecord(rebuilt.payoutRef).inputTag, depositTag);
    }

    // ================================================================== rules: shape

    function test_commitSpend_revertsOnABadShape() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c;

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.payoutRef = bytes32(0);
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ZeroPayoutRef.selector));

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.inputTag = keccak256("zecured.test.notLineage");
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.NotALineageNote.selector, vaultId, c.inputTag)
        );

        // Another vault's lineage note.
        address other = makeAddr("zecured.commits.other");
        bytes32 otherVault = _attest(other, VAULT_001);
        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.inputTag = _depositTagFor(other);
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.NotALineageNote.selector, vaultId, c.inputTag)
        );
        c.vaultId = otherVault; // ... and claimed for that vault: the claim is not its own
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, claimId));

        c = _spendCommit(vaultId, depositTag, new bytes32[](0), "shape");
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidClaimCount.selector, 0, 0));

        bytes32[] memory nine = new bytes32[](9);
        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.claimIds = nine;
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidClaimCount.selector, 9, 1));

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.claimRefs = new bytes32[](2);
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidClaimCount.selector, 1, 2));

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.changeTag = bytes32(0); // change without a tag
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidChange.selector, bytes32(0), c.changeValue)
        );

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        c.changeTag = depositTag; // a tag that is already a lineage note
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidChange.selector, depositTag, c.changeValue)
        );

        c = _spendCommit(vaultId, depositTag, _one(claimId), "shape");
        _send(c);
        c.changeTag = keccak256("zecured.test.anotherChange");
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.PayoutAlreadyCommitted.selector, c.payoutRef)
        );
    }

    /// Conservation: the lineage note's value is exactly the claims plus the change.
    function test_commitSpend_revertsUnlessValueIsConserved() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "conservation");
        c.changeValue -= 1; // one zatoshi kept back
        _expectRevertOn(
            c,
            abi.encodeWithSelector(
                IZecuredSpendCommitsEvents.ConservationBroken.selector, VAULT_005, 1_000_000, VAULT_005 - 1_000_000 - 1
            )
        );
        c.changeValue += 2;
        _expectRevertOn(
            c,
            abi.encodeWithSelector(
                IZecuredSpendCommitsEvents.ConservationBroken.selector, VAULT_005, 1_000_000, VAULT_005 - 1_000_000 + 1
            )
        );
    }

    // ================================================================== rules: claims

    function test_commitSpend_revertsOnBadClaims() public {
        bytes32 a = _partial(1_000_000);
        bytes32 b = _partial(1_000_000);
        SpendCommit memory c;

        c = _spendCommit(vaultId, depositTag, _two(a, a), "claims");
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.DuplicateClaim.selector, a));

        bytes32 unknown = keccak256("zecured.test.unknownClaim");
        c = _spendCommit(vaultId, depositTag, _one(a), "claims");
        c.claimIds[0] = unknown;
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, unknown));

        // A claim ref that is zero, or already used by another payout.
        c = _spendCommit(vaultId, depositTag, _one(a), "claims");
        c.claimRefs[0] = bytes32(0);
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimRefAlreadyUsed.selector, bytes32(0)));
        SpendCommit memory paid = _spendCommit(vaultId, depositTag, _one(a), "paid");
        _send(paid);
        // A rebuild of the same payout (same input, same claim set) with the first one's claim ref.
        c = _spendCommit(vaultId, depositTag, _one(a), "claims");
        c.claimRefs[0] = paid.claimRefs[0];
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimRefAlreadyUsed.selector, c.claimRefs[0])
        );

        // A claim already confirmed paid, named again from the paying payout's change.
        registry.confirmPayout(vaultId, _one(a), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(a))));
        c = _spendCommit(vaultId, paid.changeTag, _one(b), "claims");
        c.claimIds = _two(b, a);
        c.claimRefs = _two(keccak256("ref b"), keccak256("ref a"));
        c.claimOutputs = new uint8[](2);
        c.claimOutputs[1] = 1;
        c.changeValue = VAULT_005 - 3_000_000;
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, a));
    }

    /// E1: each claim names its own output of the payout; two claims never share one, and every claim names one.
    function test_commitSpend_eachClaimHasItsOwnOutput() public {
        bytes32 a = _partial(1_000_000);
        bytes32 b = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _two(a, b), "outputs");
        c.claimOutputs[1] = c.claimOutputs[0];
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.DuplicateClaimOutput.selector, c.claimOutputs[0])
        );
        c.claimOutputs = new uint8[](1);
        _expectRevertOn(c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.InvalidClaimCount.selector, 2, 1));

        c = _spendCommit(vaultId, depositTag, _two(a, b), "outputs");
        (c.claimOutputs[0], c.claimOutputs[1]) = (3, 1);
        _send(c);
        assertEq(registry.claimPayment(c.claimRefs[0]).outputIndex, 3);
        assertEq(registry.claimPayment(c.claimRefs[1]).outputIndex, 1);
    }

    /// Cross-language vector (red-team review § 4): the fee claim 0 bears for every shape, 1 to 8 claims with and
    /// without change, is the table `zecured_zcash_ops::spend::zip317_fee(1, outputs)` asserts in Rust.
    function test_vector_zip317FeeTable() public {
        uint64[10] memory feeFor = [uint64(0), 10_000, 10_000, 15_000, 20_000, 25_000, 30_000, 35_000, 40_000, 45_000];
        for (uint256 n = 1; n <= 8; ++n) {
            for (uint256 change; change < 2; ++change) {
                uint256 snap = vm.snapshotState();
                bytes32[] memory ids = new bytes32[](n);
                for (uint256 i; i < n; ++i) {
                    bytes32 dest = keccak256(abi.encode("fee table", i));
                    vm.prank(account);
                    ids[i] = (i == n - 1 && change == 0)
                        ? registry.requestRelease(vaultId, dest)
                        : registry.requestPartialRelease(vaultId, 200_000, dest);
                }
                SpendCommit memory c = _spendCommit(vaultId, depositTag, ids, keccak256(abi.encode("fee", n, change)));
                assertEq(c.changeValue != 0, change == 1);
                _send(c);
                (,, uint64 amount,,) = registry.claim(ids[0]);
                assertEq(amount - registry.claimPayment(c.claimRefs[0]).expected, feeFor[n + change]);
                vm.revertToState(snap);
            }
        }
    }

    /// The fee-bearing claim must cover the fee: the signers never pay dust (it stays in the vault).
    function test_commitSpend_revertsWhenTheFeeBearerIsDust() public {
        bytes32 a = _partial(1_000_000);
        _setClaimAmount(a, 10_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(a), "dust");
        _expectRevertOn(
            c, abi.encodeWithSelector(IZecuredSpendCommitsEvents.FeeBearerIsDust.selector, a, 10_000, 10_000)
        );
        _setClaimAmount(a, 10_001);
        c = _spendCommit(vaultId, depositTag, _one(a), "dust");
        _send(c);
        assertEq(registry.claimPayment(c.claimRefs[0]).expected, 1);
    }

    // ================================================================== rules: signatures (I6)

    function test_commitSpend_revertsOnBadSignatureSets() public {
        bytes32 claimId = _partial(1_000_000);
        SpendCommit memory c = _spendCommit(vaultId, depositTag, _one(claimId), "sigs");
        bytes32 digest = registry.hashSpendCommit(c);
        assertEq(
            digest, keccak256(abi.encodePacked(hex"1901", registry.domainSeparator(), ZecuredEIP712.hashSpendCommit(c)))
        );

        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 2));
        registry.commitSpend(c, _signerSigs(digest, _idx(0, 1)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.SignaturesNotSorted.selector, 2));
        registry.commitSpend(c, _signerSigs(digest, _idx(0, 2, 2)));
        // Signed over another record (here: another change value).
        SpendCommit memory other = _spendCommit(vaultId, depositTag, _one(claimId), "sigs");
        other.changeValue -= 1;
        bytes[] memory otherSigs = _threeSigs(registry.hashSpendCommit(other));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.commitSpend(c, otherSigs);

        // The bitmap names exactly the signers whose signatures were used.
        registry.commitSpend(c, _signerSigs(digest, _idx(1, 2, 3)));
        assertEq(registry.spendRecord(c.payoutRef).signerBitmap, 0x0e);
    }

    // ================================================================== open vaults (bond withdrawal, S5)

    function test_openVaultCount_dropsWhenTheLastClaimOfAClosedVaultIsPaid() public {
        assertEq(registry.openVaultCount(), 1);
        assertEq(registry.lastVaultSettledAt(), 0);
        vm.prank(account);
        bytes32 claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        assertEq(registry.openVaultCount(), 1, "closed on Base, but its ZEC is still in the vault");
        _commitNext(vaultId, _one(claimId));
        assertEq(registry.openVaultCount(), 1, "committed, not yet paid");

        vm.warp(block.timestamp + 1 hours);
        registry.confirmPayout(
            vaultId, _one(claimId), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)))
        );
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lastVaultSettledAt(), block.timestamp);
    }

    function test_openVaultCount_aPartialPayoutKeepsTheVaultOpen() public {
        bytes32 claimId = _partial(1_000_000);
        _commitNext(vaultId, _one(claimId));
        registry.confirmPayout(
            vaultId, _one(claimId), _threeSigs(registry.hashPayoutConfirmation(vaultId, _one(claimId)))
        );
        assertEq(registry.openVaultCount(), 1, "still active");
        assertEq(registry.lastVaultSettledAt(), 0);
    }

    // ================================================================== halt (a slash stops minting)

    /// After a slash (the bond halted), a new deposit lands as a refund and nothing is minted (T-02), while the
    /// existing vaults keep being released and paid out.
    function test_halt_stopsMintingButNotPayouts() public {
        address a = makeAddr("zecured.commits.haltA");
        bytes32 vaultA = _attest(a, VAULT_001);

        mockBond.setHalted(true);
        address b = makeAddr("zecured.commits.haltB");
        uint256 supply = zzec.totalSupply();
        _mockProof(_vaultIdFor(b), b, VAULT_001, _depositTagFor(b), keccak256("zecured.commits.haltAnchor"));
        assertEq(zzec.totalSupply(), supply, "nothing minted after the halt");
        // R-12: the refund waits for the relay like an activation, then opens.
        assertEq(intake.pendingRefund(_depositTagFor(b)).account, b, "the deposit landed as a pending refund");
        _finalizeRefund(_depositTagFor(b));
        assertEq(uint8(_status(_vaultIdFor(b))), uint8(VaultStatus.Cancelled), "the deposit landed as a refund");

        // The existing vault can still be released and paid out.
        vm.startPrank(a);
        MORPHO.setAuthorization(address(registry), true);
        bytes32 releaseId = registry.requestRelease(vaultA, RELEASE_COMMITMENT);
        vm.stopPrank();
        SpendCommit memory c = SpendCommit({
            vaultId: vaultA,
            inputTag: _depositTagFor(a),
            claimIds: _one(releaseId),
            claimRefs: _one(keccak256("zecured.test.haltRef")),
            claimOutputs: new uint8[](1),
            payoutRef: keccak256("zecured.test.haltPayout"),
            changeTag: bytes32(0),
            changeValue: 0
        });
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        registry.confirmPayout(
            vaultA, _one(releaseId), _threeSigs(registry.hashPayoutConfirmation(vaultA, _one(releaseId)))
        );
        (,,,, ClaimStatus status) = registry.claim(releaseId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
    }
}
