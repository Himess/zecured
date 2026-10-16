//! The Tier 2 deposit statement (docs/TIER2.md): what a Zecured deposit proof proves.
//!
//! Plain Rust, shared by the zkVM guests (which prove that [`verify`] accepts a private
//! [`Witness`]) and the hosts (which build witnesses and run the same check natively).
//!
//! **The network is a build parameter, never a witness field.** A build proves deposits on one
//! Zcash network ([`NETWORK`]): Testnet by default, Mainnet with the cargo feature `mainnet`. The
//! network fixes how the vault's address is encoded and which epochs a proof may walk (their
//! consensus branch ids). Each network has its own guests and image ids, so a Testnet note can
//! never be proven to a deployment that pins the Mainnet guest, and the reverse.
//!
//! For a vault V that the five signers described in a signed `VaultOpened` (D20: id, owner,
//! account, amount, expiry height, the hash of its address and the hash of its tag key K), an
//! **Activate** proof shows:
//!
//! 1. A v6 transaction of an allowed epoch with only an Ironwood bundle, outputs enabled (no
//!    transparent, Sapling or Orchard bundle, even an empty one: the deposit rule D5, applied by
//!    the signers' P1 through the same [`check_deposit_transaction`]) is in some block B: its
//!    txid is under B's header `merkleRoot`.
//! 2. B is in the chain history committed to by a later header T of the same epoch: B's ZIP 221
//!    history-tree leaf (block hash and height) is in the tree whose root, with T's auth data
//!    root, hashes to T's `hashBlockCommitments`. Every node hash of that tree is personalized
//!    with the epoch's branch id, and the transaction carries the same branch id. Only T is
//!    public, never B: the deposit's block stays hidden among the epoch's blocks. B's height is at
//!    most the vault's expiry height.
//! 3. One of the transaction's Ironwood actions creates a V3 (ZIP 2005) note of the vault's
//!    amount to the vault's published receiver (P1 applies the same [`vault_receiver`]): the
//!    opened note matches the action's note commitment, and the action's ciphertext decrypts to
//!    that note under the ephemeral key, so the vault (which holds the matching incoming viewing
//!    key) can find and spend it.
//! 4. The deposit tag is a PRF of the note commitment under the vault's tag key K: unique per
//!    note, and unlinkable to the note by anyone without K.
//!
//! A **Refund** proof (T-02, T-09) lands a note that cannot back the vault (another amount, a late
//! payment, a second deposit, a transaction with transparent or other parts) as a refund claim
//! instead of leaving it to an off-chain refund: any v6 transaction of the epoch with an Ironwood
//! bundle (no D5), a note of any value (the Intake enforces a minimum), mined at any height; the
//! note must still pay the vault's published receiver with a matching opening. The tag is the same
//! in both modes, so a note lands on Base once, as collateral or as a refund, never both.
//!
//! **The tag key is bound to the vault** (design review). The witness carries K,
//! not the vault's viewing key: the prover never sees the UFVK (review H-10). The signers sign
//! `tagKeyHash = keccak256(K)` in `VaultOpened`, and the journal's `vaultOpenedHash` is computed
//! with the witness's K, so the five signatures Base checks vouch for the very K the tag was
//! derived with. A proof under any other K carries a `VaultOpened` nobody signed: without that
//! binding, one note could land twice under two tags (once as collateral under a made-up K, once
//! as a refund under the real one), leaving zZEC unbacked.
//!
//! The journal ([`Journal::abi_encode`], v2) reveals the mode, the epoch's branch id, T, the vault
//! id, its account, the note's value, the tag and the EIP-712 struct hash of the vault's
//! `VaultOpened`, never the address or tag-key hashes themselves: a sender who knows the vault's
//! address (an exchange paying into it, say) cannot find the vault on Base. The Intake checks T
//! against the Zcash header relay (enough work on top), the signers' `VaultOpened` signatures over
//! that struct hash, the branch id, and that the tag is unused.

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
use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::consensus::{BranchId, NetworkType};

pub mod violation;

