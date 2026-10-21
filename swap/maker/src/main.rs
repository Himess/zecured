//! `zecured-swap-maker`: Zecured Swap's market maker (docs/SWAP.md "The market maker").
//!
//!   zecured-swap-maker run --config maker.json        # MAKER_PRIVATE_KEY: the maker's Base key
//!   zecured-swap-maker invite-hash < code.txt         # keccak256 of an invite code, for the config
//!
//! It quotes (`GET /swap/quote`: the oracle price plus or minus the spread, within its inventory,
//! its caps and ZecuredSwap's, only while it follows both chains and the relay keeps up), follows
//! its orders on Base, takes a Buy taker's destination (`POST /swap/orders/{id}/destination`),
//! pays one Ironwood note with the memo "ZSWP" || orderId and `ovk = None` after a random privacy
//! delay, proves the payment once the relay has the order's tier of work on top (after a random
//! number of extra blocks), submits the proof and finalizes; on Sell it refunds itself an order its
//! taker did not prove in time. It never logs a receiver, a salt, a key, a txid or an IP.

mod api;
mod config;
mod engine;
mod gate;
mod pricing;
mod quoting;
mod state;

use std::{io::Read, path::PathBuf, process::Command, sync::Arc};

use alloy::{
    network::EthereumWallet,
    primitives::{B256, U256, keccak256},
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
    chain::swap_version_active,
    quote::swap_domain,
};

use crate::{
    config::MakerConfig,
    engine::{Maker, Wallet},
    quoting::ChainLimits,
    state::Book,
};

/// RISC Zero's Groth16 verifier v3.0 selector behind the router; the MOCK verifier's.
const GROTH16_SELECTOR: [u8; 4] = [0x73, 0xc4, 0x57, 0xba];
const MOCK_SELECTOR: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

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

/// The image id the maker's prover proves with (`<host> payment image-id`).
fn prover_image_id(cfg: &MakerConfig) -> Result<B256> {
    let out = Command::new(&cfg.host).args(["payment", "image-id"]).output().context("run the zkVM host")?;
    ensure!(out.status.success(), "the zkVM host gave no payment image id");
    Ok(String::from_utf8(out.stdout)?.trim().parse().context("payment image id")?)
}

async fn run(cfg: MakerConfig, signer: PrivateKeySigner) -> Result<()> {
    // The payments' Zcash network: its encodings, light wallet server, peers and epochs. Its payment
    // guest (payment-test or payment-main) must be an active Swap version (checked below).
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
    // The maker never pays a payment it could not prove: its guest must be an active Swap version.
    let image_id = prover_image_id(&cfg)?;
    let selector = if cfg.mock_seal { MOCK_SELECTOR } else { GROTH16_SELECTOR };
    ensure!(
        swap_version_active(&provider, cfg.swap, selector, image_id).await?,
        "the prover's payment guest is not an active Swap version of ZecuredSwap's version registry"
    );
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
    let maker = Arc::new(Maker::new(cfg, network, signer, provider, domain, limits, book, wallet));
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
