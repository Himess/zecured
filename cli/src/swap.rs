//! Zecured Swap from the CLI wallet (docs/SWAP.md, Sell: the off-ramp): sell shielded ZEC for USDC
//! with no custodian. The maker locks USDC in ZecuredSwap; this wallet pays one Ironwood note
//! with the memo "ZSWP" || orderId and `ovk = None` to the maker's receiver, proves the payment in
//! zero knowledge and finalizes the order, which releases the USDC to the taker's Base address.
//!
//!   zecured-wallet swap sell --maker <url> --zec 0.5 [--min-usdc 19.5]
//!   zecured-wallet swap resume --order <orderId>
//!   zecured-wallet swap status --order <orderId>
//!
//! The wallet deals only with the ZecuredSwap of its pinned deployment record
//! (`--swap-deployment`), whatever contract or chain a maker names (W-9).
//!
//! TEST ONLY: the taker is an EOA (`SWAP_TAKER_PRIVATE_KEY`) that pays its own Base gas; the app
//! takes Buy orders from the user's own wallet. Each order's file (0600, next to the wallet) keeps
//! the quote, the maker's receiver and salt and, once paid, the payment note's opening and txid:
//! what `resume` needs to prove the payment, and nothing that ever leaves this machine.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use alloy::{
    eips::BlockNumberOrTag,
    network::EthereumWallet,
    primitives::{Address, B256, Bytes, U256},
    providers::{DynProvider, Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use zcash_protocol::{TxId, consensus::BlockHeight};
use zecured_base::gas_with_headroom;
use zecured_deposit_witness::{p2p, payment};
use zecured_swap::{
    OrderStatus, SIDE_SELL,
    abi::{IZecuredSwap, SwapQuote},
    chain::{finalize_settlement, payment_anchor, prove_settlement, relay_tip, settlement_ready, swap_version_active},
    pay::{PaymentOpening, authorize, build_payment, check_payment_note, select_inputs},
    quote::{check_quote_signature, swap_domain},
    seal::Prover,
};
use zecured_zcash_ops::{keys::ironwood_receiver, lwd, spend};

use crate::{load_wallet, lwd_connect, read_json, read_secret_json, scan, write_json, zcash_network, zcash_params};

#[derive(Subcommand)]
pub enum SwapCmd {
    /// Sell ZEC for USDC: take a maker's Sell quote, pay it from this wallet, prove it, settle.
    Sell {
        /// The maker's API, e.g. http://127.0.0.1:8095.
        #[arg(long, env = "SWAP_MAKER_URL")]
        maker: String,
        /// ZEC to sell.
        #[arg(long)]
        zec: f64,
        /// Refuse a quote that pays less USDC than this.
        #[arg(long)]
        min_usdc: Option<f64>,
        #[command(flatten)]
        opts: SwapOpts,
    },
    /// Continue an order from where it stopped (paid, proven, ...).
    Resume {
        #[arg(long)]
        order: B256,
        #[command(flatten)]
        opts: SwapOpts,
    },
    /// Show an order's file and its status on Base.
    Status {
        #[arg(long)]
        order: B256,
        #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
        rpc: String,
        #[command(flatten)]
        pin: PinOpts,
    },
}

#[derive(Args, Clone)]
pub struct SwapOpts {
    #[arg(long, env = "BASE_SEPOLIA_RPC_URL", default_value = "https://sepolia.base.org")]
    rpc: String,
    /// The taker's Base key: opens the order, submits the proof, receives the USDC.
    #[arg(long, env = "SWAP_TAKER_PRIVATE_KEY", hide_env_values = true)]
    taker_key: String,
    /// An invite code, when the maker is invite-only (sent as a header, never logged).
    #[arg(long, env = "SWAP_INVITE_CODE", hide_env_values = true)]
    invite: Option<String>,
    /// The zkVM host (`tier2/target/release/zecured-tier2-host`), or `mock-host` with `--mock`.
    #[arg(long, default_value = "tier2/target/release/zecured-tier2-host")]
    host: PathBuf,
    /// MOCK seal, for a ZecuredSwap behind MockRiscZeroVerifier (local fork only).
    #[arg(long)]
    mock: bool,
    #[arg(long, default_value = "data/tier2/headers_nu7.json")]
    headers: PathBuf,
    #[arg(long, default_value = "data/tier2/summaries.json")]
    summaries: PathBuf,
    /// Refuse a quote whose pay window is shorter (seconds): this wallet must pay, wait for the
    /// order's tier of work on the relay and prove before the deadline.
    #[arg(long, default_value_t = 7_200)]
    min_window: u32,
    #[command(flatten)]
    pin: PinOpts,
}

/// W-9: which ZecuredSwap this wallet deals with.
#[derive(Args, Clone)]
pub struct PinOpts {
    /// The deployment record of the ZecuredSwap this wallet deals with, as script/DeploySwap.s.sol
    /// writes it (`chainId`, `ZecuredSwap`, `usdc`, `relay`, `versions`): a quote naming another
    /// contract or chain is refused, and so is a contract whose USDC, relay or version registry is
    /// not the record's.
    #[arg(
        long,
        env = "ZECURED_SWAP_DEPLOYMENT",
        default_value = "contracts/deployments/base-sepolia.swap.json"
    )]
    swap_deployment: PathBuf,
}

