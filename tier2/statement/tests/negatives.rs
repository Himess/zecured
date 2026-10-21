//! K4: every relation of the deposit statement has a negative test, on a real deposit.
//!
//! The fixture is the witness (v2) of a real 0.01 TAZ deposit to a disposable Testnet vault
//! (TEST-ONLY, see its `_test_only` field): the same deposit whose v1 journal
//! contracts/test/fork/DepositProofs.t.sol submits to the v2 Registry. The catalog of single
//! changes (zecured-deposit-k4) runs here natively for both networks, and in the zkVM executor
//! through `zecured-tier2-host parity`; the cases below it are the ones that are not witness
//! changes, or that the statement accepts on purpose, with the reason.

use std::{collections::HashSet, fs};

use orchard::keys::{FullViewingKey, Scope};
use zcash_address::unified::{self, Container, Encoding, Fvk};
use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zecured_deposit_k4::{
    Expected, OTHER_VAULT_RECEIVER, add_transparent_output, block_height, for_network, mutations, reanchor,
};
use zecured_deposit_statement::{
    Error, Inclusion, JOURNAL_LENGTH, Journal, MAINNET, NU6_3_BRANCH_ID, NU7_BRANCH_ID, Network, TESTNET, Witness,
    check_deposit_transaction_on, check_inclusion, check_refund_transaction_on, tag_key, tag_key_hash,
    vault_opened_hash, verify_on,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_vault9_witness.json");
const F1_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_vault9_f1_witness.json");

fn witness() -> Witness {
    serde_json::from_slice(&fs::read(FIXTURE).expect("fixture")).expect("witness JSON")
}

/// The fixture is a Testnet deposit: these tests check it on Testnet whatever the build's network
/// (`verify_on(&MAINNET, ..)` where a test is about Mainnet).
fn verify(w: &Witness) -> Result<Journal, Error> {
    verify_on(&TESTNET, w)
}

fn check_deposit_transaction(tx: &TransactionData<Authorized>) -> Result<(), Error> {
    check_deposit_transaction_on(&TESTNET, tx)
}

fn witness_for(net: &Network) -> Witness {
    let mut w = witness();
    for_network(&mut w, net);
    w
}

/// The fixture vault's UFVK. A deposit witness carries only its tag key; the F1 witness of the
/// same vault keeps the full UFVK.
fn vault_ufvk() -> String {
    let f1: serde_json::Value = serde_json::from_slice(&fs::read(F1_FIXTURE).expect("fixture")).expect("JSON");
    f1["detail"]["Uncommitted"]["vault_ufvk"].as_str().expect("the vault's UFVK").to_owned()
}

fn h32(s: &str) -> [u8; 32] {
    let b: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    b.try_into().unwrap()
}

const TIP: &str = "a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000";
const TAG: &str = "d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e";
/// The vault's `VaultOpened` v2 struct hash, computed independently (Python hashlib and
/// pycryptodome's keccak) from the fixture's values.
const OPENED: &str = "aaccb41983fa9d9288296dd099d5947119c2abc7e89b57237c00c76cf2680432";

#[test]
fn the_real_deposit_proves_its_journal() {
    let j = verify(&witness()).expect("the real deposit proves");
    assert_eq!((j.mode, j.branch_id), (0, NU7_BRANCH_ID));
    assert_eq!(j.tip_hash, h32(TIP));
    assert_eq!(j.vault_id, h32("482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b"));
    assert_eq!(
        j.account.to_vec(),
        h32("000000000000000000000000fb928f182ad07d2d95d9592390556df7bfb6ed5f")[12..].to_vec()
    );
    assert_eq!(j.amount, 1_000_000);
    // The same tag as the v1 statement gave (the same K, derived from the same UFVK).
    assert_eq!(j.tag, h32(TAG));
    assert_eq!(j.vault_opened_hash, h32(OPENED));
}

/// Journal v2 of the real deposit, word by word: a cross-language vector for the Intake's decoder
/// (`DepositJournal`, contracts/src/interfaces/v3/IZecuredIntake.sol) and zecured-base's.
#[test]
fn the_journal_is_the_intakes_abi() {
    let words = |j: Journal| -> Vec<String> {
        let enc = j.abi_encode();
        assert_eq!(enc.len(), JOURNAL_LENGTH);
        enc.chunks(32).map(|w| w.iter().map(|b| format!("{b:02x}")).collect()).collect()
    };
    let activate = verify(&witness()).unwrap();
    let expected = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000077190ad9",
        TIP,
        "482ea83b696ec7530c5ab23f7d9727bc9370f47884e63a19b6fa54ad3272d23b",
        "000000000000000000000000fb928f182ad07d2d95d9592390556df7bfb6ed5f",
        "00000000000000000000000000000000000000000000000000000000000f4240",
        TAG,
        OPENED,
    ];
    assert_eq!(words(activate), expected);
    let mut w = witness();
    w.mode = 1;
    let mut refund = expected;
    refund[0] = "0000000000000000000000000000000000000000000000000000000000000001";
    assert_eq!(words(verify(&w).unwrap()), refund, "Refund: only the mode word differs");
}

