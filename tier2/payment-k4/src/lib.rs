//! K4 for the payment statement: the synthetic payment fixture and the single changes to a payment
//! witness, each with the outcome the statement must give. The statement's tests run them
//! natively; `zecured-tier2-host payment parity` runs them in the RISC Zero executor, where the
//! guest must panic with the same error or commit the same journal.
//!
//! **The fixture is synthetic** (TEST-ONLY): no swap payment had been made on Zcash Testnet when
//! the payment statement was written. It starts from the real deposit witness of the deposit
//! statement's fixture (a real v6 Ironwood-only transaction of the disposable Testnet vault 9, in a
//! real block under a real header), re-encrypts that deposit's action as a payment note carrying
//! the swap memo ([`synthesize`]), and re-anchors the changed transaction ([`reanchor`]): its block
//! B' is B with the Merkle root the changed txid reaches along B's real path, its history-tree leaf
//! is B''s, the real siblings lead to a new peak, and header T' is T with the
//! `hashBlockCommitments` of the new peaks. The statement checks no proof of work and no Halo 2
//! proof or signature (the relay and Zcash consensus do), so it accepts this witness; the relay on
//! Base would never accept T' (its hash is in no chain). Only the payment note is new: the
//! transaction's shape, its epoch, B's height and position, the history tree's shape and every
//! other byte are the real ones. [`from_deposit`] keeps the real transaction and anchor instead,
//! which the statement refuses at the memo (the deposit carries the empty memo).

use orchard::{
    Address, Note,
    note::{ExtractedNoteCommitment, NoteVersion, RandomSeed, Rho},
    note_encryption::{IronwoodDomain, IronwoodNoteEncryption},
    value::NoteValue,
};
use zcash_history::{NodeDataV3, V2, V3, Version};
use zcash_note_encryption::Domain;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
pub use zecured_deposit_k4::OTHER_VAULT_RECEIVER;
use zecured_deposit_statement::{Witness, block_commitments, sha256d, txid_path_root};
use zecured_payment_statement::{
    Error, MEMO_LENGTH, NU6_3_BRANCH_ID, Network, PaymentJournal, PaymentWitness, keccak256, payment_memo,
};

/// The synthetic fixture's order: `orderId = keccak256(abi.encode(chainId, swap, maker, nonce))`
/// with these values (contracts/test/unit/ZecuredSwap.t.sol checks the same vector).
pub const FIXTURE_CHAIN_ID: u64 = 84_532;
pub const FIXTURE_SWAP: [u8; 20] = [0x11; 20];
pub const FIXTURE_MAKER: [u8; 20] = [0x22; 20];
pub const FIXTURE_NONCE: u64 = 7;
/// The salt of the fixture's receiver commitment.
pub const FIXTURE_SALT: [u8; 32] = [0x5a; 32];
/// The payment note's value (0.025 TAZ) and rseed.
pub const FIXTURE_VALUE: u64 = 2_500_000;
pub const FIXTURE_RSEED: [u8; 32] = [0x42; 32];

/// `keccak256(abi.encode(uint256 chainId, address swap, address maker, uint256 nonce))`, the order
/// id `ZecuredSwap.open` gives the order of `maker`'s quote `nonce`.
pub fn order_id(chain_id: u64, swap: &[u8; 20], maker: &[u8; 20], nonce: u64) -> [u8; 32] {
    let mut enc = [0u8; 4 * 32];
    enc[24..32].copy_from_slice(&chain_id.to_be_bytes());
    enc[32 + 12..64].copy_from_slice(swap);
    enc[64 + 12..96].copy_from_slice(maker);
    enc[96 + 24..128].copy_from_slice(&nonce.to_be_bytes());
    keccak256(&enc)
}

/// The fixture's order id.
pub fn fixture_order_id() -> [u8; 32] {
    order_id(FIXTURE_CHAIN_ID, &FIXTURE_SWAP, &FIXTURE_MAKER, FIXTURE_NONCE)
}

/// What a change must lead to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// The statement refuses the witness with this error (the guest panics with it).
    Refused(Error),
    /// The statement accepts the witness with the base journal's order, receiver commitment and
    /// amount: the change is one the statement does not see, or a re-anchoring.
    Accepted,
    /// The statement accepts the witness, but its journal names another order, receiver
    /// commitment or amount than the base's: `ZecuredSwap` refuses it for the base's order.
    Rebound,
}

