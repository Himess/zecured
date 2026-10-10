//! Spike A: a 3-of-5 FROST vault in Zcash's Ironwood pool, end to end.
//!
//! SPIKE CODE: throwaway driver for docs/PLAN.md § 18 "Spike A". It keeps every share in
//! one JSON file on disk (gitignored), which the real signer nodes never do.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rand_core_06::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;
use zecured_threshold::{self as threshold, SignerId};
use zecured_zcash_ops::{keys, lwd};

const NETWORK: Network = Network::TestNetwork;

#[derive(Parser)]
#[command(about = "Zecured Spike A: FROST vault on Ironwood")]
struct Cli {
    /// Light wallet server URL (lightwalletd or Zaino gRPC).
    #[arg(long, env = "LIGHTWALLETD_URL", global = true)]
    lwd: Option<String>,
    /// Where the spike vault is stored.
    #[arg(long, default_value = "data/spike/vault.json", global = true)]
    vault: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the light wallet server's view of the chain.
    Info,
    /// Run a 5-party DKG (in process), derive the vault keys and print its address.
    Dkg {
        /// Overwrite an existing vault file.
        #[arg(long)]
        force: bool,
    },
    /// Print the stored vault's address and viewing key.
    Show,
}

/// Everything the spike needs to remember about its single vault.
#[derive(Serialize, Deserialize)]
struct SpikeVault {
    network: String,
    /// Chain height when the vault was created; scanning starts here.
    birthday: u64,
    ua: String,
    ufvk: String,
    ak_hex: String,
    /// ZIP 2005 `sk` agreed by the signers (hex). SECRET.
    sk_hex: String,
    public_key_package: serde_json::Value,
    /// Key package per signer index (1..=5). SECRET.
    key_packages: BTreeMap<u16, serde_json::Value>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info => info(&lwd_url(&cli.lwd)?).await,
        Cmd::Dkg { force } => dkg(&cli.vault, cli.lwd.as_deref(), force).await,
        Cmd::Show => show(&load_vault(&cli.vault)?),
    }
}

fn lwd_url(lwd: &Option<String>) -> Result<String> {
    lwd.clone()
        .context("pass --lwd <url> or set LIGHTWALLETD_URL")
}

async fn info(url: &str) -> Result<()> {
    let mut client = lwd::connect(url).await?;
    let info = lwd::lightd_info(&mut client).await?;
    let tip = lwd::latest_block(&mut client).await?;
    println!("server        {} ({})", info.vendor, info.version);
    println!("chain         {}", info.chain_name);
    println!("branch id     0x{}", info.consensus_branch_id);
    println!("server height {}", info.block_height);
    println!("tip           {}", tip.height);
    Ok(())
}

async fn dkg(path: &PathBuf, lwd_url: Option<&str>, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!("{} exists; pass --force to replace it", path.display());
    }
    let birthday = match lwd_url {
        Some(url) => lwd::latest_block(&mut lwd::connect(url).await?).await?.height,
        None => {
            eprintln!("warning: no --lwd given; birthday = 0 (scanning will be slow)");
            0
        }
    };

    let vault_keys = threshold::dkg_in_process(OsRng)?;
    let ak = vault_keys.ak_bytes()?;
    let mut sk = [0u8; 32];
    OsRng.fill_bytes(&mut sk);
    let secret = keys::VaultSecret::from_bytes(sk);
    let fvk = secret.full_viewing_key(&ak)?;
    let ua = keys::encode_ironwood_ua(&NETWORK, keys::vault_address(&fvk))?;
    let ufvk = keys::encode_ufvk(&NETWORK, &fvk)?;

    let mut key_packages = BTreeMap::new();
    for i in 1..=threshold::MAX_SIGNERS {
        let id: SignerId = threshold::signer_id(i)?;
        key_packages.insert(i, serde_json::to_value(&vault_keys.key_packages[&id])?);
    }
    let vault = SpikeVault {
        network: "test".into(),
        birthday,
        ua,
        ufvk,
        ak_hex: hex::encode(ak),
        sk_hex: hex::encode(sk),
        public_key_package: serde_json::to_value(&vault_keys.public_key_package)?,
        key_packages,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(&vault)?)?;
    println!("vault written to {}", path.display());
    show(&vault)
}

fn load_vault(path: &PathBuf) -> Result<SpikeVault> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn show(v: &SpikeVault) -> Result<()> {
    println!("network   {}", v.network);
    println!("birthday  {}", v.birthday);
    println!("ak        {}", v.ak_hex);
    println!("address   {}", v.ua);
    println!("ufvk      {}", v.ufvk);
    println!("zip321    zcash:{}?amount=0.1", v.ua);
    Ok(())
}
