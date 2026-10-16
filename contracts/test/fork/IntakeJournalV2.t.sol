// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {VaultStatus, ClaimKind, ClaimStatus} from "../../src/interfaces/v3/IZecuredRegistry.sol";
import {IZecuredIntake, DepositJournal, RefundReason} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {VersionKind} from "../../src/interfaces/v3/IZecuredVersions.sol";
import {MockRiscZeroVerifier} from "../../src/mocks/MockRiscZeroVerifier.sol";
import {MockHeaderRelay} from "../mocks/MockHeaderRelay.sol";
import {ZecuredFixture} from "../utils/ZecuredFixture.sol";

/// @notice Journal v2 (T-09): `abi.encode(mode, branchId, tipHash, vaultId, account, amount, depositTag,
///         vaultOpenedHash)`, 256 bytes. Activate mode (0) is the D5 deposit rule; Refund mode (1) lands as a refund
///         whatever the vault; the epoch's consensus branch id must be one the Intake allows. MOCK relay and verifier:
///         what the guest proves is tier2/'s subject.
contract IntakeJournalV2Test is ZecuredFixture {
    uint32 internal constant NU7 = 0x77190AD9;
    uint32 internal constant NU6_3 = 0x37A5165B;
    uint8 internal constant ACTIVATE = 0;
    uint8 internal constant REFUND = 1;

    function _journalV2() internal pure override returns (bool v2, uint32[4] memory branchIds) {
        v2 = true;
        branchIds[0] = NU7;
        branchIds[1] = NU6_3;
    }

    /// A v2 journal of `name`'s vault and its deposit.
    function _journalOf(string memory name, uint8 mode, uint32 branchId, uint64 amount)
        internal
        returns (DepositJournal memory j)
    {
        address account = makeAddr(name);
        j = DepositJournal({
            mode: mode,
            branchId: branchId,
            tipHash: keccak256(abi.encode("zecured.v2.anchor", name, mode, amount)),
            vaultId: _vaultIdFor(account),
            account: account,
            amount: amount,
            depositTag: keccak256(abi.encode("zecured.v2.tag", name, mode, amount)),
            vaultOpenedHash: keccak256(abi.encode("zecured.v2.opened", name))
        });
    }

    /// The encoded journal, its MOCK seal and the five signatures over its `VaultOpened`; the anchor on the best chain
    /// with enough work.
    function _proofOf(DepositJournal memory j)
        internal
        returns (bytes memory journal, bytes memory seal, bytes[] memory sigs)
    {
        MockHeaderRelay(address(headerRelay)).setBlock(j.tipHash, true, MIN_WORK);
        MockHeaderRelay(address(headerRelay)).setTipHeight(4_500_000);
        journal = abi.encode(j);
        seal = MockRiscZeroVerifier(address(proofVerifier)).mockSeal(proofs.imageId, sha256(journal));
        uint256[] memory five = new uint256[](5);
        (five[0], five[1], five[2], five[3], five[4]) = (0, 1, 2, 3, 4);
        sigs = _signerSigs(MessageHashUtils.toTypedDataHash(registry.domainSeparator(), j.vaultOpenedHash), five);
    }

    function _submit(DepositJournal memory j) internal {
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(j);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_config() public view {
        assertTrue(intake.journalV2());
        // T-07a: the branch ids are the deposit version's.
        uint32[4] memory branches =
        versions.version(versions.versionIdOf(VersionKind.Deposit, proofs.sealSelector, proofs.imageId)).branches;
        assertEq(branches[0], NU7);
        assertEq(branches[1], NU6_3);
        assertEq(branches[2], 0);
        assertEq(branches[3], 0);
    }

    function test_journalIsEightWords() public {
        assertEq(abi.encode(_journalOf("v2.a", ACTIVATE, NU7, VAULT_001)).length, 256);
    }

    /// Activate mode under either allowed branch id: the usual pending activation.
    function test_activateMode_pends() public {
        DepositJournal memory j = _journalOf("v2.a", ACTIVATE, NU7, VAULT_001);
        _submit(j);
        assertEq(uint8(_status(j.vaultId)), uint8(VaultStatus.Pending));
        assertEq(intake.pendingActivation(j.vaultId).depositTag, j.depositTag);

        DepositJournal memory k = _journalOf("v2.b", ACTIVATE, NU6_3, VAULT_005);
        _submit(k);
        assertEq(uint8(_status(k.vaultId)), uint8(VaultStatus.Pending));
    }

    /// Refund mode: any Ironwood note to the vault's address lands as a refund, even for a vault Base has never seen
    /// (its account's destination comes later); nothing is reserved or minted.
    function test_refundMode_landsAsARefund() public {
        DepositJournal memory j = _journalOf("v2.a", REFUND, NU7, 123_456);
        bytes32 claimId = registry.claimIdOf(j.vaultId, 0);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(j);
        vm.expectEmit(address(intake));
        emit IZecuredIntake.DepositRefunded(j.vaultId, claimId, RefundReason.ModeRefund);
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        assertEq(uint8(_status(j.vaultId)), uint8(VaultStatus.Cancelled));
        _assertClaim(claimId, j.vaultId, ClaimKind.Refund, 123_456, bytes32(0), ClaimStatus.Pending);
        assertEq(registry.vaultOf(j.account), bytes32(0));
        assertEq(intake.pendingTotal(), 0);
        assertEq(zzec.totalSupply(), 0);

        // The note's tag is used: the same note never lands again, in either mode.
        j.mode = ACTIVATE;
        (journal, seal, sigs) = _proofOf(j);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.DepositTagUsed.selector, j.depositTag));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    /// A Refund-mode proof of the refund floor's size lands; below it, it does not.
    function test_refundMode_floor() public {
        DepositJournal memory j = _journalOf("v2.a", REFUND, NU7, MIN_REFUND - 1);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(j);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.RefundTooSmall.selector, MIN_REFUND - 1, MIN_REFUND));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        _submit(_journalOf("v2.a", REFUND, NU7, MIN_REFUND));
    }

    function test_revert_modeAboveRefund() public {
        DepositJournal memory j = _journalOf("v2.a", 2, NU7, VAULT_001);
        (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(j);
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InvalidMode.selector, uint8(2)));
        intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
    }

    function test_revert_branchNotAllowed() public {
        uint32[3] memory bad = [uint32(0), 0x12345678, type(uint32).max]; // zero, and ids the Intake was not given
        for (uint256 i; i < bad.length; ++i) {
            DepositJournal memory j = _journalOf("v2.a", ACTIVATE, bad[i], VAULT_001);
            (bytes memory journal, bytes memory seal, bytes[] memory sigs) = _proofOf(j);
            vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.BranchNotAllowed.selector, bad[i]));
            intake.attestDepositWithProof(journal, seal, proofs.imageId, sigs);
        }
    }

    /// A v2 Intake reads only v2 journals: a v1 journal (6 words) is refused by its length.
    function test_revert_v1Journal() public {
        address account = makeAddr("v2.a");
        bytes memory journal = abi.encode(
            keccak256("zecured.v2.anchor"),
            _vaultIdFor(account),
            account,
            VAULT_001,
            keccak256("zecured.v2.tag"),
            keccak256("zecured.v2.opened")
        );
        vm.expectRevert(abi.encodeWithSelector(IZecuredIntake.InvalidJournalLength.selector, uint256(192)));
        intake.attestDepositWithProof(journal, "", proofs.imageId, new bytes[](0));
    }

    /// The decoder refuses dirty words (a mode or a branch id with high bits set) before anything else.
    function test_revert_dirtyWords() public {
        bytes memory journal = abi.encode(_journalOf("v2.a", ACTIVATE, NU7, VAULT_001));
        journal[0] = 0x01; // the mode word's high byte
        vm.expectRevert();
        intake.attestDepositWithProof(journal, "", proofs.imageId, new bytes[](0));
        journal = abi.encode(_journalOf("v2.a", ACTIVATE, NU7, VAULT_001));
        journal[32] = 0x01; // the branch id word's high byte
        vm.expectRevert();
        intake.attestDepositWithProof(journal, "", proofs.imageId, new bytes[](0));
    }
}
