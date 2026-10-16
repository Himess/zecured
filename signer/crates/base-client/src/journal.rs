//! The journal of a deposit proof: what the zkVM guest commits (tier2/statement
//! `Journal::abi_encode`, v2 since design notes T-09) and the Intake decodes (`DepositJournal` in
//! contracts/src/interfaces/v3/IZecuredIntake.sol), v1 or v2 as the deployment says
//! (`IZecuredIntake.journalV2`). Off-chain callers read it before they pay gas to submit a proof.

use alloy::{sol, sol_types::SolType};
use anyhow::{Result, anyhow, ensure};

sol! {
    /// `abi.encode(uint8 mode, uint32 branchId, bytes32 tipHash, bytes32 vaultId, address account,
    /// uint64 amount, bytes32 depositTag, bytes32 vaultOpenedHash)`: eight words.
    #[derive(Debug, PartialEq, Eq)]
    struct DepositJournal {
        uint8 mode;
        uint32 branchId;
        bytes32 tipHash;
        bytes32 vaultId;
        address account;
        uint64 amount;
        bytes32 depositTag;
        bytes32 vaultOpenedHash;
    }
}

/// `mode` of the vault's deposit: D5, the vault's amount, mined by its expiry height.
pub const MODE_ACTIVATE: u8 = 0;
/// `mode` of a note refunded through a claim (T-02).
pub const MODE_REFUND: u8 = 1;
/// Length of a v2 journal: eight ABI words.
pub const JOURNAL_LENGTH: usize = 8 * 32;
/// Length of a v1 journal (the guest before T-09): six ABI words, always Activate mode.
pub const JOURNAL_V1_LENGTH: usize = 6 * 32;

sol! {
    /// `abi.encode(bytes32 tipHash, bytes32 vaultId, address account, uint64 amount, bytes32
    /// depositTag, bytes32 vaultOpenedHash)`: the v1 journal, six words.
    struct DepositJournalV1 {
        bytes32 tipHash;
        bytes32 vaultId;
        address account;
        uint64 amount;
        bytes32 depositTag;
        bytes32 vaultOpenedHash;
    }
}

/// Decodes a v2 deposit journal as strictly as the Intake: exactly eight words, every word in
/// range for its type (a dirty high byte is refused, as Solidity's `abi.decode` does), a known
/// mode. Whether the branch id is allowed is the Intake's to check (its allowed set).
pub fn decode_deposit_journal(journal: &[u8]) -> Result<DepositJournal> {
    ensure!(
        journal.len() == JOURNAL_LENGTH,
        "a deposit journal (v2) has {JOURNAL_LENGTH} bytes, not {}",
        journal.len()
    );
    let j =
        <DepositJournal as SolType>::abi_decode_validate(journal).map_err(|e| anyhow!("not a deposit journal: {e}"))?;
    ensure!(j.mode == MODE_ACTIVATE || j.mode == MODE_REFUND, "unknown deposit proof mode {}", j.mode);
    Ok(j)
}

/// Decodes a deposit journal of the version the Intake reads (`v2`: `IZecuredIntake.journalV2`),
/// as strictly as the Intake does. A v1 journal is an Activate-mode proof whose branch id is not
/// recorded (zero here).
pub fn decode_deposit_journal_for(journal: &[u8], v2: bool) -> Result<DepositJournal> {
    if v2 {
        return decode_deposit_journal(journal);
    }
    ensure!(
        journal.len() == JOURNAL_V1_LENGTH,
        "this Intake reads v1 deposit journals of {JOURNAL_V1_LENGTH} bytes, not {}",
        journal.len()
    );
    let j = <DepositJournalV1 as SolType>::abi_decode_validate(journal)
        .map_err(|e| anyhow!("not a v1 deposit journal: {e}"))?;
    Ok(DepositJournal {
        mode: MODE_ACTIVATE,
        branchId: 0,
        tipHash: j.tipHash,
        vaultId: j.vaultId,
        account: j.account,
        amount: j.amount,
        depositTag: j.depositTag,
        vaultOpenedHash: j.vaultOpenedHash,
    })
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Address, B256};

    use super::*;

    /// The real deposit's journal of tier2/statement's `the_journal_is_the_intakes_abi` (vault 9 of
    /// the TEST-ONLY fixture, Activate mode, NU7), word by word.
    const REAL: [&str; 8] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000077190ad9",
        "a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000",
        "482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b",
        "000000000000000000000000fb928f182ad07d2d95d9592390556df7bfb6ed5f",
        "00000000000000000000000000000000000000000000000000000000000f4240",
        "d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e",
        "aaccb41983fa9d9288296dd099d5947119c2abc7e89b57237c00c76cf2680432",
    ];

    fn real() -> Vec<u8> {
        hex::decode(REAL.concat()).unwrap()
    }

    #[test]
    fn the_real_deposits_journal_decodes() {
        let b = |h: &str| h.parse::<B256>().unwrap();
        let j = decode_deposit_journal(&real()).unwrap();
        assert_eq!((j.mode, j.branchId, j.amount), (MODE_ACTIVATE, 0x7719_0ad9, 1_000_000));
        assert_eq!(j.tipHash, b(REAL[2]));
        assert_eq!(j.vaultId, b(REAL[3]));
        assert_eq!(j.account, "0xFb928f182AD07d2d95D9592390556Df7bfB6ED5F".parse::<Address>().unwrap());
        assert_eq!(j.depositTag, b(REAL[6]));
        assert_eq!(j.vaultOpenedHash, b(REAL[7]));
        // The encoding round-trips.
        assert_eq!(<DepositJournal as SolType>::abi_encode(&j), real());
    }

    /// A v1 journal is the v2 one without its first two words, read only by a v1 Intake.
    #[test]
    fn journals_are_read_in_the_intakes_version() {
        let v2 = decode_deposit_journal_for(&real(), true).unwrap();
        let mut v1 = real();
        v1.drain(..64);
        let read = decode_deposit_journal_for(&v1, false).unwrap();
        assert_eq!((read.mode, read.branchId), (MODE_ACTIVATE, 0));
        assert_eq!(
            (read.tipHash, read.vaultId, read.account, read.amount, read.depositTag, read.vaultOpenedHash),
            (v2.tipHash, v2.vaultId, v2.account, v2.amount, v2.depositTag, v2.vaultOpenedHash)
        );
        assert!(decode_deposit_journal_for(&v1, true).is_err());
        assert!(decode_deposit_journal_for(&real(), false).is_err());
        let mut dirty = v1.clone();
        dirty[2 * 32] = 1; // the account word
        assert!(decode_deposit_journal_for(&dirty, false).is_err());
    }

    #[test]
    fn malformed_journals_are_refused() {
        let mut v1 = real();
        v1.drain(..64); // the v1 journal: tipHash first, six words
        assert!(decode_deposit_journal(&v1).is_err());
        let mut long = real();
        long.push(0);
        assert!(decode_deposit_journal(&long).is_err());
        let mut mode = real();
        mode[31] = 2;
        assert!(decode_deposit_journal(&mode).is_err(), "unknown mode");
        mode[31] = MODE_REFUND;
        assert_eq!(decode_deposit_journal(&mode).unwrap().mode, MODE_REFUND);
        // Dirty high bytes: a mode, a branch id, an address or an amount out of range.
        for at in [0, 32, 4 * 32, 5 * 32] {
            let mut dirty = real();
            dirty[at] = 1;
            assert!(decode_deposit_journal(&dirty).is_err(), "dirty word at byte {at}");
        }
    }
}
