//! A signer's PCZT check against a malicious coordinator.
//!
//! The honest PCZT for a plan passes; every deviation a coordinator could try (other
//! recipient, split payment, OVK on a third-party output, extra or missing inputs, bad
//! expiry, fee games, an undecryptable third-party output, an unfinalized PCZT) is refused.
//! No proving is needed: all checked fields exist once the PCZT is built and IO-finalized.

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
use pczt::roles::{
    io_finalizer::IoFinalizer,
    low_level_signer::{OrchardParseError, Signer as LowLevelSigner},
};
use rand_core_06::{OsRng, RngCore};
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, BranchId, Network},
};
use zecured_policy::{
    ledger::{
        Claim, ClaimKind, ClaimState, MAX_CLAIMS_PER_PAYOUT, NextPayout, PayoutInput, PayoutPlan, PlannedPayment,
        Recipient, next_payout,
    },
    verify::{ExpectedInput, TxExpectations, VerifyError, verify_payout},
};
use zecured_threshold as threshold;
use zecured_zcash_ops::{
    keys::{VaultSecret, vault_address},
    scan::{IronwoodWitness, NotePosition, ReceivedNote},
    spend::{self, OutputPrivacy, PAYOUT_EXPIRY_DELTA, Payment, zip317_fee},
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
            spent_in: None,
        });
    }
    (out, orchard::Anchor::from(tree.root()))
}

fn expect() -> TxExpectations {
    TxExpectations {
        consensus_branch_id: u32::from(BranchId::Nu7),
        min_expiry: TARGET,
        max_expiry: TARGET + PAYOUT_EXPIRY_DELTA,
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
        input: PayoutInput::Main,
    }
}

/// The PCZT as the coordinator sends it: built and IO-finalized (dummy spends signed).
fn build(fvk: &FullViewingKey, anchor: orchard::Anchor, notes: &[&ReceivedNote], payments: &[Payment]) -> pczt::Pczt {
    IoFinalizer::new(build_unfinalized(fvk, anchor, notes, payments))
        .finalize_io(rand_010::rng())
        .unwrap()
}

fn build_unfinalized(
    fvk: &FullViewingKey,
    anchor: orchard::Anchor,
    notes: &[&ReceivedNote],
    payments: &[Payment],
) -> pczt::Pczt {
    spend::build(&Network::TestNetwork, BlockHeight::from_u32(TARGET), fvk, anchor, notes, payments)
        .unwrap()
        .pczt
}

/// `(epk, enc_ciphertext)` of the action whose output pays `to` a non-zero value.
fn output_ciphertext(pczt: &pczt::Pczt, to: &Address) -> ([u8; 32], [u8; 580]) {
    let mut found = None;
    LowLevelSigner::new(pczt.clone())
        .sign_ironwood_with(|_, bundle, _| -> Result<(), OrchardParseError> {
            for a in bundle.actions() {
                let paid = a.output().value().is_some_and(|v| v.inner() > 0);
                if paid && a.output().recipient().as_ref() == Some(to) {
                    let e = a.output().encrypted_note();
                    found = Some((e.epk_bytes, e.enc_ciphertext.0));
                }
            }
            Ok(())
        })
        .unwrap();
    found.expect("an output paying this address")
}

/// The index of the action whose output pays `to`.
fn output_action(pczt: &pczt::Pczt, to: &Address) -> usize {
    let mut found = None;
    LowLevelSigner::new(pczt.clone())
        .sign_ironwood_with(|_, bundle, _| -> Result<(), OrchardParseError> {
            for (i, a) in bundle.actions().iter().enumerate() {
                let paid = a.output().value().is_some_and(|v| v.inner() > 0);
                if paid && a.output().recipient().as_ref() == Some(to) {
                    found = Some(i);
                }
            }
            Ok(())
        })
        .unwrap();
    found.expect("an output paying this address")
}

/// The note commitment of the output paying `to`.
fn output_cmx(pczt: &pczt::Pczt, to: &Address) -> [u8; 32] {
    let mut found = None;
    LowLevelSigner::new(pczt.clone())
        .sign_ironwood_with(|_, bundle, _| -> Result<(), OrchardParseError> {
            for a in bundle.actions() {
                let paid = a.output().value().is_some_and(|v| v.inner() > 0);
                if paid && a.output().recipient().as_ref() == Some(to) {
                    found = Some(a.output().cmx().to_bytes());
                }
            }
            Ok(())
        })
        .unwrap();
    found.expect("an output paying this address")
}