/// The prover gets K, never the UFVK (review H-10), and K is the vault's (D24).
#[test]
fn the_witness_carries_the_vaults_tag_key_not_its_viewing_key() {
    let json: serde_json::Value = serde_json::from_slice(&fs::read(FIXTURE).unwrap()).unwrap();
    assert!(json.get("vault_ufvk").is_none());
    let ufvk = vault_ufvk();
    assert!(!fs::read_to_string(FIXTURE).unwrap().contains(&ufvk));
    assert_eq!(witness().tag_key, tag_key(&ufvk));
    assert_eq!(witness().tag_key, h32("60aef1865a9b24f82d1d3dd314a9d3da09bf360531d537c6ffe481c64b415578"));
    assert_eq!(
        tag_key_hash(&witness().tag_key),
        h32("598862b8e69539c7639174b267e6db4f6b25809a3add1e17e347a4bf787b8a5c")
    );
}

#[test]
fn every_catalogued_change_has_its_outcome_on_both_networks() {
    for net in [TESTNET, MAINNET] {
        let base = verify_on(&net, &witness_for(&net)).expect("the base witness proves");
        let catalog = mutations(&net);
        assert!(catalog.len() >= 50, "{} changes", catalog.len());
        for m in catalog {
            let mut w = witness_for(&net);
            (m.apply)(&mut w);
            let got = verify_on(&net, &w);
            let what = format!("{}: {}", net.name, m.name);
            match m.expected {
                Expected::Refused(e) => assert_eq!(got, Err(e), "{what}"),
                Expected::Unsigned => {
                    let j = got.unwrap_or_else(|e| panic!("{what}: {e:?}"));
                    assert_ne!(j.vault_opened_hash, base.vault_opened_hash, "{what}");
                    assert_ne!(j.tag, base.tag, "{what}");
                }
                Expected::Accepted => {
                    got.unwrap_or_else(|e| panic!("{what}: {e:?}"));
                }
            }
        }
    }
}

/// The catalog's "another address of the vault" is the vault's own, at diversifier index 1.
#[test]
fn the_other_receiver_is_the_vaults_own() {
    let (_, _, keys) = unified::Ufvk::decode(&vault_ufvk()).unwrap();
    let fvk = keys
        .items()
        .iter()
        .find_map(|k| match k {
            Fvk::Orchard(b) => FullViewingKey::from_bytes(b),
            _ => None,
        })
        .unwrap();
    assert_eq!(fvk.address_at(0u32, Scope::External).to_raw_address_bytes().to_vec(), witness().note_recipient);
    assert_eq!(OTHER_VAULT_RECEIVER, fvk.address_at(1u32, Scope::External).to_raw_address_bytes());
}

// ------------------------------------------------------------------ the network (a build parameter)

