//! `zecured-wallet`: a minimal Ironwood wallet plus the borrower's off-chain loan actions.
//!
//! The wallet holds one Orchard-protocol spending key and only ever transacts Ironwood →
//! Ironwood. Each loan gets a fresh owner key (as the browser app does), stored in the
//! loan file; every owner request is an EIP-712 signature under the Registry's domain.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, B256, Bytes},
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
use zecured_base::{
    Destination, OpenVault, RefundRequest, VaultOpened, ViewKeyRequest, check_signed_by_all, registry_domain,
    sign_typed, text_hash,
};
use zecured_zcash_ops::{
    keys::{encode_ironwood_ua, ironwood_receiver},
    lwd,
    scan::ScanState,
    spend::{self, OutputPrivacy, Payment},
};

mod base_test;
mod prove;
mod watch;

const NETWORK: Network = Network::TestNetwork;
const ZATS_PER_ZEC: f64 = 100_000_000.0;
/// Signer nodes that must all vouch for a new vault (C-4).
const SIGNERS: usize = 5;
/// Owner requests expire after this many seconds.
const REQUEST_TTL_SECS: u64 = 600;

#[derive(Parser)]
#[command(about = "Zecured CLI wallet (Ironwood only) and loan owner actions")]
struct Cli {
    #[arg(long, default_value = "data/wallet/wallet.json", global = true)]
    wallet: PathBuf,
    #[arg(
        long,
        env = "LIGHTWALLETD_URL",
        default_value = "https://testnet.zec.rocks:443",
        global = true
    )]
    lwd: String,
    #[arg(
        long,
        env = "ZECURED_BACKEND",
        default_value = "http://127.0.0.1:8080",
        global = true
    )]
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
    /// received and spent. This is how a borrower verifies their vault independently. With
    /// `--tier3`, also checks every spend of the vault's lineage against its commitment on Base
    /// and reports what a violation proof can show (F1 uncommitted spend, F2 withheld change);
    /// with `--claims-of`, also that every committed payment of this wallet's claims arrived in
    /// full in its own output (F3).
    Watch {
        /// File holding the unified full viewing key (never on the command line).
        #[arg(long)]
        ufvk_file: PathBuf,
        /// First block to scan (e.g. the vault's birthday).
        #[arg(long)]
        from: u32,
        /// Check the lineage against the Registry's Tier 3 records (docs/TIER3.md).
        #[arg(long)]
        tier3: bool,
        /// With `--tier3`: the loan (vault id) whose claims this wallet checks, with the
        /// destinations its loan file kept and the wallet's own viewing key.
        #[arg(long, requires = "tier3")]
        claims_of: Option<B256>,
        #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
        rpc: String,
    },
    #[command(subcommand)]
    Loan(LoanCmd),
}

