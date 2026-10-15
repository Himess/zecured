//! K4: single changes to a real deposit witness, each with the outcome the deposit statement must
//! give. The statement's tests run them natively; `zecured-tier2-host parity` runs them in the
//! RISC Zero executor, where the guest must panic with the same error or commit the same journal.
//! Changes the statement accepts on purpose (authorizing data, header bytes outside the
//! commitments) are pinned in the statement's own tests, with the reason.
//!
//! The catalog depends on the network the statement is built for ([`Network`]): the fixture is a
//! Testnet deposit, so for Mainnet the base witness is the same deposit with the vault's address
//! encoded for Mainnet ([`for_network`]), and the epoch changes differ (the Testnet build allows
//! NU7 only, the Mainnet build NU6.3 and NU7).
//!
//! A change to the transaction itself changes its txid, which is then in no real block. Such
//! changes are also run [`reanchor`]ed: the changed transaction alone in a synthetic block, under a
//! synthetic header that commits to it, so the rule the change is about decides alone (Refund mode
//! then proves a transaction D5 refuses). The statement checks no proof of work; the relay on Base
//! does, and would never accept such a header.

use zcash_address::unified::{self, Container, Encoding, Receiver};
use zcash_history::{V2, V3, Version};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, NetworkType};
use zecured_deposit_statement::{Error, NU6_3_BRANCH_ID, Network, Witness, block_commitments, sha256d};

/// The fixture vault's (vault 9, TEST-ONLY) external address at diversifier index 1: another
/// address of the same vault, which a deposit must not pay. A witness does not carry the vault's
/// viewing key, so it is pinned here; the statement's tests derive it again from the vault's
/// TEST-ONLY viewing key.
pub const OTHER_VAULT_RECEIVER: [u8; 43] = [
    247, 123, 229, 178, 16, 106, 31, 253, 51, 220, 255, 109, 184, 23, 238, 148, 47, 156, 236, 78, 173, 36, 159, 13,
    122, 143, 85, 153, 157, 123, 131, 241, 109, 38, 58, 14, 65, 118, 13, 237, 60, 163, 1,
];

/// What a change must lead to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// The statement refuses the witness with this error (the guest panics with it).
    Refused(Error),
    /// The statement accepts the witness, but its journal names a `VaultOpened` the vault's
    /// signers never signed (another tag key): Base refuses the proof.
    Unsigned,
    /// The statement accepts the witness: the rule the change breaks does not apply in its mode.
    Accepted,
}

/// One change and the outcome it must have.
pub struct Mutation {
    pub name: &'static str,
    pub apply: fn(&mut Witness),
    pub expected: Expected,
}

/// A revision-0 unified address with these receivers.
pub fn ua(network: NetworkType, receivers: Vec<Receiver>) -> String {
    let items = receivers.into_iter().map(unified::Uitem::Data).collect();
    unified::Address::try_from_items(unified::Revision::R0, items)
        .expect("a valid address")
        .encode(&network)
}

/// The network and the Orchard-protocol receiver of the witness's (fixture) vault address.
fn vault_address(w: &Witness) -> (NetworkType, [u8; 43]) {
    let (network, _, address) = unified::Address::decode(&w.vault_address).expect("the fixture's address");
    let receiver = address
        .items()
        .into_iter()
        .find_map(|r| match r {
            Receiver::Orchard(r) => Some(r),
            _ => None,
        })
        .expect("an Orchard-protocol receiver");
    (network, receiver)
}

/// The fixture's witness for network `net`: the same deposit, with the vault's address encoded
/// for that network (the same receiver). Nothing else the statement checks depends on the
/// network, so on Mainnet the fixture proves its note under a `VaultOpened` naming the Mainnet
/// encoding of the address.
pub fn for_network(w: &mut Witness, net: &Network) {
    let (_, receiver) = vault_address(w);
    w.vault_address = ua(net.address_network, vec![Receiver::Orchard(receiver)]);
}

fn with_leaf(w: &mut Witness, change: impl FnOnce(&mut zcash_history::NodeDataV3)) {
    let mut leaf = V3::from_bytes(w.branch_id, &w.leaf).expect("a V3 leaf");
    change(&mut leaf);
    w.leaf = V3::to_bytes(&leaf);
}

/// Block B's height, from its history-tree leaf.
pub fn block_height(w: &Witness) -> u32 {
    let leaf = V3::from_bytes(w.branch_id, &w.leaf).expect("a V3 leaf");
    u32::try_from(leaf.v2.v1.end_height).expect("a block height")
}

