// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {ZecuredVersions} from "../../src/ZecuredVersions.sol";
import {IZecuredVersions, Version, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {IZecuredIntake} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// The verifier version registry (design notes T-07a): five signers schedule a version, which verifies only after its
/// delay; three remove one at once, never the last active violation version; while every active violation version's
/// verifier is stopped by the router, a new one comes in after the shorter estop delay. The Intake takes deposit proofs
/// only from an active version (MOCK verifier and relay).
contract VersionsTest is ZecuredFixture {
    bytes32 internal constant NEW_IMAGE = keccak256("zecured.versions.newDepositImage");
    bytes32 internal constant NEW_VIOLATION = keccak256("zecured.versions.newViolationImage");
    string internal constant SCHEDULE_TYPE =
        "ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4] branches,uint256 nonce,uint64 deadline)";
    string internal constant REMOVE_TYPE = "RemoveVersion(bytes32 versionId,uint256 nonce,uint64 deadline)";

    bytes4 internal selector;
    uint32[4] internal noBranches;

    function setUp() public override {
        super.setUp();
        selector = proofs.sealSelector;
    }

    // ------------------------------------------------------------------ helpers

    function _five() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    function _schedule(IZecuredVersions reg, VersionKind kind, bytes4 sel, bytes32 image, uint32[4] memory branches)
        internal
        returns (bytes32)
    {
        uint256 nonce = reg.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes[] memory sigs = _signerSigs(reg.hashScheduleVersion(kind, sel, image, branches, nonce, deadline), _five());
        return reg.scheduleVersion(kind, sel, image, branches, nonce, deadline, sigs);
    }

    function _scheduleExpectRevert(
        VersionKind kind,
        bytes4 sel,
        bytes32 image,
        uint32[4] memory branches,
        bytes memory err
    ) internal {
        uint256 nonce = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes[] memory sigs =
            _signerSigs(versions.hashScheduleVersion(kind, sel, image, branches, nonce, deadline), _five());
        vm.expectRevert(err);
        versions.scheduleVersion(kind, sel, image, branches, nonce, deadline, sigs);
    }

    function _removeSigs(bytes32 id) internal view returns (uint256 nonce, uint64 deadline, bytes[] memory sigs) {
        nonce = versions.nonce();
        deadline = uint64(block.timestamp + 1 hours);
        sigs = _threeSigs(versions.hashRemoveVersion(id, nonce, deadline));
    }

    function _remove(bytes32 id) internal {
        (uint256 nonce, uint64 deadline, bytes[] memory sigs) = _removeSigs(id);
        versions.removeVersion(id, nonce, deadline, sigs);
    }

    function _depositId() internal view returns (bytes32) {
        return versions.versionIdOf(VersionKind.Deposit, selector, proofs.imageId);
    }

    function _violationId() internal view returns (bytes32) {
        return versions.versionIdOf(VersionKind.Violation, selector, violationImage);
    }

    /// A deposit proof with `image` (the MOCK seal of that image): journal, seal and the five `VaultOpened` signatures.
    function _proof(address account, bytes32 image)
        internal
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        bytes32 vaultId = _vaultIdFor(account);
        bytes32 anchor = keccak256(abi.encode("zecured.versions.anchor", account));
        MockHeaderRelay(address(headerRelay)).setBlock(anchor, true, proofs.minWork);
        bytes32 opened = keccak256(abi.encode("zecured.test.opened", vaultId));
        journal = abi.encode(anchor, vaultId, account, VAULT_001, _depositTagFor(account), opened);
        seal = MockRiscZeroVerifier(address(proofVerifier)).mockSeal(image, sha256(journal));
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), _five());
    }

    function _prove(address account, bytes32 image) internal {
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proof(account, image);
        intake.attestDepositWithProof(journal, seal, image, sigs);
    }

    function _proveExpectRevert(address account, bytes32 image, bytes memory err) internal {
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proof(account, image);
        vm.expectRevert(err);
        intake.attestDepositWithProof(journal, seal, image, sigs);
    }

    function _initial() internal view returns (Version[] memory) {
        return _initialVersions();
    }

    // ------------------------------------------------------------------ the initial versions

    function test_initialVersions() public view {
        assertEq(versions.router(), proofs.verifier);
        assertEq(versions.registry(), address(registry));
        assertEq(versions.depositDelay(), VERSION_DELAY);
        assertEq(versions.violationDelay(), VERSION_DELAY);
        assertEq(versions.estopDelay(), VERSION_DELAY);
        assertEq(versions.nonce(), 0);
        assertEq(intake.versions(), address(versions));

        bytes32 deposit = _depositId();
        bytes32 violation = _violationId();
        assertEq(deposit, keccak256(abi.encode(VersionKind.Deposit, selector, proofs.imageId)));
        assertTrue(versions.isActive(deposit));
        assertTrue(versions.isActive(violation));
        Version memory v = versions.version(deposit);
        assertEq(uint8(v.kind), uint8(VersionKind.Deposit));
        assertEq(v.selector, selector);
        assertEq(v.imageId, proofs.imageId);
        assertEq(v.activeAt, block.timestamp);
        assertFalse(v.removed);
        assertEq(versions.versionIds(VersionKind.Deposit).length, 1);
        assertEq(versions.versionIds(VersionKind.Violation)[0], violation);
        assertFalse(versions.violationStopped());
        assertFalse(versions.branchAllowed(deposit, 0));
    }

    function test_constructor_reverts() public {
        address r = address(registry);
        address router = proofs.verifier;
        Version[] memory initial = _initial();
        vm.expectRevert(IZecuredVersions.ZeroAddress.selector);
        new ZecuredVersions(address(0), router, 1, 1, 1, initial);
        vm.expectRevert(IZecuredVersions.ZeroAddress.selector);
        new ZecuredVersions(r, address(0), 1, 1, 1, initial);
        vm.expectRevert(IZecuredVersions.InvalidConfig.selector);
        new ZecuredVersions(r, router, 0, 1, 1, initial);
        vm.expectRevert(IZecuredVersions.InvalidConfig.selector);
        new ZecuredVersions(r, router, 1, 1, 2, initial); // the estop delay is the shorter one
        // The router must be the Intake's verifier.
        address otherRouter = address(new MockRiscZeroVerifier());
        vm.expectRevert(IZecuredVersions.InvalidConfig.selector);
        new ZecuredVersions(r, otherRouter, 1, 1, 1, initial);

        // One version of each kind, each well formed, none twice, every violation version running.
        Version[] memory one = new Version[](1);
        one[0] = initial[0];
        vm.expectRevert(IZecuredVersions.InvalidConfig.selector);
        new ZecuredVersions(r, router, 1, 1, 1, one);
        Version[] memory twice = new Version[](3);
        (twice[0], twice[1], twice[2]) = (initial[0], initial[1], initial[0]);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.VersionExists.selector, _depositId()));
        new ZecuredVersions(r, router, 1, 1, 1, twice);
        Version[] memory bad = _initial();
        bad[1].branches[0] = 1;
        vm.expectRevert(IZecuredVersions.InvalidVersion.selector);
        new ZecuredVersions(r, router, 1, 1, 1, bad);
        bad = _initial();
        bad[0].imageId = bytes32(0);
        vm.expectRevert(IZecuredVersions.InvalidVersion.selector);
        new ZecuredVersions(r, router, 1, 1, 1, bad);
        bad = _initial();
        bad[1].selector = 0x12345678; // a selector the router does not know: that verifier is stopped
        vm.expectRevert(IZecuredVersions.InvalidConfig.selector);
        new ZecuredVersions(r, router, 1, 1, 1, bad);
    }

    // ------------------------------------------------------------------ scheduling

    function test_schedule_takesAllFive_andTheDelay() public {
        uint256 nonce = versions.nonce();
        uint64 deadline = uint64(block.timestamp + 1 hours);
        bytes32 digest =
            versions.hashScheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce, deadline);
        bytes[] memory four = _signerSigs(digest, _idx4(0, 1, 2, 3));
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.InsufficientSignatures.selector, 4));
        versions.scheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce, deadline, four);
        bytes[] memory unsorted = _signerSigs(digest, _idx5(0, 2, 1, 3, 4));
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.SignaturesNotSorted.selector, 2));
        versions.scheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce, deadline, unsorted);

        bytes32 id = versions.versionIdOf(VersionKind.Deposit, selector, NEW_IMAGE);
        uint64 activeAt = uint64(block.timestamp) + VERSION_DELAY;
        vm.expectEmit(address(versions));
        emit IZecuredVersions.VersionScheduled(id, VersionKind.Deposit, selector, NEW_IMAGE, activeAt);
        bytes[] memory five = _signerSigs(digest, _five());
        vm.prank(stranger); // anyone relays
        assertEq(
            versions.scheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce, deadline, five), id
        );
        assertEq(versions.nonce(), nonce + 1);
        assertFalse(versions.isActive(id), "not before its delay");
        assertEq(versions.versionIds(VersionKind.Deposit).length, 2);

        // The same signatures again, or a message past its deadline, are refused.
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.BadNonce.selector, nonce + 1));
        versions.scheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce, deadline, five);
        vm.warp(deadline + 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.SignatureExpired.selector, deadline));
        versions.scheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, noBranches, nonce + 1, deadline, five);

        // A version that exists cannot be scheduled again; a malformed one never.
        _scheduleExpectRevert(
            VersionKind.Deposit,
            selector,
            NEW_IMAGE,
            noBranches,
            abi.encodeWithSelector(IZecuredVersions.VersionExists.selector, id)
        );
        bytes memory invalid = abi.encodeWithSelector(IZecuredVersions.InvalidVersion.selector);
        _scheduleExpectRevert(VersionKind.Deposit, bytes4(0), NEW_IMAGE, noBranches, invalid);
        uint32[4] memory branches;
        branches[0] = 0x77190AD9;
        _scheduleExpectRevert(VersionKind.Violation, selector, NEW_VIOLATION, branches, invalid);
    }

    /// A deposit guest scheduled now verifies deposits only once its delay has passed; removing the old one stops it.
    function test_newDepositVersion_takesOverThroughTheIntake() public {
        address early = makeAddr("zecured.versions.early");
        bytes32 id = _schedule(versions, VersionKind.Deposit, selector, NEW_IMAGE, noBranches);
        bytes32 seal0 = versions.versionIdOf(VersionKind.Deposit, selector, NEW_IMAGE);
        assertEq(id, seal0);
        _proveExpectRevert(early, NEW_IMAGE, abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, id));

        vm.warp(block.timestamp + VERSION_DELAY);
        assertTrue(versions.isActive(id));
        _prove(early, NEW_IMAGE);
        assertEq(uint8(_status(_vaultIdFor(early))), uint8(VaultStatus.Pending));

        // The old guest is removed at once by three signers: its proofs no longer land.
        _remove(_depositId());
        assertFalse(versions.isActive(_depositId()));
        address late = makeAddr("zecured.versions.late");
        _proveExpectRevert(
            late, proofs.imageId, abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, _depositId())
        );
        _prove(late, NEW_IMAGE);
    }

    // ------------------------------------------------------------------ removing

    function test_remove_takesThree_neverTheLastViolationVersion() public {
        bytes32 violation = _violationId();
        (uint256 nonce, uint64 deadline,) = _removeSigs(violation);
        bytes[] memory two = _signerSigs(versions.hashRemoveVersion(violation, nonce, deadline), _idx(0, 1));
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.InsufficientSignatures.selector, 2));
        versions.removeVersion(violation, nonce, deadline, two);

        (nonce, deadline,) = _removeSigs(violation);
        bytes[] memory three = _threeSigs(versions.hashRemoveVersion(violation, nonce, deadline));
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.LastViolationVersion.selector, violation));
        versions.removeVersion(violation, nonce, deadline, three);

        // A second violation version only scheduled does not count yet; once active, the first can go.
        bytes32 next = _schedule(versions, VersionKind.Violation, selector, NEW_VIOLATION, noBranches);
        (nonce, deadline, three) = _removeSigs(violation);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.LastViolationVersion.selector, violation));
        versions.removeVersion(violation, nonce, deadline, three);
        vm.warp(block.timestamp + VERSION_DELAY);
        vm.expectEmit(address(versions));
        emit IZecuredVersions.VersionRemoved(violation);
        _remove(violation);
        assertFalse(versions.isActive(violation));
        assertTrue(versions.isActive(next));

        // Removed once; scheduled again, it waits for its delay like a new one.
        (nonce, deadline, three) = _removeSigs(violation);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.UnknownVersion.selector, violation));
        versions.removeVersion(violation, nonce, deadline, three);
        assertEq(_schedule(versions, VersionKind.Violation, selector, violationImage, noBranches), violation);
        assertFalse(versions.isActive(violation));
        assertEq(versions.versionIds(VersionKind.Violation).length, 2, "listed once");
        vm.warp(block.timestamp + VERSION_DELAY);
        assertTrue(versions.isActive(violation));

        // R-23: the last working deposit version goes only with all five signers (minting stops, exits do not).
        bytes32 deposit = _depositId();
        (nonce, deadline, three) = _removeSigs(deposit);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.LastDepositVersion.selector, deposit));
        versions.removeVersion(deposit, nonce, deadline, three);
        bytes[] memory all5 = _signerSigs(versions.hashRemoveVersion(deposit, nonce, deadline), _five());
        versions.removeVersion(deposit, nonce, deadline, all5);
        assertEq(versions.versionIds(VersionKind.Deposit).length, 1);
        assertFalse(versions.isActive(deposit));
    }

    // ------------------------------------------------------------------ the emergency stop

    /// With mainnet's delays: 14 days for a deposit guest, 7 for a violation guest, 1 while the router's emergency
    /// stop has halted every active violation verifier (only RISC Zero can trigger it).
    function test_estopShortensTheViolationDelay() public {
        ZecuredVersions mainnetLike =
            new ZecuredVersions(address(registry), proofs.verifier, 14 days, 7 days, 1 days, _initial());
        uint256 t = block.timestamp;
        bytes32 a = _schedule(mainnetLike, VersionKind.Violation, selector, NEW_VIOLATION, noBranches);
        assertEq(mainnetLike.version(a).activeAt, t + 7 days);
        bytes32 d = _schedule(mainnetLike, VersionKind.Deposit, selector, NEW_IMAGE, noBranches);
        assertEq(mainnetLike.version(d).activeAt, t + 14 days);

        MockRiscZeroVerifier(address(proofVerifier)).stop();
        assertTrue(mainnetLike.violationStopped());
        bytes32 b = _schedule(mainnetLike, VersionKind.Violation, selector, keccak256("fixed verifier"), noBranches);
        assertEq(mainnetLike.version(b).activeAt, t + 1 days);
        bytes32 d2 = _schedule(mainnetLike, VersionKind.Deposit, selector, keccak256("fixed deposit"), noBranches);
        assertEq(mainnetLike.version(d2).activeAt, t + 14 days, "the shortcut is for violation versions only");
    }

    // ------------------------------------------------------------------ EIP-712

    function test_eip712_digests() public view {
        uint32[4] memory branches = [uint32(0x77190AD9), uint32(0x37A5165B), 0, 0];
        bytes32 structHash = vm.eip712HashStruct(
            SCHEDULE_TYPE, abi.encode(uint8(0), selector, NEW_IMAGE, branches, uint256(7), uint64(1_000))
        );
        assertEq(
            versions.hashScheduleVersion(VersionKind.Deposit, selector, NEW_IMAGE, branches, 7, 1_000),
            MessageHashUtils.toTypedDataHash(versions.domainSeparator(), structHash)
        );
        bytes32 id = _depositId();
        structHash = vm.eip712HashStruct(REMOVE_TYPE, abi.encode(id, uint256(3), uint64(2_000)));
        assertEq(
            versions.hashRemoveVersion(id, 3, 2_000),
            MessageHashUtils.toTypedDataHash(versions.domainSeparator(), structHash)
        );
    }

    function _idx4(uint256 a, uint256 b, uint256 c, uint256 d) internal pure returns (uint256[] memory r) {
        r = new uint256[](4);
        (r[0], r[1], r[2], r[3]) = (a, b, c, d);
    }

    function _idx5(uint256 a, uint256 b, uint256 c, uint256 d, uint256 e) internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (a, b, c, d, e);
    }
}
