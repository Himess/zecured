//! Zecured Swap from the CLI wallet (work order 5.2, Sell: "off-ramp"): sell shielded ZEC for USDC
//! with no custodian. The maker locks USDC in ZecuredSwap; this wallet pays one Ironwood note
//! with the memo "ZSWP" || orderId and `ovk = None` to the maker's receiver, proves the payment in
//! zero knowledge and finalizes the order, which releases the USDC to the taker's Base address.
//!
//!   zecured-wallet swap sell --maker <url> --zec 0.5 [--min-usdc 19.5]
//!   zecured-wallet swap resume --order <orderId>
//!   zecured-wallet swap status --order <orderId>
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

use crate::{load_wallet, lwd_connect, read_secret_json, scan, write_json, zcash_network, zcash_params};

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
        SwapCmd::Status { order, rpc } => {
            let f: SwapFile = read_secret_json(&swap_path(wallet, &order))?;
            let provider = ProviderBuilder::new().connect_http(rpc.parse()?);
            let o = IZecuredSwap::new(f.swap, &provider).order(order).call().await?;
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
    let maker_ua = v["makerReceiver"].as_str().context("makerReceiver")?;
    let maker_salt: [u8; 32] = hex::decode(v["makerSalt"].as_str().context("makerSalt")?.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("makerSalt: 32 bytes"))?;

    // Everything the order will be held to, checked here first.
    ensure!(provider.get_chain_id().await? == chain_id, "the maker quotes for another chain than the RPC's");
    ensure!(q.side == SIDE_SELL && q.taker == me && q.zatoshi == zatoshi, "the quote is not the one asked for");
    check_quote_signature(&swap_domain(chain_id, swap), &q, &signature)?;
    let receiver = ironwood_receiver(&zcash_params(), maker_ua)?.to_raw_address_bytes();
    ensure!(
        zecured_swap::receiver_commitment(&maker_salt, &receiver) == q.makerReceiverCommitment,
        "the maker's receiver does not open the quote's commitment"
    );
    ensure!(q.payWindow >= opts.min_window, "the pay window ({} s) is too short to pay and prove", q.payWindow);
    if let Some(min) = min_usdc {
        ensure!(q.usdc >= min, "the quote pays {} USDC units, less than the minimum asked", q.usdc);
    }
    let order_id = zecured_swap::order_id(chain_id, swap, q.maker, q.nonce);
    let f = SwapFile {
        order_id,
        chain_id,
        swap,
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
    let swap = IZecuredSwap::new(f.swap, &provider);
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
                    let relay = relay_tip(&provider, f.swap).await?;
                    let now = provider
                        .get_block_by_number(BlockNumberOrTag::Latest)
                        .await?
                        .context("no latest Base block")?
                        .header
                        .timestamp;
                    if settlement_ready(&p, relay, now) {
                        let tx = finalize_settlement(&provider, f.swap, f.order_id).await?;
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
    let (_, height) = lwd::raw_transaction(&mut client, &txid).await?;
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
    use super::*;

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
