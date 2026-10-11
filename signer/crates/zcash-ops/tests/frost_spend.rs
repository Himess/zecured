//! Offline end-to-end check of the vault spend pipeline: a FROST 3-of-5 vault spends an
//! Ironwood note to two payees (one with the vault OVK, one with `ovk = None`) plus change,
//! and the extracted v6 transaction passes full verification (Halo 2 proof, spend
//! authorization signatures, binding signature).
//!
//! The note and anchor come from a local tree, so the transaction is never broadcast.

use incrementalmerkletree::frontier::CommitmentTree;
use orchard::{
    Address, Note, NoteVersion,
    keys::{FullViewingKey, Scope, SpendingKey},
    note::{RandomSeed, Rho},
    tree::MerkleHashOrchard,
    value::NoteValue,
};
use pasta_curves::{
    group::ff::{Field, PrimeField},
    pallas,
};
use rand_core_06::{OsRng, RngCore};
use zcash_primitives::transaction::TxVersion;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Network},
};
use zecured_threshold as threshold;
use zecured_zcash_ops::{
    keys::{VaultSecret, vault_address},
    scan::{IronwoodWitness, NotePosition, ReceivedNote},
    spend::{self, OutputPrivacy, Payment},
    tx::TxShape,
    witness,
};

/// A height after NU7 activation on testnet (4,465,026).
const TARGET_HEIGHT: u32 = 4_486_500;
const NOTE_VALUE: u64 = 1_000_000;
const PAY_USER: u64 = 400_000;
const PAY_THIRD: u64 = 300_000;

fn random_note(recipient: Address, value: u64) -> Note {
    let mut rng = rand_010::rng();
    loop {
        let rho = Rho::from_bytes(&pallas::Base::random(&mut rng).to_repr());
        let Some(rho) = Option::from(rho) else { continue };
        let mut rseed = [0u8; 32];
        OsRng.fill_bytes(&mut rseed);
        let Some(rseed) = Option::from(RandomSeed::from_bytes(rseed, &rho)) else {
            continue;
        };
        if let Some(note) =
            Option::from(Note::from_parts(recipient, NoteValue::from_raw(value), rho, rseed, NoteVersion::V3))
        {
            return note;
        }
    }
}

fn other_address() -> (FullViewingKey, Address) {
    loop {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
            let fvk = FullViewingKey::from(&sk);
            let addr = fvk.address_at(0u32, Scope::External);
            return (fvk, addr);
        }
    }
}

struct Vault {
    keys: threshold::VaultKeys,
    fvk: FullViewingKey,
}

fn new_vault() -> Vault {
    let keys = threshold::dkg_in_process(OsRng).unwrap();
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    let fvk = VaultSecret::from_bytes(sk).full_viewing_key(&keys.ak_bytes().unwrap()).unwrap();
    Vault { keys, fvk }
}

/// A vault note committed into a fresh local tree, plus that tree's anchor.
fn funded(vault: &Vault) -> (ReceivedNote, orchard::Anchor) {
    let recipient = vault_address(&vault.fvk);
    let note = random_note(recipient, NOTE_VALUE);
    let mut tree: CommitmentTree<MerkleHashOrchard, 32> = CommitmentTree::empty();
    tree.append(MerkleHashOrchard::from_cmx(&note.commitment().into())).unwrap();
    let witness = IronwoodWitness::from_tree(tree.clone()).unwrap();
    let received = ReceivedNote {
        position: NotePosition {
            height: TARGET_HEIGHT - 10,
            tx_index: 1,
            action_index: 0,
        },
        txid: TxId::from_bytes([0u8; 32]),
        nullifier: note.nullifier(&vault.fvk),
        note,
        recipient,
        scope: Scope::External,
        witness,
        spent_at: None,
        spent_in: None,
    };
    (received, orchard::Anchor::from(tree.root()))
}

