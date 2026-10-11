//! The check every signer runs on a PCZT before contributing a FROST share.
//!
//! The coordinator builds the transaction; a signer trusts nothing in it. It recomputes
//! the payout plan from its own view of both chains and accepts the PCZT only if:
//! - it is a v6 transaction for the expected consensus branch, with a sane expiry height;
//! - it has no transparent, Sapling or Orchard-pool parts (Ironwood only);
//! - every real spend is one of the expected vault notes (and all of them are spent);
//! - the non-zero outputs are exactly the plan's payments plus the change to the vault's
//!   internal address, so the fee is exactly the plan's fee;
//! - every non-zero output's note ciphertext decrypts, for its recipient, to exactly the note
//!   the action commits to (otherwise the recipient could never find or spend it);
//! - third-party outputs cannot be recovered with the vault's outgoing viewing keys
//!   (`ovk = None`), while the borrower's outputs can (so the borrower can verify them);
//! - it is an Ironwood bundle with the standard flags and no leftover dummy spending keys.
//!
//! Anything else is refused (policy P4). On success the signer gets the sighash and the
//! per-spend randomizers it will sign for; it never signs any other action.

use orchard::{
    Note,
    bundle::{BundleVersion, Flags},
    keys::{FullViewingKey, Scope},
    note::Rho,
    note_encryption::IronwoodDomain,
    pczt::{Action, Bundle},
    value::NoteValue,
};
use pasta_curves::group::ff::PrimeField;
use pczt::{
    Pczt,
    roles::{
        low_level_signer::{OrchardParseError, Signer as LowLevelSigner},
        signer::Signer,
    },
};
use zcash_note_encryption::{Domain, try_output_recovery_with_pkd_esk};
use zcash_protocol::{PoolType, constants::V6_TX_VERSION};
use zecured_zcash_ops::spend::{OutputPrivacy, SpendAuthRequest};

use crate::ledger::PayoutPlan;

/// A vault note the plan spends, as known from the signer's own scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedInput {
    pub nullifier: [u8; 32],
    pub value: u64,
}

/// What the signer will sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The shielded sighash. These v6 transactions have no transparent part, so it is also the
    /// id of the transaction once mined (ZIP 244; checked in zcash-ops' `frost_spend` test).
    pub sighash: [u8; 32],
    pub spends: Vec<SpendAuthRequest>,
    /// The transaction's expiry height (within the accepted window).
    pub expiry_height: u32,
}

/// Transaction-level expectations.
#[derive(Clone, Copy, Debug)]
pub struct TxExpectations {
    pub consensus_branch_id: u32,
    /// The transaction must expire no later than this height (and not before `min_expiry`).
    pub max_expiry: u32,
    pub min_expiry: u32,
}

