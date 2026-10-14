// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IZecuredRegistry, VaultStatus, DepositProofConfig, MintMode} from "../../src/interfaces/IZecuredRegistry.sol";
import {IRiscZeroVerifier} from "../../src/interfaces/IRiscZeroVerifier.sol";
import {DepositAttestation} from "../../src/libraries/ZecuredEIP712.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {IZcashHeaderRelay} from "../../src/relay/IZcashHeaderRelay.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {MockSignerBond} from "../mocks/MockSignerBond.sol";
import {ISignerBond} from "../../src/interfaces/ISignerBond.sol";
import {IZecuredSpendCommits, LineageNote} from "../../src/interfaces/IZecuredSpendCommits.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice K1 (`ProofOnly`: no signer attestation can mint) and K3 (a proven deposit only mints once the relay has
///         moved on with the proof's anchor still on its best chain, and is dropped if the anchor leaves it), driven
///         through a MOCK relay whose best chain the test sets (real Equihash headers cannot fork in a test; the real
///         relay path is in DepositProofs.t.sol). The proof itself is the MOCK verifier's.
contract PendingActivationTest is ZecuredFixture {
    bytes32 internal constant IMAGE_ID = keccak256("zecured.test.pending.imageId");
    uint256 internal constant MIN_WORK = 3_500_000;
    uint64 internal constant DEPTH = 6;
    uint64 internal constant DELAY = 150;
    uint256 internal constant TIP_HEIGHT = 4_500_000;

    bytes32 internal constant ANCHOR = keccak256("zecured.test.pending.anchor");
    bytes32 internal constant ANCHOR_2 = keccak256("zecured.test.pending.anchor2");
    bytes32 internal constant VAULT = keccak256("zecured.test.pending.vault");
    bytes32 internal constant TAG = keccak256("zecured.test.pending.tag");
    bytes32 internal constant OPENED = keccak256("zecured.test.pending.vaultOpened");

    MockHeaderRelay internal relay;
    MockRiscZeroVerifier internal verifier;
    MockSignerBond internal bond;
    address internal account = makeAddr("zecured.test.pending.account");

    function _depositProofConfig() internal override returns (DepositProofConfig memory) {
        relay = new MockHeaderRelay();
        verifier = new MockRiscZeroVerifier();
        return DepositProofConfig({
            verifier: IRiscZeroVerifier(address(verifier)),
            imageId: IMAGE_ID,
            relay: IZcashHeaderRelay(address(relay)),
            minConfirmationWork: MIN_WORK,
            mintMode: MintMode.ProofOnly,
            sealSelector: verifier.SELECTOR(),
            finalizeDepth: DEPTH,
            finalizeDelay: DELAY
        });
    }

    function _signerBond() internal override returns (ISignerBond) {
        bond = new MockSignerBond();
        return ISignerBond(address(bond));
    }

    // ------------------------------------------------------------------ helpers

    function _journal(bytes32 anchor) internal view returns (bytes memory) {
        return abi.encode(anchor, VAULT, account, VAULT_001, TAG, OPENED);
    }

    function _all5() internal pure returns (uint256[] memory r) {
        r = new uint256[](5);
        (r[0], r[1], r[2], r[3], r[4]) = (0, 1, 2, 3, 4);
    }

    function _prove(bytes32 anchor) internal {
        relay.setBlock(anchor, true, MIN_WORK);
        relay.setTipHeight(TIP_HEIGHT);
        bytes memory journal = _journal(anchor);
        bytes[] memory sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), _all5());
        registry.attestDepositWithProof(journal, verifier.mockSeal(IMAGE_ID, sha256(journal)), sigs);
    }

    function _ready() internal {
        relay.setTipHeight(TIP_HEIGHT + DEPTH);
        vm.warp(block.timestamp + DELAY);
    }

    function _pendingAccount(bytes32 vaultId) internal view returns (address pendingAccount) {
        (pendingAccount,,,,,) = registry.pendingActivation(vaultId);
    }

    // ------------------------------------------------------------------ K1

    function test_proofOnly_noAttestationCanMint() public {
        assertEq(uint8(registry.mintMode()), uint8(MintMode.ProofOnly));
        DepositAttestation memory att =
            DepositAttestation({vaultId: VAULT, account: account, amount: VAULT_001, depositTag: TAG});
        // Even all five signers.
        bytes[] memory sigs = _signerSigs(registry.hashDepositAttestation(att), _all5());
        vm.expectRevert(IZecuredRegistry.MintRequiresProof.selector);
        registry.attestDeposit(att, sigs);
        assertEq(zzec.totalSupply(), 0);
    }

    // ------------------------------------------------------------------ K3: ready height, ready time, anchor

    function test_finalize_waitsForTheRelayAndTheDelay() public {
        uint256 provedAt = block.timestamp;
        _prove(ANCHOR);
        (, uint64 amount, bytes32 tag, bytes32 anchor, uint256 readyHeight, uint256 readyTime) =
            registry.pendingActivation(VAULT);
        assertEq(amount, VAULT_001);
        assertEq(tag, TAG);
        assertEq(anchor, ANCHOR);
        assertEq(readyHeight, TIP_HEIGHT + DEPTH);
        assertEq(readyTime, provedAt + DELAY);
        bytes memory notReady =
            abi.encodeWithSelector(IZecuredRegistry.ActivationNotReady.selector, VAULT, readyHeight, readyTime);

        // Neither condition.
        vm.expectRevert(notReady);
        registry.finalizeActivation(VAULT);
        // Time without the relay: the feeders may be down.
        vm.warp(readyTime);
        relay.setTipHeight(readyHeight - 1);
        vm.expectRevert(notReady);
        registry.finalizeActivation(VAULT);
        // The relay without the time: someone may be feeding a branch of their own.
        vm.warp(readyTime - 1);
        relay.setTipHeight(readyHeight);
        vm.expectRevert(notReady);
        registry.finalizeActivation(VAULT);

        vm.warp(readyTime);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.VaultActivated(VAULT, account, VAULT_001, TAG);
        vm.prank(stranger); // anyone
        registry.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), VAULT_001);
        assertEq(_pendingAccount(VAULT), address(0));

        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NoPendingActivation.selector, VAULT));
        registry.finalizeActivation(VAULT);
    }

    function test_revert_finalizeAfterTheAnchorLeftTheBestChain() public {
        _prove(ANCHOR);
        _ready();
        relay.setBlock(ANCHOR, false, 0); // a heavier branch replaced it
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorNotConfirmed.selector, ANCHOR, 0));
        registry.finalizeActivation(VAULT);
    }

    function test_revert_finalizeRechecksTheWork() public {
        _prove(ANCHOR);
        _ready();
        relay.setBlock(ANCHOR, true, MIN_WORK - 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorNotConfirmed.selector, ANCHOR, MIN_WORK - 1));
        registry.finalizeActivation(VAULT);
    }

    // ------------------------------------------------------------------ K3: dropping a pending activation

    function test_drop_onlyOnceTheAnchorLeftTheBestChain_thenTheNoteCanBeProvenAgain() public {
        _prove(ANCHOR);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.AnchorStillOnBestChain.selector, VAULT));
        registry.dropPendingActivation(VAULT);

        relay.setBlock(ANCHOR, false, 0);
        vm.expectEmit(address(registry));
        emit IZecuredRegistry.PendingActivationDropped(VAULT);
        vm.prank(stranger); // anyone
        registry.dropPendingActivation(VAULT);
        assertEq(_pendingAccount(VAULT), address(0));
        assertFalse(registry.depositTagUsed(TAG), "the tag is free again");
        assertEq(registry.vaultOf(account), bytes32(0), "the account is free again");
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.None));
        assertEq(zzec.totalSupply(), 0, "nothing was ever minted");
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NoPendingActivation.selector, VAULT));
        registry.finalizeActivation(VAULT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NoPendingActivation.selector, VAULT));
        registry.dropPendingActivation(VAULT);

        // The same deposit, anchored on the chain that won.
        _prove(ANCHOR_2);
        _ready();
        registry.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
        assertEq(zzec.totalSupply(), VAULT_001);
    }

    // ------------------------------------------------------------------ Tier 3

    /// The proven note is a lineage note from the proof on (a spend while the activation is pending is already F1)
    /// and stops being one when the activation is dropped: then it backs nothing.
    function test_lineage_fromTheProofOn_untilTheDrop() public {
        _prove(ANCHOR);
        LineageNote memory l = registry.lineageOf(TAG);
        assertEq(l.vaultId, VAULT);
        assertEq(l.value, VAULT_001);
        assertEq(registry.openVaultCount(), 1);

        relay.setBlock(ANCHOR, false, 0);
        vm.warp(block.timestamp + 1 days);
        registry.dropPendingActivation(VAULT);
        assertEq(registry.lineageOf(TAG).vaultId, bytes32(0));
        assertEq(registry.openVaultCount(), 0);
        assertEq(registry.lastVaultSettledAt(), block.timestamp);

        // Proven again on the chain that won: lineage again, through the activation.
        _prove(ANCHOR_2);
        _ready();
        registry.finalizeActivation(VAULT);
        assertEq(registry.lineageOf(TAG).value, VAULT_001);
        assertEq(registry.openVaultCount(), 1);
    }

    /// E2: an open violation case about the vault (its deposit spent during the wait) holds its activation; once the
    /// case is gone the activation goes on.
    function test_E2_anOpenCaseHoldsTheActivation() public {
        _prove(ANCHOR);
        _ready();
        bond.setOpenCases(VAULT, 1);
        vm.expectRevert(abi.encodeWithSelector(IZecuredSpendCommits.ViolationCaseOpen.selector, VAULT));
        registry.finalizeActivation(VAULT);
        bond.setOpenCases(VAULT, 0);
        registry.finalizeActivation(VAULT);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Active));
    }

    /// E3: while its activation is pending, the deposit can be neither refunded nor cancelled: the two reads a signer
    /// makes before a refund (D23, K2) say the vault is not Cancelled and the note's tag is used. After a drop the
    /// tag is free and the vault can be cancelled (test_cancel_refusedWhilePending_allowedAfterTheDrop).
    function test_E3_aPendingDepositIsNeitherRefundableNorCancellable() public {
        _prove(ANCHOR);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.None), "a signer refunds only from a Cancelled vault");
        assertTrue(registry.depositTagUsed(TAG), "or a note whose tag no vault uses");
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(VAULT));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ActivationIsPending.selector, VAULT));
        registry.cancelVault(VAULT, sigs);
    }

    /// After a slash (the bond halted), no new deposit proof is accepted and no pending one is activated.
    function test_halt_stopsProofsAndFinalization() public {
        _prove(ANCHOR);
        bond.setHalted(true);
        _ready();
        vm.expectRevert(IZecuredSpendCommits.MintingHalted.selector);
        registry.finalizeActivation(VAULT);

        relay.setBlock(ANCHOR, false, 0);
        registry.dropPendingActivation(VAULT);
        relay.setBlock(ANCHOR_2, true, MIN_WORK);
        bytes memory journal = _journal(ANCHOR_2);
        bytes memory seal = verifier.mockSeal(IMAGE_ID, sha256(journal));
        bytes[] memory sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), _all5());
        vm.expectRevert(IZecuredSpendCommits.MintingHalted.selector);
        registry.attestDepositWithProof(journal, seal, sigs);
        assertEq(zzec.totalSupply(), 0);
    }

    function test_cancel_refusedWhilePending_allowedAfterTheDrop() public {
        _prove(ANCHOR);
        bytes[] memory sigs = _threeSigs(registry.hashVaultCancellation(VAULT));
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.ActivationIsPending.selector, VAULT));
        registry.cancelVault(VAULT, sigs);

        relay.setBlock(ANCHOR, false, 0);
        registry.dropPendingActivation(VAULT);
        registry.cancelVault(VAULT, sigs);
        assertEq(uint8(_status(VAULT)), uint8(VaultStatus.Cancelled));
        // A cancelled vault can never be proven again (I14: cancelled <=> never activated).
        relay.setBlock(ANCHOR_2, true, MIN_WORK);
        bytes memory journal = _journal(ANCHOR_2);
        bytes memory seal = verifier.mockSeal(IMAGE_ID, sha256(journal));
        bytes[] memory opened =
            _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), OPENED), _all5());
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.VaultIdUsed.selector, VAULT));
        registry.attestDepositWithProof(journal, seal, opened);
    }

    function test_revert_dropOrFinalizeWithoutAPendingActivation() public {
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NoPendingActivation.selector, VAULT));
        registry.finalizeActivation(VAULT);
        vm.expectRevert(abi.encodeWithSelector(IZecuredRegistry.NoPendingActivation.selector, VAULT));
        registry.dropPendingActivation(VAULT);
    }
}
