//! K3: keeps the Zcash header relay on Base in step with the Zcash Testnet.
//!
//! Headers come from Testnet peers over the P2P protocol and are submitted in batches.
//! Submitting is permissionless: the relay checks proof of work, difficulty, time and linkage
//! itself, so a relayer can delay the relay but never feed it a header it would not accept.
//! A deposit proof only mints once the relay has moved on past it (D26), which is why the
//! relay's liveness matters: run two relayers, each with its own key, RPC endpoint and peers.
//! A header the other one already submitted is skipped. A relayer logs an alarm when the
//! relay lags the Zcash tip by `alarm_lag` blocks or more, and when its gas runs low.

use std::time::Duration;

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use tracing::{error, info, warn};
use zecured_base::events::IZcashHeaderRelay;
use zecured_deposit_witness::p2p::{self, HeaderLine};

/// EIP-7825 caps a transaction at 2^24 = 16,777,216 gas (Base Sepolia, Azul). A batch is split
/// until its estimate stays below this, with room for the headroom added on top.
pub const MAX_BATCH_GAS: u64 = 15_000_000;
/// Gas limit never set above the cap, whatever the headroom.
const GAS_CAP: u64 = 16_000_000;
/// Headers fetched per pass at most (a long gap is caught up over several passes).
const FETCH: usize = 400;
/// How far below its tip the relayer looks for a branch the peers continue (a Zcash reorg).
const STEP_BACK: u32 = 20;
// Checked at compile time: the default batch of 20 fits under the cap even at the worst
// measured cost per header (613,612 to 664,546 gas, contracts/src/relay/README.md), and every
// limit stays under 2^24.
const _: () = assert!(20 * 664_546 < MAX_BATCH_GAS && MAX_BATCH_GAS < GAS_CAP && GAS_CAP < 1 << 24);
/// Below this balance a relayer stops being reliable.
const LOW_BALANCE_WEI: u128 = 10_000_000_000_000_000; // 0.01 ETH

pub struct Relayer {
    pub relay: Address,
    pub key: PrivateKeySigner,
    pub rpc: String,
    /// Headers per transaction at most (a batch is split further if its gas would not fit).
    pub batch: usize,
    pub alarm_lag: u32,
    pub every: Duration,
}

/// Runs passes forever; a failed pass is logged and retried at the next interval.
pub async fn run(r: Relayer) -> Result<()> {
    ensure!(r.batch > 0, "the batch size must be at least 1");
    info!(relay = %r.relay, relayer = %r.key.address(), "relayer running");
    loop {
        if let Err(e) = pass(&r).await {
            warn!(error = %e, "relay pass failed");
        }
        tokio::time::sleep(r.every).await;
    }
}

/// The headers that follow the relay's best tip, from the peers; from up to `STEP_BACK` blocks
/// below the tip if the peers do not continue from it. Also returns the relay's tip height.
async fn headers_after<P: Provider>(
    relay: &IZcashHeaderRelay::IZcashHeaderRelayInstance<P>,
) -> Result<(Vec<HeaderLine>, u32)> {
    let tip = relay.bestChainTip().call().await.context("relay.bestChainTip")?;
    let height = u32::try_from(tip.height)?;
    for back in [0, STEP_BACK] {
        let from = height.saturating_sub(back);
        let hash = if back == 0 {
            tip.blockHash
        } else {
            relay.bestBlockHashAt(U256::from(from)).call().await.context("relay.bestBlockHashAt")?
        };
        let peers = p2p::seed_peers();
        let fetched = tokio::task::spawn_blocking(move || p2p::fetch(from, hash.0, FETCH, &peers)).await?;
        match fetched {
            Ok(lines) if !lines.is_empty() => return Ok((lines, height)),
            Ok(_) if back == 0 => return Ok((vec![], height)), // the relay is at the peers' tip
            _ => {}
        }
    }
    Ok((vec![], height))
}

fn internal_hash(line: &HeaderLine) -> Result<B256> {
    let mut hash = hex::decode(&line.hash)?;
    hash.reverse();
    Ok(B256::try_from(hash.as_slice())?)
}