#[test]
fn frost_vault_spend_verifies_and_hides_third_party_output() {
    let vault = new_vault();
    let (note, anchor) = funded(&vault);
    let (user_fvk, user_addr) = other_address();
    let (third_fvk, third_addr) = other_address();

    let payments = [
        Payment {
            recipient: user_addr,
            value: PAY_USER,
            privacy: OutputPrivacy::VaultOvk,
        },
        Payment {
            recipient: third_addr,
            value: PAY_THIRD,
            privacy: OutputPrivacy::NoOvk,
        },
    ];
    let unsigned = spend::build(
        &Network::TestNetwork,
        BlockHeight::from_u32(TARGET_HEIGHT),
        &vault.fvk,
        anchor,
        &[&note],
        &payments,
    )
    .unwrap();
    assert_eq!(unsigned.fee, spend::zip317_fee(1, 3));
    assert_eq!(unsigned.change, NOTE_VALUE - PAY_USER - PAY_THIRD - unsigned.fee);

    let pk = spend::proving_key();
    let pczt = spend::finalize_and_prove(unsigned.pczt, &pk).unwrap();
    let (sighash, requests) = spend::signing_requests(&pczt, &unsigned.spend_action_indices).unwrap();
    assert_eq!(requests.len(), 1);

    // A signature from another vault's key is rejected when applied.
    let impostor = new_vault();
    let bad = threshold::sign_in_process(
        &impostor.keys.key_packages,
        &impostor.keys.public_key_package,
        &[1, 2, 3],
        &sighash,
        &requests[0].alpha,
    )
    .unwrap();
    assert!(spend::apply_signatures(pczt.clone(), &[(requests[0].action_index, bad)]).is_err());

    // Three of the five real signers authorize the spend.
    let sig = threshold::sign_in_process(
        &vault.keys.key_packages,
        &vault.keys.public_key_package,
        &[1, 3, 5],
        &sighash,
        &requests[0].alpha,
    )
    .unwrap();
    let pczt = spend::apply_signatures(pczt, &[(requests[0].action_index, sig)]).unwrap();
    let tx = spend::extract(pczt).expect("fully verified transaction");

    // For a v6 transaction without transparent parts, the shielded sighash the signers sign is
    // the transaction id (ZIP 244: the transparent sighash digest is the transparent txid
    // digest), so a node can recognise the transaction it signed when it is mined (H-1).
    assert_eq!(tx.txid().as_ref(), &sighash);

    // C-1: the agreed transaction bytes must create the scanned note at its action index.
    let raw = spend::serialize(&tx).unwrap();
    let cmx0 = tx.ironwood_bundle().expect("Ironwood bundle").actions()[0].cmx().to_bytes();
    let net = Network::TestNetwork;
    witness::check_note_in_tx(&net, &tx.txid(), &raw, TARGET_HEIGHT, 0, &cmx0).unwrap();
    assert!(witness::check_note_in_tx(&net, &tx.txid(), &raw, TARGET_HEIGHT, 1, &cmx0).is_err());
    assert!(witness::check_note_in_tx(&net, &tx.txid(), &raw, TARGET_HEIGHT, 9, &cmx0).is_err());
    assert!(witness::check_note_in_tx(&net, &TxId::from_bytes([9; 32]), &raw, TARGET_HEIGHT, 0, &cmx0).is_err());

    assert_eq!(tx.version(), TxVersion::V6);
    assert_eq!(tx.consensus_branch_id(), BranchId::Nu7);
    let shape = TxShape::of(&tx);
    assert!(shape.is_ironwood_only(), "{shape:?}");

    let bundle = tx.ironwood_bundle().expect("Ironwood bundle");
    // The vault's external OVK recovers only the borrower payout; the internal OVK only
    // the change; nothing recovers the third-party output (ovk = None).
    let ext = bundle.recover_outputs_with_ovks(&[vault.fvk.to_ovk(Scope::External)]);
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0].3, user_addr);
    assert_eq!(ext[0].2.value().inner(), PAY_USER);
    let int = bundle.recover_outputs_with_ovks(&[vault.fvk.to_ovk(Scope::Internal)]);
    assert_eq!(int.len(), 1);
    assert_eq!(int[0].2.value().inner(), unsigned.change);
    assert!(
        bundle
            .recover_outputs_with_ovks(&[vault.fvk.to_ovk(Scope::External), vault.fvk.to_ovk(Scope::Internal)])
            .iter()
            .all(|r| r.3 != third_addr)
    );

    // The recipients themselves can decrypt their outputs.
    let got_user = bundle.decrypt_outputs_with_keys(&[user_fvk.to_ivk(Scope::External)]);
    let got_third = bundle.decrypt_outputs_with_keys(&[third_fvk.to_ivk(Scope::External)]);
    assert_eq!(got_user.len(), 1);
    assert_eq!(got_user[0].2.value().inner(), PAY_USER);
    assert_eq!(got_third.len(), 1);
    assert_eq!(got_third[0].2.value().inner(), PAY_THIRD);
}
