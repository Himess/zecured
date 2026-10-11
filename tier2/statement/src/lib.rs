//! The Tier 2 deposit statement (docs/TIER2.md): what a Zecured deposit proof proves.
//!
//! Plain Rust, shared by the zkVM guest (which proves that [`verify`] accepts a private
//! [`Witness`]) and the host (which builds the witness and runs the same check natively).
//!
//! For a vault V that the five signers described in a signed `VaultOpened` (D20: id, owner,
//! account, amount, expiry height and the hashes of its address and viewing key), the proof
//! shows:
//!
//! 1. A v6 transaction with only an Ironwood bundle (no transparent, Sapling or Orchard parts:
//!    the deposit rule D5) is in some block B: its txid is under B's header `merkleRoot`.
//! 2. B is in the chain history committed to by a later header T: B's ZIP 221 history-tree
//!    leaf (block hash and height) is in the tree whose root, with T's auth data root, hashes to
//!    T's `hashBlockCommitments`. Only T is public, never B: the deposit's block stays hidden
//!    among the epoch's blocks. B's height is at most the vault's expiry height.
//! 3. One of the transaction's Ironwood actions creates a note of the vault's amount to the
//!    vault's address: the opened note matches the action's note commitment, and the action's
//!    ciphertext decrypts to that note under the ephemeral key, so the vault (which holds the
//!    matching incoming viewing key) can find and spend it.
//! 4. The deposit tag is a PRF of the note commitment under a key derived from the vault's
//!    viewing key: unique per note, and unlinkable to the note by anyone without that key.
//!
//! The journal reveals T, the vault id, its account, the amount, the tag and the EIP-712 struct
//! hash of the vault's `VaultOpened`, never the address or viewing-key hashes themselves: a
//! sender who knows the vault's address (an exchange paying into it, say) cannot find the vault
//! on Base. The Registry checks T against the Zcash header relay (enough work on top), the
//! signers' `VaultOpened` signatures over that struct hash, and that the tag and the vault are
//! unused, and mints exactly as an attestation does.

use orchard::{
    Address, Note,
    note::{NoteVersion, RandomSeed, Rho},
    note_encryption::IronwoodDomain,
    value::NoteValue,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tiny_keccak::{Hasher, Keccak};
use zcash_address::unified::{self, Container, Encoding, Receiver};
use zcash_history::{NodeDataV3, V3, Version};
use zcash_note_encryption::{Domain, try_output_recovery_with_pkd_esk};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, NetworkType};

/// NU7 consensus branch ID: the epoch whose history tree the proof walks.
pub const NU7_BRANCH_ID: u32 = 0x77190AD9;
/// Serialized post-NU5 block header.
pub const HEADER_LENGTH: usize = 1487;

/// One step of a history-tree path from the deposit block's leaf up to its peak.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MmrStep {
    /// The sibling node, serialized (ZIP 221 V3).
    pub sibling: Vec<u8>,
    /// Whether the sibling is the left child.
    pub sibling_is_left: bool,
}

/// The private input of a deposit proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Witness {
    /// The deposit transaction, serialized (v6).
    pub tx: Vec<u8>,
    /// Index of the Ironwood action that creates the deposit note.
    pub action_index: u32,
    /// The deposit note's recipient (43-byte raw Orchard-protocol receiver), value and rseed.
    pub note_recipient: Vec<u8>,
    pub note_value: u64,
    pub note_rseed: [u8; 32],
    /// The transaction's position in block B and the SHA-256d Merkle path from its txid.
    pub tx_index: u32,
    pub txid_path: Vec<[u8; 32]>,
    /// Block B's header.
    pub block_header: Vec<u8>,
    /// Block B's history-tree leaf (ZIP 221 V3, serialized), its path to a peak, all peaks
    /// (oldest first) and which peak the path reaches.
    pub leaf: Vec<u8>,
    pub mmr_path: Vec<MmrStep>,
    pub peaks: Vec<Vec<u8>>,
    pub peak_index: u32,
    /// The later header T and its block's auth data root (ZIP 244).
    pub tip_header: Vec<u8>,
    pub tip_auth_data_root: [u8; 32],
    /// The vault as the signers described it in `VaultOpened`: id, owner key, smart account,
    /// unified address, unified full viewing key (exact strings) and expiry height. Its amount is
    /// the note's value.
    pub vault_id: [u8; 32],
    pub owner: [u8; 20],
    pub account: [u8; 20],
    pub vault_address: String,
    pub vault_ufvk: String,
    pub expiry_height: u32,
}