/// Whether `native`, the statement's outcome on a changed witness, is what `expected` asks, given
/// the base witness's journal `base`.
pub fn as_expected(expected: Expected, base: &PaymentJournal, native: &Result<PaymentJournal, Error>) -> bool {
    let binds_like_base = |j: &PaymentJournal| {
        j.order_id == base.order_id && j.receiver_commitment == base.receiver_commitment && j.zatoshi == base.zatoshi
    };
    match (expected, native) {
        (Expected::Refused(e), Err(got)) => *got == e,
        (Expected::Accepted, Ok(j)) => binds_like_base(j) && j.kind == base.kind && j.branch_id == base.branch_id,
        (Expected::Rebound, Ok(j)) => !binds_like_base(j) && j.kind == base.kind && j.branch_id == base.branch_id,
        _ => false,
    }
}

/// One change and the outcome it must have.
pub struct Mutation {
    pub name: &'static str,
    pub apply: fn(&mut PaymentWitness),
    pub expected: Expected,
}

/// The deposit witness `d` as a payment witness with `salt`: the real transaction, note opening and
/// anchor. The statement refuses it at the memo (a deposit's memo is not a swap payment's).
pub fn from_deposit(d: &Witness, salt: [u8; 32]) -> PaymentWitness {
    PaymentWitness {
        branch_id: d.branch_id,
        tx: d.tx.clone(),
        action_index: d.action_index,
        note_recipient: d.note_recipient.clone(),
        note_value: d.note_value,
        note_rseed: d.note_rseed,
        salt,
        tx_index: d.tx_index,
        txid_path: d.txid_path.clone(),
        block_header: d.block_header.clone(),
        leaf: d.leaf.clone(),
        mmr_path: d.mmr_path.clone(),
        peaks: d.peaks.clone(),
        peak_index: d.peak_index,
        tip_header: d.tip_header.clone(),
        tip_auth_data_root: d.tip_auth_data_root,
    }
}

/// The synthetic fixture from the deposit fixture `d`: its deposit action re-encrypted as a payment
/// of [`FIXTURE_VALUE`] to the same receiver, with the memo of [`fixture_order_id`], under the salt
/// [`FIXTURE_SALT`], re-anchored ([`reanchor`]).
pub fn synthesize(d: &Witness) -> PaymentWitness {
    let mut w = from_deposit(d, FIXTURE_SALT);
    w.note_rseed = FIXTURE_RSEED;
    w.note_value = FIXTURE_VALUE;
    reencrypt(&mut w, &payment_memo(&fixture_order_id()));
    reanchor(&mut w);
    w
}

/// Re-encrypts the witness's action as the note the witness opens (its recipient, value and rseed,
/// rho the action's nullifier), with `memo`, in place in the serialized transaction: the action's
/// note commitment, ephemeral key and note ciphertext change; its value commitment, key, proof
/// and signatures do not (the statement checks none of them). The txid changes: [`reanchor`] puts
/// the transaction back under an anchor.
pub fn reencrypt(w: &mut PaymentWitness, memo: &[u8; MEMO_LENGTH]) {
    let tx = Transaction::read(&w.tx[..], BranchId::Nu7).expect("a v6 transaction");
    let bundle = tx.ironwood_bundle().expect("an Ironwood bundle");
    let action = &bundle.actions()[w.action_index as usize];
    // v6 serializes an action as cv_net || nf || rk || cmx || epk || enc_ciphertext || out_ciphertext
    // (zcash_primitives' write_action_without_auth): cmx and epk are adjacent and unique.
    let old: Vec<u8> = [action.cmx().to_bytes(), action.encrypted_note().epk_bytes].concat();
    let rho = Rho::from_bytes(&action.nullifier().to_bytes()).expect("the action's nullifier");
    let at = {
        let mut found = w.tx.windows(old.len()).enumerate().filter(|(_, s)| *s == old.as_slice()).map(|(i, _)| i);
        let at = found.next().expect("the action's cmx and epk in the transaction");
        assert!(found.next().is_none(), "the action's cmx and epk occur once");
        at
    };

    let recipient: [u8; 43] = w.note_recipient.clone().try_into().expect("a 43-byte receiver");
    let address = Address::from_raw_address_bytes(&recipient).expect("a valid receiver");
    let rseed = RandomSeed::from_bytes(w.note_rseed, &rho).expect("an rseed valid for the action's rho");
    let note = Note::from_parts(address, NoteValue::from_raw(w.note_value), rho, rseed, NoteVersion::V3)
        .expect("a valid note");
    let cmx = ExtractedNoteCommitment::from(note.commitment()).to_bytes();
    let encryption = IronwoodNoteEncryption::new(None, note, *memo);
    let epk = IronwoodDomain::epk_bytes(encryption.epk()).0;
    let enc = encryption.encrypt_note_plaintext().0;

    w.tx[at..at + 32].copy_from_slice(&cmx);
    w.tx[at + 32..at + 64].copy_from_slice(&epk);
    w.tx[at + 64..at + 64 + enc.len()].copy_from_slice(&enc);
    let changed = Transaction::read(&w.tx[..], BranchId::Nu7).expect("still a v6 transaction");
    let action = &changed.ironwood_bundle().expect("an Ironwood bundle").actions()[w.action_index as usize];
    assert_eq!(action.cmx().to_bytes(), cmx);
    assert_eq!(action.encrypted_note().epk_bytes, epk);
    assert_eq!(action.encrypted_note().enc_ciphertext.0, enc);
}

