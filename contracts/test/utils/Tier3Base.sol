// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

// Tier 3 test base (adopted from the red-team review of 2026-10-14). Same wiring as test/fork/Tier3Slashing.t.sol: the
// real Registry, SignerBond and ZecuredSlasher on a Base Sepolia fork, with the MOCK verifier and the MOCK relay; all
// five signers bonded.

import {SignerBond} from "../../src/SignerBond.sol";
import {ZecuredSlasher} from "../../src/ZecuredSlasher.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {ViolationKind, ViolationJournal} from "../../src/interfaces/IZecuredSlasher.sol";
import {DepositProofConfig, MintMode} from "../../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "./ZecuredFixture.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

abstract contract Tier3Base is ZecuredFixture {
    bytes32 internal constant DEPOSIT_IMAGE = keccak256("tier3.depositImage");
    bytes32 internal constant VIOLATION_IMAGE = keccak256("tier3.violationImage");
    /// docs/TIER3.md § 8: >= 100 normal Testnet blocks of work (~3.8e7), i.e. ~380,000 per block.
    uint256 internal constant SLASH_WORK = 38_000_000;
    uint256 internal constant WORK_PER_BLOCK = 380_000;
    /// docs/TIER3.md § 4 / D26: the Registry's deposit-proof parameters.
    uint256 internal constant MIN_WORK = 3_500_000;
    uint64 internal constant FINALIZE_DEPTH = 6;
    uint64 internal constant FINALIZE_DELAY = 150;
    uint64 internal constant WINDOW = 1 days;
    bytes32 internal constant ANCHOR = keccak256("tier3.anchor");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    SignerBond internal bond;
    /// The nonce the bond predicted for the slasher.
    uint64 internal slasherNonce;
    ZecuredSlasher internal slasher;

    address internal watcher = makeAddr("tier3.watcher");

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        relay = new MockHeaderRelay();
        verifier = new MockRiscZeroVerifier();
        return DepositProofConfig({
            verifier: IRiscZeroVerifier(address(verifier)),
            imageId: DEPOSIT_IMAGE,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: MIN_WORK,
            mintMode: MintMode.SignerAttested, // both paths, so that tests can open vaults by attestation too
            sealSelector: verifier.SELECTOR(),
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function _signerBond() internal override returns (ISignerBond) {
        uint64 n = vm.getNonce(address(this));
        slasherNonce = n + 4;
        bond = new SignerBond(
            vm.computeCreateAddress(address(this), n + 2), vm.computeCreateAddress(address(this), n + 4), WINDOW
        );
        return ISignerBond(address(bond));
    }

    function setUp() public virtual override {
        super.setUp();
        // `forge test --isolate` runs each call as a transaction, which also advances this contract's nonce: put it
        // back where the bond expects the slasher (a no-op without --isolate).
        vm.setNonceUnsafe(address(this), slasherNonce);
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
        require(bond.slasher() == address(slasher), "slasher address prediction");
        for (uint256 i; i < 5; ++i) {
            vm.deal(signerSet[i], 1 ether);
            vm.prank(signerSet[i]);
            bond.deposit{value: 0.01 ether}();
        }
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
        return slasher.proveViolation(kind, journal, verifier.mockSeal(VIOLATION_IMAGE, sha256(journal)));
    }

    function _proveExpectRevert(bytes memory journal, bytes memory err) internal {
        ViolationKind kind = abi.decode(journal, (ViolationJournal)).kind;
        bytes memory seal = verifier.mockSeal(VIOLATION_IMAGE, sha256(journal));
        vm.expectRevert(err);
        slasher.proveViolation(kind, journal, seal);
    }

    function _all5() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    /// A Tier 2 deposit proof (MOCK seal) for `vault`, pending until the relay moves on.
    function _proveDeposit(bytes32 vault, address acct, uint64 amount, bytes32 tag, bytes32 anchor) internal {
        bytes32 opened = keccak256(abi.encode("tier3.opened", vault));
        relay.setBlock(anchor, true, MIN_WORK);
        bytes memory journal = abi.encode(anchor, vault, acct, amount, tag, opened);
        bytes[] memory sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), opened), _all5());
        registry.attestDepositWithProof(journal, verifier.mockSeal(DEPOSIT_IMAGE, sha256(journal)), sigs);
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
