//! `zecured-wallet`: a minimal Ironwood wallet plus the borrower's off-chain loan actions.
//!
//! The wallet holds one Orchard-protocol spending key and only ever transacts Ironwood →
//! Ironwood. Each loan gets a fresh owner key (as the browser app does), stored in the
//! loan file; every owner request is an EIP-712 signature under the Registry's domain.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, B256},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Parser, Subcommand};
use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey};
use pczt::roles::signer::Signer;
use rand_core_06::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zcash_protocol::consensus::{BlockHeight, Network};
use zecured_base::{Destination, OpenVault, RefundRequest, ViewKeyRequest, registry_domain, sign_typed};
use zecured_zcash_ops::{
    keys::{encode_ironwood_ua, ironwood_receiver},
    lwd,
    scan::ScanState,
    spend::{self, OutputPrivacy, Payment},
};

mod base_test;

const NETWORK: Network = Network::TestNetwork;
const ZATS_PER_ZEC: f64 = 100_000_000.0;
/// Owner requests expire after this many seconds.
const REQUEST_TTL_SECS: u64 = 600;

#[derive(Parser)]
#[command(about = "Zecured CLI wallet (Ironwood only) and loan owner actions")]
struct Cli {
    #[arg(long, default_value = "data/wallet/wallet.json", global = true)]
    wallet: PathBuf,
    #[arg(long, env = "LIGHTWALLETD_URL", default_value = "https://testnet.zec.rocks:443", global = true)]
    lwd: String,
    #[arg(long, env = "ZECURED_BACKEND", default_value = "http://127.0.0.1:8080", global = true)]
    backend: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new wallet (fresh spending key, birthday = current tip).
    Init,
    /// Print the wallet's Ironwood-only unified address.
    Address,
    /// Scan and show the wallet's notes and balance.
    Balance,
    /// Send ZEC to an address's Ironwood receiver.
    Send {
        #[arg(long)]
        to: String,
        /// Amount in ZEC.
        #[arg(long)]
        amount: f64,
    },
    /// Watch any vault (or wallet) with its unified full viewing key: shows the notes it
    /// received and spent. This is how a borrower verifies their vault independently.
    Watch {
        #[arg(long)]
        ufvk: String,
        /// First block to scan (e.g. the vault's birthday).
        #[arg(long)]
        from: u32,
    },
    #[command(subcommand)]
    Loan(LoanCmd),
}