/// Consensus branch ID of NU6.3, the upgrade that added the Ironwood pool (ZIP 258).
pub const NU6_3_BRANCH_ID: u32 = 0x37A5_165B;
/// Consensus branch ID of NU7 (ZIP 259).
pub const NU7_BRANCH_ID: u32 = 0x7719_0AD9;
/// Serialized post-NU5 block header.
pub const HEADER_LENGTH: usize = 1487;
/// Length of a deposit journal (v2): eight ABI words.
pub const JOURNAL_LENGTH: usize = 8 * 32;

/// The Zcash network a build of the statements proves on. Fixed at build time ([`NETWORK`]):
/// a witness never chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    /// `test` or `main`, as the guests' image ids are labelled (`deposit-test`, `violation-main`).
    pub name: &'static str,
    /// The network a vault's unified address (and, in violation proofs, its UFVK) is encoded for.
    pub address_network: NetworkType,
    /// The consensus branch ids of the epochs a proof may walk. A ZIP 221 history tree covers one
    /// epoch and restarts at every network upgrade, so B and T are always of the same epoch.
    pub branch_ids: &'static [u32],
}

impl Network {
    /// Whether a proof may walk the epoch whose consensus branch id is `branch_id`.
    pub fn allows(&self, branch_id: u32) -> bool {
        self.branch_ids.contains(&branch_id)
    }
}

/// Testnet: the NU7 epoch (activated at 4,465,026).
pub const TESTNET: Network = Network {
    name: "test",
    address_network: NetworkType::Test,
    branch_ids: &[NU7_BRANCH_ID],
};

/// Mainnet: the Ironwood pool exists since NU6.3 (3,428,143); NU7 follows (ZIP 259).
pub const MAINNET: Network = Network {
    name: "main",
    address_network: NetworkType::Main,
    branch_ids: &[NU6_3_BRANCH_ID, NU7_BRANCH_ID],
};

/// The network this build proves on: Testnet, or Mainnet with the cargo feature `mainnet`.
#[cfg(not(feature = "mainnet"))]
pub const NETWORK: Network = TESTNET;
/// The network this build proves on: Testnet, or Mainnet with the cargo feature `mainnet`.
#[cfg(feature = "mainnet")]
pub const NETWORK: Network = MAINNET;

/// What a deposit proof lands as (journal word 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// The vault's deposit: D5, the vault's amount, mined at or below its expiry height. The
    /// Intake activates the vault on it, or lands it as a refund when the vault cannot be
    /// activated.
    Activate = 0,
    /// Any Ironwood note to the vault's published receiver, landed as a refund claim (T-02).
    Refund = 1,
}

impl TryFrom<u8> for Mode {
    type Error = Error;

    fn try_from(mode: u8) -> Result<Self, Error> {
        match mode {
            0 => Ok(Self::Activate),
            1 => Ok(Self::Refund),
            _ => Err(Error::Mode),
        }
    }
}

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
    /// [`Mode`]: 0 Activate, 1 Refund.
    pub mode: u8,
    /// Consensus branch ID of the epoch of block B and header T (one the build allows).
    pub branch_id: u32,
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
    /// unified address (exact string), tag key K (whose keccak256 they signed), amount and expiry
    /// height. K, not the viewing key: whoever proves sees K, never the UFVK.
    pub vault_id: [u8; 32],
    pub owner: [u8; 20],
    pub account: [u8; 20],
    pub vault_address: String,
    pub tag_key: [u8; 32],
    pub vault_amount: u64,
    pub expiry_height: u32,
}

/// The public output of a deposit proof (the zkVM journal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// [`Mode`] as its byte.
    pub mode: u8,
    /// Consensus branch ID of the epoch the proof walked; the Intake checks it is allowed.
    pub branch_id: u32,
    /// SHA-256d of header T, internal byte order (as the relay stores it).
    pub tip_hash: [u8; 32],
    pub vault_id: [u8; 32],
    pub account: [u8; 20],
    /// The note's value, in zatoshi (Activate: the vault's amount).
    pub amount: u64,
    /// Deposit tag: unique per note, unlinkable without the vault's tag key.
    pub tag: [u8; 32],
    /// EIP-712 struct hash of the vault's `VaultOpened` (the Intake verifies the signers'
    /// signatures over it under the Registry's domain).
    pub vault_opened_hash: [u8; 32],
}