/// Why a PCZT was refused.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("not a v6 transaction")]
    WrongTxVersion,
    #[error("unexpected consensus branch id {0:#010x}")]
    WrongBranch(u32),
    #[error("expiry height {0} outside the accepted window")]
    BadExpiry(u32),
    #[error("transaction touches the {0} pool")]
    OtherPool(&'static str),
    #[error("PCZT parse: {0:?}")]
    Parse(OrchardParseError),
    #[error("action {0}: {1}")]
    Action(usize, String),
    #[error("action {0} spends a vault note the plan does not spend")]
    UnexpectedSpend(usize),
    #[error("action {0} spends a non-vault note with value")]
    ForeignSpend(usize),
    #[error("expected vault note was not spent (or spent twice)")]
    InputsMismatch,
    #[error("outputs do not match the plan")]
    OutputsMismatch,
    #[error("fee {actual} does not match the plan's {expected}")]
    WrongFee { actual: i128, expected: u64 },
    #[error("output in action {0} has the wrong outgoing-viewing-key visibility")]
    WrongOvk(usize),
    #[error("bundle value balance {actual} does not match the plan's fee {expected}")]
    WrongValueBalance { actual: i64, expected: u64 },
    #[error("action {0} reuses the randomized key of a spend we sign")]
    ReusedRandomizedKey(usize),
    #[error("output in action {0} does not decrypt to the note it commits to")]
    Undecryptable(usize),
    #[error("action {0} still carries a dummy spending key")]
    DummySpendKey(usize),
    #[error("unexpected bundle: {0}")]
    WrongBundle(&'static str),
    #[error("signer: {0}")]
    Signer(String),
}

impl From<OrchardParseError> for VerifyError {
    fn from(e: OrchardParseError) -> Self {
        VerifyError::Parse(e)
    }
}

/// One non-zero output found in the bundle.
struct FoundOutput {
    action: usize,
    receiver: [u8; 43],
    value: u64,
    visible_external: bool,
    visible_internal: bool,
}

/// Verifies `pczt` against `plan` for the vault with full viewing key `fvk`.
pub fn verify_payout(
    pczt: &Pczt,
    fvk: &FullViewingKey,
    inputs: &[ExpectedInput],
    plan: &PayoutPlan,
    tx: &TxExpectations,
) -> Result<Verified, VerifyError> {
    let global = pczt.global();
    if *global.tx_version() != V6_TX_VERSION {
        return Err(VerifyError::WrongTxVersion);
    }
    if *global.consensus_branch_id() != tx.consensus_branch_id {
        return Err(VerifyError::WrongBranch(*global.consensus_branch_id()));
    }
    let expiry = *global.expiry_height();
    if expiry < tx.min_expiry || expiry > tx.max_expiry {
        return Err(VerifyError::BadExpiry(expiry));
    }
    for (pool, name) in [
        (PoolType::TRANSPARENT, "transparent"),
        (PoolType::SAPLING, "Sapling"),
        (PoolType::ORCHARD, "Orchard"),
    ] {
        if pczt.has_data_in_pool(pool) {
            return Err(VerifyError::OtherPool(name));
        }
    }

    let mut spends: Vec<(usize, [u8; 32], u64, [u8; 32])> = vec![];
    let mut outputs: Vec<FoundOutput> = vec![];
    let mut value_balance: i64 = 0;
    LowLevelSigner::new(pczt.clone()).sign_ironwood_with(|_, bundle, _| -> Result<(), VerifyError> {
        check_bundle(*bundle.bundle_version(), bundle.flags())?;
        value_balance =
            i64::try_from(*bundle.value_sum()).map_err(|e| VerifyError::Action(0, format!("value sum: {e:?}")))?;
        inspect(bundle, fvk, inputs, &mut spends, &mut outputs)
    })?;

    // The bundle's value balance is what the chain enforces (binding signature): it must be
    // exactly the plan's fee, so no extra value can be burned to fees through a spend we
    // did not account for.
    if value_balance != i64::try_from(plan.fee).unwrap_or(i64::MAX) {
        return Err(VerifyError::WrongValueBalance {
            actual: value_balance,
            expected: plan.fee,
        });
    }

    // Every expected input spent exactly once, nothing else of value.
    let mut spent: Vec<[u8; 32]> = spends.iter().map(|s| s.1).collect();
    spent.sort();
    let mut expected: Vec<[u8; 32]> = inputs.iter().map(|i| i.nullifier).collect();
    expected.sort();
    if spent != expected || inputs.len() != plan.inputs {
        return Err(VerifyError::InputsMismatch);
    }

    // Outputs: the plan's payments + change, nothing else.
    let change_receiver = fvk.address_at(0u32, Scope::Internal).to_raw_address_bytes();
    let mut wanted: Vec<([u8; 43], u64, Option<OutputPrivacy>)> =
        plan.payments.iter().map(|p| (p.receiver, p.value, Some(p.privacy))).collect();
    if plan.change > 0 {
        wanted.push((change_receiver, plan.change, None));
    }
    if wanted.len() != outputs.len() {
        return Err(VerifyError::OutputsMismatch);
    }
    for out in &outputs {
        let pos = wanted
            .iter()
            .position(|(r, v, _)| *r == out.receiver && *v == out.value)
            .ok_or(VerifyError::OutputsMismatch)?;
        let (_, _, privacy) = wanted.swap_remove(pos);
        let ok = match privacy {
            // Third parties: not recoverable with any vault OVK.
            Some(OutputPrivacy::NoOvk) => !out.visible_external && !out.visible_internal,
            // Borrower payouts: recoverable with the vault's external OVK.
            Some(OutputPrivacy::VaultOvk) => out.visible_external,
            // Change: recoverable with the vault's internal OVK.
            None => out.visible_internal,
        };
        if !ok {
            return Err(VerifyError::WrongOvk(out.action));
        }
    }

    let in_total: u64 = inputs.iter().map(|i| i.value).sum();
    let out_total: u64 = outputs.iter().map(|o| o.value).sum();
    let fee = i128::from(in_total) - i128::from(out_total);
    if fee != i128::from(plan.fee) {
        return Err(VerifyError::WrongFee {
            actual: fee,
            expected: plan.fee,
        });
    }

    let sighash = Signer::new(pczt.clone()).map_err(|e| VerifyError::Signer(format!("{e:?}")))?.shielded_sighash();
    Ok(Verified {
        sighash,
        spends: spends
            .into_iter()
            .map(|(action_index, _, _, alpha)| SpendAuthRequest { action_index, alpha })
            .collect(),
        expiry_height: expiry,
    })
}

/// I-1: every payout is an Ironwood bundle built with the standard flags (spends, outputs and
/// cross-address transfers enabled).
fn check_bundle(version: BundleVersion, flags: &Flags) -> Result<(), VerifyError> {
    if version != BundleVersion::ironwood_v3() {
        return Err(VerifyError::WrongBundle("not an Ironwood bundle"));
    }
    if !(flags.spends_enabled() && flags.outputs_enabled() && flags.cross_address_enabled()) {
        return Err(VerifyError::WrongBundle("unexpected bundle flags"));
    }
    Ok(())
}

/// H-4: whether the output's note ciphertext decrypts, for its recipient, to exactly `note`.
///
/// The note commitment binds recipient, value, `rho` and `rseed`, but not the transmitted
/// ciphertext: a coordinator could garble the ciphertext (or `epk`) of a third-party output and
/// the transaction would still be valid, while the recipient could never find or spend the
/// note. Rebuilding `esk` and `pk_d` from the note and decrypting as the recipient would (which
/// also checks `epk` and the note commitment) rules that out.
fn output_decrypts(action: &Action, note: &Note) -> bool {
    let domain = IronwoodDomain::for_pczt_action(action);
    let Some(esk) = IronwoodDomain::derive_esk(note) else {
        return false;
    };
    let pk_d = IronwoodDomain::get_pk_d(note);
    matches!(try_output_recovery_with_pkd_esk(&domain, pk_d, esk, action), Some((n, _, _)) if n == *note)
}

fn inspect(
    bundle: &Bundle,
    fvk: &FullViewingKey,
    inputs: &[ExpectedInput],
    spends: &mut Vec<(usize, [u8; 32], u64, [u8; 32])>,
    outputs: &mut Vec<FoundOutput>,
) -> Result<(), VerifyError> {
    let version = *bundle.bundle_version();
    let rks: Vec<[u8; 32]> = bundle.actions().iter().map(|a| <[u8; 32]>::from(a.spend().rk())).collect();
    let ovk_ext = fvk.to_ovk(Scope::External);
    let ovk_int = fvk.to_ovk(Scope::Internal);
    let err = |i: usize, e: &dyn std::fmt::Debug| VerifyError::Action(i, format!("{e:?}"));

    for (i, action) in bundle.actions().iter().enumerate() {
        action.verify_cv_net().map_err(|e| err(i, &e))?;
        let spend = action.spend();
        // I-1: the IO Finalizer signs the dummy spends and clears their keys; a PCZT that still
        // carries one is not a finalized payout (signers must reject it).
        if spend.dummy_sk().is_some() {
            return Err(VerifyError::DummySpendKey(i));
        }
        let value = spend.value().ok_or_else(|| err(i, &"missing spend value"))?.inner();
        let nf = spend.nullifier().to_bytes();
        match inputs.iter().find(|n| n.nullifier == nf) {
            Some(expected) => {
                // A vault note: it must be the expected value and be spendable by this vault.
                if value != expected.value {
                    return Err(VerifyError::UnexpectedSpend(i));
                }
                spend.verify_nullifier(Some(fvk)).map_err(|e| err(i, &e))?;
                spend.verify_rk(Some(fvk)).map_err(|e| err(i, &e))?;
                let alpha = spend.alpha().ok_or_else(|| err(i, &"missing alpha"))?;
                // One FROST signature is valid for every action with the same `rk`; refuse
                // a bundle that would let the signature for this spend authorize another.
                if let Some(j) = rks.iter().enumerate().position(|(j, rk)| j != i && *rk == rks[i]) {
                    return Err(VerifyError::ReusedRandomizedKey(j));
                }
                spends.push((i, nf, value, alpha.to_repr()));
            }
            None => {
                // Padding: a zero-value dummy note, never signed with the vault key.
                if value != 0 {
                    return Err(VerifyError::ForeignSpend(i));
                }
            }
        }

        let output = action.output();
        output.verify_note_commitment(spend).map_err(|e| err(i, &e))?;
        let out_value = output.value().ok_or_else(|| err(i, &"missing output value"))?.inner();
        if out_value > 0 {
            let recipient = output.recipient().ok_or_else(|| err(i, &"missing recipient"))?;
            let rseed = output.rseed().ok_or_else(|| err(i, &"missing output rseed"))?;
            let rho = Option::<Rho>::from(Rho::from_bytes(&spend.nullifier().to_bytes()))
                .ok_or_else(|| err(i, &"invalid rho"))?;
            let note = Option::<Note>::from(Note::from_parts(
                recipient,
                NoteValue::from_raw(out_value),
                rho,
                rseed,
                *output.note_version(),
            ))
            .ok_or_else(|| err(i, &"invalid output note"))?;
            if !output_decrypts(action, &note) {
                return Err(VerifyError::Undecryptable(i));
            }
            let visible =
                |ovk| action.recover_output_with_ovk(ovk, version).map(|r| r.is_some()).map_err(|e| err(i, &e));
            outputs.push(FoundOutput {
                action: i,
                receiver: recipient.to_raw_address_bytes(),
                value: out_value,
                visible_external: visible(&ovk_ext)?,
                visible_internal: visible(&ovk_int)?,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_standard_ironwood_bundles_are_accepted() {
        let ironwood = BundleVersion::ironwood_v3();
        check_bundle(ironwood, &ironwood.default_flags()).unwrap();
        for flags in [
            Flags::SPENDS_DISABLED,
            Flags::OUTPUTS_DISABLED,
            Flags::CROSS_ADDRESS_DISABLED,
        ] {
            assert!(matches!(check_bundle(ironwood, &flags), Err(VerifyError::WrongBundle(_))));
        }
        let orchard = BundleVersion::orchard_v3();
        assert!(matches!(check_bundle(orchard, &orchard.default_flags()), Err(VerifyError::WrongBundle(_))));
    }
}
