// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {
    IZecuredIntake,
    MintBlock,
    RefundReason,
    WorkTier,
    PendingActivation
} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {MockCapPrice} from "../mocks/MockCapPrice.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The Intake's gates of minting: the coverage cap (T-01) at the high price with its absolute maximum, the bonds,
///         the halt and the automatic pause on an open case (T-03); its work tiers (T-08) and refund floor (T-02). With
///         the MOCK relay and verifier, a MOCK bond and slasher, and a MOCK cap price.
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
        intake.attestDepositWithProof(journal, seal, sigs);
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
    /// deposits fill 4 USDC exactly at $40; a third deposit lands as a refund, and the room reads zero.
    function test_cap_admitsUpToTheEffectiveBond_thenRefunds() public {
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
        bytes32 claimId = registry.claimIdOf(cVault, 0);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(cVault, c, SMALL, _depositTagFor(c), MIN_WORK);
        vm.expectEmit(address(intake));
        emit IZecuredIntake.DepositRefunded(cVault, claimId, RefundReason.CapExceeded);
        intake.attestDepositWithProof(journal, seal, sigs);
        assertEq(uint8(_status(cVault)), uint8(VaultStatus.Cancelled));
        assertEq(registry.lockedZatoshi() + intake.pendingTotal(), 2 * BIG + SMALL, "the refund is locked too");
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
        bytes32 seventh = _deposit("gates.big6", SMALL);
        assertEq(uint8(_status(seventh)), uint8(VaultStatus.Cancelled), "a refund");
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
        intake.attestDepositWithProof(journal, seal, sigs);

        // A second deposit to a known vault lands as a refund regardless.
        (journal, seal, sigs) = _proof(a, makeAddr("gates.a"), SMALL, keccak256("gates.second"), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, sigs);
        assertEq(registry.claimCount(a), 1);

        // The pending activation finalizes regardless of the cap price.
        _finalize(a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Active));

        // Once the price is fresh again, the waiting proof lands.
        capPrice.set(PRICE_40);
        (journal, seal, sigs) = _proof(_vaultIdFor(b), b, SMALL, _depositTagFor(b), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, sigs);
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
        intake.attestDepositWithProof(journal, seal, sigs);
        assertEq(registry.lockedZatoshi(), 3 * BIG, "over the cap");
        assertEq(intake.capRoom(), 0);
        bytes32 c = _deposit("gates.c", SMALL);
        assertEq(uint8(_status(c)), uint8(VaultStatus.Cancelled), "a new deposit: a refund");

        _release("gates.a", a);
        assertEq(uint8(_status(a)), uint8(VaultStatus.Closed), "the account still leaves");
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
        intake.attestDepositWithProof(journal, seal, sigs);
        (journal, seal, sigs) = _proof(_vaultIdFor(account), account, BIG, _depositTagFor(account), 4 * MIN_WORK);
        intake.attestDepositWithProof(journal, seal, sigs);
        assertEq(uint8(_status(_vaultIdFor(account))), uint8(VaultStatus.Pending));

        // Too large for any tier.
        address huge = makeAddr("gates.huge");
        (journal, seal, sigs) = _proof(_vaultIdFor(huge), huge, 10_000_001, _depositTagFor(huge), type(uint256).max);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AmountAboveLargestTier.selector, uint64(10_000_001)));
        intake.attestDepositWithProof(journal, seal, sigs);
    }

    /// A refund needs at least `minRefundZat`: a smaller note could never bear its payout's fee, so it stays unproven.
    function test_refundFloor() public {
        bytes32 a = _deposit("gates.a", SMALL);
        address account = makeAddr("gates.a");
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) =
            _proof(a, account, MIN_REFUND - 1, keccak256("gates.dust"), MIN_WORK);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.RefundTooSmall.selector, MIN_REFUND - 1, MIN_REFUND));
        intake.attestDepositWithProof(journal, seal, sigs);
        (journal, seal, sigs) = _proof(a, account, MIN_REFUND, keccak256("gates.dust"), MIN_WORK);
        intake.attestDepositWithProof(journal, seal, sigs);
        assertEq(registry.claimCount(a), 1);
    }
}