/// The headers of `lines` the relay does not have yet.
async fn unknown<P: Provider>(
    relay: &IZcashHeaderRelay::IZcashHeaderRelayInstance<P>,
    lines: &[HeaderLine],
) -> Result<Vec<HeaderLine>> {
    let mut out = vec![];
    for line in lines {
        if !relay.headerOf(internal_hash(line)?).call().await.context("relay.headerOf")?.known {
            out.push(line.clone());
        }
    }
    Ok(out)
}

async fn pass(r: &Relayer) -> Result<()> {
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(r.key.clone())).connect_http(r.rpc.parse()?);
    let relay = IZcashHeaderRelay::new(r.relay, &provider);
    let (lines, tip_height) = headers_after(&relay).await?;

    // Headers above the relay's tip are new unless another relayer is racing this one (then the
    // submission's estimate fails and that batch is filtered); only a step back below the tip
    // brings headers the relay already has.
    let (below, above): (Vec<HeaderLine>, Vec<HeaderLine>) =
        lines.iter().cloned().partition(|l| l.height <= tip_height);
    let mut fresh = unknown(&relay, &below).await?;
    fresh.extend(above);
    let mut submitted = 0;
    for chunk in fresh.chunks(r.batch) {
        if submit(&relay, raws(chunk)?).await.is_err() {
            let left = unknown(&relay, chunk).await?;
            if left.len() == chunk.len() {
                return submit(&relay, raws(chunk)?).await; // not a race: report the error
            }
            if !left.is_empty() {
                submit(&relay, raws(&left)?).await?;
            }
            submitted += left.len();
        } else {
            submitted += chunk.len();
        }
    }

    // Alarms.
    let relay_height = u32::try_from(relay.bestChainTip().call().await?.height)?;
    let fetched_height = lines.last().map_or(0, |l| l.height);
    let announced = tokio::task::spawn_blocking(|| p2p::announced_height(&p2p::seed_peers(), 2)).await?;
    let zcash_tip = fetched_height.max(announced.unwrap_or(0));
    let lag = zcash_tip.saturating_sub(relay_height);
    if lag >= r.alarm_lag {
        error!(lag, relay_height, zcash_tip, "ALARM: the relay lags the Zcash tip");
    } else if submitted > 0 {
        info!(submitted, relay_height, "relay fed");
    }
    let balance = provider.get_balance(r.key.address()).await?;
    if balance < U256::from(LOW_BALANCE_WEI) {
        error!(%balance, "ALARM: the relayer's gas balance is low");
    }
    Ok(())
}

fn raws(lines: &[HeaderLine]) -> Result<Vec<Bytes>> {
    lines.iter().map(|l| Ok(Bytes::from(hex::decode(&l.raw)?))).collect()
}

/// Submits `headers` (parents first) in one transaction, or in halves while the estimate would
/// not fit under the per-transaction gas cap.
async fn submit<P: Provider>(
    relay: &IZcashHeaderRelay::IZcashHeaderRelayInstance<P>,
    headers: Vec<Bytes>,
) -> Result<()> {
    let call = relay.submitHeaders(headers.clone());
    let estimate = call.estimate_gas().await.context("estimate submitHeaders")?;
    if estimate > MAX_BATCH_GAS && headers.len() > 1 {
        let (first, second) = headers.split_at(headers.len() / 2);
        Box::pin(submit(relay, first.to_vec())).await?;
        return Box::pin(submit(relay, second.to_vec())).await;
    }
    let gas = zecured_base::gas_with_headroom(estimate).min(GAS_CAP);
    let receipt = call
        .gas(gas)
        .send()
        .await
        .context("send submitHeaders")?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .context("submitHeaders receipt")?;
    ensure!(receipt.status(), "submitHeaders reverted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_turned_into_the_relays_byte_order() {
        let line = HeaderLine {
            height: 1,
            hash: format!("{}{}", "00".repeat(31), "01"),
            raw: String::new(),
        };
        let h = internal_hash(&line).unwrap();
        assert_eq!(h.0[0], 1);
        assert_eq!(h.0[31], 0);
    }
}
