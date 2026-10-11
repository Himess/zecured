//! Choosing the anchor of a Tier 2 deposit proof: a header on the Zcash header relay's best
//! chain, above the deposit's block, with the Registry's `minConfirmationWork` on top of it.

use alloy::{
    primitives::{Address, U256},
    providers::Provider,
};
use anyhow::{Context, Result};

use crate::events::{IZcashHeaderRelay, IZecuredRegistry};

/// How far below the relay's tip an anchor is looked for.
pub const MAX_ANCHOR_DEPTH: u64 = 200;

/// The highest best-chain height above `deposit_height` with enough work on top of it in the
/// relay that `registry` uses, or `None` if the relay has not got that far yet.
pub async fn anchor_above<P: Provider>(provider: &P, registry: Address, deposit_height: u32) -> Result<Option<u32>> {
    let registry = IZecuredRegistry::new(registry, provider);
    let relay = IZcashHeaderRelay::new(registry.relay().call().await.context("Registry.relay")?, provider);
    let min: U256 = registry.minConfirmationWork().call().await.context("Registry.minConfirmationWork")?;
    let tip = relay.bestChainTip().call().await.context("relay tip")?;
    let tip_height = u64::try_from(tip.height)?;
    let mut h = tip_height.saturating_sub(1);
    while h > u64::from(deposit_height) && tip_height - h <= MAX_ANCHOR_DEPTH {
        let hash = relay.bestBlockHashAt(U256::from(h)).call().await?;
        if relay.confirmationWork(hash).call().await? >= min {
            return Ok(Some(u32::try_from(h)?));
        }
        h -= 1;
    }
    Ok(None)
}