/// One order's file.
#[derive(Serialize, Deserialize)]
struct SwapFile {
    order_id: B256,
    chain_id: u64,
    swap: Address,
    quote: QuoteFields,
    signature: Bytes,
    /// The maker's raw receiver (hex) and the salt of its commitment.
    maker_receiver: String,
    maker_salt: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opening: Option<PaymentOpening>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    txid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_tx: Option<String>,
}

/// The quote as the maker's API returns it (integers as decimal strings).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteFields {
    maker: Address,
    taker: Address,
    side: u8,
    zatoshi: String,
    usdc: String,
    maker_receiver_commitment: B256,
    pay_window: u32,
    quote_deadline: String,
    nonce: String,
}

impl QuoteFields {
    fn to_quote(&self) -> Result<SwapQuote> {
        Ok(SwapQuote {
            maker: self.maker,
            taker: self.taker,
            side: self.side,
            zatoshi: self.zatoshi.parse()?,
            usdc: self.usdc.parse()?,
            makerReceiverCommitment: self.maker_receiver_commitment,
            payWindow: self.pay_window,
            quoteDeadline: self.quote_deadline.parse()?,
            nonce: self.nonce.parse()?,
        })
    }
}

/// W-9: the pinned ZecuredSwap deployment.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SwapPin {
    chain_id: u64,
    swap: Address,
    usdc: Address,
    relay: Address,
    versions: Address,
}

impl SwapPin {
    fn load(path: &Path) -> Result<Self> {
        let raw: serde_json::Value =
            read_json(path).context("the pinned ZecuredSwap deployment record (--swap-deployment)")?;
        Self::from_record(&raw).with_context(|| format!("the ZecuredSwap deployment record {}", path.display()))
    }

    /// From script/DeploySwap.s.sol's record: `chainId`, `ZecuredSwap` (and `contracts.ZecuredSwap`),
    /// `usdc`, `relay`, `versions`.
    fn from_record(v: &serde_json::Value) -> Result<Self> {
        let address = |value: &serde_json::Value, key: &str| -> Result<Address> {
            let a: Address =
                value.as_str().with_context(|| format!("no {key}"))?.parse().with_context(|| key.to_owned())?;
            ensure!(a != Address::ZERO, "{key} is zero");
            Ok(a)
        };
        let (top, nested) = (&v["ZecuredSwap"], &v["contracts"]["ZecuredSwap"]);
        let swap = if nested.is_string() {
            address(nested, "contracts.ZecuredSwap")?
        } else {
            address(top, "ZecuredSwap")?
        };
        if top.is_string() {
            ensure!(address(top, "ZecuredSwap")? == swap, "two ZecuredSwap addresses");
        }
        Ok(Self {
            chain_id: v["chainId"].as_u64().context("no chainId")?,
            swap,
            usdc: address(&v["usdc"], "usdc")?,
            relay: address(&v["relay"], "relay")?,
            versions: address(&v["versions"], "versions")?,
        })
    }

    /// A maker names the contract and the chain of its quote: they must be the pinned ones.
    fn check_quote(&self, swap: Address, chain_id: u64) -> Result<()> {
        ensure!(
            chain_id == self.chain_id,
            "the maker quotes for chain {chain_id}, not the pinned deployment's chain {}",
            self.chain_id
        );
        ensure!(swap == self.swap, "the maker's quote names {swap}, not the pinned ZecuredSwap {}", self.swap);
        Ok(())
    }

