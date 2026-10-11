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
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
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