/// The fixture's transaction with one transparent output added (D5 forbids it; its txid is no
/// longer the one in B).
pub fn add_transparent_output(w: &mut Witness) {
    // v6: 20 header bytes, then the transparent input and output counts (both 0).
    assert_eq!(&w.tx[20..22], &[0, 0]);
    let mut tx = w.tx[..21].to_vec();
    tx.push(1);
    tx.extend_from_slice(&1_000u64.to_le_bytes());
    tx.push(0);
    tx.extend_from_slice(&w.tx[22..]);
    w.tx = tx;
}

/// The fixture's transaction with its own consensus branch id set to `branch_id` (NU6.3 and NU7
/// share the v6 format and the Ironwood bundle version, so it still parses; its txid changes).
pub fn set_transaction_branch_id(w: &mut Witness, branch_id: u32) {
    // v6: the header word, the version group id, then the consensus branch id (little-endian).
    assert_eq!(BranchId::try_from(u32::from_le_bytes(w.tx[8..12].try_into().expect("4 bytes"))), Ok(BranchId::Nu7));
    w.tx[8..12].copy_from_slice(&branch_id.to_le_bytes());
}

/// The fixture's transaction without its Ironwood bundle (an empty bundle is one zero byte).
fn drop_ironwood_bundle(w: &mut Witness) {
    // v6: 20 header bytes; no transparent inputs or outputs, no Sapling spends or outputs, no
    // Orchard-pool actions; then the Ironwood bundle.
    assert_eq!(&w.tx[20..25], &[0, 0, 0, 0, 0]);
    w.tx.truncate(25);
    w.tx.push(0);
}

/// Puts the witness's transaction, as it now is, alone in a synthetic block B' (whose Merkle root
/// is its txid) at B's height, makes B''s history-tree leaf the only peak of the epoch
/// `w.branch_id` names, and sets a synthetic T''s `hashBlockCommitments` to that history: every
/// inclusion check passes again, for any transaction. The statement checks no proof of work, so it
/// accepts this anchor; the relay on Base never would (T''s hash is in no chain).
pub fn reanchor(w: &mut Witness) {
    let tx = Transaction::read(&w.tx[..], BranchId::Nu7).expect("a v6 transaction");
    let txid: [u8; 32] = *tx.txid().as_ref();
    w.block_header[36..68].copy_from_slice(&txid);
    w.tx_index = 0;
    w.txid_path.clear();
    let mut leaf = V3::from_bytes(w.branch_id, &w.leaf).expect("a V3 leaf");
    leaf.v2.v1.subtree_commitment = sha256d(&w.block_header);
    w.leaf = V3::to_bytes(&leaf);
    w.mmr_path.clear();
    w.peaks = vec![w.leaf.clone()];
    w.peak_index = 0;
    let commitments = block_commitments(&V3::hash(&leaf), &w.tip_auth_data_root);
    w.tip_header[68..100].copy_from_slice(&commitments);
}

const ACTIVATE: u8 = 0;
const REFUND: u8 = 1;