    /// An order file names the pinned contract and chain (a file written before the pin holds the
    /// maker's word for them).
    fn check_file(&self, f: &SwapFile) -> Result<()> {
        self.check_quote(f.swap, f.chain_id).context("this order's file is not of the pinned ZecuredSwap")
    }

    /// The RPC serves the pinned chain, and the pinned contract there is the deployment's: its
    /// USDC, relay and version registry are the record's.
    async fn check_contract<P: Provider>(&self, provider: &P) -> Result<()> {
        let chain_id = provider.get_chain_id().await.context("the RPC's chain id")?;
        ensure!(
            chain_id == self.chain_id,
            "the RPC serves chain {chain_id}, not the pinned deployment's chain {}",
            self.chain_id
        );
        let s = IZecuredSwap::new(self.swap, provider);
        let usdc = s.usdc().call().await.context("the pinned ZecuredSwap's usdc()")?;
        ensure!(usdc == self.usdc, "the pinned ZecuredSwap escrows {usdc}, not the deployment's USDC {}", self.usdc);
        let relay = s.relay().call().await.context("the pinned ZecuredSwap's relay()")?;
        ensure!(
            relay == self.relay,
            "the pinned ZecuredSwap follows relay {relay}, not the deployment's {}",
            self.relay
        );
        let versions = s.versions().call().await.context("the pinned ZecuredSwap's versions()")?;
        ensure!(
            versions == self.versions,
            "the pinned ZecuredSwap's version registry is {versions}, not the deployment's {}",
            self.versions
        );
        Ok(())
    }
}

fn swap_path(wallet: &Path, order: &B256) -> PathBuf {
    wallet.with_file_name(format!("swap-{}.json", hex::encode(&order[..8])))
}

fn usdc_units(usdc: f64) -> Result<U256> {
    ensure!(usdc > 0.0, "amount must be positive");
    Ok(U256::from((usdc * 1e6).round() as u64))
}

pub async fn run(wallet: &Path, lwd_url: &str, cmd: SwapCmd) -> Result<()> {
    match cmd {
        SwapCmd::Sell {
            maker,
            zec,
            min_usdc,
            opts,
        } => {
            let zatoshi = crate::zats(zec)?;
            let f = take_sell_quote(wallet, &maker, zatoshi, min_usdc.map(usdc_units).transpose()?, &opts).await?;
            advance(wallet, lwd_url, f, &opts).await
        }
        SwapCmd::Resume { order, opts } => {
            let f: SwapFile = read_secret_json(&swap_path(wallet, &order))?;
            advance(wallet, lwd_url, f, &opts).await
        }
        SwapCmd::Status { order, rpc, pin } => {
            let f: SwapFile = read_secret_json(&swap_path(wallet, &order))?;
            let pin = SwapPin::load(&pin.swap_deployment)?;
            pin.check_file(&f)?;
            let provider = ProviderBuilder::new().connect_http(rpc.parse()?);
            let o = IZecuredSwap::new(pin.swap, &provider).order(order).call().await?;
            println!(
                "order {order}: {} here, {:?} on Base, {} zatoshi for {} USDC units, pay deadline {}",
                f.status,
                OrderStatus::from_u8(o.status)?,
                o.zatoshi,
                o.usdc,
                o.payDeadline
            );
            Ok(())
        }
    }
}

fn provider(opts: &SwapOpts) -> Result<(DynProvider, Address)> {
    let key: PrivateKeySigner = opts.taker_key.parse().context("SWAP_TAKER_PRIVATE_KEY")?;
    let me = key.address();
    let p = ProviderBuilder::new().wallet(EthereumWallet::from(key)).connect_http(opts.rpc.parse()?).erased();
    Ok((p, me))
}