/// The public output of a deposit proof (the zkVM journal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// SHA-256d of header T, internal byte order (as the relay stores it).
    pub tip_hash: [u8; 32],
    pub vault_id: [u8; 32],
    pub account: [u8; 20],
    /// The deposited amount, in zatoshi.
    pub amount: u64,
    /// Deposit tag: unique per note, unlinkable without the vault's viewing key.
    pub tag: [u8; 32],
    /// EIP-712 struct hash of the vault's `VaultOpened` (the Registry verifies the signers'
    /// signatures over it under its own domain).
    pub vault_opened_hash: [u8; 32],
}

impl Journal {
    /// ABI encoding read by the Registry: `abi.encode(bytes32 tipHash, bytes32 vaultId,
    /// address account, uint64 amount, bytes32 depositTag, bytes32 vaultOpenedHash)`.
    pub fn abi_encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 * 32);
        out.extend_from_slice(&self.tip_hash);
        out.extend_from_slice(&self.vault_id);
        out.extend_from_slice(&address_word(&self.account));
        out.extend_from_slice(&word(u128::from(self.amount)));
        out.extend_from_slice(&self.tag);
        out.extend_from_slice(&self.vault_opened_hash);
        out
    }
}

fn word(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn address_word(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// EIP-712 type of `VaultOpened` (docs/INTERFACES.md § 2.5).
pub const VAULT_OPENED_TYPE: &str = "VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 ufvkHash)";

/// EIP-712 struct hash of a `VaultOpened`; `address` and `ufvk` are the exact strings whose
/// keccak256 the signers signed.
pub fn vault_opened_hash(
    vault_id: &[u8; 32],
    owner: &[u8; 20],
    account: &[u8; 20],
    amount: u64,
    expiry_height: u32,
    address: &str,
    ufvk: &str,
) -> [u8; 32] {
    let mut enc = Vec::with_capacity(8 * 32);
    enc.extend_from_slice(&keccak256(VAULT_OPENED_TYPE.as_bytes()));
    enc.extend_from_slice(vault_id);
    enc.extend_from_slice(&address_word(owner));
    enc.extend_from_slice(&address_word(account));
    enc.extend_from_slice(&word(u128::from(amount)));
    enc.extend_from_slice(&word(u128::from(expiry_height)));
    enc.extend_from_slice(&keccak256(address.as_bytes()));
    enc.extend_from_slice(&keccak256(ufvk.as_bytes()));
    keccak256(&enc)
}

/// Why a witness does not prove a deposit.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Transaction,
    NotIronwoodOnly,
    NoSuchAction,
    TxidNotInBlock,
    Header,
    LeafIsNotTheBlock,
    BlockAfterExpiry,
    HistoryPath,
    HistoryNotCommitted,
    Note,
    NoteCommitmentMismatch,
    NotDecryptable,
    Address,
    WrongRecipient,
}

pub fn sha256d(data: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(data)).into()
}

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

fn blake2b_256(personal: &[u8; 16], key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut params = blake2b_simd::Params::new();
    params.hash_length(32).personal(personal);
    if !key.is_empty() {
        params.key(key);
    }
    let mut state = params.to_state();
    for p in parts {
        state.update(p);
    }
    state.finalize().as_bytes().try_into().expect("32 bytes")
}

/// The deposit tag of note commitment `cmx` in the vault with viewing key `ufvk`.
pub fn deposit_tag(ufvk: &str, cmx: &[u8; 32]) -> [u8; 32] {
    let key = blake2b_256(b"Zecured_TagKey__", &[], &[ufvk.as_bytes()]);
    blake2b_256(b"Zecured_DepTag__", &key, &[cmx])
}

/// The root of a SHA-256d Merkle path (Bitcoin-style tree, as Zcash's `hashMerkleRoot`).
pub fn txid_path_root(txid: [u8; 32], index: u32, path: &[[u8; 32]]) -> [u8; 32] {
    let mut node = txid;
    let mut i = index;
    for sibling in path {
        node = if i & 1 == 0 {
            sha256d(&[node, *sibling].concat())
        } else {
            sha256d(&[*sibling, node].concat())
        };
        i >>= 1;
    }
    node
}