#[derive(Subcommand)]
enum LoanCmd {
    /// Open a vault for a new loan (fresh owner key). Refused unless every signer of the
    /// network config signed the vault's address (C-4).
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
    /// Fetch the vault's viewing key (UFVK) to verify it independently; refused unless it is
    /// the key the signers signed when the vault opened.
    ViewingKey { vault: B256 },
    /// Tier 2: prove the deposit on this machine and activate the vault with the proof, without
    /// any signer attestation. The backend relays the proof with its own gas (Y10); the Registry
    /// trusts only the proof.
    Prove {
        vault: B256,
        /// The zkVM host, `tier2/target/release/zecured-tier2-host`.
        #[arg(long, default_value = "tier2/target/release/zecured-tier2-host")]
        host: PathBuf,
        /// MOCK seal, for a Registry deployed with MockRiscZeroVerifier (local fork only).
        #[arg(long)]
        mock: bool,
        #[arg(long, default_value = "data/tier2/headers_nu7.json")]
        headers: PathBuf,
        #[arg(long, default_value = "data/tier2/summaries.json")]
        summaries: PathBuf,
        /// The deposit transaction (default: the one `loan deposit` sent).
        #[arg(long)]
        txid: Option<String>,
        #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
        rpc: String,
        /// TEST ONLY: submit and finalize with the owner EOA, which pays the Base gas, instead of
        /// handing the proof to the backend.
        #[arg(long)]
        self_submit: bool,
    },
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
    /// TEST ONLY: withdraw part of the collateral (D14) with the owner EOA as the account; the
    /// vault stays active. The destination is pre-validated by the signers first.
    PartialRelease {
        vault: B256,
        /// ZEC to withdraw, e.g. 0.004 (at least 0.001, leaving at least 0.001 in the vault).
        #[arg(long)]
        amount: f64,
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
    /// The vault's address, signed by every signer (C-4).
    address: String,
    registry: Address,
    chain_id: u64,
    next_nonce: u64,
    /// `keccak256` of the vault's viewing key as every signer signed it when the vault opened
    /// (C-4); `None` for loans opened before the signers signed vaults.
    #[serde(default)]
    ufvk_hash: Option<B256>,
    /// The vault's expiry height and the five signers' `VaultOpened` signatures (by node
    /// index), kept for a Tier 2 deposit proof (`loan prove`).
    #[serde(default)]
    expiry_height: Option<u32>,
    #[serde(default)]
    opened_signatures: Vec<(u16, Bytes)>,
    /// The deposit transaction sent by `loan deposit`.
    #[serde(default)]
    deposit_txid: Option<String>,
    /// Every destination given for this loan's claims, with its salt: what checks each committed
    /// payment and proves a misdirected one (F3, `watch --claims-of`).
    #[serde(default)]
    destinations: Vec<KeptDestination>,
}

/// A destination of the loan's claims: `commitment = keccak256(salt || receiver)`.
#[derive(Serialize, Deserialize, Clone)]
struct KeptDestination {
    commitment: B256,
    /// Raw Ironwood receiver (hex, 43 bytes).
    receiver: String,
    /// Hex, 32 bytes.
    salt: String,
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

/// Writes `v` as JSON to `path`, replacing it atomically. Wallet and loan files hold spending
/// and owner keys, so the file is readable by its owner only (0600) and missing directories
/// are created 0700 (L-3).
fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = create_private_file(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(v)?)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(d: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(d)
        .with_context(|| format!("create {}", d.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(d: &Path) -> Result<()> {
    Ok(fs::create_dir_all(d)?)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    // `mode` only applies to a new file; a leftover one keeps its permissions.
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<fs::File> {
    Ok(fs::File::create(path)?)
}

/// Restricts a key file written by an earlier version (with the default umask) to its owner.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        eprintln!("note: restricted {} to its owner (0600)", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_: &Path) -> Result<()> {
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)?)
}

/// Reads a wallet or loan file (they hold keys), restricting its permissions first.
pub(crate) fn read_secret_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    restrict(path).with_context(|| format!("read {}", path.display()))?;
    read_json(path)
}

fn load_wallet(path: &Path) -> Result<Wallet> {
    let f: WalletFile = read_secret_json(path)?;
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

/// One signer node of the network config: its index and attestation address.
#[derive(Deserialize)]
struct SignerParams {
    index: u16,
    eth: Address,
}

/// Network parameters the backend's signers expect for owner signatures, and the signer set
/// whose signatures a new vault needs (C-4). Read from the local network config, never from
/// the backend.
#[derive(Deserialize)]
struct NetworkParams {
    registry: Address,
    chain_id: u64,
    #[serde(default)]
    nodes: Vec<SignerParams>,
}

fn network_params() -> Result<NetworkParams> {
    let cfg: serde_json::Value = read_json(Path::new(
        &std::env::var("ZECURED_NETWORK_CONFIG").unwrap_or_else(|_| "signer/data/network.json".into()),
    ))?;
    Ok(serde_json::from_value(cfg)?)
}

/// A vault as `POST /vaults` returns it.
struct OpenedVault {
    vault_id: B256,
    address: String,
    amount: u64,
    expiry_height: u32,
    ufvk_hash: B256,
    /// `VaultOpened` signatures by node index.
    signatures: Vec<(u16, Bytes)>,
}

fn parse_opened(r: &serde_json::Value) -> Result<OpenedVault> {
    let field = |name: &str| r.get(name).with_context(|| format!("the backend's answer has no {name}"));
    let text = |name: &str| field(name)?.as_str().with_context(|| format!("malformed {name}"));
    let signatures = field("signatures")?
        .as_array()
        .context("malformed signatures")?
        .iter()
        .map(|s| {
            let index = s.get("index").and_then(|i| i.as_u64()).context("malformed signature index")?;
            let signature = s.get("signature").and_then(|x| x.as_str()).context("malformed signature")?;
            Ok((u16::try_from(index)?, signature.parse::<Bytes>()?))
        })
        .collect::<Result<_>>()?;
    Ok(OpenedVault {
        vault_id: text("vaultId")?.parse()?,
        address: text("address")?.to_owned(),
        amount: text("amount")?.parse()?,
        expiry_height: u32::try_from(field("expiryHeight")?.as_u64().context("malformed expiryHeight")?)?,
        ufvk_hash: text("ufvkHash")?.parse()?,
        signatures,
    })
}

/// C-4: a vault's address, and the hash of its viewing key, count only if every signer of
/// the local network config signed `VaultOpened` for them and for this owner's own request.
fn check_vault_opened(
    net: &NetworkParams,
    owner: Address,
    account: Address,
    amount: u64,
    v: &OpenedVault,
) -> Result<()> {
    ensure!(v.amount == amount, "the backend opened a vault for another amount");
    ensure!(
        net.nodes.len() == SIGNERS,
        "the network config must list the {SIGNERS} signer nodes (index and eth address) to check the vault"
    );
    let signers: Vec<(u16, Address)> = net.nodes.iter().map(|n| (n.index, n.eth)).collect();
    let message = VaultOpened {
        vaultId: v.vault_id,
        owner,
        account,
        amount,
        expiryHeight: v.expiry_height,
        addressHash: text_hash(&v.address),
        ufvkHash: v.ufvk_hash,
    };
    check_signed_by_all(&registry_domain(net.chain_id, net.registry), &message, &signers, &v.signatures)
}

/// C-4: the viewing key must hash to the `ufvkHash` all five signers signed when the vault
/// opened. Ok(false): a loan opened before that, with nothing to check against.
fn check_viewing_key(ufvk: &str, expected: Option<B256>) -> Result<bool> {
    let Some(expected) = expected else { return Ok(false) };
    ensure!(
        text_hash(ufvk) == expected,
        "the backend returned a viewing key that the signers did not sign for this vault: not printing it"
    );
    Ok(true)
}

/// The vault's viewing key from the backend (owner-signed `ViewKeyRequest`), refused unless it
/// is the key every signer signed when the vault opened; `false` for loans opened before that.
async fn viewing_key(wallet: &Path, backend: &str, vault: &B256) -> Result<(String, bool)> {
    let (mut l, owner, _) = owner_ctx(wallet, vault, Some(String::new()))?;
    let nonce = l.next_nonce;
    let deadline = now() + REQUEST_TTL_SECS;
    let msg = ViewKeyRequest {
        vaultId: *vault,
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
    write_json(&loan_path(wallet, vault), &l)?;
    let ufvk = r["ufvk"].as_str().context("the backend returned no viewing key")?.to_owned();
    let checked = check_viewing_key(&ufvk, l.ufvk_hash)?;
    Ok((ufvk, checked))
}

/// `zcash:` payment URI for `amount` zatoshis (ZIP 321), built here from the checked address.
fn zip321_uri(address: &str, amount: u64) -> String {
    let zats = 100_000_000;
    let (whole, frac) = (amount / zats, amount % zats);
    let amount = if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", format!("{frac:08}").trim_end_matches('0'))
    };
    format!("zcash:{address}?amount={amount}")
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
    let unsigned = spend::build(
        &NETWORK,
        BlockHeight::from_u32(state.scanned_to + 1),
        &w.fvk,
        state.anchor(),
        &inputs,
        &payments,
    )?;
    let pk = tokio::task::spawn_blocking(spend::proving_key).await?;
    let pczt = spend::finalize_and_prove(unsigned.pczt, &pk)?;
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    let ask = SpendAuthorizingKey::from(&w.sk);
    for i in &unsigned.spend_action_indices {
        signer.sign_ironwood(rand_010::rng(), *i, &ask).map_err(|e| anyhow!("sign: {e:?}"))?;
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
                println!("  {:>12} zat  h={} {:?}  tx {}", n.value(), n.position.height, n.scope, n.txid);
            }
            println!("balance {} zat (scanned to {})", s.balance(), s.scanned_to);
        }
        Cmd::Send { to, amount } => {
            let w = load_wallet(&cli.wallet)?;
            println!("broadcast {}", send(&w, &cli.lwd, &to, zats(amount)?).await?);
        }
        Cmd::Watch {
            ufvk_file,
            from,
            tier3,
            claims_of,
            rpc,
        } => {
            let ufvk = fs::read_to_string(&ufvk_file)
                .with_context(|| format!("read {}", ufvk_file.display()))?
                .trim()
                .to_owned();
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
            if tier3 {
                let net = network_params()?;
                let findings = watch::check_lineage(&ufvk, &state.notes, net.registry, &rpc).await?;
                watch::report(&findings);
                if let Some(vault) = claims_of {
                    let l: LoanFile = read_secret_json(&loan_path(&cli.wallet, &vault))?;
                    let w = load_wallet(&cli.wallet)?;
                    let own = l
                        .destinations
                        .iter()
                        .map(|d| watch::own_recipient(&d.salt, &d.receiver, &w.fvk))
                        .collect::<Result<Vec<_>>>()?;
                    let payouts = watch::committed_payouts(&findings);
                    let claims = watch::check_claims(&mut client, &own, &payouts, net.registry, &rpc).await?;
                    for f in &claims {
                        if let watch::ClaimFinding::Misdirected { evidence, .. } = f {
                            let path = loan_path(&cli.wallet, &vault).with_file_name(format!(
                                "f3-{}-{}.json",
                                hex::encode(&vault[..8]),
                                evidence.txid
                            ));
                            write_json(&path, evidence)?;
                            watch::report_claim(f, Some(&path));
                        } else {
                            watch::report_claim(f, None);
                        }
                    }
                    if claims.is_empty() {
                        println!("tier 3: no committed payment of this loan's claims yet");
                    }
                }
            }
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
            // C-4: nothing is kept or shown unless every signer vouches for the vault.
            let v = parse_opened(&r).context("refusing the vault: do NOT pay any address the backend gave for it")?;
            check_vault_opened(&net, owner.address(), account, amount, &v)
                .context("refusing the vault: do NOT pay any address the backend gave for it")?;
            write_json(
                &loan_path(wallet, &v.vault_id),
                &LoanFile {
                    vault_id: v.vault_id,
                    owner_key: hex::encode(owner.to_bytes()),
                    account,
                    amount,
                    address: v.address.clone(),
                    registry: net.registry,
                    chain_id: net.chain_id,
                    next_nonce: 1,
                    ufvk_hash: Some(v.ufvk_hash),
                    expiry_height: Some(v.expiry_height),
                    opened_signatures: v.signatures.clone(),
                    deposit_txid: None,
                    destinations: vec![],
                },
            )?;
            eprintln!("vault address signed by all {SIGNERS} signers of the network config (checked here)");
            let shown = json!({ "vaultId": v.vault_id, "address": v.address, "zip321": zip321_uri(&v.address, amount),
                                "amount": amount.to_string(), "expiryHeight": v.expiry_height });
            println!("{}", serde_json::to_string_pretty(&shown)?);
        }
        LoanCmd::Status { vault } => {
            let v: serde_json::Value =
                reqwest::get(format!("{}/vaults/{vault}", backend.trim_end_matches('/'))).await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        LoanCmd::Deposit { vault, amount } => {
            let mut l: LoanFile = read_secret_json(&loan_path(wallet, &vault))?;
            let w = load_wallet(wallet)?;
            let amount = match amount {
                Some(a) => zats(a)?,
                None => l.amount,
            };
            let txid = send(&w, lwd_url, &l.address, amount).await?;
            l.deposit_txid = Some(txid.clone());
            write_json(&loan_path(wallet, &vault), &l)?;
            println!("broadcast {txid}");
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
            let (ufvk, checked) = viewing_key(wallet, backend, &vault).await?;
            if !checked {
                eprintln!(
                    "note: this loan was opened before the signers signed vault keys, so this key could not be \
                     checked against them"
                );
            }
            println!("{ufvk}");
        }
        LoanCmd::Prove {
            vault,
            host,
            mock,
            headers,
            summaries,
            txid,
            rpc,
            self_submit,
        } => {
            let (ufvk, checked) = viewing_key(wallet, backend, &vault).await?;
            ensure!(checked, "a deposit proof needs the viewing key the signers signed (C-4)");
            let l: LoanFile = read_secret_json(&loan_path(wallet, &vault))?;
            let txid = txid.or(l.deposit_txid.clone()).context("no deposit txid: pass --txid")?;
            let net = network_params()?;
            let paths = prove::Paths {
                host,
                headers,
                summaries,
                work: wallet.with_file_name(format!("proof-{}", hex::encode(&vault[..8]))),
            };
            let seal = prove::make_seal(&l, &ufvk, &net, &paths, mock, &txid, lwd_url, &rpc).await?;
            if self_submit {
                prove::submit_with_owner(&l, &net, seal, mock, &rpc).await?;
            } else {
                post(backend, &format!("/vaults/{vault}/deposit-proof"), serde_json::to_value(&seal)?).await?;
                println!(
                    "deposit proof{} handed to the backend; the vault activates once the relay has moved on (K3)",
                    if mock { " (MOCK seal)" } else { "" }
                );
            }
        }
        LoanCmd::Borrow { vault, usdc, rpc } => base_test::borrow(wallet, &vault, usdc, &rpc).await?,
        LoanCmd::RepayRelease { vault, to, rpc } => {
            let commitment = destination(wallet, backend, &vault, to).await?;
            base_test::repay_release(wallet, &vault, commitment, &rpc).await?;
        }
        LoanCmd::PartialRelease { vault, amount, to, rpc } => {
            let amount = zats(amount)?;
            let commitment = destination(wallet, backend, &vault, to).await?;
            base_test::partial_release(wallet, &vault, amount, commitment, &rpc).await?;
        }
        LoanCmd::Close {
            vault,
            to,
            slippage_bps,
        } => {
            let (l, owner, to) = owner_ctx(wallet, &vault, to)?;
            let quote: serde_json::Value =
                reqwest::get(format!("{}/quotes/close/{vault}", backend.trim_end_matches('/')))
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
    l.destinations.push(KeptDestination {
        commitment: c,
        receiver: hex::encode(receiver),
        salt: hex::encode(salt),
    });
    write_json(&loan_path(wallet, vault), &l)?;
    Ok(c)
}

/// Loan file, owner key and destination (defaults to the wallet's own address).
fn owner_ctx(wallet: &Path, vault: &B256, to: Option<String>) -> Result<(LoanFile, PrivateKeySigner, String)> {
    let mut l: LoanFile = read_secret_json(&loan_path(wallet, vault))?;
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

/// C-4: what `loan open` and `loan viewing-key` accept from the backend.
#[cfg(test)]
mod vault_checks {
    use zecured_base::vault_opened;

    use super::*;

    const ADDRESS: &str = "utest1vault";
    const UFVK: &str = "uviewtest1vaultkey";

    struct Fixture {
        keys: Vec<PrivateKeySigner>,
        net: NetworkParams,
        owner: Address,
        account: Address,
    }

    fn fixture() -> Fixture {
        let keys: Vec<PrivateKeySigner> = (0..SIGNERS).map(|_| PrivateKeySigner::random()).collect();
        let nodes = keys
            .iter()
            .zip(1u16..)
            .map(|(k, index)| SignerParams {
                index,
                eth: k.address(),
            })
            .collect();
        Fixture {
            net: NetworkParams {
                registry: Address::repeat_byte(0x11),
                chain_id: 84_532,
                nodes,
            },
            keys,
            owner: Address::repeat_byte(0xaa),
            account: Address::repeat_byte(0xee),
        }
    }

    /// The backend's `POST /vaults` answer, signed by `signers` for `address`.
    fn answer(f: &Fixture, signers: &[PrivateKeySigner], address: &str) -> serde_json::Value {
        let (vault_id, expiry) = (B256::repeat_byte(7), 4_486_288u32);
        let msg = vault_opened(vault_id, f.owner, f.account, 1_000_000, expiry, address, UFVK);
        let domain = registry_domain(f.net.chain_id, f.net.registry);
        let signatures: Vec<serde_json::Value> = signers
            .iter()
            .zip(1u16..)
            .map(|(k, index)| json!({ "index": index, "signature": sign_typed(k, &domain, &msg).unwrap() }))
            .collect();
        json!({ "vaultId": vault_id, "address": address, "zip321": "zcash:x", "amount": "1000000",
                "expiryHeight": expiry, "ufvkHash": text_hash(UFVK), "signatures": signatures })
    }

    fn check(f: &Fixture, r: &serde_json::Value) -> Result<()> {
        check_vault_opened(&f.net, f.owner, f.account, 1_000_000, &parse_opened(r)?)
    }

    #[test]
    fn a_vault_counts_only_with_every_signers_signature() {
        let f = fixture();
        let honest = answer(&f, &f.keys, ADDRESS);
        check(&f, &honest).unwrap();
        let v = parse_opened(&honest).unwrap();
        assert_eq!((v.address.as_str(), v.amount, v.expiry_height), (ADDRESS, 1_000_000, 4_486_288));
        assert_eq!(v.ufvk_hash, text_hash(UFVK));

        // The backend's own address, signed by nobody or by keys outside the network config.
        let mut swapped = honest.clone();
        swapped["address"] = json!("utest1attacker");
        assert!(check(&f, &swapped).is_err());
        let strangers: Vec<PrivateKeySigner> = (0..SIGNERS).map(|_| PrivateKeySigner::random()).collect();
        assert!(check(&f, &answer(&f, &strangers, "utest1attacker")).is_err());
        // Four signatures, another viewing key, another amount, or no signatures at all.
        let mut four = honest.clone();
        four["signatures"].as_array_mut().unwrap().pop();
        assert!(check(&f, &four).is_err());
        let mut ufvk = honest.clone();
        ufvk["ufvkHash"] = json!(text_hash("uviewtest1attacker"));
        assert!(check(&f, &ufvk).is_err());
        let v = parse_opened(&honest).unwrap();
        assert!(check_vault_opened(&f.net, f.owner, f.account, 5_000_000, &v).is_err());
        let mut unsigned = honest.clone();
        unsigned.as_object_mut().unwrap().remove("signatures");
        assert!(check(&f, &unsigned).is_err());
        // Signatures for another owner's request.
        assert!(check_vault_opened(&f.net, Address::repeat_byte(0xbb), f.account, 1_000_000, &v).is_err());
        // A network config without the signer set cannot check anything.
        let bare = NetworkParams {
            registry: f.net.registry,
            chain_id: f.net.chain_id,
            nodes: vec![],
        };
        assert!(check_vault_opened(&bare, f.owner, f.account, 1_000_000, &v).is_err());
    }

    #[test]
    fn the_viewing_key_must_be_the_one_the_signers_signed() {
        assert!(check_viewing_key(UFVK, Some(text_hash(UFVK))).unwrap());
        assert!(check_viewing_key("uviewtest1attacker", Some(text_hash(UFVK))).is_err());
        // A loan opened before vaults were signed: nothing to check against.
        assert!(!check_viewing_key(UFVK, None).unwrap());
    }

    /// Loan files written before C-4 still load.
    #[test]
    fn old_loan_files_have_no_viewing_key_hash() {
        let old = json!({ "vault_id": B256::repeat_byte(7), "owner_key": "11", "account": Address::ZERO,
                          "amount": 1_000_000, "address": ADDRESS, "registry": Address::ZERO, "chain_id": 84_532,
                          "next_nonce": 3 });
        let l: LoanFile = serde_json::from_value(old).unwrap();
        assert!(l.ufvk_hash.is_none());
    }

    #[test]
    fn zip321_uris_are_built_here() {
        assert_eq!(zip321_uri("utest1x", 1_000_000), "zcash:utest1x?amount=0.01");
        assert_eq!(zip321_uri("utest1x", 100_000_000), "zcash:utest1x?amount=1");
        assert_eq!(zip321_uri("utest1x", 123_456_789), "zcash:utest1x?amount=1.23456789");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        TempDir(std::env::temp_dir().join(format!("zecured-cli-test-{}", hex::encode(fresh_salt()))))
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// L-3: wallet and loan files are 0600 and the directories created for them 0700.
    #[test]
    fn key_files_are_private() {
        let dir = temp_dir();
        let wallet = dir.0.join("data").join("wallet").join("wallet.json");
        write_json(&wallet, &json!({ "spending_key": "00", "birthday": 1 })).unwrap();
        assert_eq!(mode(&wallet), 0o600);
        assert_eq!(mode(wallet.parent().unwrap()), 0o700);
        assert_eq!(mode(&dir.0.join("data")), 0o700);

        // A file left world-readable by an earlier version is tightened when rewritten...
        let loan = wallet.with_file_name("loan-00.json");
        fs::write(&loan, b"{}").unwrap();
        fs::set_permissions(&loan, fs::Permissions::from_mode(0o644)).unwrap();
        write_json(&loan, &json!({ "owner_key": "11" })).unwrap();
        assert_eq!(mode(&loan), 0o600);
        // ...or when read.
        fs::set_permissions(&loan, fs::Permissions::from_mode(0o644)).unwrap();
        let v: serde_json::Value = read_secret_json(&loan).unwrap();
        assert_eq!(v["owner_key"], "11");
        assert_eq!(mode(&loan), 0o600);
    }
}
