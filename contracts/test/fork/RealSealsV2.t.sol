// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {IZecuredRegistry, VaultStatus, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {PendingActivation} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {ViolationKind} from "../../src/interfaces/v3/IZecuredSlasher.sol";
import {IZecuredDisputes, IZecuredDisputesEvents} from "../../src/interfaces/v3/IZecuredDisputes.sol";
import {Version, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {WorkTier} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {SwapQuote, SwapConfig, PendingSettlement, SIDE_BUY} from "../../src/interfaces/IZecuredSwap.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {SpendCommit} from "../../src/libraries/ZecuredEIP712.sol";
import {ZcashHeaderRelay} from "../../src/relay/ZcashHeaderRelay.sol";
import {ZecuredSwap} from "../../src/ZecuredSwap.sol";
import {ZecuredVersions} from "../../src/ZecuredVersions.sol";
import {MockUSDC} from "../../src/mocks/MockUSDC.sol";
import {RelayFixtures} from "../relay/RelayFixtures.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockVersionsHost} from "../mocks/MockVersionsHost.sol";
import {DigestWallet} from "../unit/ZecuredSwap.t.sol";
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

/// @notice O-8: statement v2's pinned violation-test guest with REAL Groth16 seals on the TEST-ONLY vault 9's real
///         Testnet payout (it spent the vault's deposit note): as an F1 case it slashes all five bonds; as an
///         `Included` proof of the same payout, once a commitment names it, it makes the claim final (proven delivery,
///         D76). Only the header relay is a MOCK here, as in the Tier 3 suites (the payout's anchor is not among the
///         relay fixtures).
/// @dev The F1 seal proves tier2/statement/tests/fixtures/TEST-ONLY_vault9_f1_witness.json; the `Included` seal a
///      witness derived from it (kind Included, the vault's tag key K, the same payout and anchor), never committed: it
///      holds K (docs/TESTING.md § 2b).
contract RealViolationSealV2Test is ZecuredFixture, RealSealsV2 {
    bytes32 internal constant TAG = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
    bytes32 internal constant PAYOUT_ANCHOR = 0xe47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000;
    /// The payout's reference under K, `payoutRef` in both journals.
    bytes32 internal constant PAYOUT_REF = 0x0a20962f2e701d49033b677adf918a7f8a38e3115e52f52ea3567140930e25ba;
    address internal constant BENEFICIARY = 0xBEbeBeBEbeBebeBeBEBEbebEBeBeBebeBeBebebe;

    /// The F1 journal (the same bytes as statement v1's: the layout did not change) and its v2 seal.
    bytes internal constant F1_JOURNAL = hex"0000000000000000000000000000000000000000000000000000000000000001"
        hex"e47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000"
        hex"000000000000000000000000bebebebebebebebebebebebebebebebebebebebe"
        hex"0a20962f2e701d49033b677adf918a7f8a38e3115e52f52ea3567140930e25ba"
        hex"d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000";
    bytes internal constant F1_SEAL = hex"73c457ba26e851e694285b0596470b211660b7901708ac54a507dc2533142ae9224c42c21e7d03d7a13baf965fdd71fe"
        hex"6b0bff69af0af8f6200339ca9ffd4fd06cad681e01ba9fe1535a0bd013b9da0ffa3fc01dc4271fbfd3c59c0dea22458b"
        hex"dd9df31b1d2db0d27d3d63870d4d09f0fa0b4cd50cd24e5748b3da3041e1533c75e6b8912fe366ddae52dd1102cca2a7"
        hex"2cd643657a0b87b4c4b5a1c27a8fb00498b6b73715413814d6e6f331fc33ecae7bb226f312f7293f43f0aa09e141d969"
        hex"7a87548b0ee40635e4b21c31f0fffdda8349d1cd5ff41fbf043ef98ea1aa092f49284220031d5eb1492924ea1e183f5b"
        hex"0281af877b5868e30c3e8cd0f6a628dedd5db0a2";

    /// The `Included` journal: kind 5, the anchor, a zero beneficiary, the payout's reference, four zero words.
    bytes internal constant INCLUDED_JOURNAL = hex"0000000000000000000000000000000000000000000000000000000000000005"
        hex"e47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0a20962f2e701d49033b677adf918a7f8a38e3115e52f52ea3567140930e25ba"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000000000000";
    bytes internal constant INCLUDED_SEAL = hex"73c457ba1e177a5dcfd4e86aa043d6423faa56c793acc1e36f117a30349b598b9b52254b264f68ca89fb22d8723aba80"
        hex"6ae57f787a5e830ca2cf5d4c91e61cc212d437520fc1e9c97de613cd511587880dffc6f5ad65868aa9af303b8da24ce5"
        hex"16993d2d060466b593c4bd214ca43136d842b7decb1beb36d9e46d96edd08a7705c1aad7175ae72478c63bdfa9bca02e"
        hex"78f3505bb8ddb1a1148534b120862850b5763bc2090172f8d1ec817c287886acc1a864bc8dbdc6961466ff93fa167809"
        hex"5af89ff026274bd29719f50e86efa736965a192f99d733a407e4a5daaea2f228f02e0d671bf19c0e23307ffdcd95459b"
        hex"6c301ee9f44310a0e28cc0cd48c297aded4f0782";

    MockHeaderRelay internal relay;
    address internal vault9Account = makeAddr("zecured.tier3.vault9");

    function _proofSetup() internal override returns (ProofSetup memory) {
        relay = new MockHeaderRelay();
        return ProofSetup({
            relay: address(relay),
            verifier: address(ROUTER),
            imageId: DEPOSIT_TEST,
            sealSelector: GROTH16,
            minWork: MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function _bonded() internal pure override returns (bool) {
        return true;
    }

    function _violationImageId() internal pure override returns (bytes32) {
        return VIOLATION_TEST;
    }

    function _journalV2() internal pure override returns (bool v2, uint32[4] memory branchIds) {
        v2 = true;
        branchIds[0] = NU7;
    }

    function setUp() public override {
        super.setUp();
        _bondAll();
        relay.setBlock(PAYOUT_ANCHOR, true, SLASH_WORK);
    }

    function _totalBonds() internal view returns (uint256 sum) {
        for (uint8 i; i < 5; ++i) {
            sum += bond.bondOf(i);
        }
    }

    /// Vault 9, activated without its proof (TEST-ONLY harness): its deposit tag is the note the payout spends.
    function _openVault9() internal returns (bytes32 vaultId) {
        vaultId = keccak256("zecured.tier3.vault9");
        intake.harnessActivate(vaultId, vault9Account, VAULT_001, TAG);
    }

    function test_realSealsV2_areAcceptedByTheRouter() public view {
        assertEq(bytes4(F1_SEAL), GROTH16);
        assertEq(bytes4(INCLUDED_SEAL), GROTH16);
        ROUTER.verify(F1_SEAL, VIOLATION_TEST, sha256(F1_JOURNAL));
        ROUTER.verify(INCLUDED_SEAL, VIOLATION_TEST, sha256(INCLUDED_JOURNAL));
    }

    function test_realF1SealV2_slashesAllFiveBondsForTheRealUncommittedSpend() public {
        bytes32 vaultId = _openVault9();
        uint256 gasBefore = gasleft();
        bytes32 caseId = slasher.proveViolation(ViolationKind.Uncommitted, F1_JOURNAL, F1_SEAL, violationImage);
        emit log_named_uint("proveViolation gas (v2 guest), real Groth16 verification included", gasBefore - gasleft());
        assertEq(slasher.caseOf(caseId).vaultId, vaultId);

        vm.warp(block.timestamp + WINDOW);
        slasher.executeSlash(caseId);
        assertEq(_totalBonds(), 0, "all five bonds slashed");
        assertEq(bond.bountyOf(BENEFICIARY), BOUNTY_CAP, "the bounty goes to the journal's beneficiary");
        assertTrue(bond.halted(), "minting halted");
    }

    /// The same payout, committed for the vault's release: the `Included` proof makes the claim final with no
    /// confirmation, and after a dispute it upholds the claim.
    function test_realIncludedSealV2_makesTheCommittedClaimFinal() public {
        bytes32 vaultId = _openVault9();
        bytes32 claimId = _commitThePayout(vaultId);

        uint256 gasBefore = gasleft();
        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutProven(vaultId, claimId, PAYOUT_REF);
        vm.prank(stranger); // anyone submits the proof
        slasher.proveInclusion(INCLUDED_JOURNAL, INCLUDED_SEAL, violationImage, claimId);
        emit log_named_uint("proveInclusion gas, real Groth16 verification included", gasBefore - gasleft());
        assertEq(uint8(registry.claimDetails(claimId).status), uint8(ClaimStatus.Paid), "final");
        assertEq(registry.vaultDetails(vaultId).unpaidClaims, 0);
    }

    function test_realIncludedSealV2_upholdsADisputedConfirmation() public {
        bytes32 vaultId = _openVault9();
        bytes32 claimId = _commitThePayout(vaultId);
        _confirmPayout(vaultId, PAYOUT_REF, _oneClaim(claimId));
        vm.prank(vault9Account);
        IZecuredDisputes(address(registry)).disputePayout(claimId);

        vm.expectEmit(address(registry));
        emit IZecuredDisputesEvents.PayoutUpheld(vaultId, claimId, PAYOUT_REF);
        slasher.proveInclusion(INCLUDED_JOURNAL, INCLUDED_SEAL, violationImage, claimId);
        assertEq(uint8(registry.claimDetails(claimId).status), uint8(ClaimStatus.Paid));
    }

    /// The seals prove their journals only: another beneficiary, or another payout, and the router refuses them.
    function test_realSealsV2_proveNoOtherJournal() public {
        _openVault9();
        bytes memory f1 = F1_JOURNAL;
        f1[95] = 0xbf; // the beneficiary's last byte
        vm.expectRevert();
        slasher.proveViolation(ViolationKind.Uncommitted, f1, F1_SEAL, violationImage);

        bytes32 claimId = _commitThePayout(keccak256("zecured.tier3.vault9"));
        bytes memory included = INCLUDED_JOURNAL;
        included[127] = 0xbb; // the payout reference's last byte
        vm.expectRevert();
        slasher.proveInclusion(included, INCLUDED_SEAL, violationImage, claimId);
    }

    function _oneClaim(bytes32 claimId) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](1);
        r[0] = claimId;
    }

    /// The account releases the vault; three signers commit the real payout (by its reference) as that claim's
    /// payment from the deposit note, with no change.
    function _commitThePayout(bytes32 vaultId) internal returns (bytes32 claimId) {
        vm.startPrank(vault9Account);
        MORPHO.setAuthorization(address(registry), true);
        MORPHO.setAuthorization(address(adapter), true);
        claimId = registry.requestRelease(vaultId, RELEASE_COMMITMENT);
        vm.stopPrank();
        bytes32[] memory refs = new bytes32[](1);
        refs[0] = keccak256("zecured.realSeals.claimRef");
        uint8[] memory outputs = new uint8[](1);
        SpendCommit memory c = SpendCommit({
            vaultId: vaultId,
            inputTag: TAG,
            claimIds: _oneClaim(claimId),
            claimRefs: refs,
            claimOutputs: outputs,
            payoutRef: PAYOUT_REF,
            changeTag: bytes32(0),
            changeValue: 0
        });
        registry.commitSpend(c, _threeSigs(registry.hashSpendCommit(c)));
    }
}

/// @notice O-8: statement v2's pinned payment-test guest (Zecured Swap) with a REAL Groth16 seal: on a ZecuredSwap at
///         0x1111..11 on chain 84532 that takes RISC Zero's router, the order of maker 0x2222..22's quote 7 settles on
///         the synthetic payment fixture's journal (tier2/payment-statement). The relay is a MOCK: the fixture's
///         anchor is a synthetic header that no relay would accept (zecured-payment-k4).
/// @dev The seal proves `zecured-tier2-host payment prove` on
///      tier2/payment-statement/tests/fixtures/TEST-ONLY_swap_payment_witness.json (docs/TESTING.md § 2b).
contract RealPaymentSealTest is Test, RealSealsV2 {
    address internal constant SWAP_AT = 0x1111111111111111111111111111111111111111;
    address internal constant FIXED_MAKER = 0x2222222222222222222222222222222222222222;
    bytes32 internal constant ORDER_ID = 0xd13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f;
    bytes32 internal constant PAYMENT_ANCHOR = 0x301c5d2807c6ed2eaebfe1e92ec0e25a2cb8bd4a7c3d8173c9d717148908d225;
    bytes32 internal constant RECEIVER_COMMITMENT = 0x0144dce00498a1cb4e3d04161414ccf1cd42b09f70fe83ddcf745973c522a0a7;
    uint64 internal constant ZATOSHI = 2_500_000;
    uint256 internal constant WORK = 1_000_000;

    bytes internal constant SEAL = hex"73c457ba2aa500c697f1998f218102612e87eed5a22e8ff883eef6e769f017a9afef4f1327f08f0afd0327bfba8aaeb2"
        hex"acb968636628854df5c48931a67f2e96db545fdd0a09ac08f8fd8046aa49498c16c0984d7164b53db3817b6ec0f38b15"
        hex"697354222d3696091198690c32fd6d0947085ebf50d99740eafcbb6090fdfecca70202d81cd8253a48c971e04f3e858f"
        hex"27805013481bb0e0a3fe29952fdbbd87efff57cb2394efd955160734b9370635dffd5145283430ebd300fdbe97fd4654"
        hex"3b15f9a00919ec0951d8321f9a5334264494ec462753d6b73f5c32b07fec2127ef3c12d11f6c8062ab8428c454aaac8a"
        hex"ed463c98b66a2d87695a089a642c2133953e441c";

    MockUSDC internal usdc;
    MockHeaderRelay internal relay;
    ZecuredVersions internal versions;
    ZecuredSwap internal swap;
    address internal taker = makeAddr("zecured.realSeals.taker");

    /// A Base Sepolia fork at the suites' pinned block, or, with `ZECURED_FORK_NETWORK=base-mainnet` as the other fork
    /// suites, Base mainnet, where RISC Zero's router is at the same address: the chain id is then set to Base
    /// Sepolia's, which the fixture's order id names.
    function setUp() public {
        if (keccak256(bytes(vm.envOr("ZECURED_FORK_NETWORK", string("base-sepolia")))) == keccak256("base-mainnet")) {
            uint256 block_ = vm.envOr("ZECURED_FORK_BLOCK", uint256(0));
            string memory mainnetRpc = vm.envOr("BASE_MAINNET_RPC_URL", string("https://mainnet.base.org"));
            if (block_ == 0) vm.createSelectFork(mainnetRpc);
            else vm.createSelectFork(mainnetRpc, block_);
            assertEq(block.chainid, 8453);
            vm.chainId(84_532);
        } else {
            string memory rpcUrl = vm.envOr("BASE_SEPOLIA_RPC_URL", string("https://sepolia.base.org"));
            vm.createSelectFork(rpcUrl, vm.envOr("ZECURED_FORK_BLOCK", uint256(47_948_000)));
            assertEq(block.chainid, 84_532);
        }
        usdc = new MockUSDC(address(this));
        relay = new MockHeaderRelay();
        relay.setTipHeight(4_500_000);
        versions = _versions();
        bytes memory creation = abi.encodePacked(type(ZecuredSwap).creationCode, abi.encode(_config()));
        vm.etch(SWAP_AT, creation);
        (bool ok, bytes memory runtime) = SWAP_AT.call("");
        assertTrue(ok);
        vm.etch(SWAP_AT, runtime);
        swap = ZecuredSwap(SWAP_AT);
        vm.etch(FIXED_MAKER, address(new DigestWallet()).code);
        usdc.mint(taker, 10e6);
        vm.prank(taker);
        usdc.approve(SWAP_AT, type(uint256).max);
    }

    /// A version registry on the router with the pinned Testnet guests, the Swap version on NU7.
    function _versions() internal returns (ZecuredVersions) {
        address[5] memory signers;
        for (uint256 i; i < 5; ++i) {
            signers[i] = address(uint160(0x5000 + i)); // strictly increasing; nothing is signed here
        }
        MockVersionsHost host = new MockVersionsHost(signers, address(ROUTER));
        uint32[4] memory nu7;
        nu7[0] = NU7;
        uint32[4] memory none;
        Version[] memory initial = new Version[](3);
        initial[0] = Version(VersionKind.Deposit, GROTH16, DEPOSIT_TEST, nu7, 0, false);
        initial[1] = Version(VersionKind.Violation, GROTH16, VIOLATION_TEST, none, 0, false);
        initial[2] = Version(VersionKind.Swap, GROTH16, PAYMENT_TEST, nu7, 0, false);
        return new ZecuredVersions(address(host), address(ROUTER), 600, 600, 600, initial);
    }

    function _config() internal view returns (SwapConfig memory c) {
        WorkTier[] memory tiers = new WorkTier[](1);
        tiers[0] = WorkTier({maxAmountZat: 1_000_000_000, minWork: WORK});
        c = SwapConfig({
            usdc: address(usdc),
            verifier: address(ROUTER),
            versions: address(versions),
            relay: address(relay),
            workTiers: tiers,
            finalizeDepth: 6,
            finalizeDelay: 150,
            maxOrderUsdc: 200e6,
            maxOrderZat: 1_000_000_000,
            minPayWindow: 1 hours,
            maxPayWindow: 6 hours,
            feeBps: 0,
            feeRecipient: address(0)
        });
    }

    function _journal(bytes32 orderId, uint64 zatoshi) internal pure returns (bytes memory) {
        return abi.encode(uint256(1), uint256(NU7), PAYMENT_ANCHOR, orderId, RECEIVER_COMMITMENT, uint256(zatoshi));
    }

    function _open() internal returns (bytes32 id) {
        SwapQuote memory q = SwapQuote({
            maker: FIXED_MAKER,
            taker: taker,
            side: SIDE_BUY,
            zatoshi: ZATOSHI,
            usdc: 1e6,
            makerReceiverCommitment: bytes32(0),
            payWindow: 6 hours,
            quoteDeadline: uint64(block.timestamp + 60),
            nonce: 7
        });
        bytes memory makerSig = abi.encodePacked(swap.hashSwapQuote(q)); // FIXED_MAKER's ERC-1271 wallet
        vm.prank(taker);
        id = swap.open(q, makerSig, RECEIVER_COMMITMENT);
    }

    function test_realPaymentSeal_isAcceptedByTheRouter() public view {
        assertEq(bytes4(SEAL), GROTH16);
        ROUTER.verify(SEAL, PAYMENT_TEST, sha256(_journal(ORDER_ID, ZATOSHI)));
    }

    function test_realPaymentSeal_settlesItsOrderThroughTheRealRouter() public {
        bytes32 id = _open();
        assertEq(id, ORDER_ID, "the fixture's order");
        relay.setBlock(PAYMENT_ANCHOR, true, WORK);
        uint256 gasBefore = gasleft();
        swap.proveSettlement(id, _journal(ORDER_ID, ZATOSHI), SEAL, PAYMENT_TEST);
        emit log_named_uint("proveSettlement gas, real Groth16 verification included", gasBefore - gasleft());
        PendingSettlement memory p = swap.pendingSettlement(id);
        assertEq(p.anchor, PAYMENT_ANCHOR);
        relay.setTipHeight(p.readyHeight);
        vm.warp(p.readyTime);
        swap.finalizeSettlement(id);
        assertEq(usdc.balanceOf(FIXED_MAKER), 1e6, "the taker's USDC to the maker who paid");
    }

    /// The seal proves this journal only: another order, or one zatoshi less, and the router refuses it.
    function test_realPaymentSeal_provesNoOtherJournal() public {
        bytes32 otherOrder = sha256(_journal(bytes32(uint256(ORDER_ID) ^ 1), ZATOSHI));
        bytes32 less = sha256(_journal(ORDER_ID, ZATOSHI - 1));
        vm.expectRevert();
        ROUTER.verify(SEAL, PAYMENT_TEST, otherOrder);
        vm.expectRevert();
        ROUTER.verify(SEAL, PAYMENT_TEST, less);
    }
}