/// Asks the maker for a Sell quote bound to this taker, checks every field of it, and keeps it.
async fn take_sell_quote(
    wallet: &Path,
    maker_url: &str,
    zatoshi: u64,
    min_usdc: Option<U256>,
    opts: &SwapOpts,
) -> Result<SwapFile> {
    let (provider, me) = provider(opts)?;
    // W-9: the contract is this wallet's to pin, never the maker's to name.
    let pin = SwapPin::load(&opts.pin.swap_deployment)?;
    pin.check_contract(&provider).await?;
    // Digits and a hex address: nothing to escape.
    let url = format!("{}/swap/quote?side=sell&zatoshi={zatoshi}&taker={me}", maker_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(url);
    if let Some(code) = &opts.invite {
        req = req.header("x-zecured-invite", code);
    }
    let resp = req.send().await.context("the maker is unreachable")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        bail!("the maker refused: {}", v.get("error").and_then(|e| e.as_str()).unwrap_or("no quote"));
    }
    let fields: QuoteFields = serde_json::from_value(v["quote"].clone()).context("the maker's quote")?;
    let q = fields.to_quote()?;
    let signature: Bytes = v["signature"].as_str().context("signature")?.parse()?;
    let swap: Address = v["swap"].as_str().context("swap")?.parse()?;
    let chain_id = v["chainId"].as_u64().context("chainId")?;
    pin.check_quote(swap, chain_id)?;
    let maker_ua = v["makerReceiver"].as_str().context("makerReceiver")?;
    let maker_salt: [u8; 32] = hex::decode(v["makerSalt"].as_str().context("makerSalt")?.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("makerSalt: 32 bytes"))?;

    // Everything the order will be held to, checked here first, against the pinned contract.
    ensure!(q.side == SIDE_SELL && q.taker == me && q.zatoshi == zatoshi, "the quote is not the one asked for");
    check_quote_signature(&swap_domain(pin.chain_id, pin.swap), &q, &signature)?;
    let receiver = ironwood_receiver(&zcash_params(), maker_ua)?.to_raw_address_bytes();
    ensure!(
        zecured_swap::receiver_commitment(&maker_salt, &receiver) == q.makerReceiverCommitment,
        "the maker's receiver does not open the quote's commitment"
    );
    ensure!(q.payWindow >= opts.min_window, "the pay window ({} s) is too short to pay and prove", q.payWindow);
    if let Some(min) = min_usdc {
        ensure!(q.usdc >= min, "the quote pays {} USDC units, less than the minimum asked", q.usdc);
    }
    let order_id = zecured_swap::order_id(pin.chain_id, pin.swap, q.maker, q.nonce);
    let f = SwapFile {
        order_id,
        chain_id: pin.chain_id,
        swap: pin.swap,
        quote: fields,
        signature,
        maker_receiver: hex::encode(receiver),
        maker_salt: hex::encode(maker_salt),
        status: "quoted".into(),
        opening: None,
        txid: None,
        raw_tx: None,
    };
    write_json(&swap_path(wallet, &order_id), &f)?;
    println!("quote: {} zatoshi for {} USDC units (order {order_id})", q.zatoshi, q.usdc);
    Ok(f)
}

