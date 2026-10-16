//! Tier 3 violation statements (docs/TIER3.md § 6): what a proof that the signers took
//! collateral proves. One statement, five kinds, one zkVM guest per network.
//!
//! Every kind first shows that a transaction X (whose txid is computed from its bytes) is in a
//! block committed by a later header T of the same epoch, with the Tier 2 inclusion code
//! ([`check_inclusion`]); the epoch's branch id is one the build allows ([`crate::NETWORK`]) and
//! X's own. Then:
//!
//! - **F1, Uncommitted.** X spends a note of the vault: the witness opens the note, whose
//!   nullifier (under the full viewing key of the vault's UFVK) is the nullifier of one of X's
//!   actions. The journal names the note by its tag and X by its `payoutRef`, both under the
//!   UFVK's tag key; the slasher checks that the tag is a lineage note on Base and that no
//!   commitment of X names it as its input. F1 keeps the full UFVK (design review): with only
//!   K and a nullifier key, anyone holding K could pair it with a made-up `nk`
//!   on a forged block.
//! - **F2, WithheldChange.** No action of X carries the change note the commitment of X
//!   promised: none whose note commitment has the committed tag and decrypts, with the vault's
//!   internal incoming viewing key, to a note of the committed value. Change goes to the internal
//!   scope only: one sent to the vault's external address is not counted as delivered, since the
//!   signers would treat it as a deposit to refund (review RT-9). F2 proves an absence, so it
//!   needs the UFVK, which `K = H(UFVK)` binds to the vault's tags.
//! - **F3, Misdirected.** X names a claim through `claimRef = keccak256(salt || txid)`; the
//!   claim's recipient is `keccak256(salt || receiver)`; the commitment of X names the action whose
//!   output pays the claim (E1). An incoming viewing key that owns the receiver shows what that one
//!   output pays that exact receiver (`value`): its note decrypts and matches its note commitment,
//!   or it counts as nothing. The slasher compares it with what the commitment promised the claim.
//! - **F4, WrongClaimRef.** With the vault's tag key K (for X's `payoutRef`) and a claim's salt and
//!   receiver, the journal gives the reference an honest commitment of X names that claim by,
//!   `keccak256(salt || txid)`; the slasher checks that the commitment of X names the claim of
//!   that recipient commitment under another one, which hides X from the claimant (review RT-1).
//!   A wrong K only yields a `payoutRef` that names no commitment on Base.
//! - **Included.** Not a violation: X, named by its `payoutRef` under K, is in a block under the
//!   anchor. A later contract consumes it (design notes T-05, "confirmation by proof": a payout
//!   is confirmed, or a disputed confirmation upheld, only on such a proof). The v2 slasher
//!   refuses it: its `ViolationKind` stops at WrongClaimRef, so decoding kind 5 reverts.
//!
//! The journal ([`ViolationJournal::abi_encode`]) is what `ZecuredSlasher.proveViolation` decodes;
//! fields a kind does not use are zero. It never contains a txid, a note commitment, a
//! nullifier or an address: only the anchor header, references Base already has (or keyed /
//! salted hashes of them) and, for F3, the value the claimant received.

use orchard::{
    Address, Note,
    keys::{FullViewingKey, IncomingViewingKey, PreparedIncomingViewingKey, Scope},
    note::{ExtractedNoteCommitment, NoteVersion, RandomSeed, Rho},
    note_encryption::IronwoodDomain,
    value::NoteValue,
};
use serde::{Deserialize, Serialize};
use zcash_address::unified::{self, Container, Encoding, Fvk};
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::transaction::{Transaction, TxVersion};
use zcash_protocol::consensus::BranchId;

use crate::{Error, Inclusion, MmrStep, NETWORK, Network, blake2b_256, check_inclusion, keccak256, note_tag, tag_key};

/// The kinds; 1 to 4 are `ViolationKind` in contracts/src/interfaces/IZecuredSlasher.sol. `Included`
/// (5) proves no violation and is not one of them (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Kind {
    Uncommitted = 1,
    WithheldChange = 2,
    Misdirected = 3,
    WrongClaimRef = 4,
    Included = 5,
}

