//! Tier 3 violation statements (docs/TIER3.md § 6) on a real Testnet transaction: the deposit of
//! the TEST-ONLY fixture vault (tests/fixtures/TEST-ONLY_vault9_witness.json), with its real
//! inclusion data, stands in for a payout X. Its output to the vault is a note the vault's keys
//! decrypt, which is what an honest change and an honest claim payment look like.

use std::fs;

use orchard::keys::{FullViewingKey, Scope};
use zcash_address::unified::{self, Container, Encoding, Fvk};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
use zecured_deposit_statement::{
    Error, Witness, deposit_tag, keccak256,
    violation::{Detail, Kind, ViolationError, ViolationWitness, claim_ref, paid_to, payout_ref, verify_violation},
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_vault9_witness.json");
const BENEFICIARY: [u8; 20] = [0xbe; 20];
const SALT: [u8; 32] = [0x5a; 32];

fn deposit() -> Witness {
    serde_json::from_slice(&fs::read(FIXTURE).expect("fixture")).expect("witness JSON")
}

/// The fixture's transaction and inclusion, with the given violation detail.
fn violation(kind: Kind, detail: Detail) -> ViolationWitness {
    let d = deposit();
    ViolationWitness {
        kind,
        beneficiary: BENEFICIARY,
        tx: d.tx,
        tx_index: d.tx_index,
        txid_path: d.txid_path,
        block_header: d.block_header,
        leaf: d.leaf,
        mmr_path: d.mmr_path,
        peaks: d.peaks,
        peak_index: d.peak_index,
        tip_header: d.tip_header,
        tip_auth_data_root: d.tip_auth_data_root,
        detail,
    }
}

fn txid() -> [u8; 32] {
    *Transaction::read(&deposit().tx[..], BranchId::Nu7).unwrap().txid().as_ref()
}

/// The vault's deposit note commitment, as the fixture's action carries it.
fn deposit_cmx() -> [u8; 32] {
    let d = deposit();
    let tx = Transaction::read(&d.tx[..], BranchId::Nu7).unwrap();
    tx.ironwood_bundle().unwrap().actions()[d.action_index as usize].cmx().to_bytes()
}

fn vault_fvk() -> FullViewingKey {
    let (_, _, keys) = unified::Ufvk::decode(&deposit().vault_ufvk).unwrap();
    keys.items()
        .iter()
        .find_map(|k| match k {
            Fvk::Orchard(b) => FullViewingKey::from_bytes(b),
            _ => None,
        })
        .unwrap()
}

fn h32(s: &str) -> [u8; 32] {
    let b: Vec<u8> = (0..64).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    b.try_into().unwrap()
}

const ANCHOR: &str = "a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000";
const DEPOSIT_TAG: &str = "d8ad478d40e42d2acd10a3846eeebc19d3ff0146c406e765654395b778ab813e";

// ------------------------------------------------------------------ references

/// The same vectors as zecured_policy (`payout_ref_vector`, `claim_ref_vector`), which were
/// computed with Python's hashlib and `cast keccak`: the guest and the signers agree.
#[test]
fn references_match_the_signers_derivations() {
    assert_eq!(
        payout_ref("uviewtest1a", &[0x42; 32]),
        h32("fe5ff98dd5dd2edcc9663d2ce6c4ed236521d441c34d44ebf2329198d80366df")
    );
    assert_eq!(
        claim_ref(&[0x11; 32], &[0x42; 32]),
        h32("51d4b707b5a706e51817d290a91ae40d0363befaad5013b6658f20cd87ddcdd6")
    );
    assert_eq!(deposit_tag(&deposit().vault_ufvk, &deposit_cmx()), h32(DEPOSIT_TAG));
    // The note tag vector `zecured_policy::derive` pins (red-team review § 4: both implementations).
    assert_eq!(
        deposit_tag("uviewtest1a", &[7; 32]),
        h32("add8548e79e01888ca52988bc44b01d0be125fdc5569db7597bd040f335f25aa")
    );
}

/// Cross-language vectors (red-team review § 4): the F3 and F4 journals of the fixture standing in
/// for a payout X (salt 0x5a * 32, the vault's own receiver, output 1), byte for byte what
/// `contracts/test/fork/RedTeamRegressions.t.sol` proves with (`test_vector_*`).
#[test]
fn journals_are_pinned_across_languages() {
    let receiver = deposit().note_recipient;
    let ivk = vault_fvk().to_ivk(Scope::External).to_bytes().to_vec();
    let words = |j: zecured_deposit_statement::violation::ViolationJournal| -> Vec<String> {
        j.abi_encode().chunks(32).map(|w| w.iter().map(|b| format!("{b:02x}")).collect()).collect()
    };
    let f3 = words(verify_violation(&misdirected(receiver.clone(), ivk, paid_action())).unwrap());
    let f4 = words(verify_violation(&wrong_claim_ref(deposit().vault_ufvk, receiver)).unwrap());
    const ANCHOR_W: &str = "a27b5e3c30e3dc292138c751bf10c85fd842fb759be90c7e9397d0477d0b0000";
    const BENEFICIARY_W: &str = "000000000000000000000000bebebebebebebebebebebebebebebebebebebebe";
    const CLAIM_REF: &str = "62840e8e15455e60905714b51e12fa370ec8cc849cd2cbf30b696ce139950869";
    const DEST: &str = "0144dce00498a1cb4e3d04161414ccf1cd42b09f70fe83ddcf745973c522a0a7";
    let word = |n: u64| format!("{n:064x}");
    assert_eq!(
        f3,
        [
            word(3),
            ANCHOR_W.into(),
            BENEFICIARY_W.into(),
            CLAIM_REF.into(),
            word(0),
            word(1_000_000),
            DEST.into(),
            word(1)
        ]
    );
    assert_eq!(
        f4,
        [
            word(4),
            ANCHOR_W.into(),
            BENEFICIARY_W.into(),
            "46b19981da842f7bf81342fa8db82ecd88933c3f9d777816c80bc712ef931496".into(),
            CLAIM_REF.into(),
            word(0),
            DEST.into(),
            word(0),
        ]
    );
}

// ------------------------------------------------------------------ F2: withheld change

fn withheld(change_tag: [u8; 32], change_value: u64) -> ViolationWitness {
    violation(
        Kind::WithheldChange,
        Detail::WithheldChange {
            vault_ufvk: deposit().vault_ufvk,
            change_tag,
            change_value,
        },
    )
}

/// RT-9: change is the vault's internal-scope note. The fixture's output pays the vault's external
/// (published) address, so as change it is not delivered: the signers would count it as a deposit
/// to refund. The real payout's internal-scope change is delivered (`f2_the_real_payouts_change_is_delivered`).
#[test]
fn f2_change_to_the_external_scope_is_not_delivered() {
    let j = verify_violation(&withheld(h32(DEPOSIT_TAG), 1_000_000)).expect("the external scope is not change");
    assert_eq!((j.note_tag, j.value), (h32(DEPOSIT_TAG), 1_000_000));
}

#[test]
fn f2_a_change_of_another_value_or_tag_is_proven_withheld() {
    let j = verify_violation(&withheld(h32(DEPOSIT_TAG), 999_999)).expect("the value differs");
    assert_eq!(j.kind, Kind::WithheldChange);
    assert_eq!(j.anchor, h32(ANCHOR));
    assert_eq!(j.beneficiary, BENEFICIARY);
    assert_eq!(j.reference, payout_ref(&deposit().vault_ufvk, &txid()));
    assert_eq!(j.note_tag, h32(DEPOSIT_TAG));
    assert_eq!(j.value, 999_999);
    assert_eq!(j.dest_commitment, [0; 32]);

    let j = verify_violation(&withheld([7; 32], 1_000_000)).expect("no output has that tag");
    assert_eq!(j.note_tag, [7; 32]);
}

/// The reference is under the witness's UFVK: another viewing key names another payout, which no
/// commitment on Base has (the slasher refuses it), so a wrong key cannot fake a withheld change.
#[test]
fn f2_the_reference_is_keyed_by_the_vault() {
    let mut w = withheld([7; 32], 1);
    let Detail::WithheldChange { vault_ufvk, .. } = &mut w.detail else {
        unreachable!()
    };
    let other = "uviewtest1rl58wrlhq2eqws08m2jxqg0gjpn2yd7me5f9cfkwmf4axnq0vjvjgvjs5t70tsny62rxklz3ha8t5cr8k30jltjmjr0xxsdhpqeqpf55w3dx0mracz99uvrvyf4hpsfxs2nc5jykraqvnt22zpkdce6tcj85lkty9lcpduhl72vz80l8ygn0q3sxn04w8";
    *vault_ufvk = other.to_owned();
    let j = verify_violation(&w).unwrap();
    assert_ne!(j.reference, payout_ref(&deposit().vault_ufvk, &txid()));
    assert_eq!(j.reference, payout_ref(other, &txid()));
}

// ------------------------------------------------------------------ F4: wrong claim reference

fn wrong_claim_ref(vault_ufvk: String, receiver: Vec<u8>) -> ViolationWitness {
    violation(
        Kind::WrongClaimRef,
        Detail::WrongClaimRef {
            vault_ufvk,
            salt: SALT,
            receiver,
        },
    )
}

/// F4 (RT-1) gives the reference an honest commitment of X names the claim by, and names X by its
/// payout reference under the vault's key, which is how the slasher finds the commitment of X.
#[test]
fn f4_gives_the_reference_an_honest_commitment_names_the_claim_by() {
    let receiver = deposit().note_recipient;
    let j = verify_violation(&wrong_claim_ref(deposit().vault_ufvk, receiver.clone())).unwrap();
    assert_eq!(j.kind, Kind::WrongClaimRef);
    assert_eq!(j.reference, payout_ref(&deposit().vault_ufvk, &txid()));
    assert_eq!(j.note_tag, claim_ref(&SALT, &txid()));
    assert_eq!(j.dest_commitment, keccak256(&[SALT.as_slice(), &receiver].concat()));
    assert_eq!((j.value, j.output_index), (0, 0));
}

#[test]
fn f4_needs_the_vaults_key_and_a_receiver() {
    let receiver = deposit().note_recipient;
    assert_eq!(verify_violation(&wrong_claim_ref("uviewtest1x".into(), receiver)), Err(ViolationError::Ufvk));
    assert_eq!(verify_violation(&wrong_claim_ref(deposit().vault_ufvk, vec![1; 42])), Err(ViolationError::Receiver));
}

// ------------------------------------------------------------------ F3: misdirected payment

fn misdirected(receiver: Vec<u8>, ivk: Vec<u8>, output_index: u32) -> ViolationWitness {
    violation(
        Kind::Misdirected,
        Detail::Misdirected {
            salt: SALT,
            receiver,
            ivk,
            output_index,
        },
    )
}

/// The action of the fixture transaction whose output pays the vault (the "claim's output").
fn paid_action() -> u32 {
    deposit().action_index
}

/// The claimant (here the vault, which the transaction pays) learns what its claim's output paid
/// it: the slasher compares that with what the commitment promised the claim.
#[test]
fn f3_the_claimant_proves_what_its_output_paid() {
    let receiver = deposit().note_recipient;
    let ivk = vault_fvk().to_ivk(Scope::External).to_bytes().to_vec();
    let j = verify_violation(&misdirected(receiver.clone(), ivk, paid_action())).expect("the vault's own key");
    assert_eq!(j.kind, Kind::Misdirected);
    assert_eq!(j.reference, claim_ref(&SALT, &txid()));
    assert_eq!(j.dest_commitment, keccak256(&[SALT.as_slice(), &receiver].concat()));
    assert_eq!(j.value, 1_000_000);
    assert_eq!(j.output_index, u8::try_from(paid_action()).unwrap());
    assert_eq!(j.note_tag, [0; 32]);
}

/// E1: only the claim's own output counts. Another action of the same transaction (here the
/// depositor's change), or an index the transaction does not have, paid the claim nothing.
#[test]
fn f3_only_the_claims_own_output_counts() {
    let receiver = deposit().note_recipient;
    let ivk = vault_fvk().to_ivk(Scope::External).to_bytes().to_vec();
    let other = 1 - paid_action(); // two actions
    assert_eq!(verify_violation(&misdirected(receiver.clone(), ivk.clone(), other)).unwrap().value, 0);
    assert_eq!(verify_violation(&misdirected(receiver.clone(), ivk.clone(), 7)).unwrap().value, 0);
    assert_eq!(verify_violation(&misdirected(receiver, ivk, 256)), Err(ViolationError::OutputIndex));
}

/// E1: the output must pay the exact receiver of the claim's commitment, not another address of
/// the same wallet: a payment to another diversified address is a deviation from the plan.
#[test]
fn f3_another_address_of_the_same_wallet_is_not_the_receiver() {
    let fvk = vault_fvk();
    let other = fvk.address_at(1u32, Scope::External).to_raw_address_bytes().to_vec();
    assert_ne!(other, deposit().note_recipient);
    let ivk = fvk.to_ivk(Scope::External).to_bytes().to_vec();
    let j = verify_violation(&misdirected(other, ivk, paid_action())).expect("the key owns that address too");
    assert_eq!(j.value, 0, "the output pays the wallet, but not this receiver");
}

/// E1 (c): a note counts only if its plaintext matches the action's note commitment. The same
/// ciphertext under another action's commitment (an output a wallet would reject) pays nothing.
#[test]
fn f3_a_note_that_does_not_match_its_commitment_pays_nothing() {
    use orchard::{Action, Address, keys::PreparedIncomingViewingKey};
    let d = deposit();
    let tx = Transaction::read(&d.tx[..], BranchId::Nu7).unwrap();
    let actions = tx.ironwood_bundle().unwrap().actions();
    let honest = &actions[d.action_index as usize];
    let other = &actions[1 - d.action_index as usize];
    let ivk = PreparedIncomingViewingKey::new(&vault_fvk().to_ivk(Scope::External));
    let receiver: [u8; 43] = d.note_recipient.clone().try_into().unwrap();
    let receiver = Address::from_raw_address_bytes(&receiver).unwrap();
    assert_eq!(paid_to(honest, &ivk, &receiver), 1_000_000);
    let forged = Action::from_parts(
        *honest.nullifier(),
        honest.rk().clone(),
        *other.cmx(),
        honest.encrypted_note().clone(),
        honest.cv_net().clone(),
        honest.authorization().clone(),
    )
    .unwrap();
    assert_eq!(paid_to(&forged, &ivk, &receiver), 0);
}

#[test]
fn f3_a_key_that_does_not_own_the_receiver_proves_nothing() {
    let receiver = deposit().note_recipient;
    // The vault's internal key owns its change address, not the deposit receiver.
    let internal = vault_fvk().to_ivk(Scope::Internal).to_bytes().to_vec();
    let p = paid_action();
    assert_eq!(verify_violation(&misdirected(receiver.clone(), internal, p)), Err(ViolationError::NotTheClaimant));
    assert_eq!(
        verify_violation(&misdirected(receiver[..42].to_vec(), vec![0; 64], p)),
        Err(ViolationError::Receiver)
    );
    assert_eq!(verify_violation(&misdirected(receiver, vec![0; 63], p)), Err(ViolationError::Ivk));
}

// ------------------------------------------------------------------ F1: uncommitted spend

/// The fixture transaction creates the vault's note but does not spend it: no F1.
#[test]
fn f1_a_note_the_transaction_does_not_spend_proves_nothing() {
    let d = deposit();
    let tx = Transaction::read(&d.tx[..], BranchId::Nu7).unwrap();
    let action = &tx.ironwood_bundle().unwrap().actions()[d.action_index as usize];
    let w = violation(
        Kind::Uncommitted,
        Detail::Uncommitted {
            vault_ufvk: d.vault_ufvk.clone(),
            action_index: d.action_index,
            note_recipient: d.note_recipient.clone(),
            note_value: d.note_value,
            note_rho: action.nullifier().to_bytes(), // an output note's rho: its action's nullifier
            note_rseed: d.note_rseed,
        },
    );
    assert_eq!(verify_violation(&w), Err(ViolationError::NotSpent));
}

// ------------------------------------------------------------------ common

#[test]
fn kind_and_detail_must_agree() {
    let mut w = withheld([7; 32], 1);
    w.kind = Kind::Misdirected;
    assert_eq!(verify_violation(&w), Err(ViolationError::KindMismatch));
    w.kind = Kind::WrongClaimRef;
    assert_eq!(verify_violation(&w), Err(ViolationError::KindMismatch));
}

#[test]
fn the_transaction_must_be_in_the_anchored_history() {
    let mut w = withheld([7; 32], 1);
    w.txid_path[0][0] ^= 1;
    assert_eq!(verify_violation(&w), Err(ViolationError::Inclusion(Error::TxidNotInBlock)));
    let mut w = withheld([7; 32], 1);
    w.tip_auth_data_root[0] ^= 1;
    assert_eq!(verify_violation(&w), Err(ViolationError::Inclusion(Error::HistoryNotCommitted)));
}

/// The journal is `abi.encode(ViolationJournal)` of IZecuredSlasher.sol: kind, anchor,
/// beneficiary, reference, note tag, value, destination commitment, one word each.
#[test]
fn the_journal_is_the_slashers_abi() {
    let j = verify_violation(&withheld(h32(DEPOSIT_TAG), 999_999)).unwrap();
    let enc = j.abi_encode();
    assert_eq!(enc.len(), 8 * 32);
    assert_eq!(enc[31], 2);
    assert!(enc[..31].iter().all(|b| *b == 0));
    assert_eq!(enc[32..64], h32(ANCHOR));
    assert_eq!(enc[64..76], [0; 12]);
    assert_eq!(enc[76..96], BENEFICIARY);
    assert_eq!(enc[96..128], j.reference);
    assert_eq!(enc[128..160], h32(DEPOSIT_TAG));
    assert_eq!(u64::from_be_bytes(enc[184..192].try_into().unwrap()), 999_999);
    assert_eq!(enc[160..184], [0; 24]);
    assert_eq!(enc[192..224], [0; 32]);
    assert_eq!(enc[224..256], [0; 32], "output index, F3 only");
}

// ------------------------------------------------------------------ F1 on a real vault spend

const F1_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/TEST-ONLY_vault9_f1_witness.json");

fn f1() -> ViolationWitness {
    serde_json::from_slice(&fs::read(F1_FIXTURE).expect("fixture")).expect("violation witness JSON")
}

/// The first payout of the fixture vault spends its deposit note: as a violation proof, X
/// spends the lineage note Base knows by the deposit tag. (That payout was committed in its
/// time; under Tier 3 the slasher would find its commitment and refuse the case.)
#[test]
fn f1_a_real_payout_spends_the_vaults_deposit_note() {
    let w = f1();
    let x = Transaction::read(&w.tx[..], BranchId::Nu7).unwrap();
    let j = verify_violation(&w).expect("the payout spends the deposit note");
    assert_eq!(j.kind, Kind::Uncommitted);
    assert_eq!(j.anchor, h32("e47bcc107dbac24aa6026bdc610e8605642035559a33ec4e1fb6e4423f2a0000"));
    assert_eq!(j.beneficiary, [0xbe; 20]);
    assert_eq!(j.note_tag, h32(DEPOSIT_TAG), "the deposit tag the Registry activated the vault with");
    assert_eq!(j.reference, payout_ref(&deposit().vault_ufvk, x.txid().as_ref()));
    assert_eq!((j.value, j.dest_commitment), (0, [0; 32]));
}

/// Every part of the opening is bound by the nullifier: change one, and X spends no such note.
#[test]
fn f1_the_opening_is_bound() {
    for change in 0..4 {
        let mut w = f1();
        let Detail::Uncommitted {
            note_value,
            note_rho,
            note_rseed,
            action_index,
            ..
        } = &mut w.detail
        else {
            unreachable!()
        };
        match change {
            0 => *note_value += 1,
            1 => note_rseed[0] ^= 1,
            2 => *action_index ^= 1,
            _ => note_rho[1] ^= 1,
        }
        assert!(
            matches!(verify_violation(&w), Err(ViolationError::NotSpent | ViolationError::Note)),
            "change {change}"
        );
    }
}

/// The nullifier key comes from the witness's UFVK: another vault's key derives another
/// nullifier, so no one can claim a spend of a note of a vault whose key it does not hold.
#[test]
fn f1_another_vaults_key_proves_nothing() {
    let mut w = f1();
    let Detail::Uncommitted { vault_ufvk, .. } = &mut w.detail else {
        unreachable!()
    };
    *vault_ufvk = "uviewtest1rl58wrlhq2eqws08m2jxqg0gjpn2yd7me5f9cfkwmf4axnq0vjvjgvjs5t70tsny62rxklz3ha8t5cr8k30jltjmjr0xxsdhpqeqpf55w3dx0mracz99uvrvyf4hpsfxs2nc5jykraqvnt22zpkdce6tcj85lkty9lcpduhl72vz80l8ygn0q3sxn04w8".to_owned();
    assert_eq!(verify_violation(&w), Err(ViolationError::NotSpent));
}

/// F2 on the same real payout: it returns its change to the vault's internal address, which the
/// vault's internal incoming viewing key decrypts.
#[test]
fn f2_the_real_payouts_change_is_delivered() {
    let w = f1();
    let x = Transaction::read(&w.tx[..], BranchId::Nu7).unwrap();
    let fvk = vault_fvk();
    let ivk = orchard::keys::PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::Internal));
    let (cmx, value) = x
        .ironwood_bundle()
        .unwrap()
        .actions()
        .iter()
        .find_map(|a| {
            zcash_note_encryption::try_note_decryption(
                &orchard::note_encryption::IronwoodDomain::for_action(a),
                &ivk,
                a,
            )
            .map(|(n, _, _)| (a.cmx().to_bytes(), n.value().inner()))
        })
        .expect("the payout's change");
    let ufvk = deposit().vault_ufvk;
    let change_tag = deposit_tag(&ufvk, &cmx);
    let withheld = |change_value| ViolationWitness {
        kind: Kind::WithheldChange,
        detail: Detail::WithheldChange {
            vault_ufvk: ufvk.clone(),
            change_tag,
            change_value,
        },
        ..f1()
    };
    assert_eq!(verify_violation(&withheld(value)), Err(ViolationError::ChangeDelivered));
    let j = verify_violation(&withheld(value + 1)).expect("not the committed value");
    assert_eq!(j.reference, payout_ref(&ufvk, x.txid().as_ref()));
}
