//! ZecuredSwap on Base, as a payer uses it: the anchor a payment proof needs (a header on the
//! relay's best chain above the payment's block, with the order's tier of work on top), whether the
//! seal's guest is an active Swap version, and the proof and finalization transactions.

use alloy::{
    primitives::{Address, B256, U256},
    providers::Provider,
};
use anyhow::{Context, Result, ensure};
use zecured_base::{
    events::{IZcashHeaderRelay, IZecuredVersions, version_id},
    gas_with_headroom,
    relay::anchor_above,
};

use crate::{VERSION_SWAP, abi::IZecuredSwap, seal::SealFile};

/// The anchor of a payment of an order of `zatoshi` mined at `height`: the swap's relay and the
/// work its tier for the order's amount requires; `None` until the relay has got that far.
pub async fn payment_anchor<P: Provider>(
    provider: &P,
    swap: Address,
    height: u32,
    zatoshi: u64,
) -> Result<Option<u32>> {
    let s = IZecuredSwap::new(swap, provider);
    let relay = s.relay().call().await.context("ZecuredSwap.relay")?;
    let min: U256 = s.minConfirmationWorkFor(zatoshi).call().await.context("ZecuredSwap.minConfirmationWorkFor")?;
    anchor_above(provider, relay, height, min).await
}

/// The relay's best-chain tip height.
pub async fn relay_tip<P: Provider>(provider: &P, swap: Address) -> Result<u64> {
    let relay = IZecuredSwap::new(swap, provider).relay().call().await.context("ZecuredSwap.relay")?;
    let tip = IZcashHeaderRelay::new(relay, provider).bestChainTip().call().await.context("relay tip")?;
    Ok(u64::try_from(tip.height)?)
}

/// Whether ZecuredSwap takes a seal of `image_id` behind router selector `selector` (an active Swap
/// version of its version registry).
pub async fn swap_version_active<P: Provider>(
    provider: &P,
    swap: Address,
    selector: [u8; 4],
    image_id: B256,
) -> Result<bool> {
    let versions = IZecuredSwap::new(swap, provider).versions().call().await.context("ZecuredSwap.versions")?;
    Ok(IZecuredVersions::new(versions, provider)
        .isActive(version_id(VERSION_SWAP, selector, image_id))
        .call()
        .await?)
}

/// Submits a payment proof of `order_id`; returns the Base transaction hash.
pub async fn prove_settlement<P: Provider>(provider: &P, swap: Address, order_id: B256, s: &SealFile) -> Result<B256> {
    let swap = IZecuredSwap::new(swap, provider);
    let call = swap.proveSettlement(order_id, s.journal.clone(), s.seal.clone(), s.image_id);
    let gas = gas_with_headroom(call.estimate_gas().await.context("estimate proveSettlement")?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "proveSettlement reverted");
    Ok(r.transaction_hash)
}

/// Finalizes `order_id`'s pending settlement; returns the Base transaction hash.
pub async fn finalize_settlement<P: Provider>(provider: &P, swap: Address, order_id: B256) -> Result<B256> {
    let swap = IZecuredSwap::new(swap, provider);
    let call = swap.finalizeSettlement(order_id);
    let gas = gas_with_headroom(call.estimate_gas().await.context("estimate finalizeSettlement")?);
    let r = call.gas(gas).send().await?.get_receipt().await?;
    ensure!(r.status(), "finalizeSettlement reverted");
    Ok(r.transaction_hash)
}

/// Whether `order_id`'s pending settlement may be finalized now: the relay reached its ready height
/// and Base's clock its ready time (`now` is the latest Base block's timestamp).
pub fn settlement_ready(pending: &IZecuredSwap::PendingSettlement, relay_height: u64, now: u64) -> bool {
    pending.anchor != B256::ZERO && relay_height >= pending.readyHeight && now >= pending.readyTime
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_needs_both_the_height_and_the_time() {
        let p = IZecuredSwap::PendingSettlement {
            anchor: B256::repeat_byte(1),
            versionId: B256::repeat_byte(2),
            readyHeight: 100,
            readyTime: 1_000,
        };
        assert!(settlement_ready(&p, 100, 1_000));
        assert!(!settlement_ready(&p, 99, 2_000));
        assert!(!settlement_ready(&p, 200, 999));
        let none = IZecuredSwap::PendingSettlement {
            anchor: B256::ZERO,
            ..p
        };
        assert!(!settlement_ready(&none, 200, 2_000));
    }
}