/// The private input of a violation proof.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViolationWitness {
    pub kind: Kind,
    /// Receives the bounty; bound inside the proof (not part of an `Included` journal).
    pub beneficiary: [u8; 20],
    /// Consensus branch ID of the epoch of X's block and of T (one the build allows).
    pub branch_id: u32,
    /// Transaction X, serialized (v6).
    pub tx: Vec<u8>,
    /// Where X is (as [`Inclusion`]).
    pub tx_index: u32,
    pub txid_path: Vec<[u8; 32]>,
    pub block_header: Vec<u8>,
    pub leaf: Vec<u8>,
    pub mmr_path: Vec<MmrStep>,
    pub peaks: Vec<Vec<u8>>,
    pub peak_index: u32,
    pub tip_header: Vec<u8>,
    pub tip_auth_data_root: [u8; 32],
    pub detail: Detail,
}

/// What each kind needs besides the transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Detail {
    /// F1: the vault's UFVK (exact string), the opening of the lineage note X spends and the
    /// index of the action of X that spends it.
    Uncommitted {
        vault_ufvk: String,
        action_index: u32,
        note_recipient: Vec<u8>,
        note_value: u64,
        note_rho: [u8; 32],
        note_rseed: [u8; 32],
    },
    /// F2: the vault's UFVK and the change the commitment of X promised.
    WithheldChange {
        vault_ufvk: String,
        change_tag: [u8; 32],
        change_value: u64,
    },
    /// F3: the claim's salt and receiver (its recipient commitment), an incoming viewing key of
    /// the claimant's wallet (raw, 64 bytes) and the action the commitment names for the claim
    /// (`claimPayment(claimRef).outputIndex` on Base).
    Misdirected {
        salt: [u8; 32],
        receiver: Vec<u8>,
        ivk: Vec<u8>,
        output_index: u32,
    },
    /// F4: the vault's tag key K (for X's `payoutRef`) and a claim's salt and receiver (its
    /// recipient commitment). Held by the borrower for its own claims and by the signers for every
    /// claim.
    WrongClaimRef {
        tag_key: [u8; 32],
        salt: [u8; 32],
        receiver: Vec<u8>,
    },
    /// Included: the tag key K of the vault X pays from (for X's `payoutRef`).
    Included { tag_key: [u8; 32] },
}

/// The public output of a violation proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViolationJournal {
    pub kind: Kind,
    /// SHA-256d of header T, internal byte order.
    pub anchor: [u8; 32],
    /// The bounty's receiver; zero for `Included`.
    pub beneficiary: [u8; 20],
    /// F1, F2, F4, Included: `payoutRef` of X; F3: `claimRef` of the claim in X.
    pub reference: [u8; 32],
    /// F1: tag of the note X spends; F2: the committed change tag; F3: zero; F4: the claim
    /// reference an honest commitment of X gives, `keccak256(salt || txid)`; Included: zero.
    pub note_tag: [u8; 32],
    /// F2: the committed change value; F3: what the claim's output pays its exact receiver.
    pub value: u64,
    /// F3, F4: the claim's recipient commitment.
    pub dest_commitment: [u8; 32],
    /// F3: the action of X the commitment names for the claim.
    pub output_index: u8,
}

impl ViolationJournal {
    /// `abi.encode(ViolationJournal)` of IZecuredSlasher.sol: eight words.
    pub fn abi_encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 * 32);
        out.extend_from_slice(&crate::word(u128::from(self.kind as u8)));
        out.extend_from_slice(&self.anchor);
        out.extend_from_slice(&crate::address_word(&self.beneficiary));
        out.extend_from_slice(&self.reference);
        out.extend_from_slice(&self.note_tag);
        out.extend_from_slice(&crate::word(u128::from(self.value)));
        out.extend_from_slice(&self.dest_commitment);
        out.extend_from_slice(&crate::word(u128::from(self.output_index)));
        out
    }
}

/// Why a witness proves no violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViolationError {
    Transaction,
    NoIronwoodBundle,
    /// X is not where the witness says: not in the anchored history, or not of an epoch the build
    /// allows (`BranchNotAllowed`), or not of the witness's epoch (`WrongEpoch`).
    Inclusion(crate::Error),
    Ufvk,
    NoSuchAction,
    Note,
    /// F1: the opened note is not the one the action spends.
    NotSpent,
    /// F2: X does return the committed change to the vault.
    ChangeDelivered,
    Receiver,
    Ivk,
    /// F3: the incoming viewing key does not own the claim's receiver.
    NotTheClaimant,
    /// F3: an output index beyond any a commitment can name.
    OutputIndex,
    /// The witness's detail is for another kind.
    KindMismatch,
}

