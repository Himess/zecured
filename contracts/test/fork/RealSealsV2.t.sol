// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZecuredRegistry, VaultStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {PendingActivation} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {ZcashHeaderRelay} from "../../src/relay/ZcashHeaderRelay.sol";
import {RelayFixtures} from "../relay/RelayFixtures.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// RISC Zero's verifier router on Base Sepolia and its Groth16 selector (docs/TIER2.md § 5), and statement v2's pinned
/// Testnet guests (tier2/pinned-image-ids.txt): the ids the judge deployment and the theft demo register.
abstract contract RealSealsV2 {
    IRiscZeroVerifier internal constant ROUTER = IRiscZeroVerifier(0x0b144E07A0826182B6b59788c34b32Bfa86Fb711);
    bytes4 internal constant GROTH16 = 0x73c457ba;
    bytes32 internal constant DEPOSIT_TEST = 0x406c6d268f29270226c775db1fe4bec04acf51ba919e250078125d71c10bff41;
    bytes32 internal constant VIOLATION_TEST = 0x65705f75c5482b2b0cf6fee12760b2364d48213bb2933f3882dc2d92f824866e;
    bytes32 internal constant PAYMENT_TEST = 0x199bf772d0780413a07081b4889f0473c8ae558c6d8c4dca96eef49b5e486134;
    /// The NU7 epoch's consensus branch id (D56): the only one the judge deployment's versions list.
    uint32 internal constant NU7 = 0x77190AD9;
}

