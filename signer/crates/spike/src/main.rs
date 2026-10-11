//! Spike A: a 3-of-5 FROST vault in Zcash's Ironwood pool, end to end.
//!
//! SPIKE CODE: throwaway driver for docs/spikes/SPIKE_A.md. It keeps every share in
//! one JSON file on disk (gitignored), which the real signer nodes never do.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rand_core_06::{OsRng, RngCore};
use reddsa::frost::redpallas::keys::{KeyPackage, PublicKeyPackage};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;
use zcash_protocol::{TxId, consensus::BlockHeight};
use zecured_threshold::{self as threshold, SignerId};
use zecured_zcash_ops::{
    keys, lwd,
    scan::ScanState,
    spend::{self, OutputPrivacy, Payment},
    tx::{self, TxShape},
};

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
    /// Scan the vault from its birthday and list its notes in P1 order.
    Scan,
    /// Show which pools a mined transaction touches (P1 structure check).
    Txshape { txid: String },
    /// Spend all vault notes: pay `--to` (vault OVK) and optionally `--third`
    /// (ovk = None), change back to the vault; FROST-sign with 3 of 5 and broadcast.
    Spend {
        #[arg(long)]
        to: String,
        /// Zatoshis for `--to`.
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        third: Option<String>,
        /// Zatoshis for `--third`.
        #[arg(long, default_value_t = 0)]
        third_amount: u64,
        /// Signer indices (1..=5) that take part.
        #[arg(long, value_delimiter = ',', default_value = "1,2,4")]
        signers: Vec<u16>,
        /// Build, prove and sign, but do not broadcast.
        #[arg(long)]
        dry_run: bool,
    },
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
        Cmd::Scan => scan(&lwd_url(&cli.lwd)?, &load_vault(&cli.vault)?).await.map(|_| ()),
        Cmd::Txshape { txid } => txshape(&lwd_url(&cli.lwd)?, &txid).await,
        Cmd::Spend {
            to,
            amount,
            third,
            third_amount,
            signers,
            dry_run,
        } => {
            spend(
                &lwd_url(&cli.lwd)?,
                &load_vault(&cli.vault)?,
                &to,
                amount,
                third.as_deref(),
                third_amount,
                &signers,
                dry_run,
            )
            .await
        }
    }
}

fn lwd_url(lwd: &Option<String>) -> Result<String> {
    lwd.clone().context("pass --lwd <url> or set LIGHTWALLETD_URL")
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

impl SpikeVault {
    fn fvk(&self) -> Result<orchard::keys::FullViewingKey> {
        let ak: [u8; 32] = hex::decode(&self.ak_hex)?.try_into().map_err(|_| anyhow::anyhow!("ak"))?;
        let sk: [u8; 32] = hex::decode(&self.sk_hex)?.try_into().map_err(|_| anyhow::anyhow!("sk"))?;
        keys::VaultSecret::from_bytes(sk).full_viewing_key(&ak)
    }

    fn frost_keys(&self) -> Result<(BTreeMap<SignerId, KeyPackage>, PublicKeyPackage)> {
        let mut kps = BTreeMap::new();
        for (i, v) in &self.key_packages {
            kps.insert(threshold::signer_id(*i)?, serde_json::from_value(v.clone())?);
        }
        Ok((kps, serde_json::from_value(self.public_key_package.clone())?))
    }
}

async fn scan(url: &str, v: &SpikeVault) -> Result<ScanState> {
    let mut client = lwd::connect(url).await?;
    let tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
    let birthday = u32::try_from(v.birthday)?;
    let mut state = ScanState::from_birthday(&mut client, v.fvk()?, birthday).await?;
    let started = std::time::Instant::now();
    state.scan_to(&mut client, tip).await?;
    state.verify_against_server(&mut client).await?;
    println!(
        "scanned {}..={} in {:.1}s; Ironwood tree root matches the server",
        birthday,
        tip,
        started.elapsed().as_secs_f64()
    );
    for n in &state.notes {
        println!(
            "  note h={} tx#{} act#{} {:>12} zat {:?} txid={} {}",
            n.position.height,
            n.position.tx_index,
            n.position.action_index,
            n.value(),
            n.scope,
            n.txid,
            n.spent_at.map_or("unspent".to_string(), |h| format!("spent@{h}"))
        );
    }
    println!("balance {} zat", state.balance());
    Ok(state)
}

async fn txshape(url: &str, txid: &str) -> Result<()> {
    let mut client = lwd::connect(url).await?;
    let txid = TxId::from_hex(txid).context("invalid txid hex")?;
    let (tx, height) = tx::fetch(&mut client, &NETWORK, &txid).await?;
    let shape = TxShape::of(&tx);
    println!("height {height}  version {:?}", tx.version());
    println!("{shape:#?}");
    println!("ironwood-only: {}", shape.is_ironwood_only());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn spend(
    url: &str,
    v: &SpikeVault,
    to: &str,
    amount: u64,
    third: Option<&str>,
    third_amount: u64,
    signers: &[u16],
    dry_run: bool,
) -> Result<()> {
    let state = scan(url, v).await?;
    let inputs = state.unspent();
    anyhow::ensure!(!inputs.is_empty(), "vault has no unspent notes");

    let mut payments = vec![Payment {
        recipient: keys::ironwood_receiver(&NETWORK, to)?,
        value: amount,
        privacy: OutputPrivacy::VaultOvk,
    }];
    if let Some(third) = third {
        payments.push(Payment {
            recipient: keys::ironwood_receiver(&NETWORK, third)?,
            value: third_amount,
            privacy: OutputPrivacy::NoOvk,
        });
    }
    let fvk = v.fvk()?;
    let target = BlockHeight::from_u32(state.scanned_to + 1);
    let unsigned = spend::build(&NETWORK, target, &fvk, state.anchor(), &inputs, &payments)?;
    println!(
        "built PCZT: {} input(s), {} payment(s), fee {} zat, change {} zat",
        inputs.len(),
        payments.len(),
        unsigned.fee,
        unsigned.change
    );

    let t = std::time::Instant::now();
    let pk = spend::proving_key();
    let pczt = spend::finalize_and_prove(unsigned.pczt, &pk)?;
    println!("proved in {:.1}s", t.elapsed().as_secs_f64());

    let (sighash, requests) = spend::signing_requests(&pczt, &unsigned.spend_action_indices)?;
    let (kps, pkp) = v.frost_keys()?;
    let mut signatures = vec![];
    for r in &requests {
        let sig = threshold::sign_in_process(&kps, &pkp, signers, &sighash, &r.alpha)?;
        signatures.push((r.action_index, sig));
    }
    println!("FROST: {} spend(s) signed by signers {signers:?}", signatures.len());
    let pczt = spend::apply_signatures(pczt, &signatures)?;
    let tx = spend::extract(pczt)?;
    let shape = TxShape::of(&tx);
    println!(
        "extracted {:?} tx {}; {shape:?}; ironwood-only {}",
        tx.version(),
        tx.txid(),
        shape.is_ironwood_only()
    );
    if dry_run {
        println!("dry run: not broadcasting");
        return Ok(());
    }
    let mut client = lwd::connect(url).await?;
    let txid = spend::broadcast(&mut client, &tx).await?;
    println!("broadcast {txid}");
    Ok(())
}