fn node(branch_id: u32, bytes: &[u8]) -> NodeDataV3 {
    V3::from_bytes(branch_id, bytes).expect("a V3 history node")
}

/// Puts the witness's transaction, as it now is, back under an anchor, keeping the shape of its
/// inclusion: block B' is B with the Merkle root the txid reaches along the witness's txid path and
/// index; B''s history-tree leaf replaces B's; the witness's real siblings lead from it to a new
/// peak at the same place; header T''s `hashBlockCommitments` commits to the new peaks (under the
/// witness's epoch, `w.branch_id`). Every inclusion check passes again; only header hashes change.
/// The statement checks no proof of work, so it accepts this anchor; the relay on Base never would.
pub fn reanchor(w: &mut PaymentWitness) {
    let tx = Transaction::read(&w.tx[..], BranchId::Nu7).expect("a v6 transaction");
    let txid: [u8; 32] = *tx.txid().as_ref();
    let root = txid_path_root(txid, w.tx_index, &w.txid_path);
    w.block_header[36..68].copy_from_slice(&root);
    let mut leaf = node(w.branch_id, &w.leaf);
    leaf.v2.v1.subtree_commitment = sha256d(&w.block_header);
    w.leaf = V3::to_bytes(&leaf);
    let mut acc = leaf;
    for step in &w.mmr_path {
        let sibling = node(w.branch_id, &step.sibling);
        acc = if step.sibling_is_left {
            V3::combine(&sibling, &acc)
        } else {
            V3::combine(&acc, &sibling)
        };
    }
    w.peaks[w.peak_index as usize] = V3::to_bytes(&acc);
    let mut peaks = w.peaks.iter();
    let mut history = node(w.branch_id, peaks.next().expect("a peak"));
    for p in peaks {
        history = V3::combine(&history, &node(w.branch_id, p));
    }
    let commitments = block_commitments(&V3::hash(&history), &w.tip_auth_data_root);
    w.tip_header[68..100].copy_from_slice(&commitments);
}

fn with_leaf(w: &mut PaymentWitness, change: impl FnOnce(&mut NodeDataV3)) {
    let mut leaf = node(w.branch_id, &w.leaf);
    change(&mut leaf);
    w.leaf = V3::to_bytes(&leaf);
}

/// The witness's transaction with one transparent output added (D5 forbids it). Adapted from
/// `zecured_deposit_k4::add_transparent_output`, which takes a deposit witness.
pub fn add_transparent_output(w: &mut PaymentWitness) {
    // v6: 20 header bytes, then the transparent input and output counts (both 0).
    assert_eq!(&w.tx[20..22], &[0, 0]);
    let mut tx = w.tx[..21].to_vec();
    tx.push(1);
    tx.extend_from_slice(&1_000u64.to_le_bytes());
    tx.push(0);
    tx.extend_from_slice(&w.tx[22..]);
    w.tx = tx;
}