/// @notice O-8: the real deposit of the TEST-ONLY vault 9 proven by statement v2's pinned deposit-test guest (journal v2,
///         D56), with a REAL Groth16 seal: RISC Zero's router checks the seal, the real header relay the anchor, and
///         the Intake reads journal v2 with the NU7 branch id as the judge deployment does (minting only on a proof).
/// @dev The seal proves `zecured-tier2-host prove` on tier2/statement/tests/fixtures/TEST-ONLY_vault9_witness.json with
///      the pinned ELF (its SHA-256 and image id checked on the prover), wrapped by `zecured-tier2-host groth16`
///      (docs/TESTING.md § 2b).
contract RealDepositSealV2Test is ZecuredFixture, RelayFixtures, RealSealsV2 {
    string internal constant ANCHOR = "test/relay/fixtures/testnet_deposit_anchor.json";
    uint256 internal constant SUITE_MIN_WORK = 1_000_000;
    uint256 internal constant CONFIRMED = 4;

    // The journal the guest committed: Activate mode and NU7's branch id, then the words of the v1 journal
    // (DepositProofs.t.sol) but for the `VaultOpened` hash, which is the v2 witness's.
    bytes32 internal constant TIP = 0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000; // 4,488,560
    bytes32 internal constant VAULT_ID = 0x482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b;
    address internal constant ACCOUNT = 0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F;
    uint64 internal constant AMOUNT = 1_000_000;
    bytes32 internal constant TAG = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
    bytes32 internal constant OPENED = 0xaaccb41983fa9d9288296dd099d5947119c2abc7e89b57237c00c76cf2680432;

    bytes internal constant SEAL = hex"73c457ba099b168e97361343a6ad75636424508fb36bdf90215043f327b63468656eda881fc8a9b6a1c76c68445c3a0b"
        hex"06516cb5a48f67e3f622eb0e2ba58932c93aa3a300e26f8349b21dfe4c1bda085eb399991559a3995293ff512ba91969"
        hex"63b7a17e163d90f5dd33338752a39a7b41c117c968418d9b083dfb97bbebebecb815e90f27c437c7640d1d836bdf443d"
        hex"03207cec47631814e9decf5476d98f175dc2d66d0fb15062fab23d2290d007fd455f4119bc493d189fa3097a8fcf3465"
        hex"de2c532011b93a24ece652b74981cdb87a45ffff9c1db6bb4575ad64bae9ae4be5c25e89044adccddcd0a53a66692d1b"
        hex"c673e497ed575a2a1728f5347a77e2f9f0ca8c7b";

    ZcashHeaderRelay internal relay;
    RealFixture internal anchor;
    uint256 internal relayed;

    function _proofSetup() internal override returns (ProofSetup memory) {
        anchor = _loadReal(ANCHOR);
        relay = _deploy(anchor);
        return ProofSetup({
            relay: address(relay),
            verifier: address(ROUTER),
            imageId: DEPOSIT_TEST,
            sealSelector: GROTH16,
            minWork: SUITE_MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function _journalV2() internal pure override returns (bool v2, uint32[4] memory branchIds) {
        v2 = true;
        branchIds[0] = NU7;
    }

    /// Submits the anchor's headers up to 4,488,560 + n - 1 (those not submitted yet).
    function _relayHeaders(uint256 n) internal {
        for (uint256 i = relayed; i < n; ++i) {
            _warpForward(anchor.headers[i]);
            relay.submitHeader(anchor.headers[i]);
        }
        if (n > relayed) relayed = n;
    }

    function _journal(uint32 branchId, uint64 amount) internal pure returns (bytes memory) {
        return abi.encode(uint8(0), branchId, TIP, VAULT_ID, ACCOUNT, amount, TAG, OPENED);
    }

    function _openedSigs() internal view returns (bytes[] memory) {
        uint256[] memory all5 = new uint256[](5);
        (all5[0], all5[1], all5[2], all5[3], all5[4]) = (0, 1, 2, 3, 4);
        return _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), all5);
    }

    function test_realSealV2_isAcceptedByTheRouter() public view {
        assertEq(bytes4(SEAL), GROTH16, "the router's Groth16 verifier");
        ROUTER.verify(SEAL, DEPOSIT_TEST, sha256(_journal(NU7, AMOUNT)));
    }

    function test_realSealV2_mintsTheRealDepositThroughTheRealRouter() public {
        _relayHeaders(CONFIRMED);
        uint256 gasBefore = gasleft();
        intake.attestDepositWithProof(_journal(NU7, AMOUNT), SEAL, DEPOSIT_TEST, _openedSigs());
        emit log_named_uint(
            "attestDepositWithProof gas (journal v2), real Groth16 verification included", gasBefore - gasleft()
        );

        _relayHeaders(CONFIRMED + FINALIZE_DEPTH);
        PendingActivation memory p = intake.pendingActivation(VAULT_ID);
        (, uint256 height,) = relay.bestChainTip();
        assertGe(height, p.readyHeight, "the relay is past the ready height");
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT_ID, ACCOUNT, AMOUNT, TAG);
        intake.finalizeActivation(VAULT_ID);

        (address account, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(account, ACCOUNT);
        assertEq(outstanding, AMOUNT);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), AMOUNT, "I1: the proven amount, minted");
        Position memory pos = MORPHO.position(registry.marketId(), ACCOUNT);
        assertEq(pos.collateral, AMOUNT, "supplied straight into the account's Morpho position");
        assertEq(zzec.balanceOf(ACCOUNT), 0, "I2: the account holds no zZEC");
    }

    /// The seal proves this journal only: one more zatoshi, or another epoch's branch id, and the router refuses it.
    function test_realSealV2_provesNoOtherJournal() public {
        bytes32 oneMore = sha256(_journal(NU7, AMOUNT + 1));
        bytes32 otherEpoch = sha256(_journal(0x37A5165B, AMOUNT)); // NU6.3
        vm.expectRevert();
        ROUTER.verify(SEAL, DEPOSIT_TEST, oneMore);
        vm.expectRevert();
        ROUTER.verify(SEAL, DEPOSIT_TEST, otherEpoch);
        _relayHeaders(CONFIRMED);
        bytes memory journal = _journal(NU7, AMOUNT + 1);
        bytes[] memory sigs = _openedSigs();
        vm.expectRevert();
        intake.attestDepositWithProof(journal, SEAL, DEPOSIT_TEST, sigs);
    }

    /// Nor for another image id: the seal is the pinned guest's.
    function test_realSealV2_isThePinnedGuests() public {
        bytes32 digest = sha256(_journal(NU7, AMOUNT));
        vm.expectRevert();
        ROUTER.verify(SEAL, VIOLATION_TEST, digest);
    }
}
