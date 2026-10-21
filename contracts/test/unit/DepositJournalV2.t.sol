// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {DepositJournal} from "../../src/interfaces/v3/IZecuredIntake.sol";
import {ZecuredEIP712} from "../../src/libraries/ZecuredEIP712.sol";

/// @notice Journal v2 of a deposit proof (design notes T-09), what the v3 Intake decodes: the cross-language vector of
///         the real Testnet deposit of tier2/statement's TEST-ONLY fixture (vault 9: 0.01 TAZ, anchored at header
///         4,488,560), as the deposit statement commits it (`the_journal_is_the_intakes_abi`) and zecured-base decodes it
///         (`journal::tests`). Its `vaultOpenedHash` is VaultOpened v2's, with `tagKeyHash = keccak256(K)`.
contract DepositJournalV2Test is Test {
    /// abi.encode(uint8 mode, uint32 branchId, bytes32 tipHash, bytes32 vaultId, address account, uint64 amount,
    /// bytes32 depositTag, bytes32 vaultOpenedHash), Activate mode, NU7.
    bytes internal constant REAL_ACTIVATE =
        hex"0000000000000000000000000000000000000000000000000000000000000000"
        hex"0000000000000000000000000000000000000000000000000000000077190ad9"
        hex"a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000"
        hex"482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b"
        hex"000000000000000000000000fb928f182ad07d2d95d9592390556df7bfb6ed5f"
        hex"00000000000000000000000000000000000000000000000000000000000f4240"
        hex"d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e"
        hex"aaccb41983fa9d9288296dd099d5947119c2abc7e89b57237c00c76cf2680432";

    // The vault's VaultOpened v2, from the fixture: its owner, expiry height and the hashes of its address and tag key.
    address internal constant OWNER = 0xa9960041265dEDCF6AbB4ADEed4b2Af9Bd736c92;
    uint32 internal constant EXPIRY_HEIGHT = 4_488_826;
    bytes32 internal constant ADDRESS_HASH = 0x932eeae25a9dbbb30474a7b43a9b6e5e72bd8bb1bddc85a7ceebdad2e2a8269b;
    bytes32 internal constant TAG_KEY_HASH = 0x598862b8e69539c7639174b267e6db4f6b25809a3add1e17e347a4bf787b8a5c;

    function decode(bytes calldata journal) external pure returns (DepositJournal memory) {
        return abi.decode(journal, (DepositJournal));
    }

    function test_theRealDepositsJournalDecodes() public pure {
        assertEq(REAL_ACTIVATE.length, 256);
        DepositJournal memory j = abi.decode(REAL_ACTIVATE, (DepositJournal));
        assertEq(j.mode, 0, "Activate");
        assertEq(j.branchId, 0x77190AD9, "NU7");
        assertEq(j.tipHash, 0xa27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000);
        assertEq(j.vaultId, 0x482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b);
        assertEq(j.account, 0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F);
        assertEq(j.amount, 1_000_000);
        assertEq(j.depositTag, 0xd8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e);
        assertEq(abi.encode(j), REAL_ACTIVATE, "the encoding round-trips");
    }

    /// The journal's struct hash is the vault's VaultOpened v2, which its five signers sign.
    function test_theJournalNamesTheVaultsVaultOpened() public pure {
        DepositJournal memory j = abi.decode(REAL_ACTIVATE, (DepositJournal));
        bytes32 opened = ZecuredEIP712.hashVaultOpened(
            j.vaultId, OWNER, j.account, j.amount, EXPIRY_HEIGHT, ADDRESS_HASH, TAG_KEY_HASH
        );
        assertEq(j.vaultOpenedHash, opened);
    }

    /// Refund mode of the same note: the same tag and VaultOpened, word 0 is 1.
    function test_refundModeDiffersInTheModeWordOnly() public pure {
        bytes memory refund = REAL_ACTIVATE;
        refund[31] = 0x01;
        DepositJournal memory a = abi.decode(REAL_ACTIVATE, (DepositJournal));
        DepositJournal memory r = abi.decode(refund, (DepositJournal));
        assertEq(r.mode, 1);
        assertEq(r.depositTag, a.depositTag);
        assertEq(r.vaultOpenedHash, a.vaultOpenedHash);
    }

    /// A dirty high byte in the mode, branch id, account or amount word does not decode.
    function test_revert_dirtyWords() public {
        uint256[4] memory at = [uint256(0), 32, 4 * 32, 5 * 32];
        for (uint256 i; i < at.length; ++i) {
            bytes memory dirty = REAL_ACTIVATE;
            dirty[at[i]] = 0x01;
            vm.expectRevert();
            this.decode(dirty);
        }
    }
}
