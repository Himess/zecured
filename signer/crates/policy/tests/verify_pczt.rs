//! A signer's PCZT check against a malicious coordinator.
//!
//! The honest PCZT for a plan passes; every deviation a coordinator could try (other
//! recipient, split payment, OVK on a third-party output, extra or missing inputs, bad
//! expiry, fee games) is refused. No proving is needed: all checked fields exist once the
//! PCZT is built.

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
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Network},
};
use zecured_policy::{
    ledger::{PayoutPlan, PlannedPayment},
    verify::{ExpectedInput, TxExpectations, VerifyError, verify_payout},
};
use zecured_threshold as threshold;
use zecured_zcash_ops::{
    keys::{VaultSecret, vault_address},
    scan::{IronwoodWitness, NotePosition, ReceivedNote},
    spend::{self, OutputPrivacy, Payment, zip317_fee},
};

const TARGET: u32 = 4_486_600;
const NOTE: u64 = 5_000_000;

fn other() -> Address {
    loop {
        let mut b = [0u8; 32];
        OsRng.fill_bytes(&mut b);
        if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
            return FullViewingKey::from(&sk).address_at(0u32, Scope::External);
        }
    }
}

fn vault_fvk() -> FullViewingKey {
    let keys = threshold::dkg_in_process(OsRng).unwrap();
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    VaultSecret::from_bytes(sk).full_viewing_key(&keys.ak_bytes().unwrap()).unwrap()
}

/// Vault notes committed to one local tree; returns them with the anchor.
fn notes(fvk: &FullViewingKey, values: &[u64]) -> (Vec<ReceivedNote>, orchard::Anchor) {
    let mut rng = rand_010::rng();
    let recipient = vault_address(fvk);
    let mut tree: CommitmentTree<MerkleHashOrchard, 32> = CommitmentTree::empty();
    let mut out: Vec<ReceivedNote> = vec![];
    for (i, &value) in values.iter().enumerate() {
        let note: Note = loop {
            let Some(rho) = Option::<Rho>::from(Rho::from_bytes(&pallas::Base::random(&mut rng).to_repr())) else {
                continue;
            };
            let mut rs = [0u8; 32];
            OsRng.fill_bytes(&mut rs);
            let Some(rseed) = Option::from(RandomSeed::from_bytes(rs, &rho)) else {
                continue;
            };
            if let Some(n) =
                Option::from(Note::from_parts(recipient, NoteValue::from_raw(value), rho, rseed, NoteVersion::V3))
            {
                break n;
            }
        };
        let node = MerkleHashOrchard::from_cmx(&note.commitment().into());
        for prev in &mut out {
            prev.witness.append(node).unwrap();
        }
        tree.append(node).unwrap();
        out.push(ReceivedNote {
            position: NotePosition {
                height: TARGET - 20,
                tx_index: 1,
                action_index: u32::try_from(i).unwrap(),
            },
            txid: TxId::from_bytes([0u8; 32]),
            nullifier: note.nullifier(fvk),
            note,
            recipient,
            scope: Scope::External,
            witness: IronwoodWitness::from_tree(tree.clone()).unwrap(),
            spent_at: None,
        });
    }
    (out, orchard::Anchor::from(tree.root()))
}

fn expect() -> TxExpectations {
    TxExpectations {
        consensus_branch_id: u32::from(BranchId::Nu7),
        min_expiry: TARGET,
        max_expiry: TARGET + 40,
    }
}

fn inputs(notes: &[&ReceivedNote]) -> Vec<ExpectedInput> {
    notes
        .iter()
        .map(|n| ExpectedInput {
            nullifier: n.nullifier.to_bytes(),
            value: n.value(),
        })
        .collect()
}