/// The witness's transaction with its own consensus branch id set to `branch_id`. Adapted from
/// `zecured_deposit_k4::set_transaction_branch_id`.
pub fn set_transaction_branch_id(w: &mut PaymentWitness, branch_id: u32) {
    // v6: the header word, the version group id, then the consensus branch id (little-endian).
    assert_eq!(BranchId::try_from(u32::from_le_bytes(w.tx[8..12].try_into().expect("4 bytes"))), Ok(BranchId::Nu7));
    w.tx[8..12].copy_from_slice(&branch_id.to_le_bytes());
}

/// The witness's transaction without its Ironwood bundle. Adapted from the deposit catalog's.
fn drop_ironwood_bundle(w: &mut PaymentWitness) {
    // v6: 20 header bytes; no transparent inputs or outputs, no Sapling spends or outputs, no
    // Orchard-pool actions; then the Ironwood bundle (an empty bundle is one zero byte).
    assert_eq!(&w.tx[20..25], &[0, 0, 0, 0, 0]);
    w.tx.truncate(25);
    w.tx.push(0);
}

/// The memo of a note of another order than the fixture's.
fn other_order_memo() -> [u8; MEMO_LENGTH] {
    payment_memo(&order_id(FIXTURE_CHAIN_ID, &FIXTURE_SWAP, &FIXTURE_MAKER, FIXTURE_NONCE + 1))
}

