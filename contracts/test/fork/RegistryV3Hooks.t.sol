// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    ClaimDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredSpendCommitsEvents} from "../../src/interfaces/v3/IZecuredSpendCommits.sol";
import {IZecuredDisputes} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {IZecuredSignerErrors} from "../../src/interfaces/v3/IZecuredSignerErrors.sol";
import {SpendCommit, CloseRequest} from "../../src/libraries/ZecuredEIP712.sol";
import {IZecuredRefunds} from "../../src/interfaces/v3/IZecuredRefunds.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice The Registry's v3 hooks for the opening fee (T-06), the bonds (T-05: a claim the bonds paid, then reimbursed
///         to a destination three signers give) and the compensator (T-04: the freeze, a compromised vault, retirement,
///         compensated claims), with a MOCK bond and slasher the test drives and a compensator address it pranks.
contract RegistryV3HooksTest is ZecuredFixture {
    bytes32 internal constant TREASURY = keccak256("zecured.hooks.treasury");
    uint16 internal constant FEE_BPS = 50;
    uint64 internal constant MIN_FEE = 20_000;
    /// 0.5 % of 0.05 ZEC.
    uint64 internal constant FEE_005 = 25_000;

    MockSignerBond internal gates;
    address internal compensatorRole = makeAddr("zecured.hooks.compensator");
    address internal account;
    uint256 internal accountKey;

    function _mockBondAndSlasher() internal override returns (address) {
        gates = new MockSignerBond();
        return address(gates);
    }

    function _compensator() internal view override returns (address) {
        return compensatorRole;
    }

    function _feeSetup() internal pure override returns (FeeSetup memory) {
        return FeeSetup({feeBps: FEE_BPS, minFeeZat: MIN_FEE, feeCommitment: TREASURY});
    }

    function setUp() public override {
        super.setUp();
        (account, accountKey) = makeAddrAndKey("zecured.hooks.account");
    }

    // ------------------------------------------------------------------ helpers

    function _ids(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _ids(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](2);
        (r[0], r[1]) = (a, b);
    }

    /// A vault of `amount` whose account released all of it (no debt): returns the vault and its Release claim.
    function _released(uint64 amount) internal returns (bytes32 vaultId, bytes32 releaseId) {
        vaultId = _attest(account, amount);
        vm.startPrank(account);
        MORPHO.setAuthorization(address(registry), true);
        releaseId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
    }

    function _commitAndConfirm(bytes32 vaultId, bytes32 inputTag, bytes32[] memory ids, bytes32 salt)
        internal
        returns (SpendCommit memory c)
    {
        c = _spendCommit(vaultId, inputTag, ids, salt);
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        _confirmPayout(vaultId, c.payoutRef, ids);
    }

    // ================================================================== T-06: the opening fee

    function test_fee_chargedAtActivation() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.FeeCharged(vaultId, feeId, FEE_005);
        intake.harnessActivate(vaultId, account, VAULT_005, _depositTagFor(account));

        assertEq(_outstanding(vaultId), VAULT_005 - FEE_005);
        assertEq(zzec.totalSupply(), VAULT_005 - FEE_005, "I1: the fee is never minted");
        assertEq(_position(account).collateral, VAULT_005 - FEE_005);
        _assertClaim(feeId, vaultId, ClaimKind.Fee, FEE_005, TREASURY, ClaimStatus.Pending);
        assertEq(registry.claimDetails(feeId).claimant, address(0), "no Base claimant");
        assertEq(registry.lockedZatoshi(), VAULT_005, "the fee stays locked until paid");
        assertEq(registry.lineageOf(_depositTagFor(account)).value, VAULT_005, "the note holds both");
        assertEq(registry.feeBps(), FEE_BPS);
        assertEq(registry.minFeeZat(), MIN_FEE);
        assertEq(registry.feeCommitment(), TREASURY);
    }

    /// A fee below `minFeeZat` would cost about as much as its own output: it is waived, exactly below the floor.
    function test_fee_waivedBelowTheFloor() public {
        address a = makeAddr("zecured.hooks.a");
        address b = makeAddr("zecured.hooks.b");
        bytes32 atFloor = _attest(a, 4_000_000); // 20,000: charged
        bytes32 belowFloor = _attest(b, 3_999_999); // 19,999: waived
        assertEq(registry.claimCount(atFloor), 1);
        assertEq(_outstanding(atFloor), 4_000_000 - MIN_FEE);
        assertEq(registry.claimCount(belowFloor), 0);
        assertEq(_outstanding(belowFloor), 3_999_999);
        bytes32 small = _attest(account, VAULT_001); // 5,000: waived
        assertEq(registry.claimCount(small), 0);
    }

    /// The fee is paid with the vault's first exit: at index >= 1 a Fee claim bears exactly its own output's 5,000
    /// zatoshi of the ZIP 317 fee, so the first claimant pays no more than without it.
    function test_fee_paidWithTheFirstExit_bearingItsOwnOutputOnly() public {
        (bytes32 vaultId, bytes32 releaseId) = _released(VAULT_005);
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        SpendCommit memory c =
            _commitAndConfirm(vaultId, _depositTagFor(account), _ids(releaseId, feeId), "zecured.hooks.feeSecond");
        assertEq(c.changeValue, 0, "the release and the fee take the whole note");
        // Two outputs: 10,000 zatoshi of fee; the Fee claim's own 5,000, the rest on the release.
        assertEq(registry.claimPayment(c.claimRefs[0]).expected, VAULT_005 - FEE_005 - 5_000);
        assertEq(registry.claimPayment(c.claimRefs[1]).expected, FEE_005 - 5_000);
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lockedZatoshi(), 0);
    }

    /// A Fee claim first in the list bears the rest of the fee itself, and the claim after it bears nothing.
    function test_fee_firstInTheList_bearsTheRest() public {
        (bytes32 vaultId, bytes32 releaseId) = _released(VAULT_005);
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        SpendCommit memory c =
            _commitAndConfirm(vaultId, _depositTagFor(account), _ids(feeId, releaseId), "zecured.hooks.feeFirst");
        assertEq(registry.claimPayment(c.claimRefs[0]).expected, FEE_005 - 10_000);
        assertEq(registry.claimPayment(c.claimRefs[1]).expected, VAULT_005 - FEE_005);
    }

    // ================================================================== T-05: claims the bonds paid

    function test_markBondPaid_onlyTheBond_onlyOncePerPendingClaim() public {
        (bytes32 vaultId, bytes32 releaseId) = _released(VAULT_001);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyBond.selector, stranger));
        registry.markBondPaid(releaseId);

        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ClaimBondPaid(vaultId, releaseId);
        vm.prank(address(gates));
        registry.markBondPaid(releaseId);
        ClaimDetails memory d = registry.claimDetails(releaseId);
        assertTrue(d.bondPaid);
        assertEq(d.commitment, bytes32(0), "its ZEC now reimburses the bonds, wherever three signers say");
        assertEq(uint8(d.status), uint8(ClaimStatus.Pending), "still owed on Zcash");

        vm.prank(address(gates));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, releaseId));
        registry.markBondPaid(releaseId);
        bytes32 unknown = keccak256("zecured.hooks.unknown");
        vm.prank(address(gates));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimNotPayable.selector, unknown));
        registry.markBondPaid(unknown);
    }

    /// R-6: the bonds of all five signers paid the claimant, so all five name where the claim's ZEC reimburses them.
    function test_bondPaidClaim_isReimbursedThroughAllFiveSigners() public {
        (bytes32 vaultId, bytes32 releaseId) = _released(VAULT_001);
        vm.prank(address(gates));
        registry.markBondPaid(releaseId);

        // No payout before the signers name the Reimburse destination.
        SpendCommit memory c = _spendCommit(vaultId, _depositTagFor(account), _ids(releaseId), "zecured.hooks.early");
        bytes[] memory commitSigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.ClaimDestinationUnset.selector, releaseId));
        registry.commitSpend(c, commitSigs);

        bytes32 dest = keccak256("zecured.hooks.reimburse");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes32 digest = registry.hashReimburseDestination(releaseId, dest, deadline);
        // Three signers, or four, are not enough any more (R-6: they could point the bonds' money at themselves).
        bytes[] memory three = _threeSigs(digest);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 3));
        registry.setReimburseDestination(releaseId, dest, deadline, three);
        uint256[] memory four = new uint256[](4);
        (four[0], four[1], four[2], four[3]) = (0, 1, 3, 4);
        bytes[] memory fourSigs = _signerSigs(digest, four);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 4));
        registry.setReimburseDestination(releaseId, dest, deadline, fourSigs);

        bytes[] memory sigs = _fiveSigs(digest);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ReimburseDestinationSet(vaultId, releaseId, dest);
        vm.prank(stranger); // anyone relays
        registry.setReimburseDestination(releaseId, dest, deadline, sigs);
        (,,, bytes32 commitment,) = registry.claim(releaseId);
        assertEq(commitment, dest);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DestinationAlreadySet.selector, releaseId));
        registry.setReimburseDestination(releaseId, dest, deadline, sigs);

        _commitAndConfirm(vaultId, _depositTagFor(account), _ids(releaseId), "zecured.hooks.reimbursed");
        (,,,, ClaimStatus status) = registry.claim(releaseId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
        assertEq(registry.openVaultCount(), 0);
    }

    function test_setReimburseDestination_reverts() public {
        (, bytes32 releaseId) = _released(VAULT_001);
        bytes32 dest = keccak256("zecured.hooks.dest");
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes[] memory sigs = _fiveSigs(registry.hashReimburseDestination(releaseId, dest, deadline));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotBondPaid.selector, releaseId));
        registry.setReimburseDestination(releaseId, dest, deadline, sigs);

        vm.prank(address(gates));
        registry.markBondPaid(releaseId);
        skip(1 hours + 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignatureExpired.selector, deadline));
        registry.setReimburseDestination(releaseId, dest, deadline, sigs);

        deadline = uint64(block.timestamp + 1 hours);
        bytes[] memory zeroSigs = _fiveSigs(registry.hashReimburseDestination(releaseId, bytes32(0), deadline));
        vm.expectRevert(IZecuredRegistry.ZeroCommitment.selector);
        registry.setReimburseDestination(releaseId, bytes32(0), deadline, zeroSigs);

        bytes32 digest = registry.hashReimburseDestination(releaseId, dest, deadline);
        bytes[] memory two = _signerSigs(digest, _idx(0, 1));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSignerErrors.InsufficientSignatures.selector, 2));
        registry.setReimburseDestination(releaseId, dest, deadline, two);
        bytes[] memory other =
            _fiveSigs(registry.hashReimburseDestination(releaseId, keccak256("zecured.hooks.other"), deadline));
        vm.expectPartialRevert(IZecuredSignerErrors.UnknownSigner.selector);
        registry.setReimburseDestination(releaseId, dest, deadline, other);
    }

    /// Once the bonds paid a refund, its account can neither move nor abandon it: its ZEC reimburses the bonds.
    function test_bondPaidRefund_isNoLongerTheAccounts() public {
        bytes32 refundId = intake.harnessRefund(_vaultIdFor(account), account, VAULT_001, _depositTagFor(account));
        vm.prank(account);
        registry.setRefundDestination(refundId, keccak256("zecured.hooks.refundDest"));
        vm.prank(address(gates));
        registry.markBondPaid(refundId);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.RefundBound.selector, refundId));
        registry.setRefundDestination(refundId, keccak256("zecured.hooks.refundDest2"));
        skip(ABANDON_PERIOD);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotARefund.selector, refundId));
        registry.abandonRefund(refundId);
    }

    // ================================================================== T-04: freeze, compromise, retirement

    /// An open F1/F2 case freezes the vault: every exit is refused (release, partial release, liquidation, close),
    /// while the signers keep paying the claims it already has. Unfrozen, it is a vault like any other.
    function test_frozenVault_refusesEveryExit_payoutsGoOn() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        _authorizeBoth(account);
        vm.prank(account);
        bytes32 partialId = registry.requestPartialRelease(vaultId, 1_000_000, RELEASE_COMMITMENT);
        _borrow(account, 900_000); // 3,975,000 zat at $40 = 1.59 USDC: 56.6 %
        gates.setFrozen(vaultId, true);

        vm.startPrank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        registry.requestPartialRelease(vaultId, 200_000, RELEASE_COMMITMENT);
        vm.stopPrank();

        oracle.setPrice(30e34); // 75 % LTV: liquidatable
        _fundAndApproveAdapter(liquidator, 1 * USDC);
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        adapter.liquidate(account, 500_000, 0, LIQUIDATOR_COMMITMENT);

        CloseRequest memory req = _closeRequest(vaultId, account, 100, 0, 1);
        bytes memory sig = _signClose(accountKey, req);
        _fundAndApproveAdapter(marketMaker, 1 * USDC);
        vm.prank(marketMaker);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        adapter.closeWithCollateral(req, sig, MM_COMMITMENT, type(uint256).max, 0);

        // The claims it already has are still paid: their payout is committed as usual, but the signers' word no longer
        // confirms it (R-3); a proof of the payout's inclusion does (here the MOCK slasher stands in for a verified one).
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        SpendCommit memory c =
            _spendCommit(vaultId, _depositTagFor(account), _ids(partialId, feeId), "zecured.hooks.frozenPayout");
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        bytes[] memory confirmSigs =
            _threeSigs(_payouts().hashPayoutConfirmation(vaultId, c.payoutRef, _ids(partialId, feeId)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        _payouts().confirmPayout(vaultId, c.payoutRef, _ids(partialId, feeId), confirmSigs);
        vm.startPrank(address(gates));
        IZecuredDisputes(address(registry)).upholdPayout(partialId, c.payoutRef);
        IZecuredDisputes(address(registry)).upholdPayout(feeId, c.payoutRef);
        vm.stopPrank();
        (,,,, ClaimStatus status) = registry.claim(partialId);
        assertEq(uint8(status), uint8(ClaimStatus.Paid));
        (uint64 paidAt,) = IZecuredDisputes(address(registry)).payoutDispute(partialId);
        assertEq(paidAt, 0, "paid for good");

        // Unfrozen (the case was rejected), the liquidation goes through.
        gates.setFrozen(vaultId, false);
        vm.prank(liquidator);
        adapter.liquidate(account, 500_000, 0, LIQUIDATOR_COMMITMENT);
        assertEq(_outstanding(vaultId), VAULT_005 - FEE_005 - 1_000_000 - 500_000);
    }

    /// An executed F1/F2 case compromises the vault: its claims are the compensator's. No payout is committed or
    /// confirmed, no late deposit lands as a refund, and no claim is written off or revived; a pending refund may
    /// still get its destination.
    function test_compromisedVault_isTheCompensators() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        bytes32 abandoned = intake.harnessRefund(vaultId, account, VAULT_001, keccak256("zecured.hooks.r1"));
        bytes32 withDest = intake.harnessRefund(vaultId, account, VAULT_001, keccak256("zecured.hooks.r2"));
        bytes32 noDest = intake.harnessRefund(vaultId, account, VAULT_001, keccak256("zecured.hooks.r3"));
        skip(ABANDON_PERIOD);
        registry.abandonRefund(abandoned);
        gates.setCompromised(vaultId, true);
        gates.setFrozen(vaultId, true);

        vm.prank(account);
        registry.setRefundDestination(withDest, keccak256("zecured.hooks.r2dest"));
        SpendCommit memory c =
            _spendCommit(vaultId, keccak256("zecured.hooks.r2"), _ids(withDest), "zecured.hooks.compromised");
        bytes[] memory sigs = _threeSigs(registry.hashSpendCommit(c));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultCompromised.selector, vaultId));
        registry.commitSpend(c, sigs);
        bytes[] memory confirmSigs = _threeSigs(_payouts().hashPayoutConfirmation(vaultId, c.payoutRef, _ids(withDest)));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultFrozen.selector, vaultId));
        _payouts().confirmPayout(vaultId, c.payoutRef, _ids(withDest), confirmSigs);

        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultCompromised.selector, vaultId));
        intake.harnessRefund(vaultId, account, VAULT_001, keccak256("zecured.hooks.r4"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultCompromised.selector, vaultId));
        registry.abandonRefund(noDest);
        vm.prank(account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultCompromised.selector, vaultId));
        registry.setRefundDestination(abandoned, keccak256("zecured.hooks.r1dest"));
    }

    function test_retireVault() public {
        bytes32 vaultId = _attest(account, VAULT_005);
        bytes32 feeId = registry.claimIdOf(vaultId, 0);
        _authorizeBoth(account);
        _borrow(account, 500_000);
        uint64 outstanding = _outstanding(vaultId);
        vm.prank(compensatorRole);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotRetirable.selector, vaultId));
        registry.retireVault(vaultId, outstanding); // not compromised

        gates.setCompromised(vaultId, true);
        gates.setFrozen(vaultId, true);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyCompensator.selector, stranger));
        registry.retireVault(vaultId, outstanding);
        vm.startPrank(compensatorRole);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DebtOutstanding.selector, vaultId));
        registry.retireVault(vaultId, outstanding);
        vm.expectRevert(IZecuredRegistry.ZeroAmount.selector);
        registry.retireVault(vaultId, 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                IZecuredRegistry.SeizeExceedsOutstanding.selector, vaultId, outstanding + 1, outstanding
            )
        );
        registry.retireVault(vaultId, outstanding + 1);
        vm.stopPrank();

        _repayAll(account); // what the compensator does with the slashed bonds (layer 3)
        bytes32 retiredId = registry.claimIdOf(vaultId, registry.claimCount(vaultId));
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultRetired(vaultId, retiredId, outstanding);
        vm.prank(compensatorRole);
        assertEq(registry.retireVault(vaultId, outstanding), outstanding);

        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Closed));
        assertEq(_outstanding(vaultId), 0);
        assertEq(zzec.totalSupply(), 0, "the unbacked zZEC is burned");
        assertEq(_position(account).collateral, 0);
        _assertClaim(retiredId, vaultId, ClaimKind.Retired, outstanding, bytes32(0), ClaimStatus.Compensated);
        assertEq(registry.lockedZatoshi(), FEE_005, "only the fee claim is left");
        assertEq(registry.openVaultCount(), 1, "the fee claim keeps it open");

        vm.prank(compensatorRole);
        _payouts().settleCompensatedClaim(feeId);
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lockedZatoshi(), 0);
        vm.prank(compensatorRole);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotRetirable.selector, vaultId));
        registry.retireVault(vaultId, 1);
    }

    /// The bonds repaid part of a compromised vault's debt: the compensator burns its collateral down to what the rest
    /// of the debt needs (Morpho refuses one zatoshi more), so that nothing can be borrowed against it again. The vault
    /// stays active, frozen, with its residual debt and collateral.
    function test_retireVault_partly_downToTheLltv() public {
        bytes32 vaultId = _attest(account, VAULT_005); // 4,975,000 zat after the fee: 1.99 USDC at $40
        _authorizeBoth(account);
        _borrow(account, 600_000); // 30 %
        gates.setCompromised(vaultId, true);
        gates.setFrozen(vaultId, true);
        uint64 outstanding = _outstanding(vaultId);

        // 600,000 of debt needs 600,000 / 0.625 = 960,000 USDC units of collateral: 2,400,000 zat at $40.
        uint64 burnable = outstanding - 2_400_000;
        vm.prank(compensatorRole);
        vm.expectRevert(bytes("insufficient collateral"));
        registry.retireVault(vaultId, burnable + 1);

        bytes32 retiredId = registry.claimIdOf(vaultId, registry.claimCount(vaultId));
        vm.prank(compensatorRole);
        assertEq(registry.retireVault(vaultId, burnable), burnable);
        assertEq(uint8(_status(vaultId)), uint8(VaultStatus.Active), "still active, with its residual debt");
        assertEq(_outstanding(vaultId), 2_400_000);
        assertEq(_position(account).collateral, 2_400_000);
        assertEq(zzec.totalSupply(), 2_400_000, "I1");
        _assertClaim(retiredId, vaultId, ClaimKind.Retired, burnable, bytes32(0), ClaimStatus.Compensated);

        // Nothing more can be borrowed against what is left.
        vm.prank(account);
        vm.expectRevert(bytes("insufficient collateral"));
        MORPHO.borrow(params, 1, 0, account, borrowReceiver);
        assertEq(registry.openVaultCount(), 1, "open until its debt is gone");
    }

    /// The Registry withdraws a retired vault's collateral with the account's own Morpho authorization; without it,
    /// Morpho refuses (the compensator then skips the vault's repayment and retirement).
    function test_retireVault_needsTheAccountsAuthorization() public {
        bytes32 vaultId = _attest(account, VAULT_001);
        gates.setCompromised(vaultId, true);
        vm.prank(compensatorRole);
        vm.expectRevert(bytes("unauthorized"));
        registry.retireVault(vaultId, VAULT_001);
    }

    function test_settleCompensatedClaim() public {
        (bytes32 vaultId, bytes32 releaseId) = _released(VAULT_001);
        vm.prank(compensatorRole);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotCompensable.selector, releaseId));
        _payouts().settleCompensatedClaim(releaseId); // not compromised

        gates.setCompromised(vaultId, true);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.OnlyCompensator.selector, stranger));
        _payouts().settleCompensatedClaim(releaseId);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ClaimCompensated(vaultId, releaseId);
        vm.prank(compensatorRole);
        _payouts().settleCompensatedClaim(releaseId);
        _assertClaim(releaseId, vaultId, ClaimKind.Release, VAULT_001, RELEASE_COMMITMENT, ClaimStatus.Compensated);
        assertEq(registry.openVaultCount(), 0, "its last claim settled");
        assertEq(registry.lockedZatoshi(), 0);
        vm.prank(compensatorRole);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NotCompensable.selector, releaseId));
        _payouts().settleCompensatedClaim(releaseId);
    }

    // ================================================================== K3: nothing is paid from a pending vault

    /// K3 and R-8: a refund proven while the vault's activation is pending waits for it (no claim on a pending vault,
    /// so none accrues liveness there), and its note is spent by no payout until its claim is open (R-12).
    function test_commitSpend_refusesAPendingVault_andAReservedRefund() public {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 secondTag = keccak256("zecured.hooks.second");
        bytes32 refundId = registry.claimIdOf(vaultId, 0);
        IZecuredRefunds refunds = IZecuredRefunds(address(registry));
        vm.startPrank(address(intake));
        registry.reserveDeposit(vaultId, account, VAULT_001, _depositTagFor(account));
        registry.reserveRefund(vaultId, account, VAULT_005, secondTag);
        assertEq(refunds.reservedRefundOf(secondTag), account);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.VaultPending.selector, vaultId));
        refunds.openReservedRefund(vaultId, secondTag);
        vm.stopPrank();
        assertEq(registry.claimCount(vaultId), 0, "no claim on a pending vault");

        // Once the vault activates (0.01 ZEC: its fee is waived, so the refund will be claim 0) ...
        vm.prank(address(intake));
        registry.activate(vaultId, account, VAULT_001, _depositTagFor(account));
        assertEq(registry.claimCount(vaultId), 0);
        // ... the reserved note is still spent by no payout (refused before its claims are even read) ...
        SpendCommit memory early = _spendCommit(vaultId, secondTag, _ids(refundId), "early");
        bytes[] memory earlySigs = _threeSigs(registry.hashSpendCommit(early));
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommitsEvents.RefundNotOpen.selector, secondTag));
        registry.commitSpend(early, earlySigs);
        // ... until its claim opens; then it pays the refund.
        vm.prank(address(intake));
        assertEq(refunds.openReservedRefund(vaultId, secondTag), refundId);
        assertEq(refunds.reservedRefundOf(secondTag), address(0));
        vm.prank(account);
        registry.setRefundDestination(refundId, keccak256("zecured.hooks.dest"));
        SpendCommit memory c = _spendCommit(vaultId, secondTag, _ids(refundId), "zecured.hooks.pending");
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
        assertEq(registry.claimInput(refundId), secondTag);
    }
}