/// `hashBlockCommitments` (ZIP 244).
pub fn block_commitments(history_root: &[u8; 32], auth_data_root: &[u8; 32]) -> [u8; 32] {
    blake2b_256(b"ZcashBlockCommit", &[], &[history_root, auth_data_root, &[0u8; 32]])
}

fn node(bytes: &[u8]) -> Result<NodeDataV3, Error> {
    <V3 as Version>::from_bytes(NU7_BRANCH_ID, bytes).map_err(|_| Error::HistoryPath)
}

/// Checks the witness and returns the public output, or why it fails.
pub fn verify(w: &Witness) -> Result<Journal, Error> {
    // 1. The transaction: v6, Ironwood only (D5), and its txid under block B's merkleRoot.
    let branch = BranchId::try_from(NU7_BRANCH_ID).map_err(|_| Error::Transaction)?;
    let tx = Transaction::read(&w.tx[..], branch).map_err(|_| Error::Transaction)?;
    if tx.transparent_bundle().is_some() || tx.sapling_bundle().is_some() || tx.orchard_bundle().is_some() {
        return Err(Error::NotIronwoodOnly);
    }
    let bundle = tx.ironwood_bundle().ok_or(Error::NotIronwoodOnly)?;
    let action = bundle.actions().get(w.action_index as usize).ok_or(Error::NoSuchAction)?;
    if w.block_header.len() != HEADER_LENGTH || w.tip_header.len() != HEADER_LENGTH {
        return Err(Error::Header);
    }
    let merkle_root: [u8; 32] = w.block_header[36..68].try_into().expect("32 bytes");
    if txid_path_root(*tx.txid().as_ref(), w.tx_index, &w.txid_path) != merkle_root {
        return Err(Error::TxidNotInBlock);
    }

    // 2. Block B is a leaf of the history committed to by header T.
    let leaf = node(&w.leaf)?;
    let v1 = &leaf.v2.v1;
    if v1.subtree_commitment != sha256d(&w.block_header) || v1.start_height != v1.end_height {
        return Err(Error::LeafIsNotTheBlock);
    }
    if v1.end_height > u64::from(w.expiry_height) {
        return Err(Error::BlockAfterExpiry);
    }
    let mut acc = leaf;
    for step in &w.mmr_path {
        let sibling = node(&step.sibling)?;
        acc = if step.sibling_is_left {
            V3::combine(&sibling, &acc)
        } else {
            V3::combine(&acc, &sibling)
        };
    }
    let peak = w.peaks.get(w.peak_index as usize).ok_or(Error::HistoryPath)?;
    if V3::to_bytes(&acc) != *peak {
        return Err(Error::HistoryPath);
    }
    let mut peaks = w.peaks.iter();
    let mut root = node(peaks.next().ok_or(Error::HistoryPath)?)?;
    for p in peaks {
        root = V3::combine(&root, &node(p)?);
    }
    let tip_commitments: [u8; 32] = w.tip_header[68..100].try_into().expect("32 bytes");
    if block_commitments(&V3::hash(&root), &w.tip_auth_data_root) != tip_commitments {
        return Err(Error::HistoryNotCommitted);
    }

    // 3. The note. Recovering the action's ciphertext with the ephemeral secret derived from the
    //    witness note (and its pk_d) checks that the plaintext is a note whose commitment is the
    //    action's cmx and whose ephemeral key is the action's (ZIP 212): the action creates that
    //    note, and an incoming viewing key for its recipient decrypts it. The recovered note is
    //    then compared field by field with the witness note (`Note`'s `PartialEq` would compute
    //    both commitments again; each costs ~4.8 M zkVM cycles).
    let recipient: [u8; 43] = w.note_recipient.clone().try_into().map_err(|_| Error::Note)?;
    let recipient = Option::<Address>::from(Address::from_raw_address_bytes(&recipient)).ok_or(Error::Note)?;
    // An action's output note has rho = the nullifier spent by the same action.
    let rho = Option::<Rho>::from(Rho::from_bytes(&action.nullifier().to_bytes())).ok_or(Error::Note)?;
    let rseed = Option::<RandomSeed>::from(RandomSeed::from_bytes(w.note_rseed, &rho)).ok_or(Error::Note)?;
    let note = Option::<Note>::from(Note::from_parts(
        recipient,
        NoteValue::from_raw(w.note_value),
        rho,
        rseed,
        NoteVersion::V3,
    ))
    .ok_or(Error::Note)?;
    let domain = IronwoodDomain::for_action(action);
    let esk = IronwoodDomain::derive_esk(&note).ok_or(Error::NotDecryptable)?;
    let pk_d = IronwoodDomain::get_pk_d(&note);
    let (recovered, _, _) =
        try_output_recovery_with_pkd_esk(&domain, pk_d, esk, action).ok_or(Error::NotDecryptable)?;
    if recovered.recipient().to_raw_address_bytes() != note.recipient().to_raw_address_bytes()
        || recovered.value() != note.value()
        || recovered.rseed().as_bytes() != note.rseed().as_bytes()
        || recovered.rho().to_bytes() != note.rho().to_bytes()
    {
        return Err(Error::NoteCommitmentMismatch);
    }

    // 4. The recipient is the vault's (Ironwood-only, testnet) address.
    let (network, _, ua) = unified::Address::decode(&w.vault_address).map_err(|_| Error::Address)?;
    if network != NetworkType::Test {
        return Err(Error::Address);
    }
    match ua.items().as_slice() {
        [Receiver::Orchard(r)] if *r == note.recipient().to_raw_address_bytes() => {}
        _ => return Err(Error::WrongRecipient),
    }

    // The deposit's note commitment, as recovery checked it.
    let cmx = action.cmx().to_bytes();
    Ok(Journal {
        tip_hash: sha256d(&w.tip_header),
        vault_id: w.vault_id,
        account: w.account,
        amount: w.note_value,
        tag: deposit_tag(&w.vault_ufvk, &cmx),
        vault_opened_hash: vault_opened_hash(
            &w.vault_id,
            &w.owner,
            &w.account,
            w.note_value,
            w.expiry_height,
            &w.vault_address,
            &w.vault_ufvk,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden `VaultOpened` vector of signer/crates/base-client (also asserted by the app):
    /// its struct hash, which with the Registry domain of that vector gives the signed digest
    /// 0x1932a4db…dbf4 (checked with `cast`).
    #[test]
    fn vault_opened_hash_matches_the_golden_vector() {
        let address = "utest1yv0hq77h4th2c882kdmcgt47xrm5zmhkfnfpw6dcack8e6kgfmg5new7vxed0trhkc67cmf2tjea8pnd09key4v85kmqdnz33yuas40f";
        let ufvk = "uviewtest1rl58wrlhq2eqws08m2jxqg0gjpn2yd7me5f9cfkwmf4axnq0vjvjgvjs5t70tsny62rxklz3ha8t5cr8k30jltjmjr0xxsdhpqeqpf55w3dx0mracz99uvrvyf4hpsfxs2nc5jykraqvnt22zpkdce6tcj85lkty9lcpduhl72vz80l8ygn0q3sxn04w8";
        let h = vault_opened_hash(&[0x55; 32], &[0x88; 20], &[0x33; 20], 1_000_000, 4_486_288, address, ufvk);
        let expected: [u8; 32] = [
            0xf0, 0x12, 0x9c, 0x9f, 0x65, 0x2e, 0xd0, 0x78, 0xd8, 0x5e, 0x45, 0x62, 0xac, 0xca, 0xf9, 0x3d, 0x8d, 0x5d,
            0x81, 0x82, 0x72, 0xa9, 0x2a, 0x9d, 0xba, 0x93, 0x1b, 0xbc, 0x37, 0x2e, 0x63, 0x02,
        ];
        assert_eq!(h, expected);
    }

    #[test]
    fn journal_abi_layout() {
        let j = Journal {
            tip_hash: [1; 32],
            vault_id: [2; 32],
            account: [3; 20],
            amount: 0x0102_0304,
            tag: [4; 32],
            vault_opened_hash: [5; 32],
        };
        let e = j.abi_encode();
        assert_eq!(e.len(), 192);
        assert_eq!(&e[0..32], &[1; 32]);
        assert_eq!(&e[32..64], &[2; 32]);
        assert_eq!(&e[64..76], &[0; 12]);
        assert_eq!(&e[76..96], &[3; 20]);
        assert_eq!(&e[96..124], &[0; 28]);
        assert_eq!(&e[124..128], &[1, 2, 3, 4]);
        assert_eq!(&e[128..160], &[4; 32]);
        assert_eq!(&e[160..192], &[5; 32]);
    }

    #[test]
    fn tag_is_keyed_by_the_viewing_key() {
        let cmx = [7u8; 32];
        assert_eq!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1a", &cmx));
        assert_ne!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1b", &cmx));
        assert_ne!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1a", &[8u8; 32]));
    }
}
