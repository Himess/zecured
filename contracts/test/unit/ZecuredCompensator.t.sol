// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {MarketParams} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IOracle} from "morpho-blue/src/interfaces/IOracle.sol";
import {ZecuredCompensator} from "../../src/ZecuredCompensator.sol";
import {IZecuredCompensator, Incident, Victim, Entitlement} from "../../src/interfaces/v3/IZecuredCompensator.sol";
import {
    IZecuredRegistry,
    VaultStatus,
    ClaimKind,
    ClaimStatus,
    VaultDetails
} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {MockZecOracle} from "../../src/mocks/MockZecOracle.sol";
import {MockMorphoForCompensator} from "../mocks/MockMorphoForCompensator.sol";
import {MockCompensatorRegistry} from "../mocks/MockCompensatorRegistry.sol";
import {MockCompensatorSlasher, MockCompensatorBond} from "../mocks/MockCompensatorPeers.sol";

/// @notice Unit tests of ZecuredCompensator (design notes T-04) against MOCK peers: the Registry, the slasher and the
///         bond as the frozen v3 interfaces describe them, and a Morpho market with Morpho's share arithmetic, health
///         check and authorization rule. Amounts: 1 ZEC = 1e8 zatoshi, 1 USDC = 1e6; ZEC is $40 unless a test says
///         otherwise.
contract ZecuredCompensatorTest is Test {
    uint256 internal constant WAD = 1e18;
    uint64 internal constant ZEC = 1e8;
    uint256 internal constant USD = 1e6;
    uint256 internal constant PRICE = 40e34; // $40 per ZEC, Morpho scale for zZEC (8 decimals) / USDC (6 decimals)
    uint256 internal constant LLTV = 0.86e18;
    uint64 internal constant WINDOW = 3 days;
    address internal constant SINK = 0x000000000000000000000000000000000000dEaD;

    bytes32 internal constant VAULT_A = keccak256("zecured.comp.vault.a");
    bytes32 internal constant VAULT_B = keccak256("zecured.comp.vault.b");
    bytes32 internal constant VAULT_C = keccak256("zecured.comp.vault.c");

    MockUSDC internal usdc;
    MockZecOracle internal oracle;
    MarketParams internal params;
    MockMorphoForCompensator internal morpho;
    MockCompensatorRegistry internal registry;
    MockCompensatorSlasher internal slasher;
    MockCompensatorBond internal bond;
    ZecuredCompensator internal comp;

    address internal alice = makeAddr("zecured.comp.alice");
    address internal bob = makeAddr("zecured.comp.bob");
    address internal carol = makeAddr("zecured.comp.carol");
    address internal liquidator = makeAddr("zecured.comp.liquidator");

    uint256 internal _caseNonce;

    function setUp() public {
        vm.warp(1_760_000_000);
        usdc = new MockUSDC(address(this));
        oracle = new MockZecOracle(address(this), PRICE);
        params = MarketParams({
            loanToken: address(usdc),
            collateralToken: makeAddr("zecured.comp.zzec"),
            oracle: address(oracle),
            irm: makeAddr("zecured.comp.irm"),
            lltv: LLTV
        });
        morpho = new MockMorphoForCompensator(params);
        registry = new MockCompensatorRegistry(morpho, params);
        slasher = new MockCompensatorSlasher();
        bond = new MockCompensatorBond();
        comp = _deploy(WINDOW, SINK);
        usdc.mint(address(bond), 1_000_000 * USD);
        usdc.mint(address(morpho), 1_000_000 * USD); // the market's liquidity, for borrows
    }

    // ------------------------------------------------------------------ helpers

    /// Wires the peers to the next CREATE address and deploys the compensator there.
    function _deploy(uint64 window, address sink) internal returns (ZecuredCompensator deployed) {
        uint64 nonce = _wireNext();
        deployed = new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, window, sink);
        assertEq(address(deployed), vm.computeCreateAddress(address(this), nonce), "compensator address prediction");
    }

    /// Wires the peers to the address this contract's next CREATE gets. The test runner also advances this contract's
    /// nonce on every call it makes (the wiring calls included): the caller puts it back with `_rewind` right before
    /// the CREATE.
    function _wireNext() internal returns (uint64 nonce) {
        nonce = vm.getNonce(address(this));
        _wire(vm.computeCreateAddress(address(this), nonce));
        _rewind(nonce);
    }

    function _rewind(uint64 nonce) internal {
        vm.setNonceUnsafe(address(this), nonce);
    }

    function _wire(address compensator) internal {
        registry.setWiring(compensator, address(slasher), address(bond));
        slasher.setWiring(address(registry), address(bond), compensator);
        bond.setWiring(address(registry), address(slasher), compensator, usdc);
    }

    /// An active vault: `outstanding` zatoshi of collateral, `debt` USDC borrowed, the Registry authorized on Morpho.
    function _openVault(bytes32 vaultId, address account, uint64 outstanding, uint256 debt) internal {
        _openVault(vaultId, account, outstanding, debt, true);
    }

    function _openVault(bytes32 vaultId, address account, uint64 outstanding, uint256 debt, bool authorize) internal {
        registry.setVault(vaultId, account, outstanding, VaultStatus.Active);
        morpho.setCollateral(account, outstanding);
        if (debt != 0) morpho.seedDebt(account, debt);
        if (authorize) {
            vm.prank(account);
            morpho.setAuthorization(address(registry), true);
        }
    }

    /// The bond's slash: `amount` USDC to the compensator.
    function _fund(uint256 amount) internal {
        bond.slash(keccak256(abi.encode("zecured.comp.case", ++_caseNonce)), amount);
    }

    /// An executed F1 on the vault.
    function _steal(bytes32 vaultId) internal {
        slasher.execute(
            keccak256(abi.encode("zecured.comp.f1", ++_caseNonce)), ViolationKind.Uncommitted, vaultId, bytes32(0), 0
        );
    }

    /// An executed F3 on the claim.
    function _misdirect(bytes32 claimId, uint64 shortfall) internal {
        slasher.execute(
            keccak256(abi.encode("zecured.comp.f3", ++_caseNonce)),
            ViolationKind.Misdirected,
            bytes32(0),
            claimId,
            shortfall
        );
    }

    /// Tallies every pending claim of the vault, then the vault.
    function _tally(bytes32 vaultId) internal {
        VaultDetails memory d = registry.vaultDetails(vaultId);
        for (uint64 s; s < d.claimCount; ++s) {
            bytes32 claimId = registry.claimIdOf(vaultId, s);
            if (registry.claimDetails(claimId).status == ClaimStatus.Pending) comp.tallyClaim(claimId);
        }
        comp.tallyVault(vaultId);
    }

    function _finalize(uint256 id) internal {
        uint64 closesAt = comp.incident(id).closesAt;
        if (block.timestamp < closesAt) vm.warp(closesAt);
        while (!comp.incident(id).finalized) comp.finalizeIncident(id);
    }

    /// I19 for the compensator: every USDC it holds is either unassigned or set aside for a finalized incident.
    function _assertBooks() internal view {
        assertEq(usdc.balanceOf(address(comp)), comp.funds() + comp.reserved(), "I19: balance == funds + reserved");
    }

    function _indexOf(Vm.Log[] memory logs, address emitter, bytes32 topic0) internal pure returns (uint256) {
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == emitter && logs[i].topics[0] == topic0) return i;
        }
        return type(uint256).max;
    }

    // ------------------------------------------------------------------ construction

    function test_constructor_keepsItsWiring() public view {
        assertEq(comp.registry(), address(registry));
        assertEq(comp.slasher(), address(slasher));
        assertEq(comp.signerBond(), address(bond));
        assertEq(address(comp.usdc()), address(usdc));
        assertEq(comp.incidentWindow(), WINDOW);
        assertEq(comp.leftoverSink(), SINK);
        assertEq(comp.currentIncident(), 0);
        assertEq(comp.funds(), 0);
        assertEq(comp.reserved(), 0);
    }

    function test_revert_constructor_zeroAddress() public {
        vm.expectRevert(IZecuredCompensator.ZeroAddress.selector);
        new ZecuredCompensator(address(0), address(slasher), address(bond), usdc, WINDOW, SINK);
        vm.expectRevert(IZecuredCompensator.ZeroAddress.selector);
        new ZecuredCompensator(address(registry), address(0), address(bond), usdc, WINDOW, SINK);
        vm.expectRevert(IZecuredCompensator.ZeroAddress.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(0), usdc, WINDOW, SINK);
        vm.expectRevert(IZecuredCompensator.ZeroAddress.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(bond), MockUSDC(address(0)), WINDOW, SINK);
        vm.expectRevert(IZecuredCompensator.ZeroAddress.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, WINDOW, address(0));
    }

    function test_constructor_incidentWindowBounds() public {
        uint64 max = comp.MAX_INCIDENT_WINDOW();
        _wireNext();
        vm.expectRevert(IZecuredCompensator.InvalidConfig.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, 0, SINK);
        _wireNext();
        vm.expectRevert(IZecuredCompensator.InvalidConfig.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, max + 1, SINK);
        assertEq(_deploy(max, SINK).incidentWindow(), max);
        assertEq(_deploy(10 minutes, SINK).incidentWindow(), 10 minutes); // the theft demo
    }

    /// Each case breaks one link of an otherwise correct wiring (the last deployment shows the correct one passes).
    function test_revert_constructor_miswired() public {
        address other = makeAddr("zecured.comp.other");
        for (uint256 i; i < 10; ++i) {
            uint64 nonce = vm.getNonce(address(this));
            address predicted = vm.computeCreateAddress(address(this), nonce);
            _wire(predicted);
            if (i == 0) registry.setWiring(other, address(slasher), address(bond));
            if (i == 1) registry.setWiring(predicted, other, address(bond));
            if (i == 2) registry.setWiring(predicted, address(slasher), other);
            if (i == 3) slasher.setWiring(address(registry), address(bond), other);
            if (i == 4) slasher.setWiring(other, address(bond), predicted);
            if (i == 5) slasher.setWiring(address(registry), other, predicted);
            if (i == 6) bond.setWiring(address(registry), address(slasher), other, usdc);
            if (i == 7) bond.setWiring(other, address(slasher), predicted, usdc);
            if (i == 8) bond.setWiring(address(registry), other, predicted, usdc);
            if (i == 9) bond.setWiring(address(registry), address(slasher), predicted, MockUSDC(other));
            _rewind(nonce);
            vm.expectRevert(IZecuredCompensator.InvalidConfig.selector);
            new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, WINDOW, SINK);
        }
        _deploy(WINDOW, SINK);

        // A market whose loan token is not the bonds' token.
        MarketParams memory wrong = params;
        wrong.loanToken = other;
        registry = new MockCompensatorRegistry(morpho, wrong);
        _wireNext();
        vm.expectRevert(IZecuredCompensator.InvalidConfig.selector);
        new ZecuredCompensator(address(registry), address(slasher), address(bond), usdc, WINDOW, SINK);
    }

    // ------------------------------------------------------------------ hooks

    function test_revert_onFunds_onlyTheBond() public {
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.OnlyBond.selector, address(this)));
        comp.onFunds(bytes32(0), 1);
    }

    function test_revert_onCase_onlyTheSlasher() public {
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.OnlySlasher.selector, address(this)));
        comp.onCase(bytes32(0), ViolationKind.Uncommitted, VAULT_A, bytes32(0), 0);
    }

    function test_onFunds_addsToTheFunds() public {
        bytes32 caseId = keccak256("zecured.comp.case.1");
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.FundsReceived(caseId, 500 * USD);
        bond.slash(caseId, 500 * USD);
        bond.slash(caseId, 0); // a later slash finds the bonds empty
        assertEq(comp.funds(), 500 * USD);
        _assertBooks();
    }

    function test_onCase_F4AndUnknownVaultsRegisterNothing() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 claimId = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        vm.recordLogs();
        slasher.execute(keccak256("f4"), ViolationKind.WrongClaimRef, VAULT_A, claimId, 0);
        slasher.execute(keccak256("f1.zero"), ViolationKind.Uncommitted, bytes32(0), bytes32(0), 0);
        assertEq(vm.getRecordedLogs().length, 0);
        assertEq(comp.currentIncident(), 0);
        assertEq(comp.victim(VAULT_A).incident, 0);
        assertEq(comp.entitlement(claimId).incident, 0);
    }

    /// `onCase` runs inside the slasher's `executeSlash`: a Registry read that fails must not block the slash.
    function test_onCase_F3NeverRevertsOnAFailingRegistryRead() public {
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_B, ClaimKind.Liquidation, ZEC, liquidator, false);
        vm.mockCallRevert(
            address(registry), abi.encodeCall(IZecuredRegistry.claimDetails, (liq)), bytes("MockRegistry: down")
        );
        _misdirect(liq, ZEC / 2);
        assertEq(comp.entitlement(liq).incident, 0, "not registered");
        assertEq(comp.currentIncident(), 0);
    }

    // ------------------------------------------------------------------ incidents

    function test_incidentWindow_groupsTheCasesItSees() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        _openVault(VAULT_C, carol, 10 * ZEC, 0);
        uint256 t0 = block.timestamp;
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.IncidentOpened(1, uint64(t0 + WINDOW));
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VictimRegistered(1, VAULT_A, keccak256(abi.encode("zecured.comp.f1", uint256(1))));
        _steal(VAULT_A);

        vm.warp(t0 + WINDOW - 1); // last second of the window
        _steal(VAULT_B);
        vm.warp(t0 + WINDOW); // closed: a new incident
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.IncidentOpened(2, uint64(t0 + 2 * WINDOW));
        _steal(VAULT_C);

        assertEq(comp.currentIncident(), 2);
        assertEq(comp.victim(VAULT_A).incident, 1);
        assertEq(comp.victim(VAULT_B).incident, 1);
        assertEq(comp.victim(VAULT_C).incident, 2);
        Incident memory one = comp.incident(1);
        assertEq(one.openedAt, t0);
        assertEq(one.closesAt, t0 + WINDOW);
        assertEq(one.victims, 2);
        assertFalse(one.finalized);
        assertEq(comp.incident(2).victims, 1);
        assertEq(comp.incident(2).openedAt, t0 + WINDOW);
    }

    function test_registration_isIdempotent() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        _steal(VAULT_A);
        vm.recordLogs();
        // An F2 on the same vault in the window, and another F1 after it: one victim, in its first incident.
        slasher.execute(keccak256("f2"), ViolationKind.WithheldChange, VAULT_A, bytes32(0), 0);
        vm.warp(block.timestamp + WINDOW + 1);
        _steal(VAULT_A);
        assertEq(vm.getRecordedLogs().length, 0, "no registration, no new incident");
        assertEq(comp.currentIncident(), 1);
        assertEq(comp.incident(1).victims, 1);
        assertEq(comp.victim(VAULT_A).incident, 1);
    }

    // ------------------------------------------------------------------ the whole flow

    function test_fullCompensation_paysEveryLayerInFull() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        bytes32 rel = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC / 2, alice, false);
        bytes32 fee = registry.addClaim(VAULT_A, ClaimKind.Fee, ZEC / 100, address(0), false);
        _fund(10_000 * USD);
        _steal(VAULT_A);

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ClaimTallied(liq, 2, ZEC);
        comp.tallyClaim(liq);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ClaimTallied(rel, 4, ZEC / 2);
        comp.tallyClaim(rel);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ClaimTallied(fee, 0, 0); // the signers' own: settled, never paid
        comp.tallyClaim(fee);
        // The Registry counts them Compensated: nothing is left to tally.
        assertEq(uint8(registry.claimDetails(liq).status), uint8(ClaimStatus.Compensated));
        assertEq(uint8(registry.claimDetails(fee).status), uint8(ClaimStatus.Compensated));
        assertEq(registry.vaultDetails(VAULT_A).unpaidClaims, 0);
        Entitlement memory e = comp.entitlement(liq);
        assertEq(e.incident, 1);
        assertEq(e.layer, 2);
        assertEq(e.zat, ZEC);
        assertEq(e.to, liquidator);
        assertFalse(e.paid);
        assertEq(comp.entitlement(fee).incident, 0);

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultTallied(VAULT_A, 10 * ZEC, 200 * USD, true);
        comp.tallyVault(VAULT_A);
        Victim memory v = comp.victim(VAULT_A);
        assertTrue(v.tallied);
        assertFalse(v.settled);
        assertTrue(v.authorized);
        assertEq(v.pendingClaims, 2);
        assertEq(v.talliedClaims, 3);
        assertEq(v.outstanding, 10 * ZEC);
        assertEq(v.debt, 200 * USD);

        vm.warp(block.timestamp + WINDOW);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.IncidentFinalized(1, PRICE, 40 * USD, 202 * USD, 218 * USD, WAD, WAD, WAD);
        comp.finalizeIncident(1);
        Incident memory inc = comp.incident(1);
        assertTrue(inc.finalized);
        assertEq(inc.price, PRICE);
        assertEq(inc.layer2, 40 * USD, "1 ZEC liquidated at $40");
        assertEq(inc.layer3, 202 * USD, "200 of debt plus the 1 % interest allowance");
        assertEq(inc.layer4, 20 * USD + 198 * USD, "the 0.5 ZEC release, and the equity $400 - $202");
        assertEq(comp.funds(), 9_540 * USD);
        assertEq(comp.reserved(), 460 * USD);
        _assertBooks();

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_A, 200 * USD, 10 * ZEC, 200 * USD);
        comp.settleVault(VAULT_A);
        assertEq(morpho.borrowSharesOf(alice), 0, "debt repaid");
        assertEq(morpho.collateralOf(alice), 0, "collateral withdrawn by the Registry");
        (, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_A);
        assertEq(outstanding, 0, "unbacked zZEC retired");
        assertEq(uint8(status), uint8(VaultStatus.Closed));
        assertEq(comp.equityOf(VAULT_A), 200 * USD, "$400 of collateral less $200 of debt");
        assertTrue(comp.victim(VAULT_A).settled);

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.EntitlementPaid(liq, liquidator, 40 * USD);
        assertEq(comp.withdrawEntitlement(liq), 40 * USD);
        assertEq(comp.withdrawEntitlement(rel), 20 * USD);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.EntitlementPaid(VAULT_A, alice, 200 * USD);
        assertEq(comp.withdrawEquity(VAULT_A), 200 * USD);
        assertEq(usdc.balanceOf(liquidator), 40 * USD);
        assertEq(usdc.balanceOf(alice), 220 * USD, "with the $200 borrowed: the 10.5 ZEC at $40");
        assertEq(comp.equityOf(VAULT_A), 0);
        assertEq(comp.reserved(), 0);
        assertEq(comp.funds(), 9_540 * USD);
        _assertBooks();
    }

    function test_settle_repaysTheDebtBeforeTheRegistryRetiresTheVault() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);

        uint256 shares = morpho.borrowSharesOf(alice);
        vm.expectCall(address(morpho), abi.encodeCall(MockMorphoForCompensator.repay, (params, 0, shares, alice, "")));
        vm.expectCall(address(registry), abi.encodeCall(MockCompensatorRegistry.retireVault, (VAULT_A, 10 * ZEC)));
        vm.recordLogs();
        comp.settleVault(VAULT_A);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 repayAt = _indexOf(logs, address(morpho), MockMorphoForCompensator.Repay.selector);
        uint256 withdrawAt = _indexOf(logs, address(morpho), MockMorphoForCompensator.WithdrawCollateral.selector);
        uint256 retiredAt = _indexOf(logs, address(registry), MockCompensatorRegistry.VaultRetired.selector);
        uint256 settledAt = _indexOf(logs, address(comp), IZecuredCompensator.VaultSettled.selector);
        assertLt(repayAt, withdrawAt, "repaid on behalf of the account first");
        assertLt(withdrawAt, retiredAt, "then the Registry withdraws the collateral and retires the vault");
        assertLt(retiredAt, settledAt);
        assertEq(usdc.allowance(address(comp), address(morpho)), 0, "Morpho pulled exactly the debt");
        _assertBooks();
    }

    // ------------------------------------------------------------------ layers

    function test_layers_partialFundsStopAtTheFirstLayerTheyDoNotCover() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        bytes32 rel = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC, alice, false);
        _fund(141 * USD); // layer 2 ($40) in full, half of layer 3 ($202), nothing of layer 4
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);

        Incident memory inc = comp.incident(1);
        assertEq(inc.r2, WAD);
        assertEq(inc.r3, WAD / 2);
        assertEq(inc.r4, 0);
        assertEq(comp.funds(), 0);
        assertEq(comp.reserved(), 141 * USD);

        // Partial settlement: the vault's share ($202 / 2) is repaid; the collateral the rest of the debt does not need
        // is burned down to the LLTV; the rest of the debt and that collateral stay.
        uint256 left = _collateralNeededFor(99 * USD);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_A, 101 * USD, uint64(10 * ZEC - left), 0);
        comp.settleVault(VAULT_A);
        assertEq(morpho.debtOf(alice), 99 * USD);
        assertEq(morpho.collateralOf(alice), left);
        (,, VaultStatus status) = registry.vault(VAULT_A);
        assertEq(uint8(status), uint8(VaultStatus.Active), "not retired in full: its residual debt stays");
        assertEq(comp.equityOf(VAULT_A), 0);
        assertEq(comp.withdrawEntitlement(liq), 40 * USD);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NothingOwed.selector, rel));
        comp.withdrawEntitlement(rel);
        assertEq(comp.reserved(), 0);
        _assertBooks();
    }

    /// A partial repayment burns the collateral the rest of the debt does not need, down to the LLTV: the account
    /// cannot borrow the repaid amount again, and the vault stays active (frozen) with its residual debt.
    function test_partialSettlement_burnsTheCollateralDownToTheLltv() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _fund(101 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);
        comp.settleVault(VAULT_A);
        assertEq(morpho.debtOf(alice), 99 * USD);
        // What $99 needs at the LLTV (2.8779 ZEC at $40), plus one zatoshi; the rest was burned.
        (, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_A);
        assertEq(outstanding, _collateralNeededFor(99 * USD));
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(morpho.collateralOf(alice), outstanding);
        assertEq(comp.victim(VAULT_A).settled, true);
        vm.prank(alice);
        vm.expectRevert(bytes("MockMorpho: insufficient collateral"));
        morpho.borrow(params, 1 * USD, 0, alice, alice);
        _assertBooks();
    }

    /// Borrowing past the share before `settleVault` (while layer 4 is not paid in full) only lands in the partial case:
    /// the share is repaid and the collateral burned down to the LLTV, so the repayment cannot be borrowed back.
    function test_partialSettlement_reachedByBorrowingPastTheShare_cannotBeBorrowedBack() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 100 * USD);
        _openVault(VAULT_B, bob, 10 * ZEC, 100 * USD);
        _fund(501 * USD); // layer 3 = $202, layer 4 = $598: r4 = 1/2, so A's share is $101 + $149.50
        _steal(VAULT_A);
        _steal(VAULT_B);
        _tally(VAULT_A);
        _tally(VAULT_B);
        _finalize(1);
        assertEq(comp.incident(1).r4, WAD / 2);

        vm.prank(alice);
        morpho.borrow(params, 240 * USD, 0, alice, alice); // $340 of debt: past the share, below the LLTV ($344)
        comp.settleVault(VAULT_A);
        uint256 debt = morpho.debtOf(alice);
        assertApproxEqAbs(debt, 89_500_000, 1, "the share ($250.50) repaid");
        assertEq(morpho.collateralOf(alice), _collateralNeededFor(debt), "burned down to what the rest needs");
        vm.prank(alice);
        vm.expectRevert(bytes("MockMorpho: insufficient collateral"));
        morpho.borrow(params, 1 * USD, 0, alice, alice);
        assertEq(usdc.balanceOf(alice), 240 * USD, "what it borrowed before the settlement, nothing more");
        _assertBooks();
    }

    /// The least collateral that keeps `debt` healthy at the LLTV and $40 (Morpho's rule, rounded up), plus one zatoshi.
    function _collateralNeededFor(uint256 debt) internal pure returns (uint256) {
        return Math.mulDiv(Math.mulDiv(debt, WAD, LLTV, Math.Rounding.Ceil), 1e36, PRICE, Math.Rounding.Ceil) + 1;
    }

    function test_layers_zeroFundsPayNobody() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _steal(VAULT_A);
        _steal(VAULT_B);
        _tally(VAULT_A);
        _tally(VAULT_B);
        _finalize(1);
        Incident memory inc = comp.incident(1);
        assertEq(inc.r2, 0);
        assertEq(inc.r3, 0);
        assertEq(inc.r4, 0);
        assertEq(comp.reserved(), 0);

        comp.settleVault(VAULT_A); // nothing to repay with
        assertEq(morpho.debtOf(alice), 200 * USD);
        // A vault without debt is still retired: retiring its unbacked zZEC costs nothing.
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_B, 0, 10 * ZEC, 0);
        comp.settleVault(VAULT_B);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NothingOwed.selector, liq));
        comp.withdrawEntitlement(liq);
        _assertBooks();
    }

    function test_layers_anEmptyLayerTakesNothing() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0); // no debt: layer 3 is empty, and so is layer 2
        _fund(100 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);
        Incident memory inc = comp.incident(1);
        assertEq(inc.layer2, 0);
        assertEq(inc.layer3, 0);
        assertEq(inc.layer4, 400 * USD);
        assertEq(inc.r2, WAD);
        assertEq(inc.r3, WAD);
        assertEq(inc.r4, WAD / 4, "the whole $100 reaches layer 4");

        comp.settleVault(VAULT_A);
        assertEq(comp.equityOf(VAULT_A), 100 * USD);
        assertEq(comp.withdrawEquity(VAULT_A), 100 * USD);
        assertEq(usdc.balanceOf(alice), 100 * USD);
        _assertBooks();
    }

    function test_layers_lendersBeforeOwnersAcrossVaults() public {
        // A: $300 of debt on $400 of collateral; B: no debt. Layer 3 = $303, layer 4 = $97 + $400.
        _openVault(VAULT_A, alice, 10 * ZEC, 300 * USD);
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        _fund(303 * USD); // exactly layer 3
        _steal(VAULT_A);
        _steal(VAULT_B);
        _tally(VAULT_A);
        _tally(VAULT_B);
        _finalize(1);
        assertEq(comp.incident(1).r3, WAD);
        assertEq(comp.incident(1).r4, 0);

        comp.settleVault(VAULT_A);
        comp.settleVault(VAULT_B);
        assertEq(morpho.debtOf(alice), 0, "lenders whole");
        assertEq(comp.equityOf(VAULT_A), 0, "owners get nothing");
        assertEq(comp.equityOf(VAULT_B), 0);
        (,, VaultStatus a) = registry.vault(VAULT_A);
        (,, VaultStatus b) = registry.vault(VAULT_B);
        assertEq(uint8(a), uint8(VaultStatus.Closed), "both retired");
        assertEq(uint8(b), uint8(VaultStatus.Closed));
        assertEq(comp.funds(), 3 * USD, "the unused interest allowance is back in the pool");
        _assertBooks();
    }

    // ------------------------------------------------------------------ the interest allowance

    function test_interestSinceTheTallyComesOutOfTheEquity() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        morpho.addInterest(2 * USD);
        _finalize(1);
        comp.settleVault(VAULT_A);
        assertEq(comp.equityOf(VAULT_A), 198 * USD, "$400 - $202 owed now");
        assertEq(comp.funds(), 600 * USD, "the whole share was used: $202 repaid, $198 of equity");
        _assertBooks();
    }

    function test_allowance_letsAnUnderwaterVaultBeRepaidAndRetired() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 300 * USD);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        morpho.addInterest(3 * USD / 2); // accrues while liquidations of the frozen vault are refused
        oracle.setPrice(28e34); // $28: 10 ZEC are worth $280, less than the debt
        _finalize(1);
        Incident memory inc = comp.incident(1);
        assertEq(inc.layer3, 303 * USD);
        assertEq(inc.layer4, 0, "no equity under water");

        // The share is $303, the debt now $301.50: repaid in full and retired; the owner had no equity.
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_A, 301_500_000, 10 * ZEC, 0);
        comp.settleVault(VAULT_A);
        assertEq(morpho.debtOf(alice), 0);
        assertEq(comp.equityOf(VAULT_A), 0);
        assertEq(comp.funds(), 1_000 * USD - 301_500_000, "the allowance's unused $1.50 is back in the pool");
        _assertBooks();
    }

    // ------------------------------------------------------------------ changes on Morpho after the tally

    function test_borrowingAfterTheTally_movesValueFromTheEquityToTheLendersOnly() public {
        // Two identical vaults; layer 4 is paid at one half. Alice borrows $150 more after the tally.
        _openVault(VAULT_A, alice, 10 * ZEC, 100 * USD);
        _openVault(VAULT_B, bob, 10 * ZEC, 100 * USD);
        _fund(501 * USD); // layer 3 = $202, layer 4 = $598: r4 = 1/2
        _steal(VAULT_A);
        _steal(VAULT_B);
        _tally(VAULT_A);
        _tally(VAULT_B);
        vm.prank(alice);
        morpho.borrow(params, 150 * USD, 0, alice, alice);
        _finalize(1);
        assertEq(comp.incident(1).r4, WAD / 2);

        comp.settleVault(VAULT_A);
        comp.settleVault(VAULT_B);
        assertEq(morpho.debtOf(alice), 0, "A's lenders are repaid all $250");
        assertEq(morpho.debtOf(bob), 0);
        assertEq(comp.equityOf(VAULT_B), 150 * USD, "B is untouched: $300 * 1/2");
        assertEq(comp.equityOf(VAULT_A), 0, "A's $150 of equity paid its new debt");
        // Both owners end with the same value: what they borrowed plus their equity.
        assertEq(usdc.balanceOf(alice) + comp.equityOf(VAULT_A), 150 * USD);
        assertEq(comp.equityOf(VAULT_B), 150 * USD);
        assertEq(comp.funds(), 1 * USD, "each vault's unused $0.50 of allowance");
        _assertBooks();
    }

    function test_debtRepaidDirectlyAfterTheTallyComesBackWithTheEquity() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        // The freeze does not stop anyone repaying on Morpho; the account repays $50 itself.
        usdc.mint(alice, 50 * USD);
        vm.startPrank(alice);
        usdc.approve(address(morpho), 50 * USD);
        morpho.repay(params, 50 * USD, 0, alice, "");
        vm.stopPrank();
        _finalize(1);
        comp.settleVault(VAULT_A);
        assertEq(comp.equityOf(VAULT_A), 250 * USD, "$200 of equity and the $50 it repaid");
        assertEq(morpho.debtOf(alice), 0);
        _assertBooks();
    }

    // ------------------------------------------------------------------ authorization

    function test_unauthorizedAtTally_skipsLayers3And4() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD, false);
        bytes32 rel = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC, alice, false);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        assertFalse(comp.victim(VAULT_A).authorized);
        _finalize(1);
        Incident memory inc = comp.incident(1);
        assertEq(inc.layer3, 0);
        assertEq(inc.layer4, 40 * USD, "its own claim only, no equity");

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_A, 0, 0, 0);
        comp.settleVault(VAULT_A);
        assertEq(morpho.debtOf(alice), 200 * USD, "nothing repaid: it could be borrowed again");
        (,, VaultStatus status) = registry.vault(VAULT_A);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(comp.withdrawEntitlement(rel), 40 * USD, "its claims are paid all the same");
        _assertBooks();
    }

    function test_authorizationRevokedAfterTheTally_givesTheShareBackToThePool() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        vm.prank(alice);
        morpho.setAuthorization(address(registry), false);
        _finalize(1);
        assertEq(comp.funds(), 600 * USD);

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_A, 0, 0, 0);
        comp.settleVault(VAULT_A);
        assertEq(comp.funds(), 1_000 * USD, "its $400 share is back in the pool");
        assertEq(comp.reserved(), 0);
        assertEq(morpho.debtOf(alice), 200 * USD);
        _assertBooks();
    }

    // ------------------------------------------------------------------ vault kinds

    function test_pendingVault_mustBeConvertedFirst() public {
        registry.setVault(VAULT_A, alice, 0, VaultStatus.Pending);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.VaultStillPending.selector, VAULT_A));
        comp.tallyVault(VAULT_A);

        // The Intake's refundPending: Pending -> Cancelled with a Refund claim for the account.
        registry.setVault(VAULT_A, alice, 0, VaultStatus.Cancelled);
        bytes32 refund = registry.addClaim(VAULT_A, ClaimKind.Refund, 2 * ZEC, alice, false);
        _tally(VAULT_A);
        _finalize(1);
        comp.settleVault(VAULT_A);
        assertEq(comp.withdrawEntitlement(refund), 80 * USD);
        assertEq(usdc.balanceOf(alice), 80 * USD);
        _assertBooks();
    }

    function test_closedVictimVault_paysItsClaims() public {
        // Released, then its payout note was stolen before the payout.
        registry.setVault(VAULT_C, carol, 0, VaultStatus.Closed);
        bytes32 rel = registry.addClaim(VAULT_C, ClaimKind.Release, 2 * ZEC, carol, false);
        assertEq(registry.openVaultCount(), 1);
        _fund(1_000 * USD);
        _steal(VAULT_C);
        comp.tallyClaim(rel);
        assertEq(registry.openVaultCount(), 0, "its last claim settled: the vault is no longer open");
        comp.tallyVault(VAULT_C);
        _finalize(1);
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.VaultSettled(VAULT_C, 0, 0, 0);
        comp.settleVault(VAULT_C);
        assertEq(comp.withdrawEntitlement(rel), 80 * USD);
        assertEq(usdc.balanceOf(carol), 80 * USD);
        _assertBooks();
    }

    // ------------------------------------------------------------------ F3

    function test_F3_registersTheShortfallInTheClaimsLayer() public {
        _openVault(VAULT_B, bob, 10 * ZEC, 0); // a sound vault: F3 freezes nothing
        bytes32 liq = registry.addClaim(VAULT_B, ClaimKind.Liquidation, ZEC, liquidator, false);
        bytes32 rel = registry.addClaim(VAULT_B, ClaimKind.Release, ZEC, bob, false);
        registry.setClaimStatus(liq, ClaimStatus.Paid);
        _fund(1_000 * USD);

        vm.expectEmit(address(comp));
        emit IZecuredCompensator.IncidentOpened(1, uint64(block.timestamp + WINDOW));
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ShortfallRegistered(1, liq, 3 * ZEC / 10);
        _misdirect(liq, 3 * ZEC / 10);
        _misdirect(rel, 2 * ZEC); // more than the claim: capped at its amount
        Entitlement memory e = comp.entitlement(rel);
        assertEq(e.layer, 4);
        assertEq(e.zat, ZEC);
        assertEq(e.to, bob);

        _finalize(1);
        Incident memory inc = comp.incident(1);
        assertEq(inc.victims, 0);
        assertEq(inc.layer2, 12 * USD);
        assertEq(inc.layer4, 40 * USD);
        assertEq(comp.withdrawEntitlement(liq), 12 * USD);
        assertEq(comp.withdrawEntitlement(rel), 40 * USD);
        assertEq(usdc.balanceOf(liquidator), 12 * USD);
        assertEq(uint8(registry.claimDetails(rel).status), uint8(ClaimStatus.Pending), "F3 settles nothing");
        _assertBooks();
    }

    function test_F3_ignoresTheSignersOwnClaimsAndEmptyShortfalls() public {
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        bytes32 fee = registry.addClaim(VAULT_B, ClaimKind.Fee, ZEC / 100, address(0), false);
        bytes32 paidByBonds = registry.addClaim(VAULT_B, ClaimKind.Liquidation, ZEC, liquidator, true);
        bytes32 liq = registry.addClaim(VAULT_B, ClaimKind.Liquidation, ZEC, liquidator, false);
        vm.recordLogs();
        _misdirect(fee, ZEC / 100);
        _misdirect(paidByBonds, ZEC);
        _misdirect(liq, 0);
        _misdirect(keccak256("unknown claim"), ZEC);
        assertEq(vm.getRecordedLogs().length, 0);
        assertEq(comp.currentIncident(), 0);
    }

    function test_F3_neverAddsToAClaimItCovers() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _misdirect(liq, 3 * ZEC / 10);
        _steal(VAULT_A); // later the vault itself is compromised
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ClaimTallied(liq, 0, 0); // settled; the shortfall is what it is owed
        comp.tallyClaim(liq);
        assertEq(comp.entitlement(liq).zat, 3 * ZEC / 10);
        assertEq(comp.victim(VAULT_A).pendingClaims, 0);
        assertEq(comp.victim(VAULT_A).talliedClaims, 1);
    }

    function test_F3_lowersAnEntitlementTalliedInFull() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        comp.tallyClaim(liq); // in full: 1 ZEC
        // The claim's payout had gone out and paid the liquidator 0.7 ZEC: what it lost is the 0.3 ZEC shortfall.
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.ShortfallRegistered(1, liq, 3 * ZEC / 10);
        _misdirect(liq, 3 * ZEC / 10);
        _misdirect(liq, ZEC / 2); // never raised again
        assertEq(comp.entitlement(liq).zat, 3 * ZEC / 10);
        comp.tallyVault(VAULT_A);
        _finalize(1);
        assertEq(comp.incident(1).layer2, 12 * USD);
        assertEq(comp.withdrawEntitlement(liq), 12 * USD);
        _assertBooks();
    }

    function test_F3_afterFinalizationGivesTheDifferenceBackToThePool() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);
        uint256 fundsBefore = comp.funds();
        _misdirect(liq, ZEC / 4);
        assertEq(comp.funds(), fundsBefore + 30 * USD, "$40 set aside, $10 owed now");
        assertEq(comp.currentIncident(), 1, "no incident opened for it");
        assertEq(comp.withdrawEntitlement(liq), 10 * USD);
        _assertBooks();
    }

    // ------------------------------------------------------------------ reverts of the steps

    function test_revert_tallyClaim() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        bytes32 a = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC, alice, false);
        bytes32 paid = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC, alice, false);
        bytes32 b = registry.addClaim(VAULT_B, ClaimKind.Release, ZEC, bob, false);
        registry.setClaimStatus(paid, ClaimStatus.Paid);
        _steal(VAULT_A);

        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, b));
        comp.tallyClaim(b); // B is not a victim
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, keccak256("x")));
        comp.tallyClaim(keccak256("x")); // no such claim
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictimClaim.selector, paid));
        comp.tallyClaim(paid); // not pending
        comp.tallyClaim(a);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadyTallied.selector, a));
        comp.tallyClaim(a);
    }

    function test_revert_tallyVault() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 one = registry.addClaim(VAULT_A, ClaimKind.Release, ZEC, alice, false);
        registry.addClaim(VAULT_A, ClaimKind.Fee, ZEC / 100, address(0), false);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictim.selector, VAULT_A));
        comp.tallyVault(VAULT_A);
        _steal(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ClaimsNotTallied.selector, VAULT_A, uint64(2)));
        comp.tallyVault(VAULT_A);
        comp.tallyClaim(one);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.ClaimsNotTallied.selector, VAULT_A, uint64(1)));
        comp.tallyVault(VAULT_A);
        _tally(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadyTallied.selector, VAULT_A));
        comp.tallyVault(VAULT_A);
    }

    function test_revert_finalizeIncident() public {
        vm.expectRevert(abi.encodeWithSelector(ZecuredCompensator.UnknownIncident.selector, 0));
        comp.finalizeIncident(0);
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        _steal(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(ZecuredCompensator.UnknownIncident.selector, 2));
        comp.finalizeIncident(2); // never opened, so never finalizable ahead of time
        uint64 closesAt = comp.incident(1).closesAt;
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.IncidentStillOpen.selector, 1, closesAt));
        comp.finalizeIncident(1);
        vm.warp(closesAt);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.VictimsNotTallied.selector, 1, uint32(1)));
        comp.finalizeIncident(1);
        _tally(VAULT_A);
        vm.mockCall(address(oracle), abi.encodeCall(IOracle.price, ()), abi.encode(uint256(0)));
        vm.expectRevert(ZecuredCompensator.ZeroPrice.selector);
        comp.finalizeIncident(1);
        vm.clearMockedCalls();
        comp.finalizeIncident(1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadyFinalized.selector, 1));
        comp.finalizeIncident(1);
    }

    function test_incidents_areFinalizedInOrder() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        _openVault(VAULT_B, bob, 10 * ZEC, 0);
        _fund(400 * USD); // what incident 1 needs, and no more
        _steal(VAULT_A);
        vm.warp(block.timestamp + WINDOW);
        _steal(VAULT_B);
        _tally(VAULT_A);
        _tally(VAULT_B);
        vm.warp(block.timestamp + WINDOW);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotFinalized.selector, 1));
        comp.finalizeIncident(2);
        comp.finalizeIncident(1);
        comp.finalizeIncident(2);
        assertEq(comp.incident(1).r4, WAD);
        assertEq(comp.incident(2).r4, 0, "the earlier incident was paid first");
    }

    function test_finalize_pricesAtMostOneBatchPerCall() public {
        uint256 batch = comp.FINALIZE_BATCH();
        uint256 n = batch + 4;
        _fund(100_000 * USD);
        for (uint256 i; i < n; ++i) {
            bytes32 vaultId = keccak256(abi.encode("zecured.comp.batch", i));
            _openVault(vaultId, address(uint160(0x10000 + i)), ZEC, 0);
            _steal(vaultId);
            comp.tallyVault(vaultId);
        }
        vm.warp(block.timestamp + WINDOW);
        vm.cool(address(comp)); // as in a fresh transaction: every victim's storage read is cold
        comp.finalizeIncident(1);
        // About 8,500 gas per victim: a full batch stays far below EIP-7825's 2^24 per transaction.
        uint256 batchGas = vm.lastFrameGas().gasTotalUsed;
        emit log_named_uint("finalizeIncident, one full batch, cold storage (gas)", batchGas);
        assertLt(batchGas, 2_500_000, "a full batch");
        Incident memory inc = comp.incident(1);
        assertFalse(inc.finalized, "one batch priced");
        assertEq(inc.price, PRICE, "P* is fixed by the first call");
        oracle.setPrice(80e34);
        comp.finalizeIncident(1);
        inc = comp.incident(1);
        assertTrue(inc.finalized);
        assertEq(inc.price, PRICE);
        assertEq(inc.layer4, n * 40 * USD, "every victim priced once, at P*");
        assertEq(inc.r4, WAD);
    }

    function test_revert_settleVault() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictim.selector, VAULT_A));
        comp.settleVault(VAULT_A);
        _steal(VAULT_A);
        _tally(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotFinalized.selector, 1));
        comp.settleVault(VAULT_A);
        _finalize(1);
        comp.settleVault(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadySettled.selector, VAULT_A));
        comp.settleVault(VAULT_A);
    }

    function test_revert_doubleWithdrawals() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotFinalized.selector, 1));
        comp.withdrawEntitlement(liq);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotSettled.selector, VAULT_A));
        comp.withdrawEquity(VAULT_A);
        _finalize(1);
        comp.settleVault(VAULT_A);

        comp.withdrawEntitlement(liq);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NothingOwed.selector, liq));
        comp.withdrawEntitlement(liq);
        comp.withdrawEquity(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NothingOwed.selector, VAULT_A));
        comp.withdrawEquity(VAULT_A);
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NothingOwed.selector, keccak256("x")));
        comp.withdrawEntitlement(keccak256("x"));
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.NotVictim.selector, VAULT_B));
        comp.withdrawEquity(VAULT_B);
        assertEq(usdc.balanceOf(liquidator), 40 * USD);
        assertEq(usdc.balanceOf(alice), 400 * USD);
        _assertBooks();
    }

    /// Defence in depth: Registry v3 refuses to revive a refund, or to open one, on a compromised vault, so no claim
    /// becomes pending after its vault was tallied. If one ever did, it could not join a finalized incident.
    function test_lateClaim_cannotJoinAFinalizedIncident() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 0);
        bytes32 refund = registry.addClaim(VAULT_A, ClaimKind.Refund, ZEC, alice, false);
        registry.setClaimStatus(refund, ClaimStatus.WrittenOff); // abandoned before the theft
        _steal(VAULT_A);
        _tally(VAULT_A);
        _finalize(1);
        registry.setClaimStatus(refund, ClaimStatus.Pending); // what the MOCK allows and the Registry refuses
        vm.expectRevert(abi.encodeWithSelector(IZecuredCompensator.AlreadyFinalized.selector, 1));
        comp.tallyClaim(refund);
    }

    // ------------------------------------------------------------------ leftover

    function test_sweepLeftover_onlyOnceNothingIsOpen() public {
        _openVault(VAULT_A, alice, 10 * ZEC, 200 * USD);
        _openVault(VAULT_B, bob, 10 * ZEC, 0); // a sound vault, still open
        bytes32 liq = registry.addClaim(VAULT_A, ClaimKind.Liquidation, ZEC, liquidator, false);
        _fund(1_000 * USD);
        _steal(VAULT_A);
        _tally(VAULT_A);
        vm.expectRevert(IZecuredCompensator.LeftoverLocked.selector);
        comp.sweepLeftover(); // the incident is open (and so are both vaults)
        _finalize(1);
        comp.settleVault(VAULT_A);
        vm.expectRevert(IZecuredCompensator.LeftoverLocked.selector);
        comp.sweepLeftover(); // B is open: it can still be stolen

        registry.setVault(VAULT_B, bob, 0, VaultStatus.Closed);
        slasher.setOpenCaseCount(1);
        vm.expectRevert(IZecuredCompensator.LeftoverLocked.selector);
        comp.sweepLeftover(); // a case may still bring a victim
        slasher.setOpenCaseCount(0);

        usdc.mint(address(comp), 5 * USD); // sent here directly
        uint256 owed = comp.reserved(); // the liquidator's $40 and Alice's $200
        uint256 expected = comp.funds() + 5 * USD;
        vm.expectEmit(address(comp));
        emit IZecuredCompensator.LeftoverSwept(SINK, expected);
        assertEq(comp.sweepLeftover(), expected);
        assertEq(usdc.balanceOf(SINK), expected);
        assertEq(comp.funds(), 0);
        assertEq(usdc.balanceOf(address(comp)), owed, "what is owed stays");
        assertEq(comp.withdrawEntitlement(liq), 40 * USD);
        assertEq(comp.withdrawEquity(VAULT_A), 200 * USD);
        assertEq(comp.sweepLeftover(), 0);
        _assertBooks();
    }

    function test_sweepLeftover_waitsForAnOpenIncidentEvenWithNoOpenVault() public {
        registry.setVault(VAULT_C, carol, 0, VaultStatus.Closed);
        bytes32 rel = registry.addClaim(VAULT_C, ClaimKind.Release, ZEC, carol, false);
        registry.setClaimStatus(rel, ClaimStatus.Paid); // confirmed paid, but the payout was misdirected
        _fund(1_000 * USD);
        _misdirect(rel, ZEC / 2);
        assertEq(registry.openVaultCount(), 0);
        vm.expectRevert(IZecuredCompensator.LeftoverLocked.selector);
        comp.sweepLeftover();
        _finalize(1);
        assertEq(comp.sweepLeftover(), 980 * USD);
        assertEq(comp.withdrawEntitlement(rel), 20 * USD);
        _assertBooks();
    }

    // ------------------------------------------------------------------ the whole flow, fuzzed

    uint256 internal constant FUZZ_VAULTS = 3;
    bytes32[] internal _fuzzClaims;

    /// Random victims (positions, authorizations, claims of every kind), random funds, interest, borrowing after the
    /// tally and P*: the compensator never pays out (to Morpho, claimants and owners) more than it received, keeps its
    /// books (I19), and leaves at most rounding dust set aside.
    function testFuzz_compensation_neverPaysMoreThanItReceived(uint256 seed, uint256 fundsIn, uint256 priceIn) public {
        uint256 received = bound(fundsIn, 0, 3_000 * USD);
        _fund(received);
        for (uint256 i; i < FUZZ_VAULTS; ++i) {
            _fuzzVictim(seed, i);
        }
        morpho.addInterest(_rand(seed, 9, 0) % (20 * USD));
        for (uint256 i; i < FUZZ_VAULTS; ++i) {
            _fuzzBorrow(seed, i);
        }
        oracle.setPrice(bound(priceIn, 5e34, 200e34));
        _finalize(1);

        uint256 out = _fuzzPayEveryone();
        assertLe(out, received, "never more than it received");
        assertEq(out + comp.funds() + comp.reserved(), received, "every USDC accounted for");
        assertLe(comp.reserved(), 64, "at most rounding dust left set aside");
        _assertBooks();
        Incident memory inc = comp.incident(1);
        if (inc.r2 < WAD) assertTrue(inc.layer3 == 0 || inc.r3 == 0);
        if (inc.r3 < WAD) assertTrue(inc.layer4 == 0 || inc.r4 == 0);
    }

    function _fuzzVaultId(uint256 i) internal pure returns (bytes32) {
        return keccak256(abi.encode("zecured.comp.fuzz", i));
    }

    function _fuzzAccount(uint256 i) internal pure returns (address) {
        return address(uint160(0x20000 + i));
    }

    /// A vault with a random position and up to four random claims, stolen and tallied.
    function _fuzzVictim(uint256 seed, uint256 i) internal {
        bytes32 vaultId = _fuzzVaultId(i);
        address account = _fuzzAccount(i);
        uint64 outstanding = uint64(bound(_rand(seed, i, 0), ZEC / 10, 20 * ZEC));
        uint256 maxDebt = uint256(outstanding) * PRICE / 1e36 * LLTV / WAD;
        _openVault(vaultId, account, outstanding, bound(_rand(seed, i, 1), 0, maxDebt), _rand(seed, i, 2) % 4 != 0);
        for (uint256 j; j < _rand(seed, i, 3) % 5; ++j) {
            _fuzzClaim(seed, i, j);
        }
        _steal(vaultId);
        _tally(vaultId);
    }

    /// A pending claim of a random kind (Release .. Fee) and amount on fuzz vault `i`; sometimes paid by the bonds.
    function _fuzzClaim(uint256 seed, uint256 i, uint256 j) internal {
        ClaimKind kind = ClaimKind(_rand(seed, i, 10 + j) % 6);
        address claimant = kind == ClaimKind.Liquidation || kind == ClaimKind.CloseMarketMaker
            ? liquidator
            : kind == ClaimKind.Fee ? address(0) : _fuzzAccount(i);
        uint64 amount = uint64(bound(_rand(seed, i, 20 + j), 1, 5 * ZEC));
        bool bondPaid = _rand(seed, i, 30 + j) % 7 == 0;
        _fuzzClaims.push(registry.addClaim(_fuzzVaultId(i), kind, amount, claimant, bondPaid));
    }

    /// An owner may borrow more after the tally, as far as Morpho lets it.
    function _fuzzBorrow(uint256 seed, uint256 i) internal {
        address account = _fuzzAccount(i);
        uint256 room = uint256(morpho.collateralOf(account)) * PRICE / 1e36 * LLTV / WAD;
        uint256 debt = morpho.debtOf(account);
        if (_rand(seed, i, 40) % 3 != 0 || room <= debt + 2) return;
        vm.prank(account);
        try morpho.borrow(params, bound(_rand(seed, i, 41), 1, room - debt - 1), 0, account, account) {} catch {}
    }

    /// Settles every vault and withdraws everything owed; returns the USDC that left the compensator.
    function _fuzzPayEveryone() internal returns (uint256 out) {
        uint256 morphoBefore = usdc.balanceOf(address(morpho));
        for (uint256 i; i < FUZZ_VAULTS; ++i) {
            comp.settleVault(_fuzzVaultId(i));
        }
        out = usdc.balanceOf(address(morpho)) - morphoBefore;
        for (uint256 i; i < _fuzzClaims.length; ++i) {
            if (comp.entitlement(_fuzzClaims[i]).incident == 0) continue;
            try comp.withdrawEntitlement(_fuzzClaims[i]) returns (uint256 amount) {
                out += amount;
            } catch {} // NothingOwed: a layer paid at ratio 0
        }
        for (uint256 i; i < FUZZ_VAULTS; ++i) {
            if (comp.equityOf(_fuzzVaultId(i)) != 0) out += comp.withdrawEquity(_fuzzVaultId(i));
        }
    }

    function _rand(uint256 seed, uint256 a, uint256 b) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(seed, a, b)));
    }
}
