//! Building, proving, authorizing and broadcasting Ironwood-only spends via PCZT.
//!
//! Every Zecured payout is an Ironwood → Ironwood transaction: inputs are vault notes,
//! outputs are Orchard-protocol receivers paid in the Ironwood pool, and change returns to
//! the vault's internal address. Third-party outputs are encrypted with `ovk = None` so the
//! vault UFVK (given to the borrower) reveals nothing about them.

use anyhow::{Context, Result, anyhow, bail, ensure};
use orchard::{
    Address,
    circuit::{OrchardCircuitVersion, ProvingKey},
    keys::{FullViewingKey, Scope},
    primitives::redpallas,
};
use pasta_curves::group::ff::PrimeField;
use pczt::{
    Pczt,
    roles::{
        creator::Creator,
        io_finalizer::IoFinalizer,
        low_level_signer::{OrchardParseError, Signer as LowLevelSigner},
        prover::Prover,
        signer::Signer,
        tx_extractor::TransactionExtractor,
    },
};
use zcash_client_backend::proto::service::RawTransaction;
use zcash_primitives::transaction::{
    Transaction,
    builder::{BuildConfig, Builder, BundlePadding},
    fees::zip317,
};
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, Parameters},
    memo::MemoBytes,
    value::Zatoshis,
};

use crate::{lwd::LwdClient, scan::ReceivedNote};

/// Who can later decrypt an output with the vault's outgoing viewing key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputPrivacy {
    /// Recoverable with the vault OVK (the borrower's own payouts).
    VaultOvk,
    /// `ovk = None`: not recoverable by any holder of the vault's keys (third parties).
    NoOvk,
}

/// One payment out of the vault.
#[derive(Clone, Debug)]
pub struct Payment {
    pub recipient: Address,
    /// Amount received by the recipient, in zatoshis (fees are handled by the caller's policy).
    pub value: u64,
    pub privacy: OutputPrivacy,
}

/// An unauthorized Ironwood PCZT plus what the signers need to authorize it.
pub struct UnsignedSpend {
    pub pczt: Pczt,
    /// Action index (in the Ironwood bundle) of each real spend, in input order.
    pub spend_action_indices: Vec<usize>,
    /// ZIP-317 fee paid by the transaction, in zatoshis.
    pub fee: u64,
    /// Change returned to the vault, in zatoshis.
    pub change: u64,
}

/// ZIP-317 fee for an Ironwood bundle with `spends` spends and `outputs` outputs.
pub fn zip317_fee(spends: usize, outputs: usize) -> u64 {
    // ZIP 317: marginal fee 5000 zatoshis per logical action, 2 grace actions; an
    // Orchard-family bundle with cross-address transfers enabled has max(spends, outputs)
    // logical actions.
    const MARGINAL_FEE: u64 = 5_000;
    const GRACE_ACTIONS: u64 = 2;
    let actions = u64::try_from(spends.max(outputs)).expect("small");
    MARGINAL_FEE * actions.max(GRACE_ACTIONS)
}

/// Blocks between a transaction's target height and its expiry height: 120, the NU7 default
/// ZIP 203 proposes (about 50 minutes at 25 s blocks; review S12). Zecured's own constant, so
/// that a change of the library's default (40 in this release) never changes the signers'
/// policy silently.
pub const PAYOUT_EXPIRY_DELTA: u32 = 120;