/// The catalog for the statement built for `net`, in the statement's order of checks.
pub fn mutations(net: &Network) -> Vec<Mutation> {
    use Error::*;
    use Expected::*;
    let m = |name, apply: fn(&mut Witness), expected| Mutation { name, apply, expected };
    let mut catalog = vec![
        // 0. The mode.
        m("mode 2 (neither Activate nor Refund)", |w| w.mode = 2, Refused(Mode)),
        // 1. The epoch: the build allows it, the witness only names it.
        m("a branch id of no epoch", |w| w.branch_id = 0xdead_beef, Refused(BranchNotAllowed)),
        m(
            "the branch id of NU6.2 (no Ironwood pool)",
            |w| w.branch_id = 0x5437_f330,
            Refused(BranchNotAllowed),
        ),
    ];
    if net.allows(NU6_3_BRANCH_ID) {
        // An allowed epoch, but not the one of B and T: the leaf's path does not hash to T's
        // peaks under NU6.3's personalization (B and T must be of the witness's epoch).
        catalog.extend([
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
        // NU6.3 is a Testnet epoch too (Ironwood since 4,134,000), but the Testnet build proves
        // the NU7 epoch only.
        catalog.push(m(
            "the branch id of NU6.3 (not an epoch this build proves)",
            |w| w.branch_id = NU6_3_BRANCH_ID,
            Refused(BranchNotAllowed),
        ));
    }
    catalog.extend([
        // 2. The transaction (D5 in Activate mode, O5).
        m("truncated transaction", |w| w.tx.truncate(100), Refused(Transaction)),
        m("Activate: a transparent output (D5)", add_transparent_output, Refused(NotIronwoodOnly)),
        m(
            "Refund: D5 does not refuse a transparent output (the changed txid is not in B)",
            |w| {
                w.mode = REFUND;
                add_transparent_output(w);
            },
            Refused(TxidNotInBlock),
        ),
        m("Activate: no Ironwood bundle", drop_ironwood_bundle, Refused(NotIronwoodOnly)),
        m(
            "Refund: no Ironwood bundle",
            |w| {
                w.mode = REFUND;
                drop_ironwood_bundle(w);
            },
            Refused(NoIronwoodBundle),
        ),
        m("action index out of range", |w| w.action_index = 99, Refused(NoSuchAction)),
        // 2b. The same changes in a block of their own (`reanchor`): the rule decides alone.
        m("re-anchored, unchanged (the statement checks no proof of work)", reanchor, Accepted),
        m(
            "Activate: a transparent output (D5), re-anchored",
            |w| {
                add_transparent_output(w);
                reanchor(w);
            },
            Refused(NotIronwoodOnly),
        ),
        m(
            "Refund: a transparent output (D5), re-anchored, proves",
            |w| {
                w.mode = REFUND;
                add_transparent_output(w);
                reanchor(w);
            },
            Accepted,
        ),
        m(
            "Activate: the transaction's own branch id NU6.3's, re-anchored",
            |w| {
                set_transaction_branch_id(w, NU6_3_BRANCH_ID);
                reanchor(w);
            },
            Refused(WrongEpoch),
        ),
        m(
            "Refund: the transaction's own branch id NU6.3's, re-anchored",
            |w| {
                w.mode = REFUND;
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
                let leaf = V3::from_bytes(w.branch_id, &w.leaf).expect("a V3 leaf");
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
        m(
            "Activate: B after the vault's expiry",
            |w| w.expiry_height = block_height(w) - 1,
            Refused(BlockAfterExpiry),
        ),
        m(
            "Refund: B after the vault's expiry passes",
            |w| {
                w.mode = REFUND;
                w.expiry_height = block_height(w) - 1;
            },
            Accepted,
        ),
        // 4. The note.
        m("the other action of the transaction", |w| w.action_index = 0, Refused(NotDecryptable)),
        m("value + 1", |w| w.note_value += 1, Refused(NoteCommitmentMismatch)),
        m("value - 1", |w| w.note_value -= 1, Refused(NoteCommitmentMismatch)),
        m("another rseed", |w| w.note_rseed[0] ^= 1, Refused(NotDecryptable)),
        m("recipient not 43 bytes", |w| w.note_recipient.truncate(42), Refused(Note)),
        m(
            "recipient: another address of the vault",
            |w| w.note_recipient = OTHER_VAULT_RECEIVER.to_vec(),
            Refused(NotDecryptable),
        ),
        // 5. The vault's receiver (K2-c), on the build's network.
        m(
            "vault address with two receivers",
            |w| {
                let (network, r) = vault_address(w);
                w.vault_address = ua(network, vec![Receiver::Orchard(r), Receiver::P2pkh([1; 20])]);
            },
            Refused(WrongRecipient),
        ),
        m(
            "the vault's address encoded for the other network",
            |w| {
                let (network, r) = vault_address(w);
                let other = if network == NetworkType::Main {
                    NetworkType::Test
                } else {
                    NetworkType::Main
                };
                w.vault_address = ua(other, vec![Receiver::Orchard(r)]);
            },
            Refused(Address),
        ),
        m("not a unified address", |w| w.vault_address = "utest1nope".into(), Refused(Address)),
        m(
            "the vault's address at another diversifier",
            |w| {
                let (network, _) = vault_address(w);
                w.vault_address = ua(network, vec![Receiver::Orchard(OTHER_VAULT_RECEIVER)]);
            },
            Refused(WrongRecipient),
        ),
        // 6. The vault's amount.
        m("Activate: a vault amount other than the note's", |w| w.vault_amount += 1, Refused(AmountMismatch)),
        m(
            "Refund: a note of another value than the vault's amount passes",
            |w| {
                w.mode = REFUND;
                w.vault_amount += 1;
            },
            Accepted,
        ),
        // 7. The tag key, bound to the vault by VaultOpened (design review, Claude Web § 2).
        m(
            "Activate: another tag key",
            |w| {
                w.mode = ACTIVATE;
                w.tag_key[0] ^= 1;
            },
            Unsigned,
        ),
        m(
            "Refund: another tag key",
            |w| {
                w.mode = REFUND;
                w.tag_key[0] ^= 1;
            },
            Unsigned,
        ),
    ]);
    catalog
}