impl Journal {
    /// Journal v2, what the Intake decodes (`DepositJournal` in IZecuredIntake.sol, eight words):
    /// `abi.encode(uint8 mode, uint32 branchId, bytes32 tipHash, bytes32 vaultId, address account,
    /// uint64 amount, bytes32 depositTag, bytes32 vaultOpenedHash)`.
    pub fn abi_encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(JOURNAL_LENGTH);
        out.extend_from_slice(&word(u128::from(self.mode)));
        out.extend_from_slice(&word(u128::from(self.branch_id)));
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

/// EIP-712 type of `VaultOpened` v2 (docs/INTERFACES.md § 2.5).
pub const VAULT_OPENED_TYPE: &str = "VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 tagKeyHash)";

/// EIP-712 struct hash of a `VaultOpened`: `address` is the exact string whose keccak256 the
/// signers signed (`addressHash`), `tag_key` the vault's tag key K, whose keccak256 they signed
/// (`tagKeyHash`, [`tag_key_hash`]).
pub fn vault_opened_hash(
    vault_id: &[u8; 32],
    owner: &[u8; 20],
    account: &[u8; 20],
    amount: u64,
    expiry_height: u32,
    address: &str,
    tag_key: &[u8; 32],
) -> [u8; 32] {
    let mut enc = Vec::with_capacity(8 * 32);
    enc.extend_from_slice(&keccak256(VAULT_OPENED_TYPE.as_bytes()));
    enc.extend_from_slice(vault_id);
    enc.extend_from_slice(&address_word(owner));
    enc.extend_from_slice(&address_word(account));
    enc.extend_from_slice(&word(u128::from(amount)));
    enc.extend_from_slice(&word(u128::from(expiry_height)));
    enc.extend_from_slice(&keccak256(address.as_bytes()));
    enc.extend_from_slice(&tag_key_hash(tag_key));
    keccak256(&enc)
}

/// Why a witness does not prove a deposit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The witness's mode is neither Activate (0) nor Refund (1).
    Mode,
    /// The witness's branch id is not one of the epochs this build proves (another network, an
    /// epoch without Ironwood, or no epoch at all).
    BranchNotAllowed,
    Transaction,
    /// Not a v6 transaction (O5).
    NotV6,
    /// The transaction's consensus branch id is not one the build allows (D5), or not the
    /// witness's epoch (O5).
    WrongEpoch,
    NotIronwoodOnly,
    /// The Ironwood bundle does not enable outputs (O5).
    OutputsDisabled,
    /// Refund mode: the transaction has no Ironwood bundle.
    NoIronwoodBundle,
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
    /// The note plaintext is not a V3 (ZIP 2005) Ironwood note (O5).
    NoteVersion,
    Address,
    WrongRecipient,
    /// Activate mode: the note's value is not the vault's amount.
    AmountMismatch,
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

/// The vault's tag key `K = BLAKE2b-256("Zecured_TagKey__", UFVK string)` (D24, the same as
/// `zecured_policy::derive::tag_key`): every reference Base sees about the vault's notes and
/// payouts is a PRF under it.
pub fn tag_key(ufvk: &str) -> [u8; 32] {
    blake2b_256(b"Zecured_TagKey__", &[], &[ufvk.as_bytes()])
}

/// `VaultOpened.tagKeyHash`: keccak256 of the 32 bytes of K.
pub fn tag_key_hash(tag_key: &[u8; 32]) -> [u8; 32] {
    keccak256(tag_key)
}

/// The tag of note commitment `cmx` under the vault's tag key K (Tier 3 tags every lineage note
/// with it).
pub fn note_tag(tag_key: &[u8; 32], cmx: &[u8; 32]) -> [u8; 32] {
    blake2b_256(b"Zecured_DepTag__", tag_key, &[cmx])
}

/// The deposit tag of note commitment `cmx` in the vault with viewing key `ufvk`:
/// [`note_tag`] under [`tag_key`]`(ufvk)`.
pub fn deposit_tag(ufvk: &str, cmx: &[u8; 32]) -> [u8; 32] {
    note_tag(&tag_key(ufvk), cmx)
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

/// v6 and of an epoch the network allows.
fn check_epoch(net: &Network, tx: &TransactionData<Authorized>) -> Result<(), Error> {
    if !matches!(tx.version(), TxVersion::V6) {
        return Err(Error::NotV6);
    }
    if !net.allows(u32::from(tx.consensus_branch_id())) {
        return Err(Error::WrongEpoch);
    }
    Ok(())
}

/// D5, strictly (O5, K2-c), on this build's network: a v6 transaction of an allowed epoch whose
/// only bundle is an Ironwood one with outputs enabled; no transparent, Sapling or Orchard-pool
/// bundle, even an empty one. The deposit statement (Activate mode) and the signers' P1 apply
/// this same function.
pub fn check_deposit_transaction(tx: &TransactionData<Authorized>) -> Result<(), Error> {
    check_deposit_transaction_on(&NETWORK, tx)
}

/// [`check_deposit_transaction`] on network `net`.
pub fn check_deposit_transaction_on(net: &Network, tx: &TransactionData<Authorized>) -> Result<(), Error> {
    check_epoch(net, tx)?;
    if tx.transparent_bundle().is_some() || tx.sapling_bundle().is_some() || tx.orchard_bundle().is_some() {
        return Err(Error::NotIronwoodOnly);
    }
    let bundle = tx.ironwood_bundle().ok_or(Error::NotIronwoodOnly)?;
    if !bundle.flags().outputs_enabled() {
        return Err(Error::OutputsDisabled);
    }
    Ok(())
}

/// What Refund mode asks of a transaction (T-09): v6, of an allowed epoch, with an Ironwood
/// bundle. Transparent, Sapling or Orchard-pool parts are allowed: the note is only refunded.
pub fn check_refund_transaction_on(net: &Network, tx: &TransactionData<Authorized>) -> Result<(), Error> {
    check_epoch(net, tx)?;
    tx.ironwood_bundle().ok_or(Error::NoIronwoodBundle)?;
    Ok(())
}

/// The vault's published receiver (K2-c) on this build's network: its unified address must be an
/// address of the network whose only receiver is an Orchard-protocol one (the address format
/// Ironwood notes use). A deposit must pay exactly this receiver, not another diversified address
/// of the vault. The deposit statement and the signers' P1 apply this same function.
pub fn vault_receiver(address: &str) -> Result<[u8; 43], Error> {
    vault_receiver_on(&NETWORK, address)
}

/// [`vault_receiver`] on network `net`.
pub fn vault_receiver_on(net: &Network, address: &str) -> Result<[u8; 43], Error> {
    let (network, _, ua) = unified::Address::decode(address).map_err(|_| Error::Address)?;
    if network != net.address_network {
        return Err(Error::Address);
    }
    match ua.items().as_slice() {
        [Receiver::Orchard(r)] => Ok(*r),
        _ => Err(Error::WrongRecipient),
    }
}

fn node(branch_id: u32, bytes: &[u8]) -> Result<NodeDataV3, Error> {
    <V3 as Version>::from_bytes(branch_id, bytes).map_err(|_| Error::HistoryPath)
}

/// Where a transaction is: its block B and B's place in the chain history a later header T
/// commits to. Shared by every statement about a mined transaction (the deposit statement here;
/// Tier 3's violation statements prove a spend's inclusion the same way).
#[derive(Clone, Copy, Debug)]
pub struct Inclusion<'a> {
    /// Consensus branch ID of the epoch of B and T. It personalizes every node hash of the
    /// history tree (ZIP 221), so B's leaf reaches T's commitment only under T's own epoch: B and
    /// T are of the same epoch. The caller checks the network allows it.
    pub branch_id: u32,
    /// The transaction's position in block B and the SHA-256d Merkle path from its txid.
    pub tx_index: u32,
    pub txid_path: &'a [[u8; 32]],
    /// Block B's header.
    pub block_header: &'a [u8],
    /// B's history-tree leaf (ZIP 221 V3), its path to a peak, all peaks (oldest first) and
    /// which peak the path reaches.
    pub leaf: &'a [u8],
    pub mmr_path: &'a [MmrStep],
    pub peaks: &'a [Vec<u8>],
    pub peak_index: u32,
    /// Header T and its block's auth data root (ZIP 244).
    pub tip_header: &'a [u8],
    pub tip_auth_data_root: &'a [u8; 32],
}

/// What [`check_inclusion`] establishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Included {
    /// Block B's height (hidden: only T is public).
    pub block_height: u64,
    /// SHA-256d of header T, internal byte order (as the relay stores it).
    pub tip_hash: [u8; 32],
}

