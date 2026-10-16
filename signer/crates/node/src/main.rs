//! `zecured-node`: run one Zecured signer node.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use zecured_node::{
    Node, NodeOptions, api,
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
    /// Tier 3 (review RT-1): writes what this signer's operator needs to audit a vault's payouts
    /// as part of the honest minority: the vault's viewing key, and the salt and receiver of every
    /// claim's recipient, each in its own file readable by the operator only. Check them with
    /// `zecured-wallet watch --ufvk-file <..>.ufvk --from <birthday> --tier3 --destinations
    /// <..>-destinations.json`, which reports any commitment that names a claim under a wrong
    /// reference (F4). Never share these files.
    ExportAudit {
        #[arg(long)]
        data: PathBuf,
        /// The vault id (0x hex).
        #[arg(long)]
        vault: alloy::primitives::B256,
        /// Directory for the two files.
        #[arg(long)]
        out: PathBuf,
    },
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
        #[arg(long, env = "NODE_ETH_PRIVATE_KEY", hide_env_values = true)]
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
        /// Token for the demo control panel (mode switch). Never set it on a production node.
        #[arg(long, env = "DEMO_ADMIN_TOKEN", hide_env_values = true)]
        admin_token: Option<String>,
        /// DEMO ONLY: let the demo panel switch this node to "compromised", where it signs any
        /// transaction without policy checks. Needs a build with the `demo` feature; set it on
        /// two nodes at most, never on a production node.
        #[arg(long)]
        demo_allow_compromise: bool,
        /// DEVELOPMENT ONLY: start although the network config names no coordinator, so that
        /// the API accepts unauthenticated requests.
        #[arg(long)]
        insecure_no_auth: bool,
        /// This operator's own light wallet server, instead of the network config's: a node
        /// should not depend on the same Zcash data source as the coordinator.
        #[arg(long, env = "NODE_LIGHTWALLETD_URL")]
        lightwalletd_url: Option<String>,
        /// Independent light wallet servers (comma-separated) that must all agree with ours on a
        /// deposit (C-1). Registry v3 activates vaults on deposit proofs only, so no signing path
        /// asks them any more; the option stays for existing operator configurations.
        #[arg(long, env = "NODE_LIGHTWALLETD_WITNESS_URLS", value_delimiter = ',')]
        lightwalletd_witness_urls: Vec<String>,
        /// This operator's own Base RPC, instead of the network config's.
        #[arg(long, env = "NODE_BASE_RPC_URL", hide_env_values = true)]
        base_rpc_url: Option<String>,
        /// An independent Base RPC that must agree with ours on every Registry event.
        #[arg(long, env = "NODE_BASE_RPC_WITNESS_URL", hide_env_values = true)]
        base_rpc_witness_url: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::ExportAudit { data, vault, out } => {
            use anyhow::Context;
            use zecured_node::store::{Store, private_dir, write_atomic};
            let store = Store::open(&data.join("store"))?;
            let v = store.vaults.get(&vault).context("this node has no such vault")?;
            private_dir(&out)?;
            let name = hex::encode(&vault[..8]);
            let ufvk_path = out.join(format!("vault-{name}.ufvk"));
            write_atomic(&ufvk_path, v.ufvk.as_bytes())?;
            let destinations: Vec<serde_json::Value> = v
                .claims
                .iter()
                .filter_map(|c| {
                    let r = store.recipient(&c.commitment)?;
                    Some(serde_json::json!({
                        "commitment": alloy::primitives::B256::from(c.commitment),
                        "receiver": hex::encode(r.receiver),
                        "salt": hex::encode(r.salt),
                    }))
                })
                .collect();
            let destinations_path = out.join(format!("vault-{name}-destinations.json"));
            write_atomic(&destinations_path, &serde_json::to_vec_pretty(&destinations)?)?;
            println!(
                "{} destinations; audit with:\n  zecured-wallet watch --ufvk-file {} --from {} --tier3 --destinations {}",
                destinations.len(),
                ufvk_path.display(),
                v.birthday,
                destinations_path.display()
            );
            Ok(())
        }
        Cmd::Init {
            index,
            data,
            url,
            eth_key,
        } => {
            let id = Identity::create(&data.join("identity.json"), index, eth_key.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&id.public_info(&url))?);
            Ok(())
        }
        Cmd::Run {
            data,
            config,
            bind,
            admin_token,
            demo_allow_compromise,
            insecure_no_auth,
            lightwalletd_url,
            lightwalletd_witness_urls,
            base_rpc_url,
            base_rpc_witness_url,
        } => {
            let mut cfg = NetworkConfig::load(&config)?;
            if let Some(url) = lightwalletd_url.filter(|u| !u.is_empty()) {
                cfg.lightwalletd_url = url;
            }
            if let Some(url) = base_rpc_url.filter(|u| !u.is_empty()) {
                cfg.base_rpc_url = url;
            }
            let opts = NodeOptions {
                admin_token: admin_token.filter(|t| !t.is_empty()),
                demo_allow_compromise,
                insecure_no_auth,
                lightwalletd_witnesses: lightwalletd_witness_urls
                    .into_iter()
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty())
                    .collect(),
                base_rpc_witness: base_rpc_witness_url.filter(|u| !u.is_empty()),
            };
            if cfg.coordinator.is_none() && insecure_no_auth {
                warn!("--insecure-no-auth: the node API accepts unauthenticated requests (development only)");
            }
            if demo_allow_compromise {
                warn!("DEMO: this node can be switched to \"compromised\" mode (signs without policy checks)");
            }
            info!(
                lightwalletd_witnesses = opts.lightwalletd_witnesses.len(),
                base_rpc_witness = opts.base_rpc_witness.is_some(),
                "independent data sources"
            );
            #[cfg(feature = "demo-tier3")]
            {
                warn!("DEMO (theft demo build): three nodes of this network can be compromised");
                zecured_base::theft_demo_guard::check_on_chain(&cfg.base_rpc_url, cfg.registry).await?;
            }
            let id = Identity::load(&data.join("identity.json"))?;
            let index = id.index;
            let node = Arc::new(Node::open(cfg, id, &data, opts)?);

            let bg = node.clone();
            tokio::spawn(async move {
                loop {
                    match bg.sync_base().await {
                        Ok(0) => {}
                        Ok(n) => info!(applied = n, "base events"),
                        Err(e) => warn!("base sync failed: {e:#}"),
                    }
                    // T-05: claims must be paid long before the bonds would pay their claimants.
                    if let Err(e) = bg.check_liveness().await {
                        warn!("liveness check failed: {e:#}");
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
