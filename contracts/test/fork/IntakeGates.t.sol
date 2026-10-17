// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IZecuredRegistry, VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {
    IZecuredIntake,
    MintBlock,
    RefundReason,
    WorkTier,
    PendingActivation
} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IZecuredRefunds, IZecuredRefundsErrors} from "../../src/interfaces/v3/IZecuredRefunds.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {MockCapPrice} from "../mocks/MockCapPrice.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The Intake's gates of minting: the coverage cap (T-01) at the high price with its absolute maximum, the bonds,
///         the halt and the automatic pause on an open case (T-03); its work tiers (T-08) and refund floor (T-02); v4: a
///         full cap delays a new vault's proof instead of cancelling the vault, and unclaimed refunds leave the cap
///         once dormant (R-11). With the MOCK relay and verifier, a MOCK bond and slasher, and a MOCK cap price.
contract IntakeGatesTest is ZecuredFixture {
    uint64 internal constant BIG = 5_000_000; // 0.05 ZEC
    uint64 internal constant SMALL = 1_000_000; // 0.01 ZEC
    uint256 internal constant BOND_FOR_A_TENTH = 4e6; // 4 USDC: 0.1 ZEC at $40, with k = 100 %
    uint64 internal constant ABSOLUTE_CAP = 30_000_000; // 0.3 ZEC
    uint256 internal constant TIP = 4_500_000;

    MockSignerBond internal gates;
    MockCapPrice internal capPrice;
    uint256 internal anchorNonce;

    function _mockBondAndSlasher() internal override returns (address) {
        gates = new MockSignerBond();
        gates.setEffectiveBond(BOND_FOR_A_TENTH);
        return address(gates);
    }

    function _capSetup() internal override returns (CapSetup memory) {
        capPrice = new MockCapPrice(PRICE_40);
        return CapSetup({
            coverageBps: 10_000, maxTotalZat: ABSOLUTE_CAP, capPriceMaxAge: 1 hours, capOracle: address(capPrice)
        });
    }

    /// Up to 0.01 ZEC needs MIN_WORK on top of the anchor, up to 0.1 ZEC four times that; nothing larger lands.
    function _workTiers() internal pure override returns (WorkTier[] memory tiers) {
        tiers = new WorkTier[](2);
        tiers[0] = WorkTier({maxAmountZat: SMALL, minWork: MIN_WORK});
        tiers[1] = WorkTier({maxAmountZat: 10_000_000, minWork: 4 * MIN_WORK});
    }

    /// The mainnet setting.
    function _autoPauseOnCase() internal pure override returns (bool) {
        return true;
    }

    // ------------------------------------------------------------------ helpers

    /// A proof of `amount` for `vaultId` (journal v1, MOCK seal, the five signatures) anchored at a fresh header with
    /// `work` on top of it.
    function _proof(bytes32 vaultId, address account, uint64 amount, bytes32 tag, uint256 work)
        internal
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        bytes32 anchor = keccak256(abi.encode("zecured.gates.anchor", anchorNonce++));
        MockHeaderRelay(address(headerRelay)).setBlock(anchor, true, work);
        MockHeaderRelay(address(headerRelay)).setTipHeight(TIP);
        bytes32 opened = keccak256(abi.encode("zecured.gates.opened", vaultId));
        journal = abi.encode(anchor, vaultId, account, amount, tag, opened);
        seal = MockRiscZeroVerifier(address(proofVerifier)).mockSeal(proofs.imageId, sha256(journal));
        uint256[] memory five = new uint256[](5);
        (five[0], five[1], five[2], five[3], five[4]) = (0, 1, 2, 3, 4);
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), five);
    }

    /// The deposit of a new vault for the account named `name`, with the work its amount needs.
    function _deposit(string memory name, uint64 amount) internal returns (bytes32 vaultId) {
        address account = makeAddr(name);
        vaultId = _vaultIdFor(account);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(vaultId, account, amount, _depositTagFor(account), intake.minConfirmationWorkFor(amount));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function _ready(bytes32 vaultId) internal {
        PendingActivation memory p = intake.pendingActivation(vaultId);
        MockHeaderRelay(address(headerRelay)).setTipHeight(p.readyHeight);
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
    }

    function _finalize(bytes32 vaultId) internal {
        _ready(vaultId);
        intake.finalizeActivation(vaultId);
    }

    function _pause(uint64 until) internal {
        uint256 nonce = intake.guardNonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        intake.pauseMinting(until, nonce, deadline, _threeSigs(intake.hashPauseMinting(until, nonce, deadline)));
    }

    /// A second deposit `tag` to `name`'s known vault: it lands as a pending refund (R-12), opened here.
    function _refundOf(string memory name, bytes32 vaultId, uint64 amount, bytes32 tag) internal returns (bytes32) {
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(vaultId, makeAddr(name), amount, tag, intake.minConfirmationWorkFor(amount));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        return _finalizeRefund(tag);
    }

    function _release(string memory name, bytes32 vaultId) internal returns (bytes32 claimId) {
        address account = makeAddr(name);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
    }

    // ------------------------------------------------------------------ T-01: the coverage cap

    function test_config() public view {
        assertEq(intake.coverageBps(), 10_000);
        assertEq(intake.maxTotalZat(), ABSOLUTE_CAP);
        assertEq(intake.capPriceMaxAge(), 1 hours);
        assertEq(intake.capOracle(), address(capPrice));
        assertTrue(intake.autoPauseOnCase());
        assertEq(intake.signerBond(), address(gates));
        assertEq(intake.slasher(), address(gates));
        assertEq(intake.minRefundZat(), MIN_REFUND);
    }

    /// Locked and pending zatoshi, valued at the cap price, stay within k times the effective bond: two 0.05 ZEC
    /// deposits fill 4 USDC exactly at $40; a third new vault's proof waits (R-11: it reverts and is sent again once
    /// there is room, instead of cancelling the vault for good), and the room reads zero.
    function test_cap_admitsUpToTheEffectiveBond_thenNewVaultsWait() public {
        assertEq(intake.capRoom(), 10_000_000);
        assertEq(uint8(intake.mintBlock(BIG)), uint8(MintBlock.None));
        bytes32 a = _deposit("gates.a", BIG);
        bytes32 b = _deposit("gates.b", BIG);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Pending));
        assertEq(uint8(_status(b)), uint8(VaultStatus.Pending), "exactly at the cap is within it");
        assertEq(intake.pendingTotal(), 2 * BIG);
        assertEq(intake.capRoom(), 0);
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.CapExceeded));

        address c = makeAddr("gates.c");
        bytes32 cVault = _vaultIdFor(c);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(cVault, c, SMALL, _depositTagFor(c), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapExceeded.selector, SMALL));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(cVault)), uint8(VaultStatus.None), "R-11: the vault is not cancelled");
        assertFalse(registry.depositTagUsed(_depositTagFor(c)));

        // Room again (the bonds grew): the same proof lands, pending.
        gates.setEffectiveBond(BOND_FOR_A_TENTH + 1e6);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(cVault)), uint8(VaultStatus.Pending));
    }

    /// The cap counts what the vaults hold for Base: the zZEC supply, pending activations and pending claims. A payout
    /// frees room once it is confirmed.
    function test_cap_countsSupplyPendingAndClaims() public {
        bytes32 a = _deposit("gates.a", BIG);
        assertEq(intake.capRoom(), 10_000_000 - BIG, "a pending activation");
        _finalize(a);
        assertEq(registry.lockedZatoshi(), BIG, "the supply");
        assertEq(intake.capRoom(), 10_000_000 - BIG);

        bytes32 claimId = _release("gates.a", a);
        assertEq(zzec.totalSupply(), 0);
        assertEq(registry.lockedZatoshi(), BIG, "a pending claim");
        assertEq(intake.capRoom(), 10_000_000 - BIG);
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = claimId;
        _commitNext(a, ids);
        registry.confirmPayout(a, ids, _threeSigs(registry.hashPayoutConfirmation(a, ids)));
        assertEq(registry.lockedZatoshi(), 0);
        assertEq(intake.capRoom(), 10_000_000);
    }

    /// The cap reads the high side of the price: when it doubles, the bonds cover half as many zatoshi. The market's own
    /// price plays no part.
    function test_cap_atTheHighPrice() public {
        capPrice.set(2 * PRICE_40);
        assertEq(intake.capRoom(), 5_000_000);
        _deposit("gates.a", BIG);
        assertEq(intake.capRoom(), 0);
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.CapExceeded));
        oracle.setPrice(PRICE_40 / 4);
        assertEq(intake.capRoom(), 0);
    }

    /// The absolute cap binds whatever the bonds.
    function test_cap_absoluteMaximum() public {
        gates.setEffectiveBond(1_000_000e6);
        assertEq(intake.capRoom(), ABSOLUTE_CAP);
        for (uint256 i; i < 6; ++i) {
            _deposit(string.concat("gates.big", vm.toString(i)), BIG);
        }
        assertEq(intake.pendingTotal(), ABSOLUTE_CAP);
        assertEq(intake.capRoom(), 0);
        address seventh = makeAddr("gates.big6");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(_vaultIdFor(seventh), seventh, SMALL, _depositTagFor(seventh), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapExceeded.selector, SMALL));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    /// A stale cap price delays an activatable proof (it reverts and can be sent again later), never turns it into a
    /// refund; a proof that lands as a refund anyway never reads the price, and a pending activation finalizes without
    /// it. The views report it without reverting where they can.
    function test_cap_stalePrice() public {
        bytes32 a = _deposit("gates.a", SMALL);
        skip(1 hours + 1);
        uint256 updatedAt = capPrice.updatedAt();
        assertEq(intake.capRoom(), 0);
        assertFalse(intake.coverageHoldsWith(1_000_000e6), "a stale price never frees a bond");
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapPriceStale.selector, updatedAt));
        intake.mintBlock(SMALL);

        // A new vault's proof waits.
        address b = makeAddr("gates.b");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapPriceStale.selector, updatedAt));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);

        // A second deposit to a known vault lands as a (pending, R-12) refund regardless.
        (journal, seal, sigs) = _proof(a, makeAddr("gates.a"), SMALL, keccak256("gates.second"), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(intake.pendingRefund(keccak256("gates.second")).amount, SMALL);

        // The pending activation finalizes regardless of the cap price, and so does the refund.
        _finalize(a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Active));
        _finalizeRefund(keccak256("gates.second"));
        assertEq(registry.claimCount(a), 1);

        // Once the price is fresh again, the waiting proof lands.
        capPrice.set(PRICE_40);
        (journal, seal, sigs) = _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(_vaultIdFor(b))), uint8(VaultStatus.Pending));
    }

    /// The bonds' partial unbonding asks whether the locked value stays covered by a given effective bond (rounded
    /// against the bonds).
    function test_coverageHoldsWith() public {
        _deposit("gates.a", BIG); // 0.05 ZEC = 2 USDC at $40
        assertTrue(intake.coverageHoldsWith(2e6));
        assertFalse(intake.coverageHoldsWith(2e6 - 1));
        capPrice.set(PRICE_40 + 1);
        assertFalse(intake.coverageHoldsWith(2e6), "the value rounds up");
        assertTrue(intake.coverageHoldsWith(2e6 + 1));
    }

    /// Refunds may push the locked total over the cap: only new mints stop, exits carry on.
    function test_refundsMayExceedTheCap_onlyNewMintsStop() public {
        bytes32 a = _deposit("gates.a", BIG);
        _finalize(a);
        bytes32 b = _deposit("gates.b", BIG);
        _finalize(b);
        assertEq(intake.capRoom(), 0);

        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(a, makeAddr("gates.a"), BIG, keccak256("gates.extra"), 4 * MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(registry.lockedZatoshi() + intake.pendingRefundTotal(), 3 * BIG, "over the cap, pending already");
        _finalizeRefund(keccak256("gates.extra"));
        assertEq(registry.lockedZatoshi(), 3 * BIG, "over the cap");
        assertEq(intake.capRoom(), 0);
        address c = makeAddr("gates.c");
        (journal, seal, sigs) = _proof(_vaultIdFor(c), c, SMALL, _depositTagFor(c), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.CapExceeded.selector, SMALL));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);

        _release("gates.a", a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Closed), "the account still leaves");
    }

    // ------------------------------------------------------------------ R-11: refunds and the cap

    /// R-11 (the review's finding): a full cap used to turn a new vault's proof into a refund, which cancelled the
    /// vault for good; that refund then counted against the cap until it was abandoned (30 days on the judge
    /// deployment), so one unwanted note filled the cap cheaply. Now the proof waits, and a refund nobody claims leaves
    /// the cap after `refundDormancy`, until its account gives a destination.
    function test_R11_anUnclaimedRefundLeavesTheCapOnceDormant() public {
        bytes32 a = _deposit("gates.a", BIG);
        _finalize(a);
        // An unwanted 0.05 ZEC note to the vault: a refund that fills the rest of the cap.
        bytes32 refundId = _refundOf("gates.a", a, BIG, keccak256("gates.unwanted"));
        assertEq(intake.capRoom(), 0, "the refund fills the cap");
        assertEq(registry.lockedZatoshi(), 2 * BIG);

        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        uint64 at = uint64(block.timestamp) + REFUND_DORMANCY;
        vm.expectRevert(abi.encodeWithSelector(IZecuredRefundsErrors.NotDormantYet.selector, refundId, at));
        refunds.markRefundDormant(refundId);
        vm.warp(at);
        capPrice.set(PRICE_40); // a fresh cap price
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.RefundDormant(a, refundId);
        vm.prank(stranger); // anyone
        refunds.markRefundDormant(refundId);
        assertTrue(refunds.isRefundDormant(refundId));
        assertEq(registry.lockedZatoshi(), BIG, "out of lockedZatoshi");
        assertEq(intake.capRoom(), 10_000_000 - BIG, "and of the cap: a new vault fits again");
        (,,,, ClaimStatus status) = registry.claim(refundId);
        assertEq(uint8(status), uint8(ClaimStatus.Pending), "still pending: its account can claim it");
        assertEq(registry.openVaultCount(), 1, "and still owed: the vault stays open");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRefundsErrors.NotDormantable.selector, refundId));
        refunds.markRefundDormant(refundId);
        bytes32 b = _deposit("gates.b", BIG);
        assertEq(uint8(_status(b)), uint8(VaultStatus.Pending));

        // Its account gives a destination: it counts again (refunds may exceed the cap, D44).
        vm.prank(makeAddr("gates.a"));
        registry.setRefundDestination(refundId, keccak256("gates.dest"));
        assertFalse(refunds.isRefundDormant(refundId));
        assertEq(registry.lockedZatoshi() + intake.pendingTotal(), 3 * BIG, "over the cap, as refunds may be");
        assertEq(intake.capRoom(), 0);
        // With a destination it is no longer dormantable.
        vm.warp(block.timestamp + REFUND_DORMANCY);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRefundsErrors.NotDormantable.selector, refundId));
        refunds.markRefundDormant(refundId);
    }

    /// R-11: a dormant refund still keeps its other ways out: abandoned after `refundAbandonPeriod` (written off, out
    /// of the books once), or revived by its account later.
    function test_R11_aDormantRefundCanStillBeAbandonedAndRevived() public {
        bytes32 a = _deposit("gates.a", SMALL);
        _finalize(a);
        bytes32 refundId = _refundOf("gates.a", a, SMALL, keccak256("gates.unwanted"));
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        vm.warp(block.timestamp + REFUND_DORMANCY);
        refunds.markRefundDormant(refundId);
        assertEq(registry.lockedZatoshi(), SMALL);
        vm.warp(block.timestamp + ABANDON_PERIOD);
        registry.abandonRefund(refundId);
        assertFalse(refunds.isRefundDormant(refundId), "closed: no longer dormant");
        assertEq(registry.lockedZatoshi(), SMALL, "taken out of the books once only");
        assertEq(registry.vaultDetails(a).unpaidClaims, 0);
        vm.prank(makeAddr("gates.a"));
        registry.setRefundDestination(refundId, keccak256("gates.dest"));
        assertEq(registry.lockedZatoshi(), 2 * SMALL, "revived: it counts again, once");
    }

    /// R-11, R-12: pending refunds count against the cap like pending activations.
    function test_R11_pendingRefundsCountInTheCap() public {
        bytes32 a = _deposit("gates.a", BIG);
        _finalize(a);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(a, makeAddr("gates.a"), BIG, keccak256("gates.extra"), 4 * MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(intake.pendingRefundTotal(), BIG);
        assertEq(intake.capRoom(), 0, "the pending refund takes the rest of the room");
        assertFalse(intake.coverageHoldsWith(4e6 - 1));
        assertTrue(intake.coverageHoldsWith(4e6));
    }

    // ------------------------------------------------------------------ the other gates

    /// What would stop a mint now, in order: the halt, the cap, the bonds, the pause.
    function test_mintBlock_reportsEachGate() public {
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.None));
        _pause(uint64(block.timestamp + 1 days));
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.Paused));
        gates.setFullyBonded(false);
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.NotBonded));
        gates.setEffectiveBond(0);
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.CapExceeded));
        gates.setHalted(true);
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.Halted));
    }

    /// While a bond is short, a proof still lands as a pending activation (within the cap), which waits; its account
    /// cannot take it back (only a halt or a long pause allows that).
    function test_notBonded_proofPends_finalizeWaits() public {
        gates.setFullyBonded(false);
        bytes32 a = _deposit("gates.a", SMALL);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Pending));
        _ready(a);
        vm.expectRevert(IZecuredIntake.SignersNotBonded.selector);
        intake.finalizeActivation(a);
        vm.prank(makeAddr("gates.a"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotRefundableYet.selector, a));
        intake.refundPending(a);
        gates.setFullyBonded(true);
        intake.finalizeActivation(a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Active));
    }

    /// T-03, mainnet setting: an open F1/F2 case pauses minting by itself. Activations wait; the account cannot
    /// convert its activation (that needs a pause by signatures that has lasted); exits never stop.
    function test_autoPauseOnCase() public {
        bytes32 a = _deposit("gates.a", SMALL);
        bytes32 b = _deposit("gates.b", SMALL);
        _finalize(b);
        _ready(a);
        gates.setOpenTheftCaseCount(1);
        assertTrue(intake.mintingPaused());
        assertEq(uint8(intake.mintBlock(SMALL)), uint8(MintBlock.Paused));
        vm.expectRevert(IZecuredIntake.MintingIsPaused.selector);
        intake.finalizeActivation(a);
        vm.prank(makeAddr("gates.a"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.NotRefundableYet.selector, a));
        intake.refundPending(a);
        _release("gates.b", b);
        assertEq(uint8(_status(b)), uint8(VaultStatus.Closed), "an exit during the pause");

        gates.setOpenTheftCaseCount(0);
        assertFalse(intake.mintingPaused());
        intake.finalizeActivation(a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Active));
    }

    // ------------------------------------------------------------------ T-08: work tiers; T-02: the refund floor

    /// The work a proof's anchor needs grows with the amount; an amount above the last tier never lands.
    function test_workTiers() public {
        assertEq(intake.minConfirmationWorkFor(SMALL), MIN_WORK);
        assertEq(intake.minConfirmationWorkFor(SMALL + 1), 4 * MIN_WORK);
        assertEq(intake.minConfirmationWorkFor(10_000_000), 4 * MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AmountAboveLargestTier.selector, uint64(10_000_001)));
        intake.minConfirmationWorkFor(10_000_001);
        WorkTier[] memory tiers = intake.workTiers();
        assertEq(tiers.length, 2);
        assertEq(tiers[1].maxAmountZat, 10_000_000);
        assertEq(tiers[1].minWork, 4 * MIN_WORK);

        // 0.05 ZEC with only the small tier's work on top of its anchor: refused.
        address account = makeAddr("gates.a");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(_vaultIdFor(account), account, BIG, _depositTagFor(account), MIN_WORK);
        (bytes32 anchor) = abi.decode(journal, (bytes32));
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, anchor, MIN_WORK));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        (journal, seal, sigs) = _proof(_vaultIdFor(account), account, BIG, _depositTagFor(account), 4 * MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(_vaultIdFor(account))), uint8(VaultStatus.Pending));

        // Too large for any tier.
        address huge = makeAddr("gates.huge");
        (journal, seal, sigs) = _proof(_vaultIdFor(huge), huge, 10_000_001, _depositTagFor(huge), type(uint256).max);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AmountAboveLargestTier.selector, uint64(10_000_001)));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    /// A refund needs at least `minRefundZat`: a smaller note could never bear its payout's fee, so it stays unproven.
    function test_refundFloor() public {
        bytes32 a = _deposit("gates.a", SMALL);
        address account = makeAddr("gates.a");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(a, account, MIN_REFUND - 1, keccak256("gates.dust"), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.RefundTooSmall.selector, MIN_REFUND - 1, MIN_REFUND));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        (journal, seal, sigs) = _proof(a, account, MIN_REFUND, keccak256("gates.dust"), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        _finalize(a);
        _finalizeRefund(keccak256("gates.dust"));
        assertEq(registry.claimCount(a), 1);
    }
}
