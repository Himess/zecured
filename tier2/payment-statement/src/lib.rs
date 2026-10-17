//! The Zecured Swap payment statement: what a payment proof proves.
//!
//! Zecured Swap releases USDC escrowed on Base (`ZecuredSwap`) once whoever paid proves, in zero
//! knowledge, that a shielded ZEC payment for that very order is in the Zcash chain. Plain Rust,
//! shared by the zkVM guest (which proves that [`verify`] accepts a private [`PaymentWitness`]) and
//! the hosts (which build witnesses and run the same check natively).
//!
//! **The network is a build parameter, never a witness field**, as for the deposit statement: a
//! build proves payments on one Zcash network ([`NETWORK`]), Testnet by default and Mainnet with the
//! cargo feature `mainnet`, which fixes the epochs a proof may walk. Each network has its own guest
//! and image id (`payment-test`, `payment-main`).
//!
//! A proof shows, for an order whose USDC sits in `ZecuredSwap`:
//!
//! 1. A v6 transaction of an allowed epoch whose only bundle is an Ironwood one with outputs
//!    enabled: no transparent input or output, and no Sapling or Orchard-pool bundle, even an empty
//!    one (the deposit rule D5, applied through the deposit statement's
//!    [`check_deposit_transaction_on`]). A payment never touches a pool or a transparent address
//!    whose amounts are public, so nothing on Zcash ties it to the order's amount on Base.
//! 2. One of its Ironwood actions creates a V3 (ZIP 2005) note, rebuilt from the witness's opening
//!    (recipient, value, rseed): the action's ciphertext decrypts, under the ephemeral secret the
//!    note derives, to exactly that note (its note commitment and ephemeral key are the action's),
//!    so the recipient's incoming viewing key finds it and its spending key can spend it.
//! 3. **The memo binds the payment to one order.** The note's 512-byte memo (the in-band memo of
//!    the V3 note plaintext, `lead byte 0x03 || d || v || rseed || memo`, as orchard 0.16's
//!    `NoteEncryptionDomain::note_plaintext_bytes` lays it out; the recovery returns those 512
//!    bytes) starts with `"ZSWP" || orderId`. One note pays one order, even when the same receiver
//!    is used for two orders; the rest of the memo is free.
//! 4. The recipient is committed to as `C = keccak256(salt || receiver)`, the 32-byte salt and the
//!    43-byte raw Orchard-protocol receiver: the only form in which a recipient ever reaches Base.
//! 5. The transaction is in a block B, and B is in the chain history committed to by a later header
//!    T of the same epoch (ZIP 221), with the deposit statement's [`check_inclusion`]: every node
//!    hash of that history is personalized with the epoch's branch id, which the transaction
//!    carries too. Only T is public, never B: the payment's block stays hidden among the epoch's.
//!
//! The journal ([`PaymentJournal::abi_encode`], 192 bytes) is
//! `abi.encode(uint8 kind = 1, uint32 branchId, bytes32 anchor, bytes32 orderId,
//! bytes32 receiverCommitment, uint64 zatoshi)`: the anchor is T's hash, `zatoshi` the note's value.
//! It never contains a txid, a note commitment, a nullifier or an address. `ZecuredSwap` checks the
//! anchor against the Zcash header relay (enough work on top for the order's amount, then K3), the
//! branch id against the active Swap version, and that the order id, the receiver commitment and an
//! amount at least the order's match the order.

use orchard::{
    Address, Note,
    note::{NoteVersion, RandomSeed, Rho},
    note_encryption::IronwoodDomain,
    value::NoteValue,
};
use serde::{Deserialize, Serialize};
use zcash_note_encryption::{Domain, try_output_recovery_with_pkd_esk};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
pub use zecured_deposit_statement::{
    Error as StatementError, HEADER_LENGTH, MAINNET, MmrStep, NETWORK, NU6_3_BRANCH_ID, NU7_BRANCH_ID, Network,
    TESTNET, keccak256,
};
use zecured_deposit_statement::{Inclusion, check_deposit_transaction_on, check_inclusion};

/// The first word of every payment journal: the payment proof's kind (`ZecuredSwap` refuses any
/// other).
pub const JOURNAL_KIND: u8 = 1;
/// Length of a payment journal: six ABI words.
pub const JOURNAL_LENGTH: usize = 6 * 32;
/// The memo prefix of a swap payment: `"ZSWP" || orderId` opens the note's memo.
pub const MEMO_PREFIX: [u8; 4] = *b"ZSWP";
/// Bytes of the memo the statement reads: the prefix and the order id.
pub const MEMO_BINDING_LENGTH: usize = MEMO_PREFIX.len() + 32;
/// Length of a note memo (orchard's `Domain::Memo` of the Ironwood domain).
pub const MEMO_LENGTH: usize = 512;