/// Builds the PCZT spending `inputs` to `payments`, returning change to the vault's
/// internal address. Fails if the inputs do not cover payments + fee.
pub fn build<P: Parameters + Clone>(
    params: &P,
    target_height: BlockHeight,
    fvk: &FullViewingKey,
    anchor: orchard::Anchor,
    inputs: &[&ReceivedNote],
    payments: &[Payment],
) -> Result<UnsignedSpend> {
    ensure!(!inputs.is_empty(), "no inputs");
    let input_total: u64 = inputs.iter().map(|n| n.value()).sum();
    let paid: u64 = payments.iter().map(|p| p.value).sum();

    // Decide on change first: fee depends on the number of outputs.
    let fee_without_change = zip317_fee(inputs.len(), payments.len());
    let fee_with_change = zip317_fee(inputs.len(), payments.len() + 1);
    let (fee, change) = if input_total == paid + fee_without_change {
        (fee_without_change, 0)
    } else if input_total > paid + fee_with_change {
        (fee_with_change, input_total - paid - fee_with_change)
    } else {
        bail!("insufficient funds: inputs {input_total}, payments {paid}, fee {fee_with_change}");
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
    for p in payments {
        let ovk = match p.privacy {
            OutputPrivacy::VaultOvk => Some(fvk.to_ovk(Scope::External)),
            OutputPrivacy::NoOvk => None,
        };
        builder
            .add_ironwood_output::<zip317::FeeError>(
                ovk,
                p.recipient,
                Zatoshis::from_u64(p.value).map_err(|e| anyhow!("value: {e:?}"))?,
                MemoBytes::empty(),
            )
            .map_err(|e| anyhow!("add Ironwood output: {e}"))?;
    }
    if change > 0 {
        builder
            .add_ironwood_output::<zip317::FeeError>(
                Some(fvk.to_ovk(Scope::Internal)),
                fvk.address_at(0u32, Scope::Internal),
                Zatoshis::from_u64(change).map_err(|e| anyhow!("change: {e:?}"))?,
                MemoBytes::empty(),
            )
            .map_err(|e| anyhow!("add change output: {e}"))?;
    }

    let built = builder
        .build_for_pczt(rand_010::rng(), &zip317::FeeRule::standard())
        .map_err(|e| anyhow!("build PCZT: {e}"))?;
    let spend_action_indices = (0..inputs.len())
        .map(|i| built.ironwood_meta.spend_action_index(i).context("missing spend action index"))
        .collect::<Result<Vec<_>>>()?;
    let pczt = Creator::build_from_parts(built.pczt_parts).context("PCZT creator")?;
    Ok(UnsignedSpend {
        pczt,
        spend_action_indices,
        fee,
        change,
    })
}

/// IO Finalizer + Prover: commits to the effects and creates the Halo 2 proof.
pub fn finalize_and_prove(pczt: Pczt, pk: &ProvingKey) -> Result<Pczt> {
    let pczt = IoFinalizer::new(pczt).finalize_io(rand_010::rng()).map_err(|e| anyhow!("IO finalizer: {e:?}"))?;
    let pczt = Prover::new(pczt)
        .create_ironwood_proof(rand_010::rng(), pk)
        .map_err(|e| anyhow!("Ironwood prover: {e:?}"))?
        .finish();
    Ok(pczt)
}

/// The Ironwood (post-NU6.3) circuit proving key. Building it takes a few seconds.
pub fn proving_key() -> ProvingKey {
    ProvingKey::build(OrchardCircuitVersion::PostNu6_3)
}

/// What a threshold signer is asked to sign for one spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendAuthRequest {
    pub action_index: usize,
    /// `alpha`, the spend authorization randomizer (`rk = ak + [alpha] G`).
    pub alpha: [u8; 32],
}

/// The shielded sighash and the per-spend randomizers of a finalized PCZT.
pub fn signing_requests(pczt: &Pczt, spend_action_indices: &[usize]) -> Result<([u8; 32], Vec<SpendAuthRequest>)> {
    // `alpha` is only exposed on the parsed bundle, which the low-level signer hands out.
    let mut requests = Vec::with_capacity(spend_action_indices.len());
    LowLevelSigner::new(pczt.clone())
        .sign_ironwood_with(|_, bundle, _| -> Result<(), OrchardParseError> {
            for &i in spend_action_indices {
                if let Some(alpha) = bundle.actions().get(i).and_then(|a| *a.spend().alpha()) {
                    requests.push(SpendAuthRequest {
                        action_index: i,
                        alpha: alpha.to_repr(),
                    });
                }
            }
            Ok(())
        })
        .map_err(|e| anyhow!("parse Ironwood bundle: {e:?}"))?;
    ensure!(requests.len() == spend_action_indices.len(), "a spend is missing its randomizer");
    let sighash = Signer::new(pczt.clone()).map_err(|e| anyhow!("signer: {e:?}"))?.shielded_sighash();
    Ok((sighash, requests))
}