/// Proves that `txid` is in a block B that is a leaf of the chain history of epoch
/// `i.branch_id` committed to by header T: the txid's Merkle path reaches B's `merkleRoot`, B's
/// ZIP 221 leaf (B alone) is on a path to one of the peaks, and the peaks with T's auth data root
/// hash to T's `hashBlockCommitments`.
pub fn check_inclusion(txid: &[u8; 32], i: &Inclusion<'_>) -> Result<Included, Error> {
    if i.block_header.len() != HEADER_LENGTH || i.tip_header.len() != HEADER_LENGTH {
        return Err(Error::Header);
    }
    let merkle_root: [u8; 32] = i.block_header[36..68].try_into().expect("32 bytes");
    if txid_path_root(*txid, i.tx_index, i.txid_path) != merkle_root {
        return Err(Error::TxidNotInBlock);
    }

    let leaf = node(i.branch_id, i.leaf)?;
    let v1 = &leaf.v2.v1;
    if v1.subtree_commitment != sha256d(i.block_header) || v1.start_height != v1.end_height {
        return Err(Error::LeafIsNotTheBlock);
    }
    let block_height = v1.end_height;
    let mut acc = leaf;
    for step in i.mmr_path {
        let sibling = node(i.branch_id, &step.sibling)?;
        acc = if step.sibling_is_left {
            V3::combine(&sibling, &acc)
        } else {
            V3::combine(&acc, &sibling)
        };
    }
    let peak = i.peaks.get(i.peak_index as usize).ok_or(Error::HistoryPath)?;
    if V3::to_bytes(&acc) != *peak {
        return Err(Error::HistoryPath);
    }
    let mut peaks = i.peaks.iter();
    let mut root = node(i.branch_id, peaks.next().ok_or(Error::HistoryPath)?)?;
    for p in peaks {
        root = V3::combine(&root, &node(i.branch_id, p)?);
    }
    let tip_commitments: [u8; 32] = i.tip_header[68..100].try_into().expect("32 bytes");
    if block_commitments(&V3::hash(&root), i.tip_auth_data_root) != tip_commitments {
        return Err(Error::HistoryNotCommitted);
    }
    Ok(Included {
        block_height,
        tip_hash: sha256d(i.tip_header),
    })
}

