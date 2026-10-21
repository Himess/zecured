//! Light wallet server (lightwalletd / Zaino) gRPC access.
//!
//! The server is trusted for data availability, not for spend authority: every signer
//! node uses its own connection and independently checks what it is asked to sign. Deposit
//! attestations can additionally require independent witness servers to agree (see
//! [`crate::witness`]).
//!
//! Every call is bounded in time (M-7): a hanging server must not freeze the node.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use tonic::{
    Code,
    transport::{Channel, ClientTlsConfig, Endpoint},
};
use zcash_client_backend::proto::{
    compact_formats::CompactBlock,
    service::{self, BlockId, ChainSpec, Empty, TxFilter, compact_tx_streamer_client::CompactTxStreamerClient},
};
use zcash_protocol::TxId;

/// A gRPC client for the CompactTxStreamer service.
pub type LwdClient = CompactTxStreamerClient<Channel>;

/// Time allowed to establish a connection (TCP and TLS).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for a call to answer (for a stream: until it starts; see
/// [`STREAM_MESSAGE_TIMEOUT`] for the messages that follow).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest wait for the next message of a streamed answer.
pub const STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Connects to a light wallet server, e.g. `https://host:443` or `http://127.0.0.1:9067`.
pub async fn connect(url: &str) -> Result<LwdClient> {
    let mut endpoint = Endpoint::from_shared(url.to_owned())
        .context("invalid server URL")?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    if url.starts_with("https://") {
        endpoint = endpoint.tls_config(ClientTlsConfig::new().with_webpki_roots()).context("TLS config")?;
    }
    let channel = endpoint.connect().await.with_context(|| format!("connect to light wallet server {url}"))?;
    // Compact block ranges can be large; allow bigger messages than tonic's 4 MiB default.
    const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
    Ok(CompactTxStreamerClient::new(channel).max_decoding_message_size(MAX_MESSAGE_BYTES))
}

/// Server identity and chain tip as reported by the server.
pub async fn lightd_info(client: &mut LwdClient) -> Result<service::LightdInfo> {
    Ok(client.get_lightd_info(Empty {}).await.map_err(|s| anyhow!("GetLightdInfo: {s}"))?.into_inner())
}

/// That the server follows `network`: a deployment's light wallet server on the wrong Zcash network
/// is refused before anything is read from it.
pub async fn check_network(client: &mut LwdClient, network: crate::network::ZcashNetwork) -> Result<()> {
    let chain = lightd_info(client).await?.chain_name;
    ensure!(
        chain == network.lightd_chain_name(),
        "the light wallet server follows Zcash chain {chain:?}, the deployment Zcash {network}"
    );
    Ok(())
}

/// Height and hash of the latest block known to the server.
pub async fn latest_block(client: &mut LwdClient) -> Result<service::BlockId> {
    Ok(client
        .get_latest_block(ChainSpec {})
        .await
        .map_err(|s| anyhow!("GetLatestBlock: {s}"))?
        .into_inner())
}

/// The compact block at `height`.
pub async fn compact_block(client: &mut LwdClient, height: u32) -> Result<CompactBlock> {
    let block = client
        .get_block(BlockId {
            height: height.into(),
            hash: vec![],
        })
        .await
        .map_err(|s| anyhow!("GetBlock({height}): {s}"))?
        .into_inner();
    ensure!(block.height == u64::from(height), "server returned block {} for {height}", block.height);
    Ok(block)
}

/// The txid (internal byte order) of every transaction the server lists in blocks `from..=to`,
/// with its block height, every pool requested. Lets a claimant find a payout by testing its
/// claim reference `keccak256(salt || txid)` against each one (Tier 3, F3).
pub async fn block_txids(client: &mut LwdClient, from: u32, to: u32) -> Result<Vec<(u32, [u8; 32])>> {
    if from > to {
        return Ok(vec![]);
    }
    let block = |height: u32| BlockId {
        height: height.into(),
        hash: vec![],
    };
    let mut stream = client
        .get_block_range(service::BlockRange {
            start: Some(block(from)),
            end: Some(block(to)),
            pool_types: vec![1, 2, 3, 4],
        })
        .await
        .map_err(|s| anyhow!("GetBlockRange({from}..={to}): {s}"))?
        .into_inner();
    let mut out = vec![];
    while let Some(b) = tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, stream.message())
        .await
        .context("GetBlockRange: the stream stalled")?
        .map_err(|s| anyhow!("GetBlockRange: {s}"))?
    {
        let height = u32::try_from(b.height)?;
        for tx in b.vtx {
            let txid: [u8; 32] =
                tx.txid.try_into().map_err(|_| anyhow!("a compact transaction's txid is not 32 bytes"))?;
            out.push((height, txid));
        }
    }
    Ok(out)
}

/// The raw bytes of a transaction and the height of the block that mined it (0 if unmined).
pub async fn raw_transaction(client: &mut LwdClient, txid: &TxId) -> Result<(Vec<u8>, u64)> {
    let raw = client
        .get_transaction(TxFilter {
            hash: txid.as_ref().to_vec(),
            ..Default::default()
        })
        .await
        .map_err(|s| anyhow!("GetTransaction: {s}"))?
        .into_inner();
    Ok((raw.data, raw.height))
}

/// True if a `GetTransaction` error status definitely means "no such transaction" (neither mined
/// nor in the server's node's mempool): lightwalletd answers `NotFound`; Zebra-backed servers may
/// relay the node's RPC error ("No such mempool or main chain transaction") with another code.
pub fn is_not_found(code: Code, message: &str) -> bool {
    match code {
        Code::NotFound => true,
        // Transport and load problems never mean "not found".
        Code::Unavailable
        | Code::DeadlineExceeded
        | Code::Cancelled
        | Code::ResourceExhausted
        | Code::Unauthenticated
        | Code::PermissionDenied => false,
        _ => message.contains("No such mempool or main chain transaction"),
    }
}

/// [`raw_transaction`], or `None` when the server definitely has no such transaction: one just
/// broadcast may not have reached its node yet, or the node dropped it (the payer rebroadcasts).
pub async fn raw_transaction_if_known(client: &mut LwdClient, txid: &TxId) -> Result<Option<(Vec<u8>, u64)>> {
    let answer = client
        .get_transaction(TxFilter {
            hash: txid.as_ref().to_vec(),
            ..Default::default()
        })
        .await;
    match answer {
        Ok(raw) => {
            let raw = raw.into_inner();
            Ok(Some((raw.data, raw.height)))
        }
        Err(status) if is_not_found(status.code(), status.message()) => Ok(None),
        // The message may carry server details; the code is enough.
        Err(status) => Err(anyhow!("GetTransaction failed ({:?})", status.code())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network: the Testnet block of the TEST-ONLY vault 9's deposit (4,488,543) lists its txid.
    /// `cargo test -p zecured-zcash-ops -- --ignored block_txids`
    #[tokio::test]
    #[ignore = "needs the Testnet light wallet server"]
    async fn block_txids_lists_a_known_deposit() {
        let mut client = connect("https://testnet.zec.rocks:443").await.unwrap();
        let txids = block_txids(&mut client, 4_488_543, 4_488_543).await.unwrap();
        let deposit = TxId::from_hex("61a4fdc0dff08cff313eb34bc233d265ea1a70b49bc33098ef25194f01305ebe").unwrap();
        assert!(txids.iter().all(|(h, _)| *h == 4_488_543));
        assert!(txids.iter().any(|(_, t)| t == deposit.as_ref()), "{txids:?}");
    }
}