/// The private input of a payment proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaymentWitness {
    /// Consensus branch ID of the epoch of block B and header T (one the build allows).
    pub branch_id: u32,
    /// The payment transaction, serialized (v6).
    pub tx: Vec<u8>,
    /// Index of the Ironwood action that creates the payment note.
    pub action_index: u32,
    /// The payment note's recipient (43-byte raw Orchard-protocol receiver), value and rseed: what
    /// the payer knows from building the transaction (with `ovk = None` nobody else can recover it
    /// from the chain, and only the recipient can decrypt it).
    pub note_recipient: Vec<u8>,
    pub note_value: u64,
    pub note_rseed: [u8; 32],
    /// The salt of the order's receiver commitment `keccak256(salt || receiver)`.
    pub salt: [u8; 32],
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
}

/// The public output of a payment proof (the zkVM journal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentJournal {
    /// Always [`JOURNAL_KIND`].
    pub kind: u8,
    /// Consensus branch ID of the epoch the proof walked; `ZecuredSwap` checks the Swap version
    /// allows it.
    pub branch_id: u32,
    /// SHA-256d of header T, internal byte order (as the relay stores it).
    pub anchor: [u8; 32],
    /// The order the memo names.
    pub order_id: [u8; 32],
    /// `keccak256(salt || receiver)` of the note's recipient.
    pub receiver_commitment: [u8; 32],
    /// The note's value, in zatoshi.
    pub zatoshi: u64,
}

impl PaymentJournal {
    /// What `ZecuredSwap.proveSettlement` decodes (`PaymentJournal` in IZecuredSwap.sol, six
    /// words): `abi.encode(uint8 kind, uint32 branchId, bytes32 anchor, bytes32 orderId,
    /// bytes32 receiverCommitment, uint64 zatoshi)`.
    pub fn abi_encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(JOURNAL_LENGTH);
        out.extend_from_slice(&word(u128::from(self.kind)));
        out.extend_from_slice(&word(u128::from(self.branch_id)));
        out.extend_from_slice(&self.anchor);
        out.extend_from_slice(&self.order_id);
        out.extend_from_slice(&self.receiver_commitment);
        out.extend_from_slice(&word(u128::from(self.zatoshi)));
        out
    }
}

/// An unsigned integer as one ABI word. Copied from the deposit statement
/// (`zecured_deposit_statement::word`, private there).
fn word(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// The receiver commitment Base sees: `keccak256(salt32 || receiver43)`, as everywhere in Zecured
/// (docs/INTERFACES.md § 1).
pub fn receiver_commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> [u8; 32] {
    let mut preimage = [0u8; 32 + 43];
    preimage[..32].copy_from_slice(salt);
    preimage[32..].copy_from_slice(receiver);
    keccak256(&preimage)
}

/// The memo a payment for order `order_id` carries: `"ZSWP" || orderId`, zero-padded to 512 bytes
/// (what `MemoBytes::from_bytes` gives for these 36 bytes).
pub fn payment_memo(order_id: &[u8; 32]) -> [u8; MEMO_LENGTH] {
    let mut memo = [0u8; MEMO_LENGTH];
    memo[..4].copy_from_slice(&MEMO_PREFIX);
    memo[4..MEMO_BINDING_LENGTH].copy_from_slice(order_id);
    memo
}

/// The order a memo binds the payment to, if it is a swap payment's.
pub fn memo_order_id(memo: &[u8; MEMO_LENGTH]) -> Option<[u8; 32]> {
    if memo[..4] != MEMO_PREFIX {
        return None;
    }
    memo[4..MEMO_BINDING_LENGTH].try_into().ok()
}

/// Why a witness does not prove a payment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The witness's branch id is not one of the epochs this build proves.
    BranchNotAllowed,
    /// The transaction does not parse.
    Transaction,
    /// Not a v6 transaction.
    NotV6,
    /// The transaction's consensus branch id is not one the build allows, or not the witness's
    /// epoch.
    WrongEpoch,
    /// Not an Ironwood-only transaction (D5): a transparent, Sapling or Orchard-pool bundle, or no
    /// Ironwood bundle.
    NotIronwoodOnly,
    /// The Ironwood bundle does not enable outputs.
    OutputsDisabled,
    NoSuchAction,
    /// B's or T's header is not 1487 bytes.
    Header,
    TxidNotInBlock,
    LeafIsNotTheBlock,
    HistoryPath,
    HistoryNotCommitted,
    /// The opening is not a note (recipient not a valid 43-byte receiver, rho or rseed invalid).
    Note,
    /// The action's ciphertext does not decrypt to a note under the opening's ephemeral secret.
    NotDecryptable,
    /// The recovered note is not the opened one.
    NoteCommitmentMismatch,
    /// The note plaintext is not a V3 (ZIP 2005) Ironwood note.
    NoteVersion,
    /// The note's memo does not start with `"ZSWP"`.
    Memo,
    /// Another error of the deposit statement's shared checks (none of them returns one today).
    Statement(StatementError),
}