/// Checks the witness on this build's network and returns the public output, or why it fails.
pub fn verify(w: &Witness) -> Result<Journal, Error> {
    verify_on(&NETWORK, w)
}

/// [`verify`] on network `net` (the guests only ever use [`NETWORK`]; hosts and tests may check
/// a witness against either network).
pub fn verify_on(net: &Network, w: &Witness) -> Result<Journal, Error> {
    let mode = Mode::try_from(w.mode)?;

    // 1. The epoch: one this build proves.
    if !net.allows(w.branch_id) {
        return Err(Error::BranchNotAllowed);
    }
    let branch = BranchId::try_from(w.branch_id).map_err(|_| Error::BranchNotAllowed)?;

    // 2. The transaction. Activate: D5 (v6, Ironwood only, outputs enabled, O5). Refund: any v6
    //    transaction with an Ironwood bundle. (v5 and v6 transactions carry their own branch ID;
    //    the one passed to `read` only matters for older versions.)
    let tx = Transaction::read(&w.tx[..], branch).map_err(|_| Error::Transaction)?;
    match mode {
        Mode::Activate => check_deposit_transaction_on(net, &tx)?,
        Mode::Refund => check_refund_transaction_on(net, &tx)?,
    }
    let bundle = tx.ironwood_bundle().ok_or(Error::NoIronwoodBundle)?;
    let action = bundle.actions().get(w.action_index as usize).ok_or(Error::NoSuchAction)?;

    // 3. It is in block B, a leaf of the history committed to by header T, both of the witness's
    //    epoch; the transaction's own branch id is that epoch's. Activate: B is at or below the
    //    vault's expiry height.
    let included = check_inclusion(
        tx.txid().as_ref(),
        &Inclusion {
            branch_id: w.branch_id,
            tx_index: w.tx_index,
            txid_path: &w.txid_path,
            block_header: &w.block_header,
            leaf: &w.leaf,
            mmr_path: &w.mmr_path,
            peaks: &w.peaks,
            peak_index: w.peak_index,
            tip_header: &w.tip_header,
            tip_auth_data_root: &w.tip_auth_data_root,
        },
    )?;
    if tx.consensus_branch_id() != branch {
        return Err(Error::WrongEpoch);
    }
    if mode == Mode::Activate && included.block_height > u64::from(w.expiry_height) {
        return Err(Error::BlockAfterExpiry);
    }

    // 4. The note. Recovering the action's ciphertext with the ephemeral secret derived from the
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
    if recovered.version() != NoteVersion::V3 {
        return Err(Error::NoteVersion);
    }

    // 5. The recipient is the vault's published (Ironwood-only) receiver on the build's network.
    if vault_receiver_on(net, &w.vault_address)? != note.recipient().to_raw_address_bytes() {
        return Err(Error::WrongRecipient);
    }

    // 6. Activate: a note of the vault's amount. A refund carries the note's own value.
    if mode == Mode::Activate && w.note_value != w.vault_amount {
        return Err(Error::AmountMismatch);
    }

    // The deposit's note commitment, as recovery checked it.
    let cmx = action.cmx().to_bytes();
    Ok(Journal {
        mode: mode as u8,
        branch_id: w.branch_id,
        tip_hash: included.tip_hash,
        vault_id: w.vault_id,
        account: w.account,
        amount: w.note_value,
        tag: note_tag(&w.tag_key, &cmx),
        vault_opened_hash: vault_opened_hash(
            &w.vault_id,
            &w.owner,
            &w.account,
            w.vault_amount,
            w.expiry_height,
            &w.vault_address,
            &w.tag_key,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTOR_ADDRESS: &str = "utest1yv0hq77h4th2c882kdmcgt47xrm5zmhkfnfpw6dcack8e6kgfmg5new7vxed0trhkc67cmf2tjea8pnd09key4v85kmqdnz33yuas40f";
    const VECTOR_UFVK: &str = "uviewtest1rl58wrlhq2eqws08m2jxqg0gjpn2yd7me5f9cfkwmf4axnq0vjvjgvjs5t70tsny62rxklz3ha8t5cr8k30jltjmjr0xxsdhpqeqpf55w3dx0mracz99uvrvyf4hpsfxs2nc5jykraqvnt22zpkdce6tcj85lkty9lcpduhl72vz80l8ygn0q3sxn04w8";

    fn h32(s: &str) -> [u8; 32] {
        let b: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        b.try_into().unwrap()
    }

    /// The golden `VaultOpened` v2 vector of signer/crates/base-client (also asserted by the app
    /// and by contracts/test/unit/ZecuredEIP712.t.sol): K of the vector's UFVK, its keccak256 and
    /// the struct hash.
    #[test]
    fn vault_opened_hash_matches_the_golden_vector() {
        let k = tag_key(VECTOR_UFVK);
        assert_eq!(k, h32("2f30a290051344422e07e110c75334c79f0fe84e23db3ea0d7666c0bedab8066"));
        assert_eq!(tag_key_hash(&k), h32("4998e90f251bfc9d596388325cf58132bb23a71bf6e71f6ff15275e11ad308b6"));
        let h = vault_opened_hash(&[0x55; 32], &[0x88; 20], &[0x33; 20], 1_000_000, 4_486_288, VECTOR_ADDRESS, &k);
        assert_eq!(h, h32("d63b647d9eba421dde278de2765b71900dc7372c7013dc925ae63dd3c28f5d13"));
    }

    #[test]
    fn journal_abi_layout() {
        let j = Journal {
            mode: 1,
            branch_id: NU7_BRANCH_ID,
            tip_hash: [1; 32],
            vault_id: [2; 32],
            account: [3; 20],
            amount: 0x0102_0304,
            tag: [4; 32],
            vault_opened_hash: [5; 32],
        };
        let e = j.abi_encode();
        assert_eq!(e.len(), JOURNAL_LENGTH);
        assert_eq!(&e[0..31], &[0; 31]);
        assert_eq!(e[31], 1, "mode");
        assert_eq!(&e[32..60], &[0; 28]);
        assert_eq!(&e[60..64], &[0x77, 0x19, 0x0a, 0xd9], "branch id, big-endian");
        assert_eq!(&e[64..96], &[1; 32]);
        assert_eq!(&e[96..128], &[2; 32]);
        assert_eq!(&e[128..140], &[0; 12]);
        assert_eq!(&e[140..160], &[3; 20]);
        assert_eq!(&e[160..188], &[0; 28]);
        assert_eq!(&e[188..192], &[1, 2, 3, 4]);
        assert_eq!(&e[192..224], &[4; 32]);
        assert_eq!(&e[224..256], &[5; 32]);
    }

    #[test]
    fn tag_is_keyed_by_the_tag_key() {
        let cmx = [7u8; 32];
        assert_eq!(deposit_tag("uviewtest1a", &cmx), note_tag(&tag_key("uviewtest1a"), &cmx));
        assert_ne!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1b", &cmx));
        assert_ne!(deposit_tag("uviewtest1a", &cmx), deposit_tag("uviewtest1a", &[8u8; 32]));
        // The tag key vector Python's hashlib gives (zecured_policy::derive pins the same).
        assert_eq!(tag_key("uviewtest1a"), h32("99b01fef42e44f44cbc75bfb5f80588989e52b4f7f7e3cb914fa5778eb034dea"));
    }

    #[test]
    fn the_networks() {
        assert_eq!(NU6_3_BRANCH_ID, u32::from(BranchId::Nu6_3));
        assert_eq!(NU7_BRANCH_ID, u32::from(BranchId::Nu7));
        assert!(TESTNET.allows(NU7_BRANCH_ID) && !TESTNET.allows(NU6_3_BRANCH_ID));
        assert!(MAINNET.allows(NU7_BRANCH_ID) && MAINNET.allows(NU6_3_BRANCH_ID));
        for not_ironwood in [
            0,
            u32::from(BranchId::Nu6_2),
            u32::from(BranchId::Nu6_1),
            u32::from(BranchId::Nu5),
        ] {
            assert!(!TESTNET.allows(not_ironwood) && !MAINNET.allows(not_ironwood));
        }
        #[cfg(not(feature = "mainnet"))]
        assert_eq!(NETWORK, TESTNET);
        #[cfg(feature = "mainnet")]
        assert_eq!(NETWORK, MAINNET);
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::try_from(0), Ok(Mode::Activate));
        assert_eq!(Mode::try_from(1), Ok(Mode::Refund));
        assert_eq!(Mode::try_from(2), Err(Error::Mode));
        assert_eq!(Mode::try_from(255), Err(Error::Mode));
    }
}
