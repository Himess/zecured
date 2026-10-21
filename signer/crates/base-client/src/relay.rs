//! Choosing the anchor of a proof: a header on the Zcash header relay's best chain, above the
//! proven transaction's block, with enough work on top of it ([`anchor_above`]), or one fixed in
//! advance ([`anchor_at`], W-10). A deposit proof needs the Intake's
//! work tier for its amount (`minConfirmationWorkFor`); a violation or inclusion proof the
//! slasher's `slashConfirmationWork`.

use alloy::{
    primitives::{Address, U256},
    providers::Provider,
};
use anyhow::{Context, Result};

use crate::events::{IZcashHeaderRelay, IZecuredIntake, IZecuredSlasher};

/// How far below the relay's tip an anchor is looked for.
pub const MAX_ANCHOR_DEPTH: u64 = 200;

/// The highest best-chain height above `height` with at least `min_work` on top of it in the
/// relay at `relay`, or `None` if the relay has not got that far yet.
pub async fn anchor_above<P: Provider>(
    provider: &P,
    relay: Address,
    height: u32,
    min_work: U256,
) -> Result<Option<u32>> {
    let relay = IZcashHeaderRelay::new(relay, provider);
    let tip = relay.bestChainTip().call().await.context("relay tip")?;
    let tip_height = u64::try_from(tip.height)?;
    let mut h = tip_height.saturating_sub(1);
    while h > u64::from(height) && tip_height - h <= MAX_ANCHOR_DEPTH {
        let hash = relay.bestBlockHashAt(U256::from(h)).call().await?;
        if relay.confirmationWork(hash).call().await? >= min_work {
            return Ok(Some(u32::try_from(h)?));
        }
        h -= 1;
    }
    Ok(None)
}

/// W-10: `height` itself once it is on the best chain of the relay at `relay` with at least
/// `min_work` on top of it, `None` until then. Unlike [`anchor_above`], the anchor is fixed in
/// advance, so it says nothing of the proven block but that it is below.
pub async fn anchor_at<P: Provider>(provider: &P, relay: Address, height: u32, min_work: U256) -> Result<Option<u32>> {
    let relay = IZcashHeaderRelay::new(relay, provider);
    let tip = relay.bestChainTip().call().await.context("relay tip")?;
    if u64::try_from(tip.height)? <= u64::from(height) {
        return Ok(None);
    }
    let hash = relay.bestBlockHashAt(U256::from(height)).call().await.context("relay.bestBlockHashAt")?;
    let work = relay.confirmationWork(hash).call().await.context("relay.confirmationWork")?;
    Ok((work >= min_work).then_some(height))
}

/// The anchor of a deposit proof of `amount` zatoshi mined at `deposit_height`: the Intake's relay
/// and the work its tier for that amount requires.
pub async fn deposit_anchor<P: Provider>(
    provider: &P,
    intake: Address,
    deposit_height: u32,
    amount: u64,
) -> Result<Option<u32>> {
    let intake = IZecuredIntake::new(intake, provider);
    let relay = intake.relay().call().await.context("Intake.relay")?;
    let min: U256 = intake.minConfirmationWorkFor(amount).call().await.context("Intake.minConfirmationWorkFor")?;
    anchor_above(provider, relay, deposit_height, min).await
}

/// The anchor of a violation or inclusion proof of a transaction mined at `height`: the
/// slasher's relay and its `slashConfirmationWork`.
pub async fn slash_anchor<P: Provider>(provider: &P, slasher: Address, height: u32) -> Result<Option<u32>> {
    let slasher = IZecuredSlasher::new(slasher, provider);
    let relay = slasher.relay().call().await.context("slasher.relay")?;
    let min: U256 = slasher.slashConfirmationWork().call().await.context("slasher.slashConfirmationWork")?;
    anchor_above(provider, relay, height, min).await
}
