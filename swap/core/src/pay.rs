//! The swap payment: one Ironwood note of the order's zatoshi to the order's receiver, with the memo
//! `"ZSWP" || orderId`, encrypted with `ovk = None` (CLAUDE.md: a third party's output; nobody
//! holding the payer's keys can recover it from the chain), change back to the payer's internal
//! address. Built from the repo's Ironwood send code (`zecured_zcash_ops::spend`: ZIP 317 fee,
//! expiry, proving, extraction, broadcast); [`build_payment`] is `spend::build` with the memo, the
//! `ovk = None` payment output and the payment note's opening kept from the PCZT, which the payer
//! needs to prove the payment ([`PaymentOpening`]). Before anything is broadcast,
//! [`check_payment_note`] runs the payment statement's note check on the final transaction: a
//! payment that could not be proven is never sent.

use alloy::primitives::B256;
use anyhow::{Context, Result, anyhow, bail, ensure};
use orchard::{
    Address, Note,
    keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey},
    note::{NoteVersion, RandomSeed, Rho},
    note_encryption::IronwoodDomain,
    value::NoteValue,
};
use pczt::roles::{creator::Creator, signer::Signer};
use zcash_note_encryption::{Domain, try_output_recovery_with_pkd_esk};
use zcash_primitives::transaction::{
    Transaction,
    builder::{BuildConfig, Builder, BundlePadding},
    fees::zip317,
};
use zcash_protocol::{
    consensus::{BlockHeight, Parameters},
    memo::MemoBytes,
    value::Zatoshis,
};
pub use zecured_deposit_witness::payment::PaymentOpening;
use zecured_payment_statement::memo_order_id;
use zecured_zcash_ops::{
    scan::ReceivedNote,
    spend::{self, PAYOUT_EXPIRY_DELTA, zip317_fee},
};

use crate::payment_memo_bytes;

/// An unauthorized payment PCZT and what proving it later needs.
pub struct UnsignedPayment {
    pub pczt: pczt::Pczt,
    /// Action index (in the Ironwood bundle) of each spend, in input order.
    pub spend_action_indices: Vec<usize>,
    /// The payment note's opening (and the order's salt): keep it until the payment is proven.
    pub opening: PaymentOpening,
    pub fee: u64,
    pub change: u64,
}

/// Picks unspent notes, oldest first, until they cover `amount` and the fee of a payment with
/// change.
pub fn select_inputs<'a>(unspent: &[&'a ReceivedNote], amount: u64) -> Result<Vec<&'a ReceivedNote>> {
    let mut inputs = vec![];
    let mut total = 0u64;
    for n in unspent {
        if total >= amount.saturating_add(zip317_fee(inputs.len().max(1), 2)) {
            break;
        }
        total += n.value();
        inputs.push(*n);
    }
    ensure!(
        total >= amount.saturating_add(zip317_fee(inputs.len().max(1), 2)),
        "insufficient funds: {total} zatoshi for a payment of {amount}"
    );
    Ok(inputs)
}