/// Moves the order on, one step after another, until it is settled.
async fn advance(wallet: &Path, lwd_url: &str, mut f: SwapFile, opts: &SwapOpts) -> Result<()> {
    let (provider, me) = provider(opts)?;
    // W-9: orders of the pinned contract only (from here on `f.swap` is `pin.swap`).
    let pin = SwapPin::load(&opts.pin.swap_deployment)?;
    pin.check_file(&f)?;
    pin.check_contract(&provider).await?;
    let swap = IZecuredSwap::new(pin.swap, &provider);
    let q = f.quote.to_quote()?;
    let path = swap_path(wallet, &f.order_id);
    loop {
        match f.status.as_str() {
            "quoted" => {
                ensure!(q.taker == me, "this order's quote is for another taker key");
                let call = swap.open(q.clone(), f.signature.clone(), B256::ZERO);
                let gas = gas_with_headroom(call.estimate_gas().await.context("estimate open")?);
                let r = call.gas(gas).send().await?.get_receipt().await?;
                ensure!(r.status(), "open reverted");
                println!("order opened on Base (tx {})", r.transaction_hash);
                f.status = "opened".into();
            }
            "opened" => {
                // Marks the order "paid" (with the opening) before the broadcast: a resume never
                // pays twice, it rebroadcasts.
                pay(wallet, lwd_url, &mut f, &q, &path).await?;
            }
            "paid" => {
                if prove(wallet, lwd_url, &f, &q, opts, &provider).await? {
                    f.status = "proven".into();
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            }
            "proven" => {
                let p = swap.pendingSettlement(f.order_id).call().await?;
                if p.anchor == B256::ZERO {
                    let o = swap.order(f.order_id).call().await?;
                    match OrderStatus::from_u8(o.status)? {
                        OrderStatus::Settled => f.status = "settled".into(),
                        OrderStatus::Refunded => bail!("the order was refunded to the maker: the proof came too late"),
                        _ => f.status = "paid".into(), // dropped: prove again
                    }
                } else if swap.settlementStale(f.order_id).call().await? {
                    let call = swap.dropSettlement(f.order_id);
                    let gas = gas_with_headroom(call.estimate_gas().await?);
                    call.gas(gas).send().await?.get_receipt().await?;
                    f.status = "paid".into();
                } else {
                    let relay = relay_tip(&provider, pin.swap).await?;
                    let now = provider
                        .get_block_by_number(BlockNumberOrTag::Latest)
                        .await?
                        .context("no latest Base block")?
                        .header
                        .timestamp;
                    if settlement_ready(&p, relay, now) {
                        let tx = finalize_settlement(&provider, pin.swap, f.order_id).await?;
                        println!("settled: {} USDC units to {me} (tx {tx})", q.usdc);
                        f.status = "settled".into();
                    } else {
                        eprintln!("relay at {relay} of {}; waiting (K3)", p.readyHeight);
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        continue;
                    }
                }
            }
            "settled" => {
                // The payment's opening is no longer needed.
                f.opening = None;
                f.raw_tx = None;
                write_json(&path, &f)?;
                println!("order {} settled", f.order_id);
                return Ok(());
            }
            other => bail!("unknown order state {other}"),
        }
        write_json(&path, &f)?;
    }
}

/// Pays the maker's receiver: one note of the order's zatoshi, the swap memo, `ovk = None`; the
/// opening is written to the order's file before the broadcast.
async fn pay(wallet: &Path, lwd_url: &str, f: &mut SwapFile, q: &SwapQuote, path: &Path) -> Result<()> {
    let w = load_wallet(wallet)?;
    let state = scan(&w, lwd_url).await?;
    let unspent = state.unspent();
    let inputs = select_inputs(&unspent, q.zatoshi)?;
    let receiver: [u8; 43] = hex::decode(&f.maker_receiver)?.try_into().map_err(|_| anyhow!("the maker's receiver"))?;
    let salt: [u8; 32] = hex::decode(&f.maker_salt)?.try_into().map_err(|_| anyhow!("the maker's salt"))?;
    let recipient = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&receiver))
        .context("the maker's receiver")?;
    let unsigned = build_payment(
        &zcash_params(),
        BlockHeight::from_u32(state.scanned_to + 1),
        &w.fvk,
        state.anchor(),
        &inputs,
        recipient,
        q.zatoshi,
        f.order_id,
        salt,
    )?;
    let opening = unsigned.opening.clone();
    let pk = tokio::task::spawn_blocking(spend::proving_key).await?;
    let tx = authorize(unsigned, &w.sk, &pk)?;
    check_payment_note(&tx, &opening, f.order_id)?;
    let raw = spend::serialize(&tx)?;
    f.opening = Some(opening);
    f.txid = Some(tx.txid().to_string());
    f.raw_tx = Some(hex::encode(&raw));
    f.status = "paid".into();
    write_json(path, f)?;
    let mut client = lwd_connect(lwd_url).await?;
    spend::broadcast_raw(&mut client, raw).await?;
    println!("paid {} zatoshi to the maker's receiver (memo bound to the order)", q.zatoshi);
    Ok(())
}

