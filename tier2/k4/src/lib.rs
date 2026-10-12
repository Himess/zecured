//! K4: single changes to a real deposit witness, each with the exact error the deposit
//! statement must report. The statement's tests run them natively; `zecured-tier2-host parity`
//! runs them in the RISC Zero executor, where the guest must panic with the same error. Changes
//! the statement accepts on purpose (authorizing data, header bytes outside the commitments)
//! are pinned in the statement's own tests, with the reason.

use orchard::keys::{FullViewingKey, Scope};
use zcash_address::unified::{self, Container, Encoding, Fvk, Receiver, Ufvk};
use zcash_history::{V2, V3, Version};
use zcash_protocol::consensus::NetworkType;
use zecured_deposit_statement::{Error, NU7_BRANCH_ID, Witness, vault_receiver};

/// One change and the error it must cause.
pub struct Mutation {
    pub name: &'static str,
    pub apply: fn(&mut Witness),
    pub expected: Error,
}

/// The vault's Orchard-protocol full viewing key, from its unified viewing key.
pub fn vault_fvk(w: &Witness) -> FullViewingKey {
    let (_, _, ufvk) = Ufvk::decode(&w.vault_ufvk).expect("a unified viewing key");
    let bytes = ufvk
        .items()
        .into_iter()
        .find_map(|item| match item {
            Fvk::Orchard(b) => Some(b),
            _ => None,
        })
        .expect("an Orchard-protocol viewing key");
    FullViewingKey::from_bytes(&bytes).expect("a valid viewing key")
}

/// A revision-0 unified address with these receivers.
pub fn ua(network: NetworkType, receivers: Vec<Receiver>) -> String {
    let items = receivers.into_iter().map(unified::Uitem::Data).collect();
    unified::Address::try_from_items(unified::Revision::R0, items)
        .expect("a valid address")
        .encode(&network)
}

fn with_leaf(w: &mut Witness, change: impl FnOnce(&mut zcash_history::NodeDataV3)) {
    let mut leaf = V3::from_bytes(NU7_BRANCH_ID, &w.leaf).expect("a V3 leaf");
    change(&mut leaf);
    w.leaf = V3::to_bytes(&leaf);
}

/// Block B's height, from its history-tree leaf.
pub fn block_height(w: &Witness) -> u32 {
    let leaf = V3::from_bytes(NU7_BRANCH_ID, &w.leaf).expect("a V3 leaf");
    u32::try_from(leaf.v2.v1.end_height).expect("a block height")
}

/// The catalog, in the statement's order of checks. Every entry expects an error.
pub fn mutations() -> Vec<Mutation> {
    use Error::*;
    let m = |name, apply: fn(&mut Witness), expected| Mutation { name, apply, expected };
    vec![
        // 1. The transaction (D5, O5).
        m("truncated transaction", |w| w.tx.truncate(100), Transaction),
        m(
            "a transparent output",
            |w| {
                // v6: 20 header bytes, then the transparent input and output counts (both 0).
                assert_eq!(&w.tx[20..22], &[0, 0]);
                let mut tx = w.tx[..21].to_vec();
                tx.push(1);
                tx.extend_from_slice(&1_000u64.to_le_bytes());
                tx.push(0);
                tx.extend_from_slice(&w.tx[22..]);
                w.tx = tx;
            },
            NotIronwoodOnly,
        ),
        m("action index out of range", |w| w.action_index = 99, NoSuchAction),
        // 2. Block B in T's chain history (ZIP 221).
        m(
            "block header one byte short",
            |w| {
                w.block_header.pop();
            },
            Header,
        ),
        m("tip header one byte long", |w| w.tip_header.push(0), Header),
        m("txid path changed", |w| w.txid_path[0][0] ^= 1, TxidNotInBlock),
        m("transaction index changed", |w| w.tx_index ^= 1, TxidNotInBlock),
        m(
            "txid path one step short",
            |w| {
                w.txid_path.pop();
            },
            TxidNotInBlock,
        ),
        m("another Merkle root in B", |w| w.block_header[40] ^= 1, TxidNotInBlock),
        m("leaf spans two blocks", |w| with_leaf(w, |l| l.v2.v1.start_height -= 1), LeafIsNotTheBlock),
        m(
            "leaf of another block",
            |w| with_leaf(w, |l| l.v2.v1.subtree_commitment = [0; 32]),
            LeafIsNotTheBlock,
        ),
        m("another block B", |w| w.block_header[1000] ^= 1, LeafIsNotTheBlock),
        m(
            "leaf of another epoch (V2 node)",
            |w| {
                let leaf = V3::from_bytes(NU7_BRANCH_ID, &w.leaf).expect("a V3 leaf");
                w.leaf = V2::to_bytes(&leaf.v2);
            },
            HistoryPath,
        ),
        m("history path on the wrong side", |w| w.mmr_path[0].sibling_is_left ^= true, HistoryPath),
        m("history path sibling changed", |w| w.mmr_path[0].sibling[0] ^= 1, HistoryPath),
        m(
            "history path one step short",
            |w| {
                w.mmr_path.pop();
            },
            HistoryPath,
        ),
        m("peak index out of range", |w| w.peak_index = 99, HistoryPath),
        m("another peak", |w| w.peak_index -= 1, HistoryPath),
        m("an earlier peak changed", |w| w.peaks[0][0] ^= 1, HistoryNotCommitted),
        m(
            "a peak missing",
            |w| {
                w.peaks.remove(0);
                w.peak_index -= 1;
            },
            HistoryNotCommitted,
        ),
        m("another auth data root", |w| w.tip_auth_data_root[0] ^= 1, HistoryNotCommitted),
        m("T's hashBlockCommitments changed", |w| w.tip_header[80] ^= 1, HistoryNotCommitted),
        m("B after the vault's expiry", |w| w.expiry_height = block_height(w) - 1, BlockAfterExpiry),
        // 3. The note.
        m("the other action of the transaction", |w| w.action_index = 0, NotDecryptable),
        m("value + 1", |w| w.note_value += 1, NoteCommitmentMismatch),
        m("value - 1", |w| w.note_value -= 1, NoteCommitmentMismatch),
        m("another rseed", |w| w.note_rseed[0] ^= 1, NotDecryptable),
        m("recipient not 43 bytes", |w| w.note_recipient.truncate(42), Note),
        m(
            "recipient: another address of the vault",
            |w| w.note_recipient = vault_fvk(w).address_at(1u32, Scope::External).to_raw_address_bytes().to_vec(),
            NotDecryptable,
        ),
        // 4. The vault's receiver (K2-c).
        m(
            "vault address with two receivers",
            |w| {
                let r = vault_receiver(&w.vault_address).expect("the fixture's address");
                w.vault_address = ua(NetworkType::Test, vec![Receiver::Orchard(r), Receiver::P2pkh([1; 20])]);
            },
            WrongRecipient,
        ),
        m(
            "a Mainnet vault address",
            |w| {
                let r = vault_receiver(&w.vault_address).expect("the fixture's address");
                w.vault_address = ua(NetworkType::Main, vec![Receiver::Orchard(r)]);
            },
            Address,
        ),
        m("not a unified address", |w| w.vault_address = "utest1nope".into(), Address),
        m(
            "the vault's address at another diversifier",
            |w| {
                let other = vault_fvk(w).address_at(1u32, Scope::External).to_raw_address_bytes();
                w.vault_address = ua(NetworkType::Test, vec![Receiver::Orchard(other)]);
            },
            WrongRecipient,
        ),
    ]
}
