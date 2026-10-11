//! Light wallet server (lightwalletd / Zaino) gRPC access.
//!
//! The server is trusted for data availability, not for spend authority: every signer
//! node uses its own connection and independently checks what it is asked to sign.

use anyhow::{Context, Result, anyhow};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use zcash_client_backend::proto::service::{
    self, ChainSpec, Empty, compact_tx_streamer_client::CompactTxStreamerClient,
};

/// A gRPC client for the CompactTxStreamer service.
pub type LwdClient = CompactTxStreamerClient<Channel>;

/// Connects to a light wallet server, e.g. `https://host:443` or `http://127.0.0.1:9067`.
pub async fn connect(url: &str) -> Result<LwdClient> {
    let mut endpoint = Endpoint::from_shared(url.to_owned()).context("invalid server URL")?;
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