/// Proves a mined payment and submits the proof; false while it is unmined or its anchor is not on
/// the relay yet.
async fn prove(
    wallet: &Path,
    lwd_url: &str,
    f: &SwapFile,
    q: &SwapQuote,
    opts: &SwapOpts,
    provider: &DynProvider,
) -> Result<bool> {
    let mut client = lwd_connect(lwd_url).await?;
    let txid = TxId::from_hex(f.txid.as_deref().context("no txid")?).context("stored txid")?;
    // Unknown to the server (a payment just broadcast may not have reached its node yet, or the
    // node dropped it) or in its mempool: rebroadcast and wait.
    let height = lwd::raw_transaction_if_known(&mut client, &txid).await?.map_or(0, |(_, h)| h);
    if height == 0 {
        if let Some(raw) = &f.raw_tx {
            let _ = spend::broadcast_raw(&mut client, hex::decode(raw)?).await;
        }
        eprintln!("the payment is not mined yet; waiting");
        return Ok(false);
    }
    let height = u32::try_from(height)?;
    let Some(anchor) = payment_anchor(provider, f.swap, height, q.zatoshi).await? else {
        eprintln!("the relay has not the order's work on top of the payment yet; waiting");
        return Ok(false);
    };
    let (headers, network) = (opts.headers.clone(), zcash_network());
    let have =
        tokio::task::spawn_blocking(move || p2p::extend_file(network, &headers, &p2p::seed_peers(network))).await??;
    ensure!(have >= anchor, "the P2P peers are behind the relay");
    let opening = f.opening.clone().context("no opening")?;
    let built = payment::build(
        &mut client,
        network,
        &opts.headers,
        &opts.summaries,
        txid,
        &opening,
        Some(f.order_id.0),
        Some(q.makerReceiverCommitment.0),
        anchor,
    )
    .await?;
    let prover = Prover {
        host: opts.host.clone(),
        mock: opts.mock,
        work: wallet.with_file_name(format!("proof-swap-{}", hex::encode(&f.order_id[..8]))),
    };
    let witness = built.witness;
    let seal = tokio::task::spawn_blocking(move || prover.seal(&witness)).await??;
    ensure!(
        swap_version_active(provider, f.swap, seal.selector()?, seal.image_id).await?,
        "ZecuredSwap takes no proof of this guest and verifier (not an active Swap version)"
    );
    let tx = prove_settlement(provider, f.swap, f.order_id, &seal).await?;
    println!(
        "payment proven{} (tx {tx}); the settlement waits for the relay to move on (K3)",
        if opts.mock { " (MOCK seal)" } else { "" }
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::sol_types::SolCall;
    use orchard::keys::{FullViewingKey, Scope, SpendingKey};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use zcash_protocol::consensus::Network;
    use zecured_swap::quote::sign_quote;
    use zecured_zcash_ops::keys::encode_ironwood_ua;

    use super::*;

    const PINNED: Address = Address::repeat_byte(0x11);

    /// The pinned contract's USDC, relay and version registry.
    fn deployment() -> [Address; 3] {
        [
            Address::repeat_byte(0x55),
            Address::repeat_byte(0x56),
            Address::repeat_byte(0x57),
        ]
    }

    /// script/DeploySwap.s.sol's record of a deployment of `swap` on Base Sepolia.
    fn record(swap: Address) -> serde_json::Value {
        let [usdc, relay, versions] = deployment();
        json!({
            "network": "base-sepolia",
            "chainId": 84_532,
            "zcashNetwork": "zcash-testnet",
            "ZecuredSwap": swap,
            "block": 1,
            "contracts": { "ZecuredSwap": swap },
            "usdc": usdc,
            "relay": relay,
            "versions": versions,
        })
    }

    fn opts(rpc: &str, deployment: &Path, taker: &PrivateKeySigner) -> SwapOpts {
        SwapOpts {
            rpc: rpc.into(),
            taker_key: taker.to_bytes().to_string(),
            invite: None,
            host: "mock-host".into(),
            mock: true,
            headers: "headers.json".into(),
            summaries: "summaries.json".into(),
            min_window: 10_800,
            pin: PinOpts {
                swap_deployment: deployment.into(),
            },
        }
    }

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("zecured-cli-swap-{}", hex::encode(zecured_swap::random_bytes32())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A maker's Sell quote for `taker`, signed for `swap` on chain `chain_id`, as its API answers it.
    fn sell_quote(taker: Address, swap: Address, chain_id: u64, zatoshi: u64) -> serde_json::Value {
        let maker = PrivateKeySigner::random();
        let sk = (1u8..).find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes([b; 32]))).unwrap();
        let address = FullViewingKey::from(&sk).address_at(7u32, Scope::External);
        let salt = [0x5a; 32];
        let q = SwapQuote {
            maker: maker.address(),
            taker,
            side: SIDE_SELL,
            zatoshi,
            usdc: U256::from(19_800_000u64),
            makerReceiverCommitment: zecured_swap::receiver_commitment(&salt, &address.to_raw_address_bytes()),
            payWindow: 21_600,
            quoteDeadline: 4_000_000_000,
            nonce: U256::from(7),
        };
        let signature = sign_quote(&maker, &swap_domain(chain_id, swap), &q).unwrap();
        let fields = QuoteFields {
            maker: q.maker,
            taker,
            side: q.side,
            zatoshi: zatoshi.to_string(),
            usdc: q.usdc.to_string(),
            maker_receiver_commitment: q.makerReceiverCommitment,
            pay_window: q.payWindow,
            quote_deadline: q.quoteDeadline.to_string(),
            nonce: q.nonce.to_string(),
        };
        json!({
            "quote": fields,
            "signature": signature,
            "swap": swap,
            "chainId": chain_id,
            "orderId": zecured_swap::order_id(chain_id, swap, q.maker, q.nonce),
            "expiresAt": q.quoteDeadline.to_string(),
            "makerReceiver": encode_ironwood_ua(&Network::TestNetwork, address).unwrap(),
            "makerSalt": format!("0x{}", hex::encode(salt)),
        })
    }

    /// A maker's API and a Base JSON-RPC on one loopback port: `GET /swap/quote` answers `quote`;
    /// the JSON-RPC answers `eth_chainId` (`chain_id`) and ZecuredSwap's `usdc()`, `relay()` and
    /// `versions()` (`contract`).
    async fn stub(quote: serde_json::Value, chain_id: u64, contract: [Address; 3]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let quote = Arc::new(quote);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(answer(sock, quote.clone(), chain_id, contract));
            }
        });
        url
    }

    async fn answer(
        mut sock: tokio::net::TcpStream,
        quote: Arc<serde_json::Value>,
        chain_id: u64,
        contract: [Address; 3],
    ) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let end = loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let length = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        while buf.len() < end + length {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = if head.starts_with("GET /swap/quote") {
            quote.to_string()
        } else {
            let req: serde_json::Value = serde_json::from_slice(&buf[end..]).unwrap_or_default();
            rpc(&req, chain_id, contract).to_string()
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    fn rpc(req: &serde_json::Value, chain_id: u64, [usdc, relay, versions]: [Address; 3]) -> serde_json::Value {
        let error = |message: &str| json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": -32000, "message": message } });
        let result = match req["method"].as_str() {
            Some("eth_chainId") => json!(format!("0x{chain_id:x}")),
            Some("eth_call") => {
                let call = &req["params"][0];
                let data = call["input"].as_str().or(call["data"].as_str()).unwrap_or_default();
                let selector =
                    hex::decode(data.trim_start_matches("0x").get(..8).unwrap_or_default()).unwrap_or_default();
                let answer = if selector == IZecuredSwap::usdcCall::SELECTOR {
                    usdc
                } else if selector == IZecuredSwap::relayCall::SELECTOR {
                    relay
                } else if selector == IZecuredSwap::versionsCall::SELECTOR {
                    versions
                } else {
                    return error("unexpected call");
                };
                json!(format!("0x{:0>64}", hex::encode(answer)))
            }
            _ => return error("unexpected method"),
        };
        json!({ "jsonrpc": "2.0", "id": req["id"], "result": result })
    }

    /// W-9: a maker's quote naming another ZecuredSwap (signed for it), or another chain, is refused
    /// and no order file is kept; the pinned contract's quote is taken.
    #[tokio::test]
    async fn w9_a_quote_naming_a_foreign_swap_is_refused() {
        let dir = temp_dir();
        let deployment_file = dir.join("base-sepolia.swap.json");
        std::fs::write(&deployment_file, serde_json::to_vec(&record(PINNED)).unwrap()).unwrap();
        let wallet = dir.join("wallet.json");
        let taker = PrivateKeySigner::random();
        let order_files = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("swap-"))
                .count()
        };
        for (swap, chain_id) in [(Address::repeat_byte(0x66), 84_532), (PINNED, 8_453)] {
            let url = stub(sell_quote(taker.address(), swap, chain_id, 50_000_000), 84_532, deployment()).await;
            let err = take_sell_quote(&wallet, &url, 50_000_000, None, &opts(&url, &deployment_file, &taker))
                .await
                .err()
                .expect("a quote of another contract or chain is refused");
            assert!(err.to_string().contains("pinned"), "{err:#}");
            assert_eq!(order_files(), 0, "nothing kept");
        }
        let url = stub(sell_quote(taker.address(), PINNED, 84_532, 50_000_000), 84_532, deployment()).await;
        let f = take_sell_quote(&wallet, &url, 50_000_000, None, &opts(&url, &deployment_file, &taker))
            .await
            .unwrap();
        assert_eq!((f.swap, f.chain_id), (PINNED, 84_532));
        assert_eq!(order_files(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// W-9: the pinned contract on the RPC's chain must escrow the record's USDC and follow its
    /// relay and version registry.
    #[tokio::test]
    async fn w9_the_pinned_contract_must_be_the_deployments() {
        let pin = SwapPin::from_record(&record(PINNED)).unwrap();
        let provider = |url: &str| ProviderBuilder::new().connect_http(url.parse().unwrap());
        let [usdc, relay, versions] = deployment();
        let url = stub(json!(null), 84_532, deployment()).await;
        pin.check_contract(&provider(&url)).await.unwrap();
        let other = Address::repeat_byte(0x99);
        for wrong in [[other, relay, versions], [usdc, other, versions], [usdc, relay, other]] {
            let url = stub(json!(null), 84_532, wrong).await;
            assert!(pin.check_contract(&provider(&url)).await.is_err());
        }
        let url = stub(json!(null), 8_453, deployment()).await;
        let e = pin.check_contract(&provider(&url)).await.unwrap_err();
        assert!(e.to_string().contains("chain 8453"), "{e:#}");
    }

    fn file() -> SwapFile {
        SwapFile {
            order_id: B256::repeat_byte(1),
            chain_id: 84_532,
            swap: PINNED,
            quote: serde_json::from_value(
                sell_quote(Address::repeat_byte(2), PINNED, 84_532, 50_000_000)["quote"].clone(),
            )
            .unwrap(),
            signature: Bytes::new(),
            maker_receiver: "ab".repeat(43),
            maker_salt: "5a".repeat(32),
            status: "opened".into(),
            opening: None,
            txid: None,
            raw_tx: None,
        }
    }

    /// W-9: the pin is DeploySwap's record; an order file of another contract or chain is refused.
    #[test]
    fn w9_the_pin_is_deploy_swaps_record() {
        let [usdc, relay, versions] = deployment();
        let pin = SwapPin::from_record(&record(PINNED)).unwrap();
        assert_eq!(
            pin,
            SwapPin {
                chain_id: 84_532,
                swap: PINNED,
                usdc,
                relay,
                versions
            }
        );
        let mut two = record(PINNED);
        two["contracts"]["ZecuredSwap"] = json!(Address::repeat_byte(0x12));
        assert!(SwapPin::from_record(&two).is_err(), "two contracts");
        let mut nested_only = record(PINNED);
        nested_only.as_object_mut().unwrap().remove("ZecuredSwap");
        assert_eq!(SwapPin::from_record(&nested_only).unwrap().swap, PINNED);
        let mut zero = record(PINNED);
        zero["usdc"] = json!(Address::ZERO);
        assert!(SwapPin::from_record(&zero).is_err());
        let mut missing = record(PINNED);
        missing.as_object_mut().unwrap().remove("versions");
        assert!(SwapPin::from_record(&missing).is_err());
        let mut f = file();
        pin.check_file(&f).unwrap();
        f.swap = Address::repeat_byte(0x66);
        assert!(pin.check_file(&f).is_err(), "an order file written from a maker's word");
        f.swap = PINNED;
        f.chain_id = 8_453;
        assert!(pin.check_file(&f).is_err());
    }

    #[test]
    fn quote_fields_round_trip() {
        let f = QuoteFields {
            maker: Address::repeat_byte(1),
            taker: Address::repeat_byte(2),
            side: SIDE_SELL,
            zatoshi: "50000000".into(),
            usdc: "19800000".into(),
            maker_receiver_commitment: B256::repeat_byte(3),
            pay_window: 21_600,
            quote_deadline: "1760000120".into(),
            nonce: "115792089237316195423570985008687907853269984665640564039457584007913129639935".into(),
        };
        let q = f.to_quote().unwrap();
        assert_eq!(q.zatoshi, 50_000_000);
        assert_eq!(q.nonce, U256::MAX);
        assert_eq!(q.quoteDeadline, 1_760_000_120);
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["makerReceiverCommitment"], serde_json::json!(B256::repeat_byte(3)));
        assert!(usdc_units(19.8).unwrap() == U256::from(19_800_000u64));
        assert!(usdc_units(0.0).is_err());
    }
}