impl From<StatementError> for Error {
    /// The errors the deposit statement's shared checks ([`check_deposit_transaction_on`],
    /// [`check_inclusion`]) return, under the same names.
    fn from(e: StatementError) -> Self {
        match e {
            StatementError::NotV6 => Error::NotV6,
            StatementError::WrongEpoch => Error::WrongEpoch,
            StatementError::NotIronwoodOnly => Error::NotIronwoodOnly,
            StatementError::OutputsDisabled => Error::OutputsDisabled,
            StatementError::Header => Error::Header,
            StatementError::TxidNotInBlock => Error::TxidNotInBlock,
            StatementError::LeafIsNotTheBlock => Error::LeafIsNotTheBlock,
            StatementError::HistoryPath => Error::HistoryPath,
            StatementError::HistoryNotCommitted => Error::HistoryNotCommitted,
            other => Error::Statement(other),
        }
    }
}

/// Checks the witness on this build's network and returns the public output, or why it fails.
pub fn verify(w: &PaymentWitness) -> Result<PaymentJournal, Error> {
    verify_on(&NETWORK, w)
}

/// [`verify`] on network `net` (the guests only ever use [`NETWORK`]; hosts and tests may check a
/// witness against either network).
pub fn verify_on(net: &Network, w: &PaymentWitness) -> Result<PaymentJournal, Error> {
    // 1. The epoch: one this build proves.
    if !net.allows(w.branch_id) {
        return Err(Error::BranchNotAllowed);
    }
    let branch = BranchId::try_from(w.branch_id).map_err(|_| Error::BranchNotAllowed)?;

    // 2. The transaction: D5, v6, Ironwood only, outputs enabled. (v5 and v6 transactions carry
    //    their own branch id; the one passed to `read` only matters for older versions.)
    let tx = Transaction::read(&w.tx[..], branch).map_err(|_| Error::Transaction)?;
    check_deposit_transaction_on(net, &tx)?;
    let bundle = tx.ironwood_bundle().ok_or(Error::NotIronwoodOnly)?;
    let action = bundle.actions().get(w.action_index as usize).ok_or(Error::NoSuchAction)?;

    // 3. It is in block B, a leaf of the history committed to by header T, both of the witness's
    //    epoch; the transaction's own branch id is that epoch's.
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

    // 4. The note, rebuilt from the opening as the deposit statement rebuilds a deposit: recovering
    //    the action's ciphertext with the ephemeral secret derived from the witness note (and its
    //    pk_d) checks that the plaintext is a note whose commitment is the action's cmx and whose
    //    ephemeral key is the action's (ZIP 212). The recovered note is compared field by field
    //    with the opened one (`Note`'s `PartialEq` would compute both commitments again).
    let recipient: [u8; 43] = w.note_recipient.clone().try_into().map_err(|_| Error::Note)?;
    let address = Option::<Address>::from(Address::from_raw_address_bytes(&recipient)).ok_or(Error::Note)?;
    // An action's output note has rho = the nullifier spent by the same action.
    let rho = Option::<Rho>::from(Rho::from_bytes(&action.nullifier().to_bytes())).ok_or(Error::Note)?;
    let rseed = Option::<RandomSeed>::from(RandomSeed::from_bytes(w.note_rseed, &rho)).ok_or(Error::Note)?;
    let note =
        Option::<Note>::from(Note::from_parts(address, NoteValue::from_raw(w.note_value), rho, rseed, NoteVersion::V3))
            .ok_or(Error::Note)?;
    let domain = IronwoodDomain::for_action(action);
    let esk = IronwoodDomain::derive_esk(&note).ok_or(Error::NotDecryptable)?;
    let pk_d = IronwoodDomain::get_pk_d(&note);
    let (recovered, _, memo) =
        try_output_recovery_with_pkd_esk(&domain, pk_d, esk, action).ok_or(Error::NotDecryptable)?;
    if recovered.recipient().to_raw_address_bytes() != recipient
        || recovered.value() != note.value()
        || recovered.rseed().as_bytes() != note.rseed().as_bytes()
        || recovered.rho().to_bytes() != note.rho().to_bytes()
    {
        return Err(Error::NoteCommitmentMismatch);
    }
    if recovered.version() != NoteVersion::V3 {
        return Err(Error::NoteVersion);
    }

    // 5. The memo names the order.
    let order_id = memo_order_id(&memo).ok_or(Error::Memo)?;

    Ok(PaymentJournal {
        kind: JOURNAL_KIND,
        branch_id: w.branch_id,
        anchor: included.tip_hash,
        order_id,
        receiver_commitment: receiver_commitment(&w.salt, &recipient),
        zatoshi: w.note_value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_abi_layout() {
        let j = PaymentJournal {
            kind: JOURNAL_KIND,
            branch_id: NU7_BRANCH_ID,
            anchor: [1; 32],
            order_id: [2; 32],
            receiver_commitment: [3; 32],
            zatoshi: 0x0102_0304_0506,
        };
        let e = j.abi_encode();
        assert_eq!(e.len(), JOURNAL_LENGTH);
        assert_eq!(e.len(), 192, "six words, as frozen");
        assert_eq!(&e[0..31], &[0; 31]);
        assert_eq!(e[31], 1, "kind");
        assert_eq!(&e[32..60], &[0; 28]);
        assert_eq!(&e[60..64], &[0x77, 0x19, 0x0a, 0xd9], "branch id, big-endian");
        assert_eq!(&e[64..96], &[1; 32], "anchor");
        assert_eq!(&e[96..128], &[2; 32], "order id");
        assert_eq!(&e[128..160], &[3; 32], "receiver commitment");
        assert_eq!(&e[160..186], &[0; 26]);
        assert_eq!(&e[186..192], &[1, 2, 3, 4, 5, 6], "zatoshi, big-endian");
    }

    #[test]
    fn the_memo_binds_one_order() {
        let order = [0xab; 32];
        let memo = payment_memo(&order);
        assert_eq!(&memo[..4], b"ZSWP");
        assert_eq!(&memo[4..36], &order);
        assert!(memo[36..].iter().all(|b| *b == 0), "zero-padded, as MemoBytes::from_bytes pads");
        assert_eq!(memo_order_id(&memo), Some(order));
        // The rest of the memo is free.
        let mut annotated = memo;
        annotated[100] = 7;
        assert_eq!(memo_order_id(&annotated), Some(order));
        // Any other prefix, and the empty memo (ZIP 302: 0xF6 then zeros), bind nothing.
        let mut other = memo;
        other[3] = b'Q';
        assert_eq!(memo_order_id(&other), None);
        let mut empty = [0u8; MEMO_LENGTH];
        empty[0] = 0xf6;
        assert_eq!(memo_order_id(&empty), None);
        assert_eq!(memo_order_id(&[0u8; MEMO_LENGTH]), None);
    }

    /// `keccak256(salt32 || receiver43)` over 32 bytes of 0x11 then 43 bytes of 0x22, the vector
    /// Foundry's `cast keccak` gives.
    #[test]
    fn receiver_commitment_is_keccak_of_salt_and_receiver() {
        let c = receiver_commitment(&[0x11; 32], &[0x22; 43]);
        let expected: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&"bae7b4976157459acb192cab961fec4003a1c8714e9cdee2a2bfa7f779ef6e10"[i..i + 2], 16)
                    .unwrap()
            })
            .collect();
        assert_eq!(c.to_vec(), expected);
        assert_ne!(c, receiver_commitment(&[0x12; 32], &[0x22; 43]));
        assert_ne!(c, receiver_commitment(&[0x11; 32], &[0x23; 43]));
    }

    #[test]
    fn the_networks_are_the_deposit_statements() {
        assert_eq!(TESTNET.branch_ids, &[NU7_BRANCH_ID]);
        assert_eq!(MAINNET.branch_ids, &[NU6_3_BRANCH_ID, NU7_BRANCH_ID]);
        #[cfg(not(feature = "mainnet"))]
        assert_eq!(NETWORK, TESTNET);
        #[cfg(feature = "mainnet")]
        assert_eq!(NETWORK, MAINNET);
    }
}
