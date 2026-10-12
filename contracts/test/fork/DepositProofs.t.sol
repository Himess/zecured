// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Position} from "morpho-blue/src/interfaces/IMorpho.sol";
import {ZZEC} from "../../src/ZZEC.sol";
import {ZecuredRegistry} from "../../src/ZecuredRegistry.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {IZecuredRegistry, VaultStatus, DepositProofConfig, MintMode} from "../../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {DepositAttestation, ZecuredEIP712} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {ZcashHeaderRelay} from "../../src/relay/ZcashHeaderRelay.sol";
import {RelayFixtures} from "../relay/RelayFixtures.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Tier 2 deposit proofs (`attestDepositWithProof`, pending until `finalizeActivation`, K3) and vault
///         cancellation (`cancelVault`, L-8), against the REAL header relay. This deployment keeps both mint paths
///         (`SignerAttested`) so that the rules between them are tested here; `ProofOnly` (K1) and the pending
///         activation's reorg cases are in PendingActivation.t.sol.
/// @dev The journal is that of a real Zcash Testnet deposit: vault 9 of the local-fork runs of 2026-10-11 (0.01 TAZ in
///      an Ironwood-only transaction at height 4,488,543), proven against header 4,488,560 by the deposit statement
///      (tier2/statement) on real chain data: the transaction's txid path, the block's ZIP 221 history-tree leaf,
///      path and peaks, and the header's `hashBlockCommitments`. That header and 11 more are submitted to the REAL
///      relay below, which checks their Equihash solutions, difficulty and linkage on-chain (fixture cross-checked
///      against CipherScan). The proof itself is replaced by the MOCK verifier: Groth16 seals need an x86 machine.
contract DepositProofsTest is ZecuredFixture, RelayFixtures {
    string internal constant ANCHOR = "test/relay/fixtures/testnet_deposit_anchor.json";
    bytes32 internal constant IMAGE_ID = keccak256("zecured.test.depositImageId");
    uint256 internal constant MIN_WORK = 1_000_000;
    uint64 internal constant FINALIZE_DEPTH = 6;
    uint64 internal constant FINALIZE_DELAY = 150;
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

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        anchor = _loadReal(ANCHOR);
        relay = _deploy(anchor);
        verifier = new MockRiscZeroVerifier();
        return DepositProofConfig({
            verifier: IRiscZeroVerifier(address(verifier)),
            imageId: IMAGE_ID,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: MIN_WORK,
            mintMode: MintMode.SignerAttested,
            sealSelector: MockRiscZeroVerifier(address(verifier)).SELECTOR(),
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    // ------------------------------------------------------------------ helpers

    /// Submits the anchor's headers up to 4,488,560 + n - 1 (those not submitted yet).
    function _relayHeaders(uint256 n) internal {
        for (uint256 i = relayed; i < n; ++i) {
            _warpTo(anchor.headers[i]);
            relay.submitHeader(anchor.headers[i]);
        }
        if (n > relayed) relayed = n;
    }

    /// Lets the relay reach the pending activation's ready height and time.
    function _makeReady(bytes32 vaultId) internal {
        (,,,, uint256 readyHeight, uint256 readyTime) = registry.pendingActivation(vaultId);
        _relayHeaders(CONFIRMED + FINALIZE_DEPTH);
        (, uint256 height,) = relay.bestChainTip();
        assertGe(height, readyHeight, "the fixture reaches the ready height");
        if (block.timestamp < readyTime) vm.warp(readyTime);
    }

    function _finalize(bytes32 vaultId) internal {
        _makeReady(vaultId);
        registry.finalizeActivation(vaultId);
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
        registry.attestDepositWithProof(journal, _seal(journal), _openedSigs(OPENED));
    }

    // ------------------------------------------------------------------ the real deposit

    function test_realDeposit_pendsThenMintsAgainstARelayVerifiedHeader() public {
        _relayHeaders(CONFIRMED);
        assertTrue(relay.isInBestChain(TIP), "anchor on the best chain");
        assertGe(relay.confirmationWork(TIP), MIN_WORK, "enough work on top");
        (, uint256 tipHeight,) = relay.bestChainTip();

        bytes memory journal = _realJournal();
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.ActivationPending(
            VAULT_ID, ACCOUNT, AMOUNT, TAG, tipHeight + FINALIZE_DEPTH, block.timestamp + FINALIZE_DELAY
        );
        uint256 gasBefore = gasleft();
        registry.attestDepositWithProof(journal, _seal(journal), _openedSigs(OPENED));
        emit log_named_uint(
            "attestDepositWithProof gas (MOCK verifier, excl. the ~276k of a Groth16 check)", gasBefore - gasleft()
        );

        // K3: reserved, not minted.
        (address account, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.None));
        assertEq(registry.vaultOf(ACCOUNT), VAULT_ID, "the account is reserved");
        assertTrue(registry.depositTagUsed(TAG), "the tag is reserved");
        assertEq(zzec.totalSupply(), 0, "nothing minted before finalization");
        (address pAccount, uint64 pAmount, bytes32 pTag, bytes32 pAnchor, uint256 readyHeight, uint256 readyTime) =
            registry.pendingActivation(VAULT_ID);
        assertEq(pAccount, ACCOUNT);
        assertEq(pAmount, AMOUNT);
        assertEq(pTag, TAG);
        assertEq(pAnchor, TIP);
        assertEq(readyHeight, tipHeight + FINALIZE_DEPTH);
        assertEq(readyTime, block.timestamp + FINALIZE_DELAY);
        vm.expectRevert(
            abi.encodeWithSelector(IZecuredRegistry.ActivationNotReady.selector, VAULT_ID, readyHeight, readyTime)
        );
        registry.finalizeActivation(VAULT_ID);

        _makeReady(VAULT_ID);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT_ID, ACCOUNT, AMOUNT, TAG);
        gasBefore = gasleft();
        registry.finalizeActivation(VAULT_ID);
        emit log_named_uint("finalizeActivation gas (mint and Morpho supply included)", gasBefore - gasleft());

        (account, outstanding, status) = registry.vault(VAULT_ID);
        assertEq(account, ACCOUNT);
        assertEq(outstanding, AMOUNT);
        assertEq(uint8(status), uint8(VaultStatus.Active));
        assertEq(registry.vaultOf(ACCOUNT), VAULT_ID);
        assertTrue(registry.depositTagUsed(TAG));
        (pAccount,,,,,) = registry.pendingActivation(VAULT_ID);
        assertEq(pAccount, address(0), "no longer pending");
        Position memory p = MORPHO.position(marketId, ACCOUNT);
        assertEq(p.collateral, AMOUNT, "collateral supplied on behalf of the account (I1)");
        assertEq(zzec.totalSupply(), AMOUNT);
        assertEq(zzec.balanceOf(address(registry)), 0, "no zZEC left with the Registry (I2)");
        assertEq(zzec.balanceOf(ACCOUNT), 0, "the account holds no zZEC (I2)");
    }

    function test_theJournalIsTheProofsOutput() public pure {
        // Six ABI words, exactly what the guest commits.
        assertEq(_realJournal().length, 192);
    }

    // ------------------------------------------------------------------ the anchor

    function test_revert_anchorUnknownToTheRelay() public {
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorNotConfirmed.selector, TIP, 0));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_notEnoughWorkOnTopOfTheAnchor() public {
        _relayHeaders(2); // the anchor and one block (~0.5 M work < 1 M)
        uint256 work = relay.confirmationWork(TIP);
        assertGt(work, 0);
        assertLt(work, MIN_WORK);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorNotConfirmed.selector, TIP, work));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_anchorIsTheRelayTip() public {
        _relayHeaders(1); // on the best chain, nothing on top
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorNotConfirmed.selector, TIP, 0));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    // ------------------------------------------------------------------ VaultOpened signatures

    function test_revert_fourOfFiveOpenedSignatures() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _signerSigs(_openedDigest(OPENED), _idx4(0, 1, 2, 4));
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InsufficientSignatures.selector, 4));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_signaturesOverAnotherVaultOpened() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(keccak256("another vault"));
        bytes memory seal = _seal(journal);
        vm.expectRevert(); // recovers to an address outside the set
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_duplicateOpenedSignature() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        uint256[] memory idx = _all5();
        idx[4] = 3;
        bytes[] memory sigs = _signerSigs(_openedDigest(OPENED), idx);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.SignaturesNotSorted.selector, 4));
        registry.attestDepositWithProof(journal, seal, sigs);
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
        registry.attestDepositWithProof(otherAccount, seal, sigs);

        bytes memory moreZec = _journal(TIP, VAULT_ID, ACCOUNT, 5_000_000, TAG, OPENED);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        registry.attestDepositWithProof(moreZec, seal, sigs);
    }

    function test_revert_sealForAnotherImage() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes memory seal = verifier.mockSeal(keccak256("another guest"), sha256(journal));
        bytes[] memory sigs = _openedSigs(OPENED);
        vm.expectRevert(MockRiscZeroVerifier.MockSealInvalid.selector);
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_sealWithAnotherSelector() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        seal[0] = 0x73; // a verifier the router might add later under a new selector
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.UnexpectedSealSelector.selector, bytes4(0x73ffffff)));
        registry.attestDepositWithProof(journal, seal, sigs);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.UnexpectedSealSelector.selector, bytes4(0)));
        registry.attestDepositWithProof(journal, hex"ffffff", sigs);
    }

    function test_revert_journalLength() public {
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory longer = abi.encodePacked(journal, uint8(0));
        bytes memory longerSeal = _seal(longer);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InvalidJournalLength.selector, 193));
        registry.attestDepositWithProof(longer, longerSeal, sigs);
        bytes memory shorter = new bytes(191);
        bytes memory shorterSeal = _seal(shorter);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InvalidJournalLength.selector, 191));
        registry.attestDepositWithProof(shorter, shorterSeal, sigs);
    }

    function test_revert_nonCanonicalAccountWord() public {
        _relayHeaders(CONFIRMED);
        bytes memory journal = _realJournal();
        journal[64] = 0x01; // high byte of the account word
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(); // abi.decode rejects a dirty address word
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    // ------------------------------------------------------------------ I4, I5

    function test_revert_sameDepositTwice() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ActivationIsPending.selector, VAULT_ID));
        registry.attestDepositWithProof(journal, seal, sigs);

        _finalize(VAULT_ID);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, VAULT_ID));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_attestationOfAVaultWithAPendingProof() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        DepositAttestation memory att = DepositAttestation({
            vaultId: VAULT_ID, account: ACCOUNT, amount: AMOUNT, depositTag: keccak256("2nd note")
        });
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ActivationIsPending.selector, VAULT_ID));
        registry.attestDeposit(att, sigs);
    }

    function test_revert_pendingTagAndAccountAreReserved() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        // The same note for another vault: the tag is taken.
        DepositAttestation memory att =
            DepositAttestation({vaultId: keccak256("other vault"), account: stranger, amount: AMOUNT, depositTag: TAG});
        bytes[] memory sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, TAG));
        registry.attestDeposit(att, sigs);
        // Another note for another vault of the same account: the account is taken (I4).
        att = DepositAttestation({
            vaultId: keccak256("other vault"), account: ACCOUNT, amount: AMOUNT, depositTag: keccak256("2nd note")
        });
        sigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, ACCOUNT, VAULT_ID));
        registry.attestDeposit(att, sigs);
    }

    function test_revert_tagAlreadyUsedByAnAttestation() public {
        _relayHeaders(CONFIRMED);
        // A Tier 1 attestation of the same note (same tag) for another vault and account.
        DepositAttestation memory att =
            DepositAttestation({vaultId: keccak256("other vault"), account: stranger, amount: AMOUNT, depositTag: TAG});
        registry.attestDeposit(att, _threeSigs(registry.hashDepositAttestation(att)));
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.DepositTagUsed.selector, TAG));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    function test_revert_accountAlreadyHasAVault() public {
        _relayHeaders(CONFIRMED);
        bytes32 other = _attest(ACCOUNT, VAULT_001);
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AccountHasVault.selector, ACCOUNT, other));
        registry.attestDepositWithProof(journal, seal, sigs);
    }

    // ------------------------------------------------------------------ cancellation (L-8)

    function _cancel(bytes32 vaultId) internal {
        registry.cancelVault(vaultId, _threeSigs(registry.hashVaultCancellation(vaultId)));
    }

    function test_cancel_blocksTheProofAndTheAttestation() public {
        _relayHeaders(CONFIRMED);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultCancelled(VAULT_ID);
        _cancel(VAULT_ID);
        (address account, uint64 outstanding, VaultStatus status) = registry.vault(VAULT_ID);
        assertEq(uint8(status), uint8(VaultStatus.Cancelled));
        assertEq(account, address(0));
        assertEq(outstanding, 0);

        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, VAULT_ID));
        registry.attestDepositWithProof(journal, seal, sigs);

        DepositAttestation memory att =
            DepositAttestation({vaultId: VAULT_ID, account: ACCOUNT, amount: AMOUNT, depositTag: TAG});
        bytes[] memory attSigs = _threeSigs(registry.hashDepositAttestation(att));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, VAULT_ID));
        registry.attestDeposit(att, attSigs);
        assertFalse(registry.depositTagUsed(TAG));
        assertEq(registry.vaultOf(ACCOUNT), bytes32(0), "the account stays free for another vault");
    }

    function test_revert_cancelWhileAProofIsPending() public {
        _relayHeaders(CONFIRMED);
        _prove(_realJournal());
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(VAULT_ID));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ActivationIsPending.selector, VAULT_ID));
        registry.cancelVault(VAULT_ID, sigs);
    }

    function test_revert_cancelAnActiveVault() public {
        bytes32 vaultId = _attest(stranger, VAULT_001);
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(vaultId));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, vaultId));
        registry.cancelVault(vaultId, sigs);
    }

    function test_revert_cancelTwice() public {
        _cancel(VAULT_ID);
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(VAULT_ID));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, VAULT_ID));
        registry.cancelVault(VAULT_ID, sigs);
    }

    function test_revert_cancelWithTwoSignatures() public {
        bytes[] memory sigs = _signerSigs(registry.hashVaultCancellation(VAULT_ID), _idx(1, 3));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.InsufficientSignatures.selector, 2));
        registry.cancelVault(VAULT_ID, sigs);
    }

    function test_revert_cancelWithAnotherVaultsSignatures() public {
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(keccak256("another vault")));
        vm.expectRevert(); // recovers to addresses outside the set
        registry.cancelVault(VAULT_ID, sigs);
    }

    function test_revert_cancelZeroVault() public {
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(bytes32(0)));
        vm.expectRevert(IZecuredRegistry.ZeroVaultId.selector);
        registry.cancelVault(bytes32(0), sigs);
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
            0xd35968aa50b8474e84cfea69d0eff1947f4ca7211cff9741684554da48617ad5
        );
        assertEq(structHash, 0xf0129c9f652ed078d85e4562accaf93d8d5d818272a92a9dba931bbc372e6302);
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
            0x1932a4dbbbf80e6972e48b0115a69e3ed93e16a745c88a2f1347c21ed3addbf4
        );
    }

    function test_cancellationDigestMatchesTheTypeString() public view {
        bytes32 structHash = keccak256(abi.encode(keccak256("VaultCancellation(bytes32 vaultId)"), VAULT_ID));
        assertEq(registry.hashVaultCancellation(VAULT_ID), _openedDigest(structHash));
    }

    // ------------------------------------------------------------------ configuration

    function test_config() public view {
        assertEq(address(registry.depositVerifier()), address(verifier));
        assertEq(registry.depositImageId(), IMAGE_ID);
        assertEq(address(registry.relay()), address(relay));
        assertEq(registry.minConfirmationWork(), MIN_WORK);
        assertEq(uint8(registry.mintMode()), uint8(MintMode.SignerAttested));
        assertEq(registry.depositSealSelector(), bytes4(0xffffffff));
        assertEq(registry.finalizeDepth(), FINALIZE_DEPTH);
        assertEq(registry.finalizeDelay(), FINALIZE_DELAY);
        assertEq(registry.JOURNAL_LENGTH(), 192);
    }

    function _goodConfig() internal view returns (DepositProofConfig memory) {
        return DepositProofConfig({
            verifier: IRiscZeroVerifier(address(verifier)),
            imageId: IMAGE_ID,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: MIN_WORK,
            mintMode: MintMode.ProofOnly,
            sealSelector: bytes4(0xffffffff),
            finalizeDepth: FINALIZE_DEPTH,
            finalizeDelay: FINALIZE_DELAY
        });
    }

    function test_revert_inconsistentConfig() public {
        DepositProofConfig[10] memory bad;
        for (uint256 i; i < bad.length; ++i) {
            bad[i] = _goodConfig();
        }
        // Proofs enabled without what they need.
        bad[0].relay = IZcashHeaderRelay(address(0));
        bad[1].imageId = bytes32(0);
        bad[2].sealSelector = bytes4(0);
        bad[3].finalizeDepth = 0;
        // Proofs disabled (no verifier) with anything else set, including ProofOnly, which could never mint.
        for (uint256 i = 4; i < bad.length; ++i) {
            bad[i] = _noProofs();
        }
        bad[4].relay = IZcashHeaderRelay(address(relay));
        bad[5].imageId = IMAGE_ID;
        bad[6].minConfirmationWork = 1;
        bad[7].mintMode = MintMode.ProofOnly;
        bad[8].sealSelector = bytes4(0xffffffff);
        bad[9].finalizeDelay = 1;
        for (uint256 i; i < bad.length; ++i) {
            (ZZEC z, address a) = _freshZzec();
            vm.expectRevert(IZecuredRegistry.InvalidDepositProofConfig.selector);
            new ZecuredRegistry(
                MORPHO, z, a, address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, signerSet, bad[i], NO_BOND
            );
        }
        // The good one deploys.
        (ZZEC z2, address a2) = _freshZzec();
        new ZecuredRegistry(
            MORPHO, z2, a2, address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, signerSet, _goodConfig(), NO_BOND
        );
    }

    function test_revert_proofsDisabled() public {
        (ZZEC z, address a) = _freshZzec();
        ZecuredRegistry plain = new ZecuredRegistry(
            MORPHO, z, a, address(usdc), address(oracle), ADAPTIVE_CURVE_IRM, LLTV, signerSet, _noProofs(), NO_BOND
        );
        bytes memory journal = _realJournal();
        bytes[] memory sigs = _openedSigs(OPENED);
        bytes memory seal = _seal(journal);
        vm.expectRevert(IZecuredRegistry.DepositProofsDisabled.selector);
        plain.attestDepositWithProof(journal, seal, sigs);
    }

    /// A ZZEC wired to the Registry deployed right after it (and a dummy Adapter address).
    function _freshZzec() internal returns (ZZEC z, address adapterAddress) {
        uint64 nonce = vm.getNonce(address(this));
        address predictedRegistry = vm.computeCreateAddress(address(this), nonce + 1);
        adapterAddress = makeAddr("zecured.test.adapter2");
        z = new ZZEC(predictedRegistry, adapterAddress, address(MORPHO));
    }

    function _idx4(uint256 a, uint256 b, uint256 c, uint256 d) internal pure returns (uint256[] memory r) {
        r = new uint256[](4);
        (r[0], r[1], r[2], r[3]) = (a, b, c, d);
    }
}
