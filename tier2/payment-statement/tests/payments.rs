//! K4 for the payment statement: every relation has a negative test.
//!
//! The fixture (`TEST-ONLY_swap_payment_witness.json`) is synthetic (see zecured-payment-k4): the
//! real deposit transaction of the deposit statement's TEST-ONLY fixture with its deposit action
//! re-encrypted as a swap payment, re-anchored along B's real paths. It is checked here to be
//! exactly that synthesis. The real deposit itself, under its real anchor, is a payment witness the
//! statement refuses only at the memo: every check before it runs on real chain data. The catalog
//! of single changes (zecured-payment-k4) runs here natively for both networks, and in the zkVM
//! executor through `zecured-tier2-host payment parity`.

use std::fs;

use zecured_deposit_statement::{Witness, sha256d};
use zecured_payment_k4::{
    Expected, FIXTURE_CHAIN_ID, FIXTURE_MAKER, FIXTURE_NONCE, FIXTURE_SALT, FIXTURE_SWAP, FIXTURE_VALUE, as_expected,
    fixture_order_id, from_deposit, mutations, order_id, synthesize,
};
use zecured_payment_statement::{
    Error, JOURNAL_KIND, JOURNAL_LENGTH, MAINNET, NU7_BRANCH_ID, Network, PaymentJournal, PaymentWitness, TESTNET,
    receiver_commitment, verify_on,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_swap_payment_witness.json");
const DEPOSIT_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../statement/tests/fixtures/TEST-ONLY_vault9_witness.json");

const TEST_ONLY: &str = "TEST-ONLY fixture (CLAUDE.md exception, review S20). SYNTHETIC: the real deposit transaction of \
     tier2/statement/tests/fixtures/TEST-ONLY_vault9_witness.json (the disposable Zcash Testnet vault 9; never a demo or \
     judge vault) with its deposit action re-encrypted as a Zecured Swap payment note (memo \"ZSWP\" || orderId, \
     ovk = None) to the same receiver, re-anchored along the block's real paths under a synthetic header that no \
     relay would accept (zecured_payment_k4::synthesize). It holds no key; the payer's note opening and the salt are \
     test values.";

fn witness() -> PaymentWitness {
    serde_json::from_slice(&fs::read(FIXTURE).expect("fixture")).expect("witness JSON")
}

fn deposit() -> Witness {
    serde_json::from_slice(&fs::read(DEPOSIT_FIXTURE).expect("deposit fixture")).expect("deposit witness JSON")
}

/// The fixture is a Testnet payment: these tests check it on Testnet whatever the build's network.
fn verify(w: &PaymentWitness) -> Result<PaymentJournal, Error> {
    verify_on(&TESTNET, w)
}

fn h32(s: &str) -> [u8; 32] {
    let b: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    b.try_into().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Writes the fixture from the deposit fixture (run once, with `--ignored`).
#[test]
#[ignore = "writes the fixture"]
fn write_fixture() {
    let w = synthesize(&deposit());
    let mut json = serde_json::to_value(&w).unwrap();
    json.as_object_mut().unwrap().insert("_test_only".into(), TEST_ONLY.into());
    fs::write(FIXTURE, serde_json::to_vec(&json).unwrap()).unwrap();
}

/// The fixture is exactly the synthesis of the real deposit: anyone can rebuild it.
#[test]
fn the_fixture_is_the_synthesis_of_the_real_deposit() {
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(FIXTURE).unwrap()).unwrap();
    assert_eq!(raw["_test_only"], TEST_ONLY);
    let rebuilt = serde_json::to_value(synthesize(&deposit())).unwrap();
    let mut committed = raw.clone();
    committed.as_object_mut().unwrap().remove("_test_only");
    assert_eq!(committed, rebuilt);
}

/// The order id vector (`keccak256(abi.encode(chainId, swap, maker, nonce))`), checked by
/// contracts/test/unit/ZecuredSwap.t.sol with the same values.
#[test]
fn the_order_id_vector() {
    assert_eq!(
        hex(&order_id(FIXTURE_CHAIN_ID, &FIXTURE_SWAP, &FIXTURE_MAKER, FIXTURE_NONCE)),
        ORDER_ID,
        "keccak256(abi.encode(84532, 0x1111..11, 0x2222..22, 7))"
    );
    assert_eq!(fixture_order_id(), h32(ORDER_ID));
    assert_ne!(order_id(FIXTURE_CHAIN_ID + 1, &FIXTURE_SWAP, &FIXTURE_MAKER, FIXTURE_NONCE), h32(ORDER_ID));
    assert_ne!(order_id(FIXTURE_CHAIN_ID, &[0x12; 20], &FIXTURE_MAKER, FIXTURE_NONCE), h32(ORDER_ID));
    assert_ne!(order_id(FIXTURE_CHAIN_ID, &FIXTURE_SWAP, &FIXTURE_MAKER, FIXTURE_NONCE + 1), h32(ORDER_ID));
}

/// `keccak256(abi.encode(uint256(84532), address(0x1111..11), address(0x2222..22), uint256(7)))`,
/// computed independently with Foundry's `cast abi-encode` and `cast keccak`.
const ORDER_ID: &str = "d13da7cdae4854b425b999bef6b63f91c51b597b93c5cf0805513f2a42ff904f";

#[test]
fn the_fixture_proves_its_journal() {
    let w = witness();
    let j = verify(&w).expect("the fixture proves");
    assert_eq!(j.kind, JOURNAL_KIND);
    assert_eq!(j.branch_id, NU7_BRANCH_ID);
    assert_eq!(j.anchor, sha256d(&w.tip_header), "T's hash");
    assert_eq!(j.order_id, fixture_order_id(), "the memo's order");
    let receiver: [u8; 43] = w.note_recipient.clone().try_into().unwrap();
    assert_eq!(j.receiver_commitment, receiver_commitment(&FIXTURE_SALT, &receiver));
    assert_eq!(j.zatoshi, FIXTURE_VALUE);
    // Mainnet allows the NU7 epoch too: the same witness proves the same journal on a Mainnet build.
    assert_eq!(verify_on(&MAINNET, &w), Ok(j));
}

/// The journal, word by word: a cross-language vector for `ZecuredSwap.proveSettlement`'s decoder
/// (contracts/test/unit/ZecuredSwap.t.sol `test_theRustFixturesJournalSettlesItsOrder` settles an
/// order on these 192 bytes). The anchor is SHA-256d of the fixture's T' (Python's hashlib); the
/// receiver commitment `keccak256(0x5a.. || receiver)` (Foundry's `cast keccak`).
#[test]
fn the_journal_is_the_swaps_abi() {
    let w = witness();
    let j = verify(&w).unwrap();
    let enc = j.abi_encode();
    assert_eq!(enc.len(), JOURNAL_LENGTH);
    let words: Vec<String> = enc.chunks(32).map(hex).collect();
    assert_eq!(
        words,
        [
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000077190ad9",
            ANCHOR,
            ORDER_ID,
            RECEIVER_COMMITMENT,
            "00000000000000000000000000000000000000000000000000000000002625a0",
        ]
    );
    let receiver: [u8; 43] = w.note_recipient.clone().try_into().unwrap();
    assert_eq!(hex(&sha256d(&w.tip_header)), ANCHOR);
    assert_eq!(hex(&receiver_commitment(&FIXTURE_SALT, &receiver)), RECEIVER_COMMITMENT);
}

const ANCHOR: &str = "301c5d2807c6ed2eaebfe1e92ec0e25a2cb8bd4a7c3d8173c9d717148908d225";
const RECEIVER_COMMITMENT: &str = "0144dce00498a1cb4e3d04161414ccf1cd42b09f70fe83ddcf745973c522a0a7";

/// The real deposit (a real transaction in a real block under the real header 4,488,560) is a payment
/// witness up to the memo: the statement walks its real inclusion and recovers its real note, then
/// refuses it because its memo (the empty memo) names no order.
#[test]
fn the_real_deposit_is_refused_at_the_memo_only() {
    let d = deposit();
    let w = from_deposit(&d, FIXTURE_SALT);
    assert_eq!(verify(&w), Err(Error::Memo));
    assert_eq!(verify_on(&MAINNET, &w), Err(Error::Memo));
    // Its inclusion is the real one (the deposit statement's own fixture proves under the same anchor).
    assert_eq!(hex(&sha256d(&w.tip_header)), "a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000");
}

/// The synthetic fixture keeps everything of the real deposit but the payment note and the hashes
/// the re-anchoring recomputes: the same epoch, transaction shape, block height and position, and
/// history tree shape.
#[test]
fn the_synthesis_changes_only_the_note_and_the_anchor_hashes() {
    let (d, w) = (deposit(), witness());
    assert_eq!(w.branch_id, d.branch_id);
    assert_eq!((w.action_index, w.tx_index), (d.action_index, d.tx_index));
    assert_eq!(w.tx.len(), d.tx.len());
    let differing = w.tx.iter().zip(&d.tx).filter(|(a, b)| a != b).count();
    assert!(
        differing <= 32 + 32 + 580,
        "only the action's cmx, epk and note ciphertext change ({differing} bytes)"
    );
    assert_eq!(w.txid_path, d.txid_path, "B's other transaction is the real one");
    assert_eq!(w.mmr_path.len(), d.mmr_path.len());
    assert!(w.mmr_path.iter().zip(&d.mmr_path).all(|(a, b)| a.sibling == b.sibling));
    assert_eq!(w.peaks.len(), d.peaks.len());
    let changed_peaks: Vec<usize> = (0..w.peaks.len()).filter(|i| w.peaks[*i] != d.peaks[*i]).collect();
    assert_eq!(changed_peaks, vec![d.peak_index as usize], "only the payment's own peak changes");
    assert_eq!(w.block_header[..36], d.block_header[..36]);
    assert_eq!(w.block_header[68..], d.block_header[68..], "only B's Merkle root changes");
    assert_eq!(w.tip_header[..68], d.tip_header[..68]);
    assert_eq!(w.tip_header[100..], d.tip_header[100..], "only T's hashBlockCommitments changes");
    assert_eq!(w.note_recipient, d.note_recipient);
}

/// K4: every single change of the catalog has its outcome natively, on both networks.
#[test]
fn every_change_has_its_outcome_on_both_networks() {
    for net in [TESTNET, MAINNET] {
        check_catalog(&net);
    }
}

fn check_catalog(net: &Network) {
    let base = witness();
    let real = verify_on(net, &base).expect("the base witness proves");
    let mut failures = vec![];
    let catalog = mutations(net);
    assert!(catalog.len() >= 40, "the catalog covers every relation ({} changes)", catalog.len());
    for m in catalog {
        let mut w = base.clone();
        (m.apply)(&mut w);
        let native = verify_on(net, &w);
        println!("{:<4} {:<66} {:?}", net.name, m.name, native.as_ref().map(|_| "proves"));
        if !as_expected(m.expected, &real, &native) {
            failures.push(format!("{}: expected {:?}, got {native:?}", m.name, m.expected));
        }
    }
    assert!(failures.is_empty(), "{} network:\n{}", net.name, failures.join("\n"));
}

/// The catalog's expectations themselves: a refusal is never read as an acceptance, and a journal
/// that binds another order, receiver or amount never passes for the base's.
#[test]
fn the_outcome_check_is_strict() {
    let real = verify(&witness()).unwrap();
    assert!(as_expected(Expected::Accepted, &real, &Ok(real)));
    assert!(!as_expected(Expected::Rebound, &real, &Ok(real)));
    assert!(!as_expected(Expected::Accepted, &real, &Err(Error::Memo)));
    assert!(as_expected(Expected::Refused(Error::Memo), &real, &Err(Error::Memo)));
    assert!(!as_expected(Expected::Refused(Error::Memo), &real, &Err(Error::Note)));
    for change in [
        PaymentJournal {
            order_id: [0; 32],
            ..real
        },
        PaymentJournal {
            receiver_commitment: [0; 32],
            ..real
        },
        PaymentJournal {
            zatoshi: real.zatoshi + 1,
            ..real
        },
    ] {
        assert!(as_expected(Expected::Rebound, &real, &Ok(change)));
        assert!(!as_expected(Expected::Accepted, &real, &Ok(change)));
    }
}

/// The witness carries the payer's note opening and the salt: no viewing key, no tag key.
#[test]
fn the_witness_carries_no_key() {
    let json: serde_json::Value = serde_json::from_slice(&fs::read(FIXTURE).unwrap()).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    for forbidden in ["vault_ufvk", "ufvk", "tag_key", "spending_key"] {
        assert!(!keys.iter().any(|k| k.as_str() == forbidden), "{forbidden}");
    }
}