/// The same deposit proves on Mainnet only under a Mainnet address, and on Testnet only under a
/// Testnet address: a note is never proven to the other network's deployment.
#[test]
fn each_network_takes_its_own_addresses_only() {
    assert_eq!(verify_on(&MAINNET, &witness()), Err(Error::Address), "a Testnet address on Mainnet");
    assert_eq!(verify_on(&TESTNET, &witness_for(&MAINNET)), Err(Error::Address));
    let main = verify_on(&MAINNET, &witness_for(&MAINNET)).unwrap();
    let test = verify(&witness()).unwrap();
    assert_eq!(main.tag, test.tag);
    assert_ne!(main.vault_opened_hash, test.vault_opened_hash, "another address string");
}

/// B and T must be of the witness's epoch: the history tree's node hashes are personalized with
/// its branch id, so T's history opens under that epoch only. Mainnet allows NU6.3 and NU7, and
/// this NU7 history does not open as NU6.3's.
#[test]
fn the_history_opens_under_its_own_epoch_only() {
    let w = witness();
    let tx = Transaction::read(&w.tx[..], BranchId::Nu7).unwrap();
    let at = |branch_id| Inclusion {
        branch_id,
        tx_index: w.tx_index,
        txid_path: &w.txid_path,
        block_header: &w.block_header,
        leaf: &w.leaf,
        mmr_path: &w.mmr_path,
        peaks: &w.peaks,
        peak_index: w.peak_index,
        tip_header: &w.tip_header,
        tip_auth_data_root: &w.tip_auth_data_root,
    };
    assert!(check_inclusion(tx.txid().as_ref(), &at(NU7_BRANCH_ID)).is_ok());
    assert_eq!(check_inclusion(tx.txid().as_ref(), &at(NU6_3_BRANCH_ID)), Err(Error::HistoryPath));
}

// ------------------------------------------------------------------ the modes

#[test]
fn the_expiry_height_is_inclusive() {
    let mut w = witness();
    w.expiry_height = block_height(&w);
    assert!(verify(&w).is_ok(), "mined at the expiry height: accepted");
}

/// A late payment cannot activate the vault, but it lands as a refund (T-09): the expiry binds
/// Activate mode only. The refund names the same `VaultOpened` (the expiry the signers signed).
#[test]
fn the_expiry_binds_activate_mode_only() {
    let mut w = witness();
    w.expiry_height = block_height(&w) - 1;
    assert_eq!(verify(&w), Err(Error::BlockAfterExpiry));
    w.mode = 1;
    let j = verify(&w).expect("a late note lands as a refund");
    assert_eq!((j.mode, j.amount, j.tag), (1, 1_000_000, h32(TAG)));
    assert_eq!(
        j.vault_opened_hash,
        vault_opened_hash(
            &w.vault_id,
            &w.owner,
            &w.account,
            w.vault_amount,
            w.expiry_height,
            &w.vault_address,
            &w.tag_key
        )
    );
}

/// Activate mode needs a note of the vault's amount; a refund carries the note's own value, and
/// its `VaultOpened` the vault's amount.
#[test]
fn the_vaults_amount_binds_activate_mode_only() {
    let mut w = witness();
    w.vault_amount = 5_000_000;
    assert_eq!(verify(&w), Err(Error::AmountMismatch));
    w.mode = 1;
    let j = verify(&w).expect("a note of another value lands as a refund");
    assert_eq!(j.amount, 1_000_000, "the note's value");
    let signed =
        vault_opened_hash(&w.vault_id, &w.owner, &w.account, 5_000_000, w.expiry_height, &w.vault_address, &w.tag_key);
    assert_eq!(j.vault_opened_hash, signed, "the vault's own VaultOpened (5,000,000)");
}

