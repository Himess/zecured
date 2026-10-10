//! `zecured-node`: run one Zecured signer node.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use zecured_node::{
    Node, api,
    config::{Identity, NetworkConfig},
};

#[derive(Parser)]
#[command(about = "Zecured signer node")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create this node's identity and print its public info for the network config.
    Init {
        #[arg(long)]
        index: u16,
        #[arg(long)]
        data: PathBuf,
        /// URL the coordinator will use to reach this node.
        #[arg(long)]
        url: String,
        /// Base attestation key (hex); random if omitted.
        #[arg(long, env = "NODE_ETH_PRIVATE_KEY")]
        eth_key: Option<String>,
    },
    /// Run the node.
    Run {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "127.0.0.1:9101")]
        bind: String,
        /// Token for the demo control panel (mode switch).
        #[arg(long, env = "DEMO_ADMIN_TOKEN")]
        admin_token: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::Init { index, data, url, eth_key } => {
            let id = Identity::create(&data.join("identity.json"), index, eth_key.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&id.public_info(&url))?);
            Ok(())
        }
        Cmd::Run { data, config, bind, admin_token } => {
            let cfg = NetworkConfig::load(&config)?;
            let id = Identity::load(&data.join("identity.json"))?;
            let index = id.index;
            let node = Arc::new(Node::open(cfg, id, &data, admin_token.filter(|t| !t.is_empty()))?);

            let bg = node.clone();
            tokio::spawn(async move {
                loop {
                    match bg.sync_base().await {
                        Ok(0) => {}
                        Ok(n) => info!(applied = n, "base events"),
                        Err(e) => warn!("base sync failed: {e:#}"),
                    }
                    // Public Base RPCs rate-limit aggressively; nodes poll every ~15 s,
                    // staggered by index so five nodes do not burst together.
                    tokio::time::sleep(Duration::from_secs(15) + Duration::from_millis(700 * u64::from(index))).await;
                }
            });

            let listener = tokio::net::TcpListener::bind(&bind).await?;
            info!(index, %bind, "signer node listening");
            axum::serve(listener, api::router(node)).await?;
            Ok(())
        }
    }
}