/// A liquidation-like plan: liquidator (no OVK) gets `seized - fee`, the rest is change.
fn plan(liquidator: Address, seized: u64) -> PayoutPlan {
    let fee = zip317_fee(1, 2);
    PayoutPlan {
        claims: vec![[7; 32]],
        payments: vec![PlannedPayment {
            claim: [7; 32],
            receiver: liquidator.to_raw_address_bytes(),
            value: seized - fee,
            privacy: OutputPrivacy::NoOvk,
        }],
        inputs: 1,
        change: NOTE - seized,
        fee,
    }
}

fn build(fvk: &FullViewingKey, anchor: orchard::Anchor, notes: &[&ReceivedNote], payments: &[Payment]) -> pczt::Pczt {
    spend::build(&Network::TestNetwork, BlockHeight::from_u32(TARGET), fvk, anchor, notes, payments)
        .unwrap()
        .pczt
}

fn honest_payments(p: &PayoutPlan) -> Vec<Payment> {
    p.payments
        .iter()
        .map(|x| Payment {
            recipient: Option::from(Address::from_raw_address_bytes(&x.receiver)).unwrap(),
            value: x.value,
            privacy: x.privacy,
        })
        .collect()
}

#[test]
fn honest_pczt_passes_and_yields_one_spend() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let liq = other();
    let p = plan(liq, 3_500_000);
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let v = verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()).unwrap();
    assert_eq!(v.spends.len(), 1);
    assert_ne!(v.sighash, [0; 32]);
}

#[test]
fn other_recipient_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let mut pay = honest_payments(&p);
    pay[0].recipient = other(); // coordinator pays itself instead
    let pczt = build(&fvk, anchor, &[&ns[0]], &pay);
    assert!(matches!(
        verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()),
        Err(VerifyError::OutputsMismatch)
    ));
}

#[test]
fn split_payment_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let liq = other();
    let p = plan(liq, 3_500_000);
    let mut pay = honest_payments(&p);
    pay[0].value -= 100_000;
    pay.push(Payment {
        recipient: other(),
        value: 100_000,
        privacy: OutputPrivacy::NoOvk,
    });
    let pczt = build(&fvk, anchor, &[&ns[0]], &pay);
    assert!(verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()).is_err());
}

#[test]
fn third_party_output_with_vault_ovk_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let mut pay = honest_payments(&p);
    pay[0].privacy = OutputPrivacy::VaultOvk; // would reveal the liquidator to the borrower
    let pczt = build(&fvk, anchor, &[&ns[0]], &pay);
    assert!(matches!(
        verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()),
        Err(VerifyError::WrongOvk(_))
    ));
}

#[test]
fn spending_an_extra_vault_note_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE, 1_000_000]);
    let p = plan(other(), 3_500_000);
    // The plan spends only the first note; the coordinator also spends the second one
    // (e.g. a non-conforming deposit) and pockets its value as extra change.
    let pczt = build(&fvk, anchor, &[&ns[0], &ns[1]], &honest_payments(&p));
    let res = verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect());
    assert!(
        matches!(
            res,
            Err(VerifyError::UnexpectedSpend(_) | VerifyError::ForeignSpend(_) | VerifyError::WrongValueBalance { .. })
        ),
        "{res:?}"
    );
}

#[test]
fn missing_input_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE, 2_000_000]);
    let mut p = plan(other(), 3_500_000);
    p.inputs = 2;
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    assert!(matches!(
        verify_payout(&pczt, &fvk, &inputs(&[&ns[0], &ns[1]]), &p, &expect()),
        Err(VerifyError::InputsMismatch)
    ));
}

#[test]
fn expiry_outside_window_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let mut tight = expect();
    tight.max_expiry = TARGET + 5; // builder default expiry is target + 40
    assert!(matches!(verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &tight), Err(VerifyError::BadExpiry(_))));
}

#[test]
fn fee_mismatch_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let mut greedy = p.clone();
    greedy.fee += 1;
    assert!(verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &greedy, &expect()).is_err());
}

#[test]
fn wrong_vault_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let stranger = vault_fvk();
    assert!(verify_payout(&pczt, &stranger, &inputs(&[&ns[0]]), &p, &expect()).is_err());
}
