//! ZecuredSwap on Base, as a payer uses it: the anchor a payment proof needs (the relay's
//! best-chain header at the height the payer planned, with the order's tier of work on top: W-10),
//! whether a proof could land now (the guest's Swap version working, the relay keeping up: W-7),
//! and the proof and finalization transactions.

use alloy::{
    primitives::{Address, B256, U256},
    providers::Provider,
};
use anyhow::{Context, Result, ensure};
use zecured_base::{
    events::{IZcashHeaderRelay, IZecuredVersions, version_id},
    gas_with_headroom,
};

use crate::{VERSION_SWAP, abi::IZecuredSwap, seal::SealFile};

/// W-10: the hash of the relay's best-chain header at exactly `height`, once the work the order's
/// tier asks (`minConfirmationWorkFor(zatoshi)`) is on top of it; `None` until then (the relay's
/// tip is below `height`, or the work is not there yet). A payment's proof anchors at this header,
/// at the height its payer fixed before paying ([`crate::anchor`]): never at "the highest header
/// with enough work", which sits right above the payment's block.
pub async fn anchor_at<P: Provider>(provider: &P, swap: Address, height: u32, zatoshi: u64) -> Result<Option<B256>> {
    let s = IZecuredSwap::new(swap, provider);
    let relay = IZcashHeaderRelay::new(s.relay().call().await.context("ZecuredSwap.relay")?, provider);
    let min: U256 = s.minConfirmationWorkFor(zatoshi).call().await.context("ZecuredSwap.minConfirmationWorkFor")?;
    let hash = relay.bestBlockHashAt(U256::from(height)).call().await.context("relay bestBlockHashAt")?;
    if hash == B256::ZERO {
        return Ok(None);
    }
    let work = relay.confirmationWork(hash).call().await.context("relay confirmationWork")?;
    Ok((work >= min).then_some(hash))
}

/// W-20: about how many relay headers the work of the order's tier takes now:
/// `minConfirmationWorkFor(zatoshi)` over the average work of the relay's last `sample` best-chain
/// headers, rounded up.
pub async fn blocks_for_work<P: Provider>(provider: &P, swap: Address, zatoshi: u64, sample: u32) -> Result<u32> {
    let s = IZecuredSwap::new(swap, provider);
    let relay = IZcashHeaderRelay::new(s.relay().call().await.context("ZecuredSwap.relay")?, provider);
    let min: U256 = s.minConfirmationWorkFor(zatoshi).call().await.context("ZecuredSwap.minConfirmationWorkFor")?;
    let tip = relay.bestChainTip().call().await.context("relay tip")?;
    let tip_height = u64::try_from(tip.height)?;
    let sample = u64::from(sample.max(1)).min(tip_height);
    ensure!(sample > 0, "the relay has no headers");
    let old = relay
        .bestBlockHashAt(U256::from(tip_height - sample))
        .call()
        .await
        .context("relay bestBlockHashAt")?;
    ensure!(old != B256::ZERO, "the relay has fewer than {sample} headers");
    let old_work = relay.headerOf(old).call().await.context("relay headerOf")?.chainWork;
    let per_block = tip.chainWork.saturating_sub(old_work) / U256::from(sample);
    ensure!(!per_block.is_zero(), "no work on the relay's last headers");
    let blocks = (min + per_block - U256::from(1u64)) / per_block;
    Ok(u32::try_from(blocks).unwrap_or(u32::MAX))
}

/// W-7: what decides whether a payment made now could still be proven: whether the payer's Swap
/// version is working, and how many blocks the relay lags the Zcash tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofPath {
    pub version_working: bool,
    pub relay_lag: u64,
}

impl ProofPath {
    /// Why no payment should be made now, if so.
    pub fn problem(&self, max_relay_lag: u64) -> Option<&'static str> {
        if !self.version_working {
            Some(
                "ZecuredSwap takes no proof of the payment guest now: its Swap version is not working (not active, removed, or its verifier stopped)",
            )
        } else if self.relay_lag > max_relay_lag {
            Some("the Zcash header relay lags the Zcash tip")
        } else {
            None
        }
    }
}

/// The id ZecuredVersions gives the Swap version of seal selector `selector` and guest `image_id`.
pub fn swap_version_id(selector: [u8; 4], image_id: B256) -> B256 {
    version_id(VERSION_SWAP, selector, image_id)
}

/// W-7: whether ZecuredSwap verifies seals of Swap version `version_id` now: active, its router
/// selector routed and its emergency-stop wrapper not paused (`ZecuredVersions.isWorking`). A payer
/// pays only while it does: otherwise the proof could never land, and after the deadline the USDC
/// would go back to whoever locked it, the payment lost.
pub async fn swap_version_working<P: Provider>(provider: &P, swap: Address, version_id: B256) -> Result<bool> {
    let versions = IZecuredSwap::new(swap, provider).versions().call().await.context("ZecuredSwap.versions")?;
    IZecuredVersions::new(versions, provider)
        .isWorking(version_id)
        .call()
        .await
        .context("ZecuredVersions.isWorking")
}

/// Swap version `version_id` as ZecuredSwap's version registry holds it (all zero: never
/// scheduled).
pub async fn swap_version<P: Provider>(
    provider: &P,
    swap: Address,
    version_id: B256,
) -> Result<IZecuredVersions::Version> {
    let versions = IZecuredSwap::new(swap, provider).versions().call().await.context("ZecuredSwap.versions")?;
    IZecuredVersions::new(versions, provider)
        .version(version_id)
        .call()
        .await
        .context("ZecuredVersions.version")
}

/// The relay's best-chain tip height.
pub async fn relay_tip<P: Provider>(provider: &P, swap: Address) -> Result<u64> {
    let relay = IZecuredSwap::new(swap, provider).relay().call().await.context("ZecuredSwap.relay")?;
    let tip = IZcashHeaderRelay::new(relay, provider).bestChainTip().call().await.context("relay tip")?;
    Ok(u64::try_from(tip.height)?)
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