/// The catalog for the statement built for `net`, in the statement's order of checks. The base is
/// a witness that proves (the synthetic fixture).
pub fn mutations(net: &Network) -> Vec<Mutation> {
    use Error::*;
    use Expected::*;
    let m = |name, apply: fn(&mut PaymentWitness), expected| Mutation { name, apply, expected };
    let mut catalog = vec![
        // 1. The epoch: the build allows it, the witness only names it.
        m("a branch id of no epoch", |w| w.branch_id = 0xdead_beef, Refused(BranchNotAllowed)),
        m(
            "the branch id of NU6.2 (no Ironwood pool)",
            |w| w.branch_id = 0x5437_f330,
            Refused(BranchNotAllowed),
        ),
    ];
    if net.allows(NU6_3_BRANCH_ID) {
        catalog.extend([
            // An allowed epoch, but not the one of B and T: the history does not open under NU6.3's
            // personalization.
            m(
                "B and T in another epoch than the witness's (NU6.3)",
                |w| w.branch_id = NU6_3_BRANCH_ID,
                Refused(HistoryPath),
            ),
            // A history that does open under NU6.3, with the transaction of another epoch.
            m(
                "B and T re-anchored in NU6.3's history, the transaction NU7's",
                |w| {
                    w.branch_id = NU6_3_BRANCH_ID;
                    reanchor(w);
                },
                Refused(WrongEpoch),
            ),
        ]);
    } else {
        catalog.push(m(
            "the branch id of NU6.3 (not an epoch this build proves)",
            |w| w.branch_id = NU6_3_BRANCH_ID,
            Refused(BranchNotAllowed),
        ));
    }
    catalog.extend([
        // 2. The transaction (D5: Ironwood only).
        m("truncated transaction", |w| w.tx.truncate(100), Refused(Transaction)),
        m("a transparent output (D5)", add_transparent_output, Refused(NotIronwoodOnly)),
        m("no Ironwood bundle", drop_ironwood_bundle, Refused(NotIronwoodOnly)),
        m("action index out of range", |w| w.action_index = 99, Refused(NoSuchAction)),
        m("re-anchored, unchanged (the statement checks no proof of work)", reanchor, Accepted),
        m(
            "a transparent output (D5), re-anchored",
            |w| {
                add_transparent_output(w);
                reanchor(w);
            },
            Refused(NotIronwoodOnly),
        ),
        m(
            "the transaction's own branch id NU6.3's, re-anchored",
            |w| {
                set_transaction_branch_id(w, NU6_3_BRANCH_ID);
                reanchor(w);
            },
            Refused(WrongEpoch),
        ),
        // 3. Block B in T's chain history (ZIP 221).
        m(
            "block header one byte short",
            |w| {
                w.block_header.pop();
            },
            Refused(Header),
        ),
        m("tip header one byte long", |w| w.tip_header.push(0), Refused(Header)),
        m("txid path changed", |w| w.txid_path[0][0] ^= 1, Refused(TxidNotInBlock)),
        m("transaction index changed", |w| w.tx_index ^= 1, Refused(TxidNotInBlock)),
        m(
            "txid path one step short",
            |w| {
                w.txid_path.pop();
            },
            Refused(TxidNotInBlock),
        ),
        m("another Merkle root in B", |w| w.block_header[40] ^= 1, Refused(TxidNotInBlock)),
        m("leaf spans two blocks", |w| with_leaf(w, |l| l.v2.v1.start_height -= 1), Refused(LeafIsNotTheBlock)),
        m(
            "leaf of another block",
            |w| with_leaf(w, |l| l.v2.v1.subtree_commitment = [0; 32]),
            Refused(LeafIsNotTheBlock),
        ),
        m("another block B", |w| w.block_header[1000] ^= 1, Refused(LeafIsNotTheBlock)),
        m(
            "leaf of another epoch (V2 node)",
            |w| {
                let leaf = node(w.branch_id, &w.leaf);
                w.leaf = V2::to_bytes(&leaf.v2);
            },
            Refused(HistoryPath),
        ),
        m("history path on the wrong side", |w| w.mmr_path[0].sibling_is_left ^= true, Refused(HistoryPath)),
        m("history path sibling changed", |w| w.mmr_path[0].sibling[0] ^= 1, Refused(HistoryPath)),
        m(
            "history path one step short",
            |w| {
                w.mmr_path.pop();
            },
            Refused(HistoryPath),
        ),
        m("peak index out of range", |w| w.peak_index = 99, Refused(HistoryPath)),
        m("another peak", |w| w.peak_index -= 1, Refused(HistoryPath)),
        m("an earlier peak changed", |w| w.peaks[0][0] ^= 1, Refused(HistoryNotCommitted)),
        m(
            "a peak missing",
            |w| {
                w.peaks.remove(0);
                w.peak_index -= 1;
            },
            Refused(HistoryNotCommitted),
        ),
        m("another auth data root", |w| w.tip_auth_data_root[0] ^= 1, Refused(HistoryNotCommitted)),
        m("T's hashBlockCommitments changed", |w| w.tip_header[80] ^= 1, Refused(HistoryNotCommitted)),
        // 4. The note.
        m("the other action of the transaction", |w| w.action_index ^= 1, Refused(NotDecryptable)),
        m("value + 1", |w| w.note_value += 1, Refused(NoteCommitmentMismatch)),
        m("value - 1", |w| w.note_value -= 1, Refused(NoteCommitmentMismatch)),
        m("another rseed", |w| w.note_rseed[0] ^= 1, Refused(NotDecryptable)),
        m("recipient not 43 bytes", |w| w.note_recipient.truncate(42), Refused(Note)),
        m(
            "another recipient (a valid receiver)",
            |w| w.note_recipient = OTHER_VAULT_RECEIVER.to_vec(),
            Refused(NotDecryptable),
        ),
        // 5. The memo binds one order (re-encrypted and re-anchored: the rule decides alone).
        m(
            "a memo with another prefix",
            |w| {
                let mut memo = payment_memo(&fixture_order_id());
                memo[3] = b'Q';
                reencrypt(w, &memo);
                reanchor(w);
            },
            Refused(Memo),
        ),
        m(
            "the empty memo (ZIP 302: 0xF6)",
            |w| {
                let mut memo = [0u8; MEMO_LENGTH];
                memo[0] = 0xf6;
                reencrypt(w, &memo);
                reanchor(w);
            },
            Refused(Memo),
        ),
        m(
            "the order id without its prefix",
            |w| {
                let mut memo = [0u8; MEMO_LENGTH];
                memo[..32].copy_from_slice(&fixture_order_id());
                reencrypt(w, &memo);
                reanchor(w);
            },
            Refused(Memo),
        ),
        m(
            "a payment of another order",
            |w| {
                reencrypt(w, &other_order_memo());
                reanchor(w);
            },
            Rebound,
        ),
        m(
            "the same order, text after the binding",
            |w| {
                let mut memo = payment_memo(&fixture_order_id());
                memo[36..42].copy_from_slice(b"thanks");
                reencrypt(w, &memo);
                reanchor(w);
            },
            Accepted,
        ),
        // 6. The receiver commitment and the amount are the journal's to bind.
        m("another salt", |w| w.salt[0] ^= 1, Rebound),
        m(
            "a note of another value",
            |w| {
                w.note_value -= 1;
                reencrypt(w, &payment_memo(&fixture_order_id()));
                reanchor(w);
            },
            Rebound,
        ),
    ]);
    catalog
}