/// The sighash and the randomizer of every spend in a PCZT that still needs a signature,
/// without any policy check. DEMO ONLY (feature `demo`): used by a signer node switched to
/// "compromised" mode to show that two misbehaving signers cannot move funds on their own.
#[cfg(feature = "demo")]
pub fn signing_requests_all(pczt: &Pczt) -> Result<([u8; 32], Vec<SpendAuthRequest>)> {
    let mut requests = vec![];
    LowLevelSigner::new(pczt.clone())
        .sign_ironwood_with(|_, bundle, _| -> Result<(), OrchardParseError> {
            for (i, action) in bundle.actions().iter().enumerate() {
                let spend = action.spend();
                // Dummy spends carry their key until the IO Finalizer signs them with it.
                if spend.dummy_sk().is_none()
                    && spend.spend_auth_sig().is_none()
                    && let Some(alpha) = spend.alpha()
                {
                    requests.push(SpendAuthRequest {
                        action_index: i,
                        alpha: alpha.to_repr(),
                    });
                }
            }
            Ok(())
        })
        .map_err(|e| anyhow!("parse Ironwood bundle: {e:?}"))?;
    let sighash = Signer::new(pczt.clone()).map_err(|e| anyhow!("signer: {e:?}"))?.shielded_sighash();
    Ok((sighash, requests))
}

/// Applies externally produced (FROST) spend authorization signatures.
pub fn apply_signatures(pczt: Pczt, signatures: &[(usize, [u8; 64])]) -> Result<Pczt> {
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    for (index, sig) in signatures {
        signer
            .apply_ironwood_signature(*index, redpallas::Signature::<redpallas::SpendAuth>::from(*sig))
            .map_err(|e| anyhow!("apply signature to action {index}: {e:?}"))?;
    }
    Ok(signer.finish())
}

/// Extracts the final transaction (adds binding signatures, verifies proofs and signatures).
pub fn extract(pczt: Pczt) -> Result<Transaction> {
    TransactionExtractor::new(pczt)
        .extract(rand_010::rng())
        .map_err(|e| anyhow!("extract transaction: {e:?}"))
}

/// Broadcasts `tx` through the light wallet server.
pub async fn broadcast(client: &mut LwdClient, tx: &Transaction) -> Result<TxId> {
    broadcast_raw(client, serialize(tx)?).await?;
    Ok(tx.txid())
}

/// Consensus serialization of `tx`, as broadcast.
pub fn serialize(tx: &Transaction) -> Result<Vec<u8>> {
    let mut data = vec![];
    tx.write(&mut data).context("serialize transaction")?;
    Ok(data)
}

/// Broadcasts an already serialized transaction (e.g. to rebroadcast a stored payout).
pub async fn broadcast_raw(client: &mut LwdClient, data: Vec<u8>) -> Result<()> {
    let response = client
        .send_transaction(RawTransaction { data, height: 0 })
        .await
        .map_err(|s| anyhow!("SendTransaction: {s}"))?
        .into_inner();
    ensure!(
        response.error_code == 0,
        "server rejected transaction: {} ({})",
        response.error_message,
        response.error_code
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-language vector (red-team review § 4): ZIP 317's fee for one input and 1 to 9
    /// outputs, the table `contracts/test/fork/SpendCommits.t.sol` checks against `commitSpend`.
    #[test]
    fn zip317_fee_table() {
        let table: Vec<u64> = (1..=9).map(|outputs| zip317_fee(1, outputs)).collect();
        assert_eq!(table, [10_000, 10_000, 15_000, 20_000, 25_000, 30_000, 35_000, 40_000, 45_000]);
    }
}