/// `payoutRef` of a txid (internal byte order) under the vault's tag key K (docs/TIER3.md § 2;
/// the same as `zecured_policy::derive::payout_ref` of the UFVK K is derived from).
pub fn payout_ref(tag_key: &[u8; 32], txid: &[u8; 32]) -> [u8; 32] {
    blake2b_256(b"Zecured_PayRef__", tag_key, &[txid])
}

/// `claimRef` of a claim paid by a transaction: `keccak256(salt || txid)` (the same as
/// `zecured_policy::ledger::claim_ref`).
pub fn claim_ref(salt: &[u8; 32], txid: &[u8; 32]) -> [u8; 32] {
    keccak256(&[salt.as_slice(), txid].concat())
}

/// What `action`'s output pays `receiver`, as the holder of `ivk` sees it: the note decrypts with
/// the key, its plaintext matches the action's note commitment (`try_note_decryption` checks the
/// commitment and the ephemeral key, ZIP 212), and it is addressed to `receiver` exactly. Anything
/// else pays the receiver nothing (E1).
pub fn paid_to<A>(action: &orchard::Action<A>, ivk: &PreparedIncomingViewingKey, receiver: &Address) -> u64 {
    match try_note_decryption(&IronwoodDomain::for_action(action), ivk, action) {
        Some((note, to, _)) if to == *receiver => note.value().inner(),
        _ => 0,
    }
}

/// The Orchard-protocol full viewing key of a UFVK string of network `net`.
fn vault_fvk(net: &Network, ufvk: &str) -> Result<FullViewingKey, ViolationError> {
    let (network, _, keys) = unified::Ufvk::decode(ufvk).map_err(|_| ViolationError::Ufvk)?;
    if network != net.address_network {
        return Err(ViolationError::Ufvk);
    }
    keys.items()
        .iter()
        .find_map(|k| match k {
            Fvk::Orchard(bytes) => FullViewingKey::from_bytes(bytes),
            _ => None,
        })
        .ok_or(ViolationError::Ufvk)
}

/// Checks the witness on this build's network and returns the public output, or why it proves
/// no violation.
pub fn verify_violation(w: &ViolationWitness) -> Result<ViolationJournal, ViolationError> {
    verify_violation_on(&NETWORK, w)
}

