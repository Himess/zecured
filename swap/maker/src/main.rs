//! `zecured-swap-maker`: Zecured Swap's market maker (docs/SWAP.md "The market maker").
//!
//!   zecured-swap-maker run --config maker.json        # MAKER_PRIVATE_KEY: the maker's Base key
//!   zecured-swap-maker invite-hash < code.txt         # keccak256 of an invite code, for the config
//!
//! It quotes (`GET /swap/quote`: the oracle price plus or minus the spread, within its inventory,
//! its caps and ZecuredSwap's, only while it follows both chains, the relay keeps up and its
//! guest's Swap version works: W-7), follows its orders on Base, takes a Buy taker's destination
//! (`POST /swap/orders/{id}/destination`), fixes the height of the proof's anchor, pays one
//! Ironwood note with the memo "ZSWP" || orderId and `ovk = None` after a random privacy delay,
//! proves the payment under the header at exactly that height once the relay has the order's tier
//! of work on top of it (W-10), submits the proof and finalizes; on Sell it refunds itself an order
//! its taker did not prove in time. It never logs a receiver, a salt, a key, a txid or an IP, nor a
//! payment's block next to its order.

mod api;
mod config;
mod engine;
mod gate;
mod pricing;
mod quoting;
mod state;
#[cfg(test)]
mod testkit;

use std::{io::Read, path::PathBuf, sync::Arc};

use alloy::{
    network::EthereumWallet,
    primitives::{U256, keccak256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use zecured_base::gas_with_headroom;
use zecured_swap::{
    Side,
    abi::{IERC20, IZecuredSwap},
    chain::{swap_version, swap_version_id, swap_version_working},
    quote::swap_domain,
};

use crate::{
    config::MakerConfig,
    engine::{Maker, Wallet},
    quoting::ChainLimits,
    state::Book,
};

#[derive(Parser)]
#[command(about = "Zecured Swap market maker")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the maker: the HTTP API and the order engine.
    Run {
        #[arg(long, env = "MAKER_CONFIG", default_value = "data/maker/maker.json")]
        config: PathBuf,
        /// The maker's Base key (never on the command line).
        #[arg(long, env = "MAKER_PRIVATE_KEY", hide_env_values = true)]
        key: String,
    },
    /// Print keccak256 of the invite code read from standard input (for `invite_code_hashes`).
    InviteHash,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::InviteHash => {
            let mut code = String::new();
            std::io::stdin().read_to_string(&mut code)?;
            let code = code.trim();
            ensure!(!code.is_empty(), "no invite code on standard input");
            println!("{}", keccak256(code.as_bytes()));
            Ok(())
        }
        Cmd::Run { config, key } => run(MakerConfig::load(&config)?, key.parse().context("MAKER_PRIVATE_KEY")?).await,
    }
}

async fn run(cfg: MakerConfig, signer: PrivateKeySigner) -> Result<()> {
    // The payments' Zcash network: its encodings, light wallet server, peers and epochs. Its payment
    // guest (payment-test or payment-main) must be a Swap version (checked below).
    let network = cfg.zcash()?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_http(cfg.rpc.parse()?)
        .erased();
    let chain_id = provider.get_chain_id().await.context("Base chain id")?;
    ensure!(chain_id == cfg.chain_id, "the RPC is chain {chain_id}, the config says {}", cfg.chain_id);

    let swap = IZecuredSwap::new(cfg.swap, &provider);
    let limits = ChainLimits {
        max_order_usdc: swap.maxOrderUsdc().call().await.context("ZecuredSwap.maxOrderUsdc")?,
        max_order_zat: swap.maxOrderZat().call().await?,
        min_pay_window: swap.minPayWindow().call().await?,
        max_pay_window: swap.maxPayWindow().call().await?,
    };
    ensure!(
        (limits.min_pay_window..=limits.max_pay_window).contains(&cfg.pay_window_secs),
        "pay_window_secs is outside ZecuredSwap's {}..={}",
        limits.min_pay_window,
        limits.max_pay_window
    );
    // The maker never pays a payment it could not prove (W-7): it quotes, takes destinations, pays
    // and proves only while its guest's Swap version is working, as read before each. A guest never
    // scheduled is a misconfigured host; a version that does not work now (not active yet, removed,
    // its verifier stopped) leaves the maker running, to finalize and refund what it has open.
    let prover = cfg.prover();
    let version_id = swap_version_id(prover.selector(), prover.image_id()?);
    ensure!(
        swap_version(&provider, cfg.swap, version_id).await?.activeAt != 0,
        "the prover's payment guest was never scheduled as a Swap version of ZecuredSwap's version registry"
    );
    if !swap_version_working(&provider, cfg.swap, version_id).await? {
        warn!(
            "the prover's Swap version is not working (not active yet, removed, or its verifier stopped): no quote, destination or payment until it is"
        );
    }
    if cfg.sides.contains(&Side::Sell) {
        let usdc = IERC20::new(swap.usdc().call().await?, &provider);
        let need = U256::from(cfg.max_order_usdc) * U256::from(cfg.max_open_orders);
        if usdc.allowance(signer.address(), cfg.swap).call().await? < need {
            let call = usdc.approve(cfg.swap, U256::MAX);
            let gas = gas_with_headroom(call.estimate_gas().await.context("estimate approve")?);
            let r = call.gas(gas).send().await?.get_receipt().await?;
            ensure!(r.status(), "approve reverted");
            info!("approved ZecuredSwap for the maker's USDC (Sell orders)");
        }
    }

    let book = Book::load(&cfg.state)?;
    let wallet = Wallet::load(&cfg.wallet)?;
    let domain = swap_domain(cfg.chain_id, cfg.swap);
    let bind = cfg.bind;
    let maker = Arc::new(Maker::new(cfg, network, signer, provider, domain, limits, book, wallet, version_id));
    if maker.cfg.local_fork {
        warn!("local fork: privacy delays may be zero and seals MOCK");
    }
    tokio::spawn(maker.clone().run());
    // O-7: the operators' inventory endpoint, on a loopback address of its own (never proxied).
    if let Some(status_bind) = maker.cfg.status_bind {
        let listener = tokio::net::TcpListener::bind(status_bind).await.context("bind status_bind")?;
        let status = api::status_router(maker.clone());
        tokio::spawn(async move {
            if axum::serve(listener, status).await.is_err() {
                warn!("the operators' inventory endpoint stopped");
            }
        });
        info!("operators' inventory endpoint on the loopback address of status_bind");
    }
    let app = api::router(maker.clone());
    info!(maker = %maker.signer.address(), "swap maker listening");
    axum::serve(tokio::net::TcpListener::bind(bind).await?, app).await?;
    Ok(())
}
