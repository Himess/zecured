// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

// Tier 3 test base (adopted from the red-team review of 2026-10-14): the real Registry, Intake, SignerBondV2,
// ZecuredSlasher and ZecuredCompensator on a Base Sepolia fork, with the MOCK verifier and the MOCK relay; all five
// signers bonded (MockUSDC).

import {ViolationKind, ViolationJournal} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "./ZecuredFixture.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

abstract contract Tier3Base is ZecuredFixture {
    bytes32 internal constant DEPOSIT_IMAGE = keccak256("tier3.depositImage");
    bytes32 internal constant ANCHOR = keccak256("tier3.anchor");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;

    address internal watcher = makeAddr("tier3.watcher");

    function _proofSetup() internal virtual override returns (ProofSetup memory) {
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

    function _bonded() internal pure virtual override returns (bool) {
        return true;
    }

    function setUp() public virtual override {
        super.setUp();
        _bondAll();
        relay.setBlock(ANCHOR, true, SLASH_WORK);
    }

    // ------------------------------------------------------------------ helpers

    function _journal(
        ViolationKind kind,
        bytes32 anchor,
        bytes32 ref,
        bytes32 noteTag,
        uint64 value,
        bytes32 dest,
        uint8 output
    ) internal view returns (bytes memory) {
        return abi.encode(
            ViolationJournal({
                kind: kind,
                anchor: anchor,
                beneficiary: watcher,
                ref: ref,
                noteTag: noteTag,
                value: value,
                destCommitment: dest,
                outputIndex: output
            })
        );
    }

    function _prove(bytes memory journal) internal returns (bytes32) {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        return
            slasher.proveViolation(kind, journal, verifier.mockSeal(VIOLATION_IMAGE, sha256(journal)), violationImage);
    }

    function _proveExpectRevert(bytes memory journal, bytes memory err) internal {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        bytes memory seal = verifier.mockSeal(VIOLATION_IMAGE, sha256(journal));
        vm.expectRevert(err);
        slasher.proveViolation(kind, journal, seal, violationImage);
    }

    function _all5() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    /// The v1 journal of a Tier 2 deposit proof and its MOCK seal and five `VaultOpened` signatures for `vault`.
    function _depositProof(bytes32 vault, address acct, uint64 amount, bytes32 tag, bytes32 anchor)
        internal
        view
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        bytes32 opened = keccak256(abi.encode("tier3.opened", vault));
        journal = abi.encode(anchor, vault, acct, amount, tag, opened);
        seal = verifier.mockSeal(DEPOSIT_IMAGE, sha256(journal));
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), _all5());
    }

    /// A Tier 2 deposit proof (MOCK seal) for `vault`, pending until the relay moves on.
    function _proveDeposit(bytes32 vault, address acct, uint64 amount, bytes32 tag, bytes32 anchor) internal {
        relay.setBlock(anchor, true, MIN_WORK);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _depositProof(vault, acct, amount, tag, anchor);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function _one(bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = a;
    }

    function _two(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](2);
        (r[0], r[1]) = (a, b);
    }

    function _confirm(bytes32 vaultId, bytes32[] memory claimIds) internal {
        registry.confirmPayout(vaultId, claimIds, _threeSigs(registry.hashPayoutConfirmation(vaultId, claimIds)));
    }

    function _send(SpendCommit memory c) internal {
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
    }
}