/// The PCZT a malicious coordinator would send after rewriting the ephemeral key and note
/// ciphertext of the output paying `to`. The v2 encoding normally stores an honest ciphertext
/// as its memo plaintext (re-encrypted and checked when parsed); a rewritten one has to be
/// stored verbatim, which the parser accepts as is.
fn with_output(pczt: pczt::Pczt, to: &Address, tamper: impl Fn(&mut [u8; 32], &mut [u8; 580])) -> pczt::Pczt {
    let (epk, mut ct) = output_ciphertext(&pczt, to);
    let mut new_epk = epk;
    tamper(&mut new_epk, &mut ct);
    let bytes = pczt.serialize().unwrap();
    let start = bytes.windows(32).position(|w| w == epk.as_slice()).expect("epk in the encoding");
    let at = start + 32;
    // `EncCiphertext`: variant 0 = Encrypted(Vec<u8>), 1 = MemoPlaintext(Vec<u8>).
    let old_len = match bytes[at] {
        0 => {
            assert_eq!(bytes[at + 1..at + 3], [0xC4, 0x04], "580 as a varint");
            3 + 580
        }
        1 => {
            let memo_len = usize::from(bytes[at + 1]);
            assert!(memo_len < 0x80, "single-byte varint");
            2 + memo_len
        }
        tag => panic!("unexpected enc_ciphertext variant {tag}"),
    };
    let mut patched = bytes[..start].to_vec();
    patched.extend_from_slice(&new_epk);
    patched.extend_from_slice(&[0x00, 0xC4, 0x04]);
    patched.extend_from_slice(&ct);
    patched.extend_from_slice(&bytes[at + old_len..]);
    pczt::Pczt::parse(&patched).expect("still a well-formed PCZT")
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
    // Tier 3: the change note's commitment, whose tag becomes the vault's next lineage note.
    let change = fvk.address_at(0u32, Scope::Internal);
    assert_eq!(v.change_cmx, Some(output_cmx(&pczt, &change)));
    assert_ne!(v.change_cmx, Some(output_cmx(&pczt, &liq)));
    // E1: the action that pays the plan's one payment (the liquidator) is the one committed for it.
    assert_eq!(v.payment_actions, vec![output_action(&pczt, &liq)]);
}

/// Red-team review § 4: in a two-claim payout (a close: the borrower, then the market maker) each
/// payment's committed output is the action that pays exactly that receiver that value, so F3 checks
/// the right output for both claims.
#[test]
fn a_close_pairs_payments_map_to_their_own_actions() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let (user, mm) = (other(), other());
    let fee = zip317_fee(1, 3);
    let p = PayoutPlan {
        claims: vec![[3; 32], [4; 32]],
        payments: vec![
            PlannedPayment {
                claim: [3; 32],
                receiver: user.to_raw_address_bytes(),
                value: 1_200_000 - fee,
                privacy: OutputPrivacy::VaultOvk,
            },
            PlannedPayment {
                claim: [4; 32],
                receiver: mm.to_raw_address_bytes(),
                value: 2_300_000,
                privacy: OutputPrivacy::NoOvk,
            },
        ],
        inputs: 1,
        change: NOTE - 1_200_000 - 2_300_000,
        fee,
        input: PayoutInput::Main,
    };
    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let v = verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()).unwrap();
    assert_eq!(v.payment_actions, vec![output_action(&pczt, &user), output_action(&pczt, &mm)]);
    assert_ne!(v.payment_actions[0], v.payment_actions[1]);
}

