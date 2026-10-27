//! K3: keeps the Zcash header relay on Base in step with the Zcash network it follows (its
//! `networkId`: Testnet or Mainnet).
//!
//! Headers come from that network's peers over the P2P protocol and are submitted in batches.
//! Submitting is permissionless: the relay checks proof of work, difficulty, time and linkage
//! itself, so a relayer can delay the relay but never feed it a header it would not accept.
//! A deposit proof only mints once the relay has moved on past it (D26), which is why the
//! relay's liveness matters: run two relayers, each with its own key, RPC endpoint and peers.
//! A header the other one already submitted is skipped. A relayer logs an alarm when the
//! relay lags the Zcash tip by `alarm_lag` blocks or more, and when its gas runs low.

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256, keccak256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use tracing::{error, info, warn};
use zecured_base::events::IZcashHeaderRelay;
use zecured_deposit_witness::{
    ZcashNetwork,
    p2p::{self, HeaderLine},
};

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
    /// 0: submit every pass. Otherwise a standby relayer (D30): it submits only once the relay
    /// lags the Zcash tip by this many blocks, so two relayers do not both pay for every header
    /// while the active one keeps up.
    pub standby_lag: u32,
    pub every: Duration,
}

/// Whether a pass submits: always for an active relayer, and for a standby one only when the
/// relay (at `relay_height`) lags the highest height seen by `standby_lag` blocks or more.
pub fn should_submit(standby_lag: u32, seen: u32, relay_height: u32) -> bool {
    standby_lag == 0 || seen.saturating_sub(relay_height) >= standby_lag
}

/// Peers are probed again this often, or after a failed pass.
const PROBE_EVERY: Duration = Duration::from_secs(300);

/// The Zcash network a relay follows: its `networkId` is keccak256 of `zcash-testnet` or
/// `zcash-mainnet` (`IZcashHeaderRelay.sol`).
pub fn network_of(network_id: B256) -> Option<ZcashNetwork> {
    [ZcashNetwork::Testnet, ZcashNetwork::Mainnet]
        .into_iter()
        .find(|n| keccak256(format!("zcash-{n}")) == network_id)
}

/// The peers that answered the last probe, highest announced height first. Most seed
/// addresses are unreachable at any time, and some reachable peers lag: asking them in turn on
/// every pass cost a connect timeout each, and a lagging peer's empty answer looked like the tip.
#[derive(Default)]
struct PeerBook {
    reachable: Vec<p2p::Reachable>,
    probed: Option<Instant>,
    /// Set when the relay refused the headers that continue its tip. Some peers can follow a branch whose next
    /// blocks the relay rejects, and while the relay's tip is on it only they answer (Testnet, 2026-10-27): the
    /// next pass then fetches from `STEP_BACK` below the tip, as after a reorg.
    step_back: bool,
}

/// Runs passes forever; a failed pass is logged and retried at the next interval.
pub async fn run(r: Relayer) -> Result<()> {
    ensure!(r.batch > 0, "the batch size must be at least 1");
    info!(relay = %r.relay, relayer = %r.key.address(), "relayer running");
    let mut book = PeerBook::default();
    // The relay's network, read once (`networkId` never changes); an unreachable RPC is retried.
    let mut network = None;
    loop {
        if network.is_none() {
            let provider = ProviderBuilder::new().connect_http(r.rpc.parse()?);
            match IZcashHeaderRelay::new(r.relay, &provider).networkId().call().await {
                Ok(id) => {
                    let n = network_of(id).context("the relay follows a Zcash network this relayer does not know")?;
                    info!(%n, "the relay follows this Zcash network");
                    network = Some(n);
                }
                Err(e) => warn!(error = %e, "relay.networkId failed"),
            }
        }
        if let Some(n) = network
            && let Err(e) = pass(&r, n, &mut book).await
        {
            warn!(error = %e, "relay pass failed");
            book.probed = None;
        }
        tokio::time::sleep(r.every).await;
    }
}