#[derive(Subcommand)]
enum LoanCmd {
    /// Open a vault for a new loan (fresh owner key).
    Open {
        /// Collateral in ZEC (a standard denomination, e.g. 0.01).
        #[arg(long)]
        amount: f64,
        /// Smart account address (defaults to the owner address for CLI testing).
        #[arg(long)]
        account: Option<Address>,
    },
    /// Show the vault status from the backend.
    Status { vault: B256 },
    /// Pay the vault from the wallet (`--amount` overrides the denomination, for tests).
    Deposit {
        vault: B256,
        #[arg(long)]
        amount: Option<f64>,
    },
    /// Pre-validate where the remaining ZEC goes (defaults to this wallet).
    Destination {
        vault: B256,
        #[arg(long)]
        to: Option<String>,
    },
    /// Ask for a non-conforming deposit back (defaults to this wallet).
    Refund {
        vault: B256,
        #[arg(long)]
        note: B256,
        #[arg(long)]
        to: Option<String>,
    },
    /// Fetch the vault's viewing key (UFVK) to verify it independently.
    ViewingKey { vault: B256 },
    /// TEST ONLY (the app uses a gasless smart account): borrow USDC with the owner EOA as
    /// the account, after authorizing the Registry and the Adapter on Morpho.
    Borrow {
        vault: B256,
        /// USDC amount (6 decimals), e.g. 1.5
        #[arg(long)]
        usdc: f64,
        #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
        rpc: String,
    },
    /// TEST ONLY: repay all debt with the owner EOA, pre-validate the destination and call
    /// requestRelease (P2).
    RepayRelease {
        vault: B256,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
        rpc: String,
    },
    /// Close from collateral (P6): take the market maker's quote, sign the CloseRequest and
    /// hand it to the market maker through the backend.
    Close {
        vault: B256,
        #[arg(long)]
        to: Option<String>,
        /// Extra slippage tolerance on the quoted user amount, in basis points.
        #[arg(long, default_value_t = 200)]
        slippage_bps: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct WalletFile {
    spending_key: String,
    birthday: u32,
}

#[derive(Serialize, Deserialize)]
struct LoanFile {
    vault_id: B256,
    owner_key: String,
    account: Address,
    amount: u64,
    address: String,
    registry: Address,
    chain_id: u64,
    next_nonce: u64,
}

struct Wallet {
    sk: SpendingKey,
    fvk: FullViewingKey,
    birthday: u32,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn zats(zec: f64) -> Result<u64> {
    ensure!(zec > 0.0, "amount must be positive");
    Ok((zec * ZATS_PER_ZEC).round() as u64)
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(path, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)?)
}

fn load_wallet(path: &Path) -> Result<Wallet> {
    let f: WalletFile = read_json(path)?;
    let bytes: [u8; 32] = hex::decode(&f.spending_key)?.try_into().map_err(|_| anyhow!("bad key"))?;
    let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes(bytes)).context("invalid spending key")?;
    Ok(Wallet {
        fvk: FullViewingKey::from(&sk),
        sk,
        birthday: f.birthday,
    })
}

fn loan_path(wallet: &Path, vault: &B256) -> PathBuf {
    wallet.with_file_name(format!("loan-{}.json", hex::encode(&vault[..8])))
}

/// Network parameters the backend's signers expect for owner signatures.
#[derive(Deserialize)]
struct NetworkParams {
    registry: Address,
    chain_id: u64,
}

fn network_params() -> Result<NetworkParams> {
    let cfg: serde_json::Value = read_json(Path::new(
        &std::env::var("ZECURED_NETWORK_CONFIG").unwrap_or_else(|_| "signer/data/network.json".into()),
    ))?;
    Ok(serde_json::from_value(cfg)?)
}

async fn post(backend: &str, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let resp = reqwest::Client::new()
        .post(format!("{}{path}", backend.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .context("backend unreachable")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or(json!(null));
    if !status.is_success() {
        bail!("backend: {}", v.get("error").and_then(|e| e.as_str()).unwrap_or("request failed"));
    }
    Ok(v)
}

async fn scan(w: &Wallet, url: &str) -> Result<ScanState> {
    let mut client = lwd::connect(url).await?;
    let tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
    let mut state = ScanState::from_birthday(&mut client, w.fvk.clone(), w.birthday).await?;
    state.scan_to(&mut client, tip).await?;
    Ok(state)
}

/// Sends `amount` zatoshis to `to`'s Ironwood receiver; change returns to the wallet.
async fn send(w: &Wallet, url: &str, to: &str, amount: u64) -> Result<String> {
    let state = scan(w, url).await?;
    let mut inputs = vec![];
    let mut total = 0;
    for n in state.unspent() {
        if total >= amount + spend::zip317_fee(inputs.len() + 1, 2) {
            break;
        }
        total += n.value();
        inputs.push(n);
    }
    let payments = [Payment {
        recipient: ironwood_receiver(&NETWORK, to)?,
        value: amount,
        privacy: OutputPrivacy::VaultOvk,
    }];
    let unsigned = spend::build(&NETWORK, BlockHeight::from_u32(state.scanned_to + 1), &w.fvk, state.anchor(), &inputs, &payments)?;
    let pk = tokio::task::spawn_blocking(spend::proving_key).await?;
    let pczt = spend::finalize_and_prove(unsigned.pczt, &pk)?;
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    let ask = SpendAuthorizingKey::from(&w.sk);
    for i in &unsigned.spend_action_indices {
        signer
            .sign_ironwood(rand_010::rng(), *i, &ask)
            .map_err(|e| anyhow!("sign: {e:?}"))?;
    }
    let tx = spend::extract(signer.finish())?;
    let mut client = lwd::connect(url).await?;
    Ok(spend::broadcast(&mut client, &tx).await?.to_string())
}

fn fresh_salt() -> [u8; 32] {
    let mut s = [0u8; 32];
    OsRng.fill_bytes(&mut s);
    s
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => {
            ensure!(!cli.wallet.exists(), "{} exists", cli.wallet.display());
            let sk = loop {
                let mut b = [0u8; 32];
                OsRng.fill_bytes(&mut b);
                if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
                    break sk;
                }
            };
            let tip = u32::try_from(lwd::latest_block(&mut lwd::connect(&cli.lwd).await?).await?.height)?;
            write_json(
                &cli.wallet,
                &WalletFile {
                    spending_key: hex::encode(sk.to_bytes()),
                    birthday: tip,
                },
            )?;
            let w = load_wallet(&cli.wallet)?;
            println!("{}", encode_ironwood_ua(&NETWORK, w.fvk.address_at(0u32, Scope::External))?);
        }
        Cmd::Address => {
            let w = load_wallet(&cli.wallet)?;
            println!("{}", encode_ironwood_ua(&NETWORK, w.fvk.address_at(0u32, Scope::External))?);
        }
        Cmd::Balance => {
            let w = load_wallet(&cli.wallet)?;
            let s = scan(&w, &cli.lwd).await?;
            for n in s.unspent() {
                println!("  {:>12} zat  h={} {:?}", n.value(), n.position.height, n.scope);
            }
            println!("balance {} zat (scanned to {})", s.balance(), s.scanned_to);
        }
        Cmd::Send { to, amount } => {
            let w = load_wallet(&cli.wallet)?;
            println!("broadcast {}", send(&w, &cli.lwd, &to, zats(amount)?).await?);
        }
        Cmd::Watch { ufvk, from } => {
            let fvk = zcash_keys::keys::UnifiedFullViewingKey::decode(&NETWORK, &ufvk)
                .map_err(|e| anyhow!("invalid UFVK: {e}"))?
                .orchard()
                .cloned()
                .context("UFVK has no Orchard-protocol key")?;
            let mut client = lwd::connect(&cli.lwd).await?;
            let tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
            let mut state = ScanState::from_birthday(&mut client, fvk, from).await?;
            state.scan_to(&mut client, tip).await?;
            state.verify_against_server(&mut client).await?;
            for n in &state.notes {
                println!(
                    "  {:>12} zat  received h={} ({:?})  {}",
                    n.value(),
                    n.position.height,
                    n.scope,
                    n.spent_at.map_or("unspent".to_owned(), |h| format!("spent h={h}"))
                );
            }
            println!("unspent {} zat (scanned {from}..={tip}, tree root verified)", state.balance());
        }
        Cmd::Loan(cmd) => loan(&cli.wallet, &cli.lwd, &cli.backend, cmd).await?,
    }
    Ok(())
}

async fn loan(wallet: &Path, lwd_url: &str, backend: &str, cmd: LoanCmd) -> Result<()> {
    match cmd {
        LoanCmd::Open { amount, account } => {
            let net = network_params()?;
            let owner = PrivateKeySigner::random();
            let account = account.unwrap_or(owner.address());
            let amount = zats(amount)?;
            let mut rid = [0u8; 32];
            OsRng.fill_bytes(&mut rid);
            let deadline = now() + REQUEST_TTL_SECS;
            let msg = OpenVault {
                owner: owner.address(),
                account,
                amount,
                requestId: B256::from(rid),
                deadline,
            };
            let sig = sign_typed(&owner, &registry_domain(net.chain_id, net.registry), &msg)?;
            let r = post(
                backend,
                "/vaults",
                json!({ "owner": owner.address(), "account": account, "amount": amount.to_string(),
                        "requestId": B256::from(rid), "deadline": deadline.to_string(), "signature": sig }),
            )
            .await?;
            let vault_id: B256 = serde_json::from_value(r["vaultId"].clone())?;
            write_json(
                &loan_path(wallet, &vault_id),
                &LoanFile {
                    vault_id,
                    owner_key: hex::encode(owner.to_bytes()),
                    account,
                    amount,
                    address: r["address"].as_str().unwrap_or_default().to_owned(),
                    registry: net.registry,
                    chain_id: net.chain_id,
                    next_nonce: 1,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        LoanCmd::Status { vault } => {
            let v: serde_json::Value = reqwest::get(format!("{}/vaults/{vault}", backend.trim_end_matches('/')))
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        LoanCmd::Deposit { vault, amount } => {
            let l: LoanFile = read_json(&loan_path(wallet, &vault))?;
            let w = load_wallet(wallet)?;
            let amount = match amount {
                Some(a) => zats(a)?,
                None => l.amount,
            };
            println!("broadcast {}", send(&w, lwd_url, &l.address, amount).await?);
        }
        LoanCmd::Destination { vault, to } => {
            let c = destination(wallet, backend, &vault, to).await?;
            println!("{}", json!({ "commitment": c }));
        }
        LoanCmd::Refund { vault, note, to } => {
            let (mut l, owner, to) = owner_ctx(wallet, &vault, to)?;
            let salt = fresh_salt();
            let receiver = ironwood_receiver(&NETWORK, &to)?.to_raw_address_bytes();
            let nonce = l.next_nonce;
            let deadline = now() + REQUEST_TTL_SECS;
            let msg = RefundRequest {
                vaultId: vault,
                noteId: note,
                commitment: B256::from(commitment(&salt, &receiver)),
                nonce,
                deadline,
            };
            let sig = sign_typed(&owner, &registry_domain(l.chain_id, l.registry), &msg)?;
            let r = post(
                backend,
                &format!("/vaults/{vault}/refund"),
                json!({ "noteId": note, "receiver": to, "salt": hex::encode(salt), "nonce": nonce.to_string(),
                        "deadline": deadline.to_string(), "signature": sig }),
            )
            .await?;
            l.next_nonce += 1;
            write_json(&loan_path(wallet, &vault), &l)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        LoanCmd::ViewingKey { vault } => {
            let (mut l, owner, _) = owner_ctx(wallet, &vault, Some(String::new()))?;
            let nonce = l.next_nonce;
            let deadline = now() + REQUEST_TTL_SECS;
            let msg = ViewKeyRequest {
                vaultId: vault,
                nonce,
                deadline,
            };
            let sig = sign_typed(&owner, &registry_domain(l.chain_id, l.registry), &msg)?;
            let r = post(
                backend,
                &format!("/vaults/{vault}/viewing-key"),
                json!({ "nonce": nonce.to_string(), "deadline": deadline.to_string(), "signature": sig }),
            )
            .await?;
            l.next_nonce += 1;
            write_json(&loan_path(wallet, &vault), &l)?;
            println!("{}", r["ufvk"].as_str().unwrap_or_default());
        }
        LoanCmd::Borrow { vault, usdc, rpc } => base_test::borrow(wallet, &vault, usdc, &rpc).await?,
        LoanCmd::RepayRelease { vault, to, rpc } => {
            let commitment = destination(wallet, backend, &vault, to).await?;
            base_test::repay_release(wallet, &vault, commitment, &rpc).await?;
        }
        LoanCmd::Close { vault, to, slippage_bps } => {
            let (l, owner, to) = owner_ctx(wallet, &vault, to)?;
            let quote: serde_json::Value = reqwest::get(format!("{}/quotes/close/{vault}", backend.trim_end_matches('/')))
                .await?
                .json()
                .await?;
            ensure!(quote.get("error").is_none(), "quote failed: {}", quote["error"]);
            let user_amount: u64 = quote["userAmount"].as_str().context("quote")?.parse()?;
            let min_user = user_amount.saturating_mul(10_000 - slippage_bps) / 10_000;
            let commitment = destination(wallet, backend, &vault, Some(to)).await?;
            let deployments = base_test::Deployment::load()?;
            let mut nonce_bytes = [0u8; 16];
            OsRng.fill_bytes(&mut nonce_bytes);
            let req = zecured_base::CloseRequest {
                vaultId: vault,
                account: l.account,
                executor: quote["executor"].as_str().context("quote")?.parse()?,
                marginBps: u16::try_from(quote["marginBps"].as_u64().context("quote")?)?,
                minUserAmount: min_user,
                userCommitment: commitment,
                nonce: alloy::primitives::U256::from(u128::from_be_bytes(nonce_bytes)),
                deadline: now() + REQUEST_TTL_SECS,
            };
            let sig = sign_typed(&owner, &zecured_base::adapter_domain(l.chain_id, deployments.adapter), &req)?;
            let r = post(
                backend,
                "/closes",
                json!({ "request": {
                            "vaultId": req.vaultId, "account": req.account, "executor": req.executor,
                            "marginBps": req.marginBps, "minUserAmount": req.minUserAmount.to_string(),
                            "userCommitment": req.userCommitment, "nonce": req.nonce.to_string(),
                            "deadline": req.deadline.to_string() },
                        "signature": sig }),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
    }
    Ok(())
}

/// Pre-validates a destination with the signers; returns its commitment.
async fn destination(wallet: &Path, backend: &str, vault: &B256, to: Option<String>) -> Result<B256> {
    let (mut l, owner, to) = owner_ctx(wallet, vault, to)?;
    let salt = fresh_salt();
    let receiver = ironwood_receiver(&NETWORK, &to)?.to_raw_address_bytes();
    let nonce = l.next_nonce;
    let deadline = now() + REQUEST_TTL_SECS;
    let c = B256::from(commitment(&salt, &receiver));
    let msg = Destination {
        vaultId: *vault,
        commitment: c,
        nonce,
        deadline,
    };
    let sig = sign_typed(&owner, &registry_domain(l.chain_id, l.registry), &msg)?;
    post(
        backend,
        &format!("/vaults/{vault}/destination"),
        json!({ "receiver": to, "salt": hex::encode(salt), "nonce": nonce.to_string(),
                "deadline": deadline.to_string(), "signature": sig }),
    )
    .await?;
    l.next_nonce += 1;
    write_json(&loan_path(wallet, vault), &l)?;
    Ok(c)
}

/// Loan file, owner key and destination (defaults to the wallet's own address).
fn owner_ctx(wallet: &Path, vault: &B256, to: Option<String>) -> Result<(LoanFile, PrivateKeySigner, String)> {
    let mut l: LoanFile = read_json(&loan_path(wallet, vault))?;
    // Sign under the network's current Registry domain (the signers verify against it).
    if let Ok(net) = network_params() {
        l.registry = net.registry;
        l.chain_id = net.chain_id;
    }
    let owner: PrivateKeySigner = l.owner_key.parse()?;
    let to = match to {
        Some(t) => t,
        None => {
            let w = load_wallet(wallet)?;
            encode_ironwood_ua(&NETWORK, w.fvk.address_at(0u32, Scope::External))?
        }
    };
    Ok((l, owner, to))
}

/// `keccak256(salt32 || receiver43)`.
fn commitment(salt: &[u8; 32], receiver: &[u8; 43]) -> [u8; 32] {
    let mut preimage = salt.to_vec();
    preimage.extend_from_slice(receiver);
    alloy::primitives::keccak256(preimage).0
}

