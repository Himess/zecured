//! K4: every relation of the deposit statement has a negative test, on a real deposit.
//!
//! The fixture is the witness of a real 0.01 TAZ deposit to a disposable Testnet vault (TEST-ONLY,
//! see its `_test_only` field): the same deposit whose journal contracts/test/fork/DepositProofs.t.sol
//! submits to the Registry. The catalog of single changes (zecured-deposit-k4) runs here natively
//! and in the zkVM executor through `zecured-tier2-host parity`; the cases below it are the ones
//! that are not witness changes, or that the statement accepts on purpose, with the reason.

use std::fs;

use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zecured_deposit_k4::{block_height, mutations};
use zecured_deposit_statement::{Error, Witness, check_deposit_transaction, verify};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_vault9_witness.json");

fn witness() -> Witness {
    serde_json::from_slice(&fs::read(FIXTURE).expect("fixture")).expect("witness JSON")
}

fn h32(s: &str) -> [u8; 32] {
    let b: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    b.try_into().unwrap()
}

#[test]
fn the_real_deposit_proves_the_journal_the_contracts_test_submits() {
    let j = verify(&witness()).expect("the real deposit proves");
    assert_eq!(j.tip_hash, h32("a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000"));
    assert_eq!(j.vault_id, h32("482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b"));
    assert_eq!(
        j.account.to_vec(),
        h32("000000000000000000000000fb928f182ad07d2d95d9592390556df7bfb6ed5f")[12..].to_vec()
    );
    assert_eq!(j.amount, 1_000_000);
    assert_eq!(j.tag, h32("d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e"));
    assert_eq!(j.vault_opened_hash, h32("385c78ae562975cc3c03af28374fd4e9d9e3f3405a18fbf81fe16d4f9e28c0f5"));
}

#[test]
fn every_catalogued_change_is_refused_with_its_error() {
    let catalog = mutations();
    assert!(catalog.len() >= 30, "{} changes", catalog.len());
    for m in catalog {
        let mut w = witness();
        (m.apply)(&mut w);
        assert_eq!(verify(&w), Err(m.expected), "{}", m.name);
    }
}

#[test]
fn the_expiry_height_is_inclusive() {
    let mut w = witness();
    w.expiry_height = block_height(&w);
    assert!(verify(&w).is_ok(), "mined at the expiry height: accepted");
}

// ------------------------------------------------------------------ the transaction's shape

/// An "empty Sapling bundle" cannot exist: with zero spends and outputs, v5 and v6 encode no
/// value balance or anchor, and the parsed transaction has no Sapling bundle at all.
#[test]
fn an_empty_sapling_part_is_no_sapling_bundle() {
    let w = witness();
    assert_eq!(&w.tx[22..24], &[0, 0], "zero Sapling spends and outputs");
    let tx = Transaction::read(&w.tx[..], BranchId::Nu7).unwrap();
    assert!(tx.sapling_bundle().is_none());
}

fn data_of(w: &Witness) -> TransactionData<Authorized> {
    Transaction::read(&w.tx[..], BranchId::Nu7).unwrap().into_data()
}

/// Both pools use `orchard::Bundle`: the Ironwood bundle placed in the Orchard field too.
#[test]
fn an_orchard_pool_bundle_makes_it_a_pool_crossing() {
    let d = data_of(&witness());
    let ironwood = d.ironwood_bundle().cloned();
    let both = TransactionData::from_parts_v6(
        BranchId::Nu7,
        d.lock_time(),
        d.expiry_height(),
        None,
        None,
        ironwood.clone(),
        ironwood,
    );
    assert_eq!(check_deposit_transaction(&both), Err(Error::NotIronwoodOnly));
}

#[test]
fn only_v6_transactions_of_the_nu7_epoch() {
    let d = data_of(&witness());
    let ironwood = d.ironwood_bundle().cloned();
    let nu6_3 =
        TransactionData::from_parts_v6(BranchId::Nu6_3, d.lock_time(), d.expiry_height(), None, None, None, ironwood);
    assert_eq!(check_deposit_transaction(&nu6_3), Err(Error::NotNu7V6));
    let v5 = TransactionData::from_parts(
        TxVersion::V5,
        BranchId::Nu6_1,
        0,
        BlockHeight::from_u32(4_000_000),
        None,
        None,
        None,
        None,
    );
    assert_eq!(check_deposit_transaction(&v5), Err(Error::NotNu7V6));
    let none = TransactionData::from_parts_v6(BranchId::Nu7, 0, d.expiry_height(), None, None, None, None);
    assert_eq!(check_deposit_transaction(&none), Err(Error::NotIronwoodOnly));
    assert_eq!(check_deposit_transaction(&d), Ok(()));
}

// ------------------------------------------------------------------ accepted on purpose

/// The transaction's authorizing data (proofs and signatures) is not part of its txid (ZIP 244,
/// as modified by ZIP 229), so changing it changes nothing the statement proves. Consensus, not
/// the statement, checks it; the relay only checks proof of work.
#[test]
fn accepted_on_purpose_changed_signature_bytes() {
    let real = verify(&witness()).unwrap();
    let mut w = witness();
    let last = w.tx.len() - 1; // inside the Ironwood binding signature, the last field
    w.tx[last] ^= 1;
    assert_eq!(verify(&w), Ok(real));
}

/// Only T's `hashBlockCommitments` (bytes 68..100) is checked here; every other byte of T only
/// changes `tip_hash`, which the Registry looks up in the header relay (where a changed header
/// fails its proof of work).
#[test]
fn accepted_on_purpose_other_bytes_of_t_only_change_its_hash() {
    let real = verify(&witness()).unwrap();
    let mut w = witness();
    w.tip_header[1000] ^= 1;
    let changed = verify(&w).unwrap();
    assert_ne!(changed.tip_hash, real.tip_hash);
    assert_eq!(changed.tag, real.tag);
}

// ------------------------------------------------------------------ what the journal binds

/// Everything else the witness says about the vault goes into the journal, where the Registry
/// checks it: the vault id directly, the rest through the signers' `VaultOpened` signatures
/// over `vault_opened_hash`, and the viewing key also through the tag.
#[test]
fn every_vault_field_changes_the_journal() {
    let real = verify(&witness()).unwrap();
    let mut w = witness();
    w.vault_id[0] ^= 1;
    assert_ne!(verify(&w).unwrap().vault_id, real.vault_id);
    assert_ne!(verify(&w).unwrap().vault_opened_hash, real.vault_opened_hash);
    for change in [
        (|w: &mut Witness| w.owner[0] ^= 1) as fn(&mut Witness),
        |w| w.account[0] ^= 1,
        |w| w.expiry_height += 1,
        |w| w.vault_ufvk.push('x'),
    ] {
        let mut w = witness();
        change(&mut w);
        assert_ne!(verify(&w).unwrap().vault_opened_hash, real.vault_opened_hash);
    }
    let mut w = witness();
    w.vault_ufvk.push('x');
    assert_ne!(verify(&w).unwrap().tag, real.tag, "the tag is keyed by the viewing key");
}