/// Builds the payment PCZT: `value` zatoshi to `recipient` with the memo of `order_id`
/// (`ovk = None`), change to the payer's internal address (its internal OVK), the ZIP 317 fee.
#[allow(clippy::too_many_arguments)]
pub fn build_payment<P: Parameters + Clone>(
    params: &P,
    target_height: BlockHeight,
    fvk: &FullViewingKey,
    anchor: orchard::Anchor,
    inputs: &[&ReceivedNote],
    recipient: Address,
    value: u64,
    order_id: B256,
    salt: [u8; 32],
) -> Result<UnsignedPayment> {
    ensure!(!inputs.is_empty(), "no inputs");
    ensure!(value > 0, "a payment of zero");
    let input_total: u64 = inputs.iter().map(|n| n.value()).sum();
    let fee_without_change = zip317_fee(inputs.len(), 1);
    let fee_with_change = zip317_fee(inputs.len(), 2);
    let (fee, change) = if input_total == value + fee_without_change {
        (fee_without_change, 0)
    } else if input_total > value + fee_with_change {
        (fee_with_change, input_total - value - fee_with_change)
    } else {
        bail!("insufficient funds: inputs {input_total}, payment {value}, fee {fee_with_change}");
    };

    let mut builder = Builder::new(
        params.clone(),
        target_height,
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(anchor),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    )
    .with_expiry_height(target_height + PAYOUT_EXPIRY_DELTA);
    for input in inputs {
        builder
            .add_ironwood_spend::<zip317::FeeError>(fvk.clone(), input.note, input.merkle_path()?)
            .map_err(|e| anyhow!("add Ironwood spend: {e}"))?;
    }
    builder
        .add_ironwood_output::<zip317::FeeError>(
            None,
            recipient,
            Zatoshis::from_u64(value).map_err(|e| anyhow!("value: {e:?}"))?,
            payment_memo_bytes(order_id),
        )
        .map_err(|e| anyhow!("add the payment output: {e}"))?;
    if change > 0 {
        builder
            .add_ironwood_output::<zip317::FeeError>(
                Some(fvk.to_ovk(Scope::Internal)),
                fvk.address_at(0u32, Scope::Internal),
                Zatoshis::from_u64(change).map_err(|e| anyhow!("change: {e:?}"))?,
                MemoBytes::empty(),
            )
            .map_err(|e| anyhow!("add the change output: {e}"))?;
    }

    let built = builder
        .build_for_pczt(rand_010::rng(), &zip317::FeeRule::standard())
        .map_err(|e| anyhow!("build PCZT: {e}"))?;
    let spend_action_indices = (0..inputs.len())
        .map(|i| built.ironwood_meta.spend_action_index(i).context("missing spend action index"))
        .collect::<Result<Vec<_>>>()?;
    let payment_action = built.ironwood_meta.output_action_index(0).context("missing payment action index")?;
    let pczt = Creator::build_from_parts(built.pczt_parts).context("PCZT creator")?;

    // The payment note's opening, which the PCZT keeps for the prover.
    let output = pczt.ironwood().actions().get(payment_action).context("payment action")?.output();
    ensure!(
        *output.recipient() == Some(recipient.to_raw_address_bytes()) && *output.value() == Some(value),
        "the PCZT's payment output is not the payment"
    );
    let rseed = (*output.rseed()).context("the PCZT does not keep the payment's rseed")?;
    let opening = PaymentOpening {
        action_index: u32::try_from(payment_action)?,
        recipient: recipient.to_raw_address_bytes().to_vec(),
        value,
        rseed,
        salt,
    };
    Ok(UnsignedPayment {
        pczt,
        spend_action_indices,
        opening,
        fee,
        change,
    })
}

/// Proves the payment (Halo 2), signs its spends with the payer's key and extracts the final
/// transaction.
pub fn authorize(
    unsigned: UnsignedPayment,
    sk: &SpendingKey,
    pk: &orchard::circuit::ProvingKey,
) -> Result<Transaction> {
    let pczt = spend::finalize_and_prove(unsigned.pczt, pk)?;
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    let ask = SpendAuthorizingKey::from(sk);
    for i in &unsigned.spend_action_indices {
        signer.sign_ironwood(rand_010::rng(), *i, &ask).map_err(|e| anyhow!("sign: {e:?}"))?;
    }
    spend::extract(signer.finish())
}

/// The payment statement's note check (tier2/payment-statement, steps 1, 2 and 4: a v6
/// Ironwood-only transaction; the opening's V3 note recovered from its action; the memo naming
/// `order_id`), on the final transaction before it is broadcast. Inclusion comes once it is mined.
pub fn check_payment_note(tx: &Transaction, opening: &PaymentOpening, order_id: B256) -> Result<()> {
    // D5, as the payment statement applies it (the deposit statement's rule, this build's network).
    zecured_deposit_statement::check_deposit_transaction(tx)
        .map_err(|e| anyhow!("not a payment the statement accepts: {e:?}"))?;
    let bundle = tx.ironwood_bundle().context("no Ironwood bundle")?;
    let action = bundle.actions().get(opening.action_index as usize).context("no such action")?;
    let recipient: [u8; 43] = opening.recipient.clone().try_into().map_err(|_| anyhow!("recipient: 43 bytes"))?;
    let address = Option::<Address>::from(Address::from_raw_address_bytes(&recipient)).context("recipient")?;
    let rho = Option::<Rho>::from(Rho::from_bytes(&action.nullifier().to_bytes())).context("rho")?;
    let rseed = Option::<RandomSeed>::from(RandomSeed::from_bytes(opening.rseed, &rho)).context("rseed")?;
    let note = Option::<Note>::from(Note::from_parts(
        address,
        NoteValue::from_raw(opening.value),
        rho,
        rseed,
        NoteVersion::V3,
    ))
    .context("note")?;
    let esk = IronwoodDomain::derive_esk(&note).context("esk")?;
    let (recovered, _, memo) = try_output_recovery_with_pkd_esk(
        &IronwoodDomain::for_action(action),
        IronwoodDomain::get_pk_d(&note),
        esk,
        action,
    )
    .context("the payment action does not decrypt to the opened note")?;
    ensure!(
        recovered.value() == note.value()
            && recovered.recipient().to_raw_address_bytes() == recipient
            && recovered.version() == NoteVersion::V3,
        "the payment action carries another note"
    );
    ensure!(memo_order_id(&memo) == Some(order_id.0), "the payment's memo names another order");
    Ok(())
}