/// Refund mode drops D5: a pool-crossing transaction (here the Ironwood bundle placed in the
/// Orchard field too) cannot activate a vault but can be refunded. It still needs an Ironwood
/// bundle, v6 and an allowed epoch.
#[test]
fn refund_mode_does_not_apply_d5() {
    let d = data_of(&witness());
    let ironwood = d.ironwood_bundle().cloned();
    let crossing = TransactionData::from_parts_v6(
        BranchId::Nu7,
        d.lock_time(),
        d.expiry_height(),
        None,
        None,
        ironwood.clone(),
        ironwood.clone(),
    );
    assert_eq!(check_deposit_transaction(&crossing), Err(Error::NotIronwoodOnly));
    assert_eq!(check_refund_transaction_on(&TESTNET, &crossing), Ok(()));
    let none = TransactionData::from_parts_v6(BranchId::Nu7, 0, d.expiry_height(), None, None, None, None);
    assert_eq!(check_refund_transaction_on(&TESTNET, &none), Err(Error::NoIronwoodBundle));
    let nu6_3 =
        TransactionData::from_parts_v6(BranchId::Nu6_3, d.lock_time(), d.expiry_height(), None, None, None, ironwood);
    assert_eq!(check_refund_transaction_on(&TESTNET, &nu6_3), Err(Error::WrongEpoch));
    assert_eq!(check_refund_transaction_on(&MAINNET, &nu6_3), Ok(()));
}

/// The same rule on a whole witness: the fixture's transaction with a transparent output added,
/// alone in a synthetic block under a synthetic anchor (`reanchor`; the relay on Base would never
/// accept that anchor, and the statement checks no proof of work). Activate mode refuses it (D5);
/// Refund mode lands its note to the vault, with the tag and `VaultOpened` of the real deposit.
#[test]
fn refund_mode_proves_a_transaction_d5_refuses() {
    let mut w = witness();
    add_transparent_output(&mut w);
    reanchor(&mut w);
    assert_eq!(verify(&w), Err(Error::NotIronwoodOnly));
    w.mode = 1;
    let j = verify(&w).expect("a refund needs no D5");
    assert_eq!((j.mode, j.branch_id, j.amount), (1, NU7_BRANCH_ID, 1_000_000));
    assert_eq!((j.tag, j.vault_opened_hash), (h32(TAG), h32(OPENED)));
    assert_ne!(j.tip_hash, h32(TIP), "a synthetic anchor");
}

/// One note, one tag, in both modes: Activate and Refund proofs of the same note carry the same
/// tag, so the Intake's `DepositTagUsed` lets it land once.
#[test]
fn a_note_has_one_tag_in_both_modes() {
    let activate = verify(&witness()).unwrap();
    let mut w = witness();
    w.mode = 1;
    let refund = verify(&w).unwrap();
    assert_eq!(activate.tag, refund.tag);
    assert_eq!(activate.vault_opened_hash, refund.vault_opened_hash);
    assert_ne!(activate.abi_encode(), refund.abi_encode());
}

// ------------------------------------------------------------------ red-team regression (design review)

/// What the Intake does with a deposit journal, as far as this regression needs: all five
/// signers signed one `VaultOpened` for the vault (its struct hash), and a tag lands once.
struct Base {
    signed: [u8; 32],
    used: HashSet<[u8; 32]>,
}

impl Base {
    fn new(signed: [u8; 32]) -> Self {
        Self {
            signed,
            used: HashSet::new(),
        }
    }

    fn land(&mut self, j: &Journal) -> Result<(), &'static str> {
        if j.vault_opened_hash != self.signed {
            return Err("VaultOpened not signed");
        }
        if !self.used.insert(j.tag) {
            return Err("DepositTagUsed");
        }
        Ok(())
    }
}