/// [`verify_violation`] on network `net` (the guests only ever use [`NETWORK`]).
pub fn verify_violation_on(net: &Network, w: &ViolationWitness) -> Result<ViolationJournal, ViolationError> {
    // The epoch: one this build proves.
    if !net.allows(w.branch_id) {
        return Err(ViolationError::Inclusion(Error::BranchNotAllowed));
    }
    let branch = BranchId::try_from(w.branch_id).map_err(|_| ViolationError::Inclusion(Error::BranchNotAllowed))?;
    // X: any v6 transaction with an Ironwood bundle (a theft need not follow the payout format).
    let tx = Transaction::read(&w.tx[..], branch).map_err(|_| ViolationError::Transaction)?;
    if !matches!(tx.version(), TxVersion::V6) {
        return Err(ViolationError::Transaction);
    }
    let bundle = tx.ironwood_bundle().ok_or(ViolationError::NoIronwoodBundle)?;
    let txid: [u8; 32] = *tx.txid().as_ref();
    let included = check_inclusion(
        &txid,
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
    )
    .map_err(ViolationError::Inclusion)?;
    if tx.consensus_branch_id() != branch {
        return Err(ViolationError::Inclusion(Error::WrongEpoch));
    }
    let mut journal = ViolationJournal {
        kind: w.kind,
        anchor: included.tip_hash,
        beneficiary: w.beneficiary,
        reference: [0; 32],
        note_tag: [0; 32],
        value: 0,
        dest_commitment: [0; 32],
        output_index: 0,
    };

    match (&w.detail, w.kind) {
        (
            Detail::Uncommitted {
                vault_ufvk,
                action_index,
                note_recipient,
                note_value,
                note_rho,
                note_rseed,
            },
            Kind::Uncommitted,
        ) => {
            let fvk = vault_fvk(net, vault_ufvk)?;
            let action = bundle.actions().get(*action_index as usize).ok_or(ViolationError::NoSuchAction)?;
            let recipient: [u8; 43] = note_recipient.clone().try_into().map_err(|_| ViolationError::Note)?;
            let recipient =
                Option::<Address>::from(Address::from_raw_address_bytes(&recipient)).ok_or(ViolationError::Note)?;
            let rho = Option::<Rho>::from(Rho::from_bytes(note_rho)).ok_or(ViolationError::Note)?;
            let rseed =
                Option::<RandomSeed>::from(RandomSeed::from_bytes(*note_rseed, &rho)).ok_or(ViolationError::Note)?;
            let note = Option::<Note>::from(Note::from_parts(
                recipient,
                NoteValue::from_raw(*note_value),
                rho,
                rseed,
                NoteVersion::V3,
            ))
            .ok_or(ViolationError::Note)?;
            // The nullifier binds the full viewing key's nk and the note: X spends this note of the
            // vault. Its tag under the same UFVK's K is what Base knows the note by.
            if note.nullifier(&fvk).to_bytes() != action.nullifier().to_bytes() {
                return Err(ViolationError::NotSpent);
            }
            let k = tag_key(vault_ufvk);
            let cmx = ExtractedNoteCommitment::from(note.commitment()).to_bytes();
            journal.reference = payout_ref(&k, &txid);
            journal.note_tag = note_tag(&k, &cmx);
        }
        (
            Detail::WithheldChange {
                vault_ufvk,
                change_tag,
                change_value,
            },
            Kind::WithheldChange,
        ) => {
            let fvk = vault_fvk(net, vault_ufvk)?;
            let k = tag_key(vault_ufvk);
            // Change is the vault's internal-scope note (RT-9): honest payouts send it there.
            let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::Internal));
            for action in bundle.actions() {
                if note_tag(&k, &action.cmx().to_bytes()) != *change_tag {
                    continue;
                }
                let delivered = try_note_decryption(&IronwoodDomain::for_action(action), &ivk, action)
                    .is_some_and(|(n, _, _)| n.value().inner() == *change_value);
                if delivered {
                    return Err(ViolationError::ChangeDelivered);
                }
            }
            journal.reference = payout_ref(&k, &txid);
            journal.note_tag = *change_tag;
            journal.value = *change_value;
        }
        (
            Detail::Misdirected {
                salt,
                receiver,
                ivk,
                output_index,
            },
            Kind::Misdirected,
        ) => {
            let receiver: [u8; 43] = receiver.clone().try_into().map_err(|_| ViolationError::Receiver)?;
            let address =
                Option::<Address>::from(Address::from_raw_address_bytes(&receiver)).ok_or(ViolationError::Receiver)?;
            let ivk: [u8; 64] = ivk.clone().try_into().map_err(|_| ViolationError::Ivk)?;
            let ivk =
                Option::<IncomingViewingKey>::from(IncomingViewingKey::from_bytes(&ivk)).ok_or(ViolationError::Ivk)?;
            // The key must own the receiver, or a wrong key would see nothing in an honest payment.
            if ivk.diversifier_index(&address).is_none() {
                return Err(ViolationError::NotTheClaimant);
            }
            let index = u8::try_from(*output_index).map_err(|_| ViolationError::OutputIndex)?;
            // The claim's own output only (E1): no other output, even to the same receiver, counts;
            // an index X does not have pays nothing.
            let prepared = PreparedIncomingViewingKey::new(&ivk);
            journal.value = bundle.actions().get(usize::from(index)).map_or(0, |a| paid_to(a, &prepared, &address));
            journal.reference = claim_ref(salt, &txid);
            journal.dest_commitment = keccak256(&[salt.as_slice(), &receiver].concat());
            journal.output_index = index;
        }
        (
            Detail::WrongClaimRef {
                tag_key,
                salt,
                receiver,
            },
            Kind::WrongClaimRef,
        ) => {
            let receiver: [u8; 43] = receiver.clone().try_into().map_err(|_| ViolationError::Receiver)?;
            Option::<Address>::from(Address::from_raw_address_bytes(&receiver)).ok_or(ViolationError::Receiver)?;
            // X under the vault's key (the commitment of X is found by it), and the reference an
            // honest commitment of X names this claim by.
            journal.reference = payout_ref(tag_key, &txid);
            journal.note_tag = claim_ref(salt, &txid);
            journal.dest_commitment = keccak256(&[salt.as_slice(), &receiver].concat());
        }
        (Detail::Included { tag_key }, Kind::Included) => {
            // Kind, anchor and X's reference; no bounty, nothing else.
            journal.beneficiary = [0; 20];
            journal.reference = payout_ref(tag_key, &txid);
        }
        _ => return Err(ViolationError::KindMismatch),
    }
    Ok(journal)
}
