// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZecuredIntake} from "../../src/ZecuredIntake.sol";
import {IZecuredRegistry, VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {
    IZecuredIntake,
    IntakeConfig,
    WorkTier,
    PendingActivation,
    RefundReason
} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {IZecuredVersions, VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {ZecuredEIP712} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {ZcashHeaderRelay} from "../../src/relay/ZcashHeaderRelay.sol";
import {RelayFixtures} from "../relay/RelayFixtures.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Tier 2 deposit proofs through the Intake (`attestDepositWithProof`, pending until `finalizeActivation`, K3),
///         against the REAL header relay; a proof that cannot activate its vault lands as a refund (T-02).
/// @dev The journal is that of a real Zcash Testnet deposit: vault 9 of the local-fork runs of 2026-10-11 (0.01 TAZ in
///      an Ironwood-only transaction at height 4,488,543), proven against header 4,488,560 by the deposit statement
///      (tier2/statement) on real chain data: the transaction's txid path, the block's ZIP 221 history-tree leaf,
///      path and peaks, and the header's `hashBlockCommitments`. That header and 11 more are submitted to the REAL
///      relay below, which checks their Equihash solutions, difficulty and linkage on-chain (fixture cross-checked
///      against CipherScan). The proof itself is replaced by the MOCK verifier here; RealGroth16Seal.t.sol runs the
///      same journal with its real Groth16 seal through RISC Zero's verifier router.
contract DepositProofsTest is ZecuredFixture, RelayFixtures {
    string internal constant ANCHOR = "test/relay/fixtures/testnet_deposit_anchor.json";
    bytes32 internal constant IMAGE_ID = keccak256("zecured.test.depositImageId");
    uint256 internal constant SUITE_MIN_WORK = 1_000_000;
    /// Headers 4,488,560 .. 4,488,563: the anchor and three blocks (~1.5 M work) on top of it.
    uint256 internal constant CONFIRMED = 4;

    // The real deposit's journal (tier2/statement `Journal::abi_encode`).
    bytes32 internal constant TIP = 0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000; // 4,488,560
    bytes32 internal constant VAULT_ID = 0x482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b;
    address internal constant ACCOUNT = 0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F;
    uint64 internal constant AMOUNT = 1_000_000;
    bytes32 internal constant TAG = 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e;
    bytes32 internal constant OPENED = 0x385c78ae562975cc3c03af28374fd4e9d9e3f3405a18fbf81fe16d4f9e28c0f5;

    ZcashHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    RealFixture internal anchor;
    uint256 internal relayed;

    function _proofSetup() internal override returns (ProofSetup memory) {
        anchor = _loadReal(ANCHOR);
        relay = _deploy(anchor);
        verifier = new MockRiscZeroVerifier();
        return ProofSetup({
            relay: address(relay),
            verifier: address(verifier),
            imageId: IMAGE_ID,
            sealSelector: verifier.SELECTOR(),
            minWork: SUITE_MIN_WORK,
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    // ------------------------------------------------------------------ helpers

    /// Submits the anchor's headers up to 4,488,560 + n - 1 (those not submitted yet).
    function _relayHeaders(uint256 n) internal {
        for (uint256 i = relayed; i < n; ++i) {
            _warpForward(anchor.headers[i]);
            relay.submitHeader(anchor.headers[i]);
        }
        if (n > relayed) relayed = n;
    }

    /// Lets the relay reach the pending activation's ready height and time.
    function _makeReady(bytes32 vaultId) internal {
        PendingActivation memory p = intake.pendingActivation(vaultId);
        _relayHeaders(CONFIRMED + FINALIZE_DEPTH);
        (, uint256 height,) = relay.bestChainTip();
        assertGe(height, p.readyHeight, "the fixture reaches the ready height");
        if (block.timestamp < p.readyTime) vm.warp(p.readyTime);
    }

    function _finalize(bytes32 vaultId) internal {
        _makeReady(vaultId);
        intake.finalizeActivation(vaultId);
    }

    function _journal(bytes32 tip, bytes32 vaultId, address account, uint64 amount, bytes32 tag, bytes32 opened)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(tip, vaultId, account, amount, tag, opened);
    }

    function _realJournal() internal pure returns (bytes memory) {
        return _journal(TIP, VAULT_ID, ACCOUNT, AMOUNT, TAG, OPENED);
    }

    function _seal(bytes memory journal) internal view returns (bytes memory) {
        return verifier.mockSeal(IMAGE_ID, sha256(journal));
    }

    function _openedDigest(bytes32 structHash) internal view returns (bytes32) {
        return MessageHashUtils.toTypedDataHash(registry.domainSeparator(), structHash);
    }

    function _all5() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    function _openedSigs(bytes32 structHash) internal view returns (bytes[] memory) {
        return _signerSigs(_openedDigest(structHash), _all5());
    }

    function _prove(bytes memory journal) internal {
        intake.attestDepositWithProof(journal, _seal(journal), proofs.imageId, _openedSigs(OPENED));
    }

    // ------------------------------------------------------------------ the real deposit

    function test_realDeposit_pendsThenMintsAgainstARelayVerifiedHeader() public {
        _relayHeaders(CONFIRMED);
        assertTrue(relay.isInBestChain(TIP), "anchor on the best chain");
        assertGe(relay.confirmationWork(TIP), SUITE_MIN_WORK, "enough work on top");
        (, uint256 tipHeight,) = relay.bestChainTip();

        bytes memory journal = _realJournal();
        vm.expectEmit(address(intake));
        emit IZecuredIntake.ActivationPending(
            VAULT_ID, ACCOUNT, AMOUNT, TAG, tipHeight + FINALIZE_DEPTH, block.timestamp + FINALIZE_DELAY
        );
        uint256 gasBefore = gasleft();
        intake.attestDepositWithProof(journal, _seal(journal), proofs.imageId, _openedSigs(OPENED));
        emit log_named_uint(
            "attestDepositWithProof gas (MOCK verifier; the router's Groth16 check adds ~240k)", gasBefore - gasleft()
        );

        // K3: reserved, not minted.
        (address account, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.Pending));
        assertEq(account, ACCOUNT);
        assertEq(outstanding, 0);
        assertEq(registry.vaultOf(ACCOUNT), VAULT_ID, "the account is reserved");
        assertTrue(registry.depositTagUsed(TAG), "the tag is reserved");
        assertEq(registry.lineageOf(TAG).value, AMOUNT, "the deposit note is a lineage note from now on");
        assertEq(zzec.totalSupply(), 0, "nothing minted before finalization");
        assertEq(intake.pendingTotal(), AMOUNT);
        PendingActivation memory p = intake.pendingActivation(VAULT_ID);
        assertEq(p.account, ACCOUNT);
        assertEq(p.amount, AMOUNT);
        assertEq(p.depositTag, TAG);
        assertEq(p.anchor, TIP);
        assertEq(p.readyHeight, tipHeight + FINALIZE_DEPTH);
        assertEq(p.readyTime, block.timestamp + FINALIZE_DELAY);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredIntake.ActivationNotReady.selector, VAULT_ID, p.readyHeight, p.readyTime)
        );
        intake.finalizeActivation(VAULT_ID);

        _makeReady(VAULT_ID);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT_ID, ACCOUNT, AMOUNT, TAG);
        gasBefore = gasleft();
        intake.finalizeActivation(VAULT_ID);
        emit log_named_uint("finalizeActivation gas (mint and Morpho supply included)", gasBefore - gasleft());

        (account, outstanding, status) = registry.vault(VAULT_ID);
        assertEq(account, ACCOUNT);
        assertEq(outstanding, AMOUNT);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(registry.vaultOf(ACCOUNT), VAULT_ID);
        assertTrue(registry.depositTagUsed(TAG));
        assertEq(intake.pendingActivation(VAULT_ID).account, address(0), "no longer pending");
        assertEq(intake.pendingTotal(), 0);
        Position memory pos = MORPHO.position(marketId, ACCOUNT);
        assertEq(pos.collateral, AMOUNT, "collateral supplied on behalf of the account (I1)");
        assertEq(zzec.totalSupply(), AMOUNT);
        assertEq(zzec.balanceOf(address(registry)), 0, "no zZEC left with the Registry (I2)");
        assertEq(zzec.balanceOf(ACCOUNT), 0, "the account holds no zZEC (I2)");
    }

    function test_theJournalIsTheProofsOutput() public pure {
        // Six ABI words, exactly what today's guest commits (journal v1).
        assertEq(_realJournal().length, 192);
    }

    // ------------------------------------------------------------------ the anchor

    function test_revert_anchorUnknownToTheRelay() public {
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, TIP, 0));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_notEnoughWorkOnTopOfTheAnchor() public {
        _relayHeaders(2); // the anchor and one block (~0.5 M work < 1 M)
        uint256 work = relay.confirmationWork(TIP);
        assertGt(work, 0);
        assertLt(work, SUITE_MIN_WORK);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, TIP, work));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_anchorIsTheRelayTip() public {
        _relayHeaders(1); // on the best chain, nothing on top
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.AnchorNotConfirmed.selector, TIP, 0));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    // ------------------------------------------------------------------ VaultOpened signatures

    function test_revert_fourOfFiveOpenedSignatures() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _signerSigs(_openedDigest(OPENED), _idx4(0, 1, 2, 4));
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InsufficientSignatures.selector, 4));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_signaturesOverAnotherVaultOpened() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(keccak256("another vault"));
        bytes memory seal = _seal(journal);
        vm.expectRevert(); // recovers to an address outside the set
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_duplicateOpenedSignature() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        uint256[] memory idx = _all5();
        idx[4] = 3;
        bytes[] memory sigs = _signerSigs(_openedDigest(OPENED), idx);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.SignaturesNotSorted.selector, 4));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    // ------------------------------------------------------------------ the proof binds the journal

    function test_revert_sealOfAnotherJournal() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        // Same signatures (they only cover the VaultOpened hash): only the proof binds account and amount to it.
        bytes memory otherAccount = _journal(TIP, VAULT_ID, stranger, AMOUNT, TAG, OPENED);
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        intake.attestDepositWithProof(otherAccount, seal, proofs.imageId, sigs);

        bytes memory moreZec = _journal(TIP, VAULT_ID, ACCOUNT, 5_000_000, TAG, OPENED);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        intake.attestDepositWithProof(moreZec, seal, proofs.imageId, sigs);
    }

    function test_revert_sealForAnotherImage() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes memory seal = verifier.mockSeal(keccak256("another guest"), sha256(journal));
        bytes[] memory sigs = _openedSigs(OPENED);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_sealWithAnotherSelector() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        seal[0] = 0x73; // a verifier the router might add later under a new selector: no version names it (T-07a)
        bytes32 other = versions.versionIdOf(VersionKind.Deposit, bytes4(0x73ffffff), proofs.imageId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, other));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        bytes32 none = versions.versionIdOf(VersionKind.Deposit, bytes4(0), proofs.imageId);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, none));
        intake.attestDepositWithProof(journal, hex"ffffff", proofs.imageId, sigs);
        // Nor an image id that is not a version, even with the right selector.
        bytes32 otherImage = versions.versionIdOf(VersionKind.Deposit, bytes4(0xffffffff), keccak256("other image"));
        bytes memory goodSeal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredVersions.VersionNotActive.selector, otherImage));
        intake.attestDepositWithProof(journal, goodSeal, keccak256("other image"), sigs);
    }

    function test_revert_journalLength() public {
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory longer = abi.encodePacked(journal, uint8(0));
        bytes memory longerSeal = _seal(longer);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InvalidJournalLength.selector, 193));
        intake.attestDepositWithProof(longer, longerSeal, proofs.imageId, sigs);
        bytes memory shorter = new bytes(191);
        bytes memory shorterSeal = _seal(shorter);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InvalidJournalLength.selector, 191));
        intake.attestDepositWithProof(shorter, shorterSeal, proofs.imageId, sigs);
    }

    function test_revert_nonCanonicalAccountWord() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        journal[64] = 0x01; // high byte of the account word
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(); // abi.decode rejects a dirty address word
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    // ------------------------------------------------------------------ I4, I5: one note lands once

    function test_revert_sameDepositTwice() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.DepositTagUsed.selector, TAG));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);

        _finalize(VAULT_ID);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.DepositTagUsed.selector, TAG));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_tagAlreadyUsedByAnotherVault() public {
        _relayHeaders(CONFIRMED);
        // The same note (same tag) already backs another vault and account.
        intake.harnessActivate(keccak256("other vault"), stranger, AMOUNT, TAG);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.DepositTagUsed.selector, TAG));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    // ------------------------------------------------------------------ T-02: what cannot activate lands as a refund

    function test_accountWithAnotherVault_landsAsARefund() public {
        _relayHeaders(CONFIRMED);
        bytes32 other = _attest(ACCOUNT, VAULT_001);
        bytes memory journal = _realJournal();
        vm.expectEmit(address(intake));
        emit IZecuredIntake.DepositRefunded(VAULT_ID, registry.claimIdOf(VAULT_ID, 0), RefundReason.AccountHasVault);
        _prove(journal);
        (address account,, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.Cancelled));
        assertEq(account, ACCOUNT);
        assertEq(registry.vaultOf(ACCOUNT), other, "the account keeps its own vault");
        _assertClaim(
            registry.claimIdOf(VAULT_ID, 0), VAULT_ID, ClaimKind.Refund, AMOUNT, bytes32(0), ClaimStatus.Pending
        );
        assertEq(registry.lineageOf(TAG).value, AMOUNT, "the refunded note is a lineage note");
        assertEq(zzec.totalSupply(), VAULT_001, "nothing minted for the refund");
    }

    function test_secondProofForAPendingVault_landsAsARefund() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        bytes32 tag2 = keccak256("2nd note");
        bytes memory journal2 = _journal(TIP, VAULT_ID, ACCOUNT, AMOUNT, tag2, OPENED);
        intake.attestDepositWithProof(journal2, _seal(journal2), proofs.imageId, _openedSigs(OPENED));
        (,, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.Pending), "the first deposit still pends");
        _assertClaim(
            registry.claimIdOf(VAULT_ID, 0), VAULT_ID, ClaimKind.Refund, AMOUNT, bytes32(0), ClaimStatus.Pending
        );
        // The first one still activates.
        _finalize(VAULT_ID);
        (,, status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), AMOUNT);
    }

    /// The cross-language VaultOpened vector of signer/crates/base-client (also asserted by the app and by
    /// tier2/statement): the struct hash a proof computes and the digest the signers sign for it.
    function test_vaultOpenedGoldenVector() public pure {
        bytes32 structHash = ZecuredEIP712.hashVaultOpened(
            bytes32(uint256(0x5555555555555555555555555555555555555555555555555555555555555555)),
            address(0x8888888888888888888888888888888888888888),
            address(0x3333333333333333333333333333333333333333),
            1_000_000,
            4_486_288,
            0x4871c7074d79798b7991d7f71f57e723823958fec9d143805fd5127ccfa55f59,
            0xd35968aa50b8474e84cfea69d0eff1947f4ca7211cff9741684554da48617ad5 // tagKeyHash (any 32 bytes here)
        );
        // VaultOpened v2 (the tag key's hash), recomputed independently with `cast abi-encode` and `cast keccak`; the
        // unit suite pins the vector of a real tag key (test/unit/ZecuredEIP712.t.sol).
        assertEq(structHash, 0xcb1157d9c2fcb81a66281f750f8c2b2d51a688d158d63d986bfca146fa5bb9ab);
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("ZecuredRegistry"),
                keccak256("1"),
                84532,
                address(0x1111111111111111111111111111111111111111)
            )
        );
        assertEq(
            MessageHashUtils.toTypedDataHash(domain, structHash),
            0x804f353ce8dff686e3d102b105f230cefaa2a114b343393a5597c4ee05b7bfd1
        );
    }

    // ------------------------------------------------------------------ configuration

    function test_config() public view {
        assertEq(address(intake.verifier()), address(verifier));
        assertEq(intake.versions(), address(versions));
        assertTrue(versions.isActive(versions.versionIdOf(VersionKind.Deposit, bytes4(0xffffffff), IMAGE_ID)));
        assertEq(address(intake.relay()), address(relay));
        assertEq(intake.minConfirmationWorkFor(AMOUNT), SUITE_MIN_WORK);
        assertEq(intake.finalizeDepth(), FINALIZE_DEPTH);
        assertEq(intake.finalizeDelay(), FINALIZE_DELAY);
        assertFalse(intake.journalV2());
        assertEq(intake.JOURNAL_V1_LENGTH(), 192);
        assertEq(intake.registry(), address(registry));
        assertEq(registry.intake(), address(intake));
    }

    /// An Intake config the Registry under test would accept (it names `address(intake)`: deploy at that address).
    function _goodIntakeConfig() internal view returns (IntakeConfig memory c) {
        Predicted memory p;
        p.versions = address(versions);
        c = _intakeConfig(p, CapSetup(0, 0, 0, address(0)));
    }

    function test_revert_inconsistentIntakeConfig() public {
        IntakeConfig[5] memory bad;
        for (uint256 i; i < bad.length; ++i) {
            bad[i] = _goodIntakeConfig();
        }
        bad[0].finalizeDepth = 0;
        bad[1].maxPause = 0;
        bad[2].minRefundZat = registry.DUST_LIMIT();
        bad[3].workTiers = new WorkTier[](0);
        bad[4].coverageBps = 10_000; // a cap without bonds or a price
        for (uint256 i; i < bad.length; ++i) {
            vm.expectRevert(IZecuredIntake.InvalidConfig.selector);
            new ZecuredIntake(bad[i]);
        }
        // The Registry must name the Intake: a good config at another address is refused too.
        vm.expectRevert(IZecuredIntake.InvalidConfig.selector);
        new ZecuredIntake(_goodIntakeConfig());
        IntakeConfig memory noRelay = _goodIntakeConfig();
        noRelay.relay = IZcashHeaderRelay(address(0));
        vm.expectRevert(IZecuredIntake.ZeroAddress.selector);
        new ZecuredIntake(noRelay);
        IntakeConfig memory noVerifier = _goodIntakeConfig();
        noVerifier.verifier = IRiscZeroVerifier(address(0));
        vm.expectRevert(IZecuredIntake.ZeroAddress.selector);
        new ZecuredIntake(noVerifier);
        IntakeConfig memory noVersions = _goodIntakeConfig();
        noVersions.versions = address(0);
        vm.expectRevert(IZecuredIntake.ZeroAddress.selector);
        new ZecuredIntake(noVersions);
    }

    function _idx4(uint256 a, uint256 b, uint256 c, uint256 d) internal pure returns (uint256[] memory r) {
        r = new uint256[](4);
        (r[0], r[1], r[2], r[3]) = (a, b, c, d);
    }
}