/// The attack the separate tag key opened: prove the note once under a made-up K' (tag T1), then
/// again under the real K (tag T2 != T1), so one note backs zZEC and a refund. With
/// `tagKeyHash = keccak256(K)` in `VaultOpened`, a proof under K' carries a struct hash nobody
/// signed. The same note cannot land under two tags, in either mode or order.
#[test]
fn the_same_note_cannot_land_under_two_tags() {
    // What every honest node signs: VaultOpened with keccak256 of K derived from the UFVK it holds.
    let w = witness();
    let signed = vault_opened_hash(
        &w.vault_id,
        &w.owner,
        &w.account,
        w.vault_amount,
        w.expiry_height,
        &w.vault_address,
        &tag_key(&vault_ufvk()),
    );
    let proof = |mode: u8, k: Option<[u8; 32]>| {
        let mut w = witness();
        w.mode = mode;
        if let Some(k) = k {
            w.tag_key = k;
        }
        verify(&w).expect("the statement itself accepts any K")
    };
    for forged in [[0x42; 32], tag_key("uviewtest1attacker"), {
        let mut k = witness().tag_key;
        k[31] ^= 1;
        k
    }] {
        for (first, second) in [(0, 1), (1, 0), (0, 0), (1, 1)] {
            let mut base = Base::new(signed);
            let under_forged = proof(first, Some(forged));
            assert_ne!(under_forged.tag, proof(first, None).tag, "another K, another tag");
            assert_eq!(base.land(&under_forged), Err("VaultOpened not signed"));
            base.land(&proof(first, None)).expect("the honest proof lands");
            assert_eq!(base.land(&proof(second, None)), Err("DepositTagUsed"));
            assert_eq!(base.land(&proof(second, Some(forged))), Err("VaultOpened not signed"));
        }
    }
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

/// D5: v6 transactions of an epoch the network allows. NU6.3 is a Mainnet epoch only.
#[test]
fn only_v6_transactions_of_an_allowed_epoch() {
    let d = data_of(&witness());
    let ironwood = d.ironwood_bundle().cloned();
    let nu6_3 =
        TransactionData::from_parts_v6(BranchId::Nu6_3, d.lock_time(), d.expiry_height(), None, None, None, ironwood);
    assert_eq!(check_deposit_transaction(&nu6_3), Err(Error::WrongEpoch));
    assert_eq!(check_deposit_transaction_on(&MAINNET, &nu6_3), Ok(()));
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
    assert_eq!(check_deposit_transaction(&v5), Err(Error::NotV6));
    assert_eq!(check_deposit_transaction_on(&MAINNET, &v5), Err(Error::NotV6));
    let none = TransactionData::from_parts_v6(BranchId::Nu7, 0, d.expiry_height(), None, None, None, None);
    assert_eq!(check_deposit_transaction(&none), Err(Error::NotIronwoodOnly));
    assert_eq!(check_deposit_transaction(&d), Ok(()));
    assert_eq!(check_deposit_transaction_on(&MAINNET, &d), Ok(()));
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
/// changes `tip_hash`, which the Intake looks up in the header relay (where a changed header
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

/// Everything else the witness says about the vault goes into the journal, where the Intake
/// checks it: the vault id directly, the rest through the signers' `VaultOpened` signatures over
/// `vault_opened_hash`, and the tag key also through the tag.
#[test]
fn every_vault_field_changes_the_journal() {
    let real = verify(&witness()).unwrap();
    let mut w = witness();
    w.vault_id[0] ^= 1;
    assert_ne!(verify(&w).unwrap().vault_id, real.vault_id);
    assert_ne!(verify(&w).unwrap().vault_opened_hash, real.vault_opened_hash);
    let refund = |f: fn(&mut Witness)| {
        let mut w = witness();
        w.mode = 1;
        f(&mut w);
        verify(&w).unwrap()
    };
    let real_refund = refund(|_| {});
    for change in [
        (|w: &mut Witness| w.owner[0] ^= 1) as fn(&mut Witness),
        |w| w.account[0] ^= 1,
        |w| w.expiry_height += 1,
        |w| w.vault_amount += 1,
        |w| w.tag_key[0] ^= 1,
    ] {
        assert_ne!(refund(change).vault_opened_hash, real_refund.vault_opened_hash);
    }
    let mut w = witness();
    w.tag_key[0] ^= 1;
    assert_ne!(verify(&w).unwrap().tag, real.tag, "the tag is keyed by the tag key");
}