/// Review H-06 / İ-01: the planner's payout of eight claims (the Registry's most per commitment)
/// builds and verifies like any other: each payment maps to its own action, the first claim bears
/// the fee of nine outputs, the change returns to the vault; the same transaction without one of
/// the eight is refused.
#[test]
fn a_planned_payout_of_eight_claims_builds_and_verifies() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let receivers: Vec<Address> = (0..10).map(|_| other()).collect();
    let recipients: Vec<Recipient> = receivers
        .iter()
        .zip(1u8..)
        .map(|(r, i)| Recipient {
            receiver: r.to_raw_address_bytes(),
            salt: [i; 32],
        })
        .collect();
    let claims: Vec<Claim> = recipients
        .iter()
        .zip(1u8..)
        .map(|(r, i)| Claim {
            id: [i; 32],
            kind: ClaimKind::Liquidation,
            amount: 300_000,
            commitment: r.commitment(),
            order: (100 + u64::from(i), 0),
            created_at: 1_000,
            partner: None,
            state: ClaimState::Unpaid,
            refund_tag: None,
            bond_paid: false,
        })
        .collect();
    let recipient = |c: &[u8; 32]| recipients.iter().find(|r| r.commitment() == *c).copied();
    let NextPayout::Pay(p) = next_payout(&claims, recipient, NOTE, 1, 2_000, &[]).unwrap() else {
        panic!("a payout")
    };
    assert_eq!(p.claims.len(), MAX_CLAIMS_PER_PAYOUT);
    assert_eq!(p.fee, zip317_fee(1, MAX_CLAIMS_PER_PAYOUT + 1));
    assert_eq!(p.change, NOTE - 8 * 300_000);

    let pczt = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let v = verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()).unwrap();
    assert_eq!(v.spends.len(), 1);
    let actions: Vec<usize> = receivers[..8].iter().map(|r| output_action(&pczt, r)).collect();
    assert_eq!(v.payment_actions, actions, "each claim names the action paying its receiver");
    let mut distinct = actions.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 8);
    let change = fvk.address_at(0u32, Scope::Internal);
    assert_eq!(v.change_cmx, Some(output_cmx(&pczt, &change)));

    // The coordinator leaves one of the eight claims out of the transaction: refused.
    let short = honest_payments(&p)[..7].to_vec();
    let pczt = build(&fvk, anchor, &[&ns[0]], &short);
    assert!(verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect()).is_err());
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
    tight.max_expiry = TARGET + 5; // payouts expire PAYOUT_EXPIRY_DELTA (120) blocks after the target
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

/// H-4 (security review 2026-10-11): the coordinator garbles the liquidator's note ciphertext
/// (or its ephemeral key). Commitment, value and recipient still verify and the transaction
/// would be valid, but the liquidator could never find or spend the note.
#[test]
fn undecryptable_third_party_output_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let liq = other();
    let p = plan(liq, 3_500_000);
    let honest = build(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let ins = inputs(&[&ns[0]]);

    // Re-encoding the honest ciphertext verbatim keeps the PCZT valid (the patching is sound).
    let same = with_output(honest.clone(), &liq, |_, _| {});
    verify_payout(&same, &fvk, &ins, &p, &expect()).unwrap();

    let garbled = with_output(honest.clone(), &liq, |_, ct| ct[100] ^= 0x01);
    let res = verify_payout(&garbled, &fvk, &ins, &p, &expect());
    assert!(matches!(res, Err(VerifyError::Undecryptable(_))), "{res:?}");

    // Another valid ephemeral key (the change output's): the AEAD key no longer matches.
    let (change_epk, _) = output_ciphertext(&honest, &fvk.address_at(0u32, Scope::Internal));
    let wrong_epk = with_output(honest, &liq, |epk, _| *epk = change_epk);
    let res = verify_payout(&wrong_epk, &fvk, &ins, &p, &expect());
    assert!(matches!(res, Err(VerifyError::Undecryptable(_))), "{res:?}");
}

/// I-1: a PCZT whose dummy spends still carry their spending keys is not IO-finalized; signers
/// must reject it.
#[test]
fn unfinalized_pczt_is_refused() {
    let fvk = vault_fvk();
    let (ns, anchor) = notes(&fvk, &[NOTE]);
    let p = plan(other(), 3_500_000);
    let pczt = build_unfinalized(&fvk, anchor, &[&ns[0]], &honest_payments(&p));
    let res = verify_payout(&pczt, &fvk, &inputs(&[&ns[0]]), &p, &expect());
    assert!(matches!(res, Err(VerifyError::DummySpendKey(_))), "{res:?}");
}