/// The headers that follow the relay's best tip, from the peers; from up to `STEP_BACK` blocks
/// below the tip if the peers do not continue from it, or at once with `step_back`. Also returns
/// the relay's tip height and the peer that answered.
async fn headers_after<P: Provider>(
    relay: &IZcashHeaderRelay::IZcashHeaderRelayInstance<P>,
    network: ZcashNetwork,
    peers: &[SocketAddr],
    step_back: bool,
) -> Result<(Vec<HeaderLine>, u32, Option<SocketAddr>)> {
    let tip = relay.bestChainTip().call().await.context("relay.bestChainTip")?;
    let height = u32::try_from(tip.height)?;
    let backs: &[u32] = if step_back { &[STEP_BACK] } else { &[0, STEP_BACK] };
    for &back in backs {
        let from = height.saturating_sub(back);
        let hash = if back == 0 {
            tip.blockHash
        } else {
            relay.bestBlockHashAt(U256::from(from)).call().await.context("relay.bestBlockHashAt")?
        };
        let peers = peers.to_vec();
        let fetched =
            tokio::task::spawn_blocking(move || p2p::fetch_with_peer(network, from, hash.0, FETCH, &peers)).await?;
        match fetched {
            Ok((lines, peer)) if !lines.is_empty() => return Ok((lines, height, peer)),
            Ok((_, peer)) if back == 0 => return Ok((vec![], height, peer)), // at the peers' tip
            _ => {}
        }
    }
    Ok((vec![], height, None))
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

async fn pass(r: &Relayer, network: ZcashNetwork, book: &mut PeerBook) -> Result<()> {
    let started = Instant::now();
    let provider = ProviderBuilder::new().wallet(EthereumWallet::from(r.key.clone())).connect_http(r.rpc.parse()?);
    let relay = IZcashHeaderRelay::new(r.relay, &provider);
    if book.reachable.is_empty() || book.probed.is_none_or(|t| t.elapsed() > PROBE_EVERY) {
        book.reachable = tokio::task::spawn_blocking(move || p2p::probe(network, &p2p::seed_peers(network))).await?;
        book.probed = Some(Instant::now());
        let best = book.reachable.first().map(|p| p.height);
        info!(reachable = book.reachable.len(), best_height = ?best, %network, "peers probed");
    }
    let peers: Vec<SocketAddr> = book.reachable.iter().map(|p| p.addr).collect();
    let (lines, tip_height, _) = headers_after(&relay, network, &peers, book.step_back).await?;
    let fetch_ms = elapsed_ms(started);
    let seen = lines.last().map_or(0, |l| l.height).max(book.reachable.first().map_or(0, |p| p.height));
    if !should_submit(r.standby_lag, seen, tip_height) {
        return Ok(()); // standby: the active relayer keeps up
    }

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
                book.step_back = true; // not a race: the next pass steps back below the tip
                return submit(&relay, raws(chunk)?).await; // and this one reports the error
            }
            if !left.is_empty() {
                submit(&relay, raws(&left)?).await?;
            }
            submitted += left.len();
        } else {
            submitted += chunk.len();
        }
    }
    book.step_back = false;

    let submit_ms = elapsed_ms(started) - fetch_ms;

    // Alarms. A load-balanced RPC endpoint may answer from a node that has not seen the
    // submission's block yet: the relay holds at least what this pass submitted.
    let submitted_top = fresh.last().map_or(0, |l| l.height);
    let relay_height = u32::try_from(relay.bestChainTip().call().await?.height)?.max(submitted_top);
    let fetched_height = lines.last().map_or(0, |l| l.height);
    // The best height peers announced when probed (up to PROBE_EVERY old), or what was fetched.
    let announced = book.reachable.first().map_or(0, |p| p.height);
    let zcash_tip = fetched_height.max(announced);
    let lag = zcash_tip.saturating_sub(relay_height);
    let pass_ms = elapsed_ms(started);
    if lag >= r.alarm_lag {
        error!(lag, relay_height, zcash_tip, fetch_ms, submit_ms, pass_ms, "ALARM: the relay lags the Zcash tip");
    } else if submitted > 0 {
        info!(submitted, relay_height, fetch_ms, submit_ms, pass_ms, "relay fed");
    }
    let balance = provider.get_balance(r.key.address()).await?;
    if balance < U256::from(LOW_BALANCE_WEI) {
        error!(%balance, "ALARM: the relayer's gas balance is low");
    }
    Ok(())
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
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
    fn a_standby_relayer_waits_until_the_relay_lags() {
        assert!(should_submit(0, 100, 100), "an active relayer always submits");
        assert!(!should_submit(10, 109, 100));
        assert!(should_submit(10, 110, 100));
        assert!(!should_submit(10, 90, 100), "a relay ahead of the peers seen");
    }

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

    /// The relayer follows its relay's network: `networkId` is keccak256 of `zcash-testnet` or
    /// `zcash-mainnet` (IZcashHeaderRelay.sol's ZCASH_TESTNET and ZCASH_MAINNET).
    #[test]
    fn the_relays_network_id_names_its_zcash_network() {
        assert_eq!(network_of(keccak256("zcash-testnet")), Some(ZcashNetwork::Testnet));
        assert_eq!(network_of(keccak256("zcash-mainnet")), Some(ZcashNetwork::Mainnet));
        assert_eq!(network_of(keccak256("zcash-regtest")), None);
        assert_eq!(network_of(B256::ZERO), None);
    }
}
