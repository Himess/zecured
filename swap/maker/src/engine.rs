//! The order engine: follows the maker's orders on Base and moves each one along. Buy: wait for
//! the taker's destination, fix the height of the proof's anchor and pay after a random privacy
//! delay (W-10, H-05; `zecured_swap::anchor`), wait for the payment's block and for the order's
//! tier of work on top of the header at exactly that height, build the payment witness, prove it,
//! submit the proof, finalize once ready. Sell: the taker pays and proves; the maker finalizes a
//! ready settlement and refunds itself after the deadline. Every decision is the pure function
//! [`decide`] of an order and the facts read for it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use alloy::{
    eips::BlockNumberOrTag,
    primitives::{Address, B256},
    providers::{DynProvider, Provider},
    rpc::types::Filter,
    signers::local::PrivateKeySigner,
    sol_types::{Eip712Domain, SolEvent},
};
use anyhow::{Context, Result, anyhow};
use orchard::{
    circuit::ProvingKey,
    keys::{FullViewingKey, SpendingKey},
};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, Network},
};
use zecured_base::gas_with_headroom;
use zecured_deposit_witness::{p2p, payment};
use zecured_swap::{
    OrderStatus, Side,
    abi::IZecuredSwap,
    anchor,
    chain::{ProofPath, anchor_at, finalize_settlement, prove_settlement, relay_tip, swap_version_working},
    pay::{Seen, authorize, build_payment, check_payment_note, payment_seen, select_inputs},
    seal::Prover,
};
use zecured_zcash_ops::{lwd, network::ZcashNetwork, scan::ScanState, spend};

use crate::{
    api::ApiError,
    config::MakerConfig,
    gate::{Destination, check_destination},
    quoting::{ChainLimits, PAYMENT_FEE_HEADROOM},
    state::{Book, MakerStatus, OrderRecord},
};

/// The facts about one order the engine reads before it decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// Base's clock (the latest block's timestamp).
    pub now: u64,
    /// The light wallet server's Zcash tip.
    pub zcash_tip: u32,
    /// The order's status on Base.
    pub status: OrderStatus,
    /// Its pending settlement, if any: whether it is stale (droppable) or ready (finalizable).
    pub pending: Option<Pending>,
    /// Paid: what the light wallet server says of the payment (W-8: a transport error is no
    /// answer, and skips the order for the pass).
    pub payment: Option<Seen>,
    /// W-8: the wallet's scan, past the payment's expiry height, shows every input note of the
    /// payment unspent: the payment never happened.
    pub inputs_unspent: bool,
    /// Another payment of the maker is broadcast and not mined yet (one at a time: its inputs are
    /// not marked spent before it is mined).
    pub other_payment_unmined: bool,
    /// W-20: the last block of the tip's epoch (`None` while no later activation is known), and
    /// whether the payment statement proves on that epoch.
    pub tip_epoch_last: Option<u32>,
    pub tip_epoch_provable: bool,
    /// W-20: mined, the last block of the payment's epoch.
    pub payment_epoch_last: Option<u32>,
    /// W-7: whether a proof could land (read once per pass).
    pub proofs: ProofPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub stale: bool,
    pub ready: bool,
}

/// What the engine does next for an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// W-10: fix the payment's proof anchor before the payment exists, and its privacy delay:
    /// broadcast once the Zcash tip reaches `pay_after`; anchor at `planned_anchor`, `draw` the
    /// order's `r`.
    SchedulePayment {
        pay_after: u32,
        planned_anchor: u32,
        draw: u32,
    },
    Pay,
    /// The payment is broadcast and unmined: send it again (a server may have dropped it).
    Rebroadcast,
    /// The payment expired unmined, its inputs unspent: it never happened; pay again if there is
    /// time (W-8).
    PaymentExpired,
    /// Record the payment's block and the anchor height of its proof (W-10: the planned one, or a
    /// late payment's).
    ScheduleAnchor {
        mined: u32,
        anchor: u32,
    },
    Prove,
    Drop,
    Finalize,
    Refund,
    /// Back to proving (the pending settlement disappeared: someone dropped it).
    Reprove,
    Finish(MakerStatus),
    /// The maker stops paying or proving (too late); the refund follows after the deadline.
    Abandon(&'static str),
}

/// The next step for `rec` given `f`. `pay_delay` (1 to `pay_delay_max_blocks`) and `draw` (0 to
/// `pay_delay_max_blocks`, the anchor's `r`) are the random draws (`zecured_swap::anchor::draw`),
/// made by the caller; an order keeps the first `draw` it is scheduled with.
pub fn decide(cfg: &MakerConfig, rec: &OrderRecord, f: &Facts, pay_delay: u32, draw: u32) -> Step {
    match f.status {
        OrderStatus::Settled => return Step::Finish(MakerStatus::Settled),
        OrderStatus::Refunded => return Step::Finish(MakerStatus::Refunded),
        OrderStatus::None => return Step::Wait,
        OrderStatus::Open => {}
    }
    // A proof is pending, whoever made it: drop it if it fell, finalize it once ready.
    if let Some(p) = f.pending {
        return if p.stale {
            Step::Drop
        } else if p.ready {
            Step::Finalize
        } else {
            Step::Wait
        };
    }
    let past_deadline = f.now > rec.pay_deadline;
    let draw = rec.anchor_draw.unwrap_or(draw);
    match (rec.side, rec.status) {
        (_, MakerStatus::Settled | MakerStatus::Refunded) => Step::Wait,
        (Side::Sell, _) => {
            if past_deadline {
                Step::Refund
            } else {
                Step::Wait
            }
        }
        (Side::Buy, MakerStatus::AwaitingDestination | MakerStatus::Abandoned) => {
            if past_deadline {
                Step::Refund
            } else {
                Step::Wait
            }
        }
        (Side::Buy, MakerStatus::DestinationReceived) => {
            if f.now.saturating_add(cfg.required_secs()) > rec.pay_deadline {
                return Step::Abandon("too late to pay and prove before the deadline");
            }
            // W-7: never a payment whose proof could not land (a removed Swap version, a stopped
            // router, a relay behind); the deadline check abandons the order if it lasts.
            if !f.proofs.version_working || f.proofs.relay_lag > cfg.max_relay_lag_blocks {
                return Step::Wait;
            }
            // W-20: the payment, its anchor and the order's tier of work in one epoch the statement
            // proves on, or no payment yet (past the activation, the next epoch may take it).
            if !anchor::fits_in_epoch(f.zcash_tip, cfg.anchor_pipeline_blocks(), f.tip_epoch_provable, f.tip_epoch_last)
            {
                return Step::Wait;
            }
            let tip = f.zcash_tip;
            match (rec.pay_after_height, rec.planned_anchor_height) {
                // Scheduled, and a payment broadcast now still lands below its planned anchor.
                (Some(after), Some(planned)) if tip.saturating_add(1) < planned => {
                    if tip >= after && !f.other_payment_unmined {
                        Step::Pay
                    } else {
                        Step::Wait
                    }
                }
                // W-10: not scheduled yet, or the schedule went by unpaid: the anchor is planned
                // (again) from the block being mined now, before the payment exists, with the
                // order's draw.
                _ => {
                    let c = anchor::current_height(tip);
                    Step::SchedulePayment {
                        pay_after: anchor::pay_after(c, pay_delay),
                        planned_anchor: anchor::planned_anchor(c, cfg.pay_delay_max_blocks, draw),
                        draw,
                    }
                }
            }
        }
        (Side::Buy, MakerStatus::Paid) => {
            if past_deadline {
                return Step::Abandon("paid, but not proven before the deadline");
            }
            match f.payment {
                Some(Seen::Mined(mined)) => match rec.anchor_height {
                    // The block seen when the anchor was scheduled (a reorganization may move it); W-7:
                    // no proving run while ZecuredSwap would not take the proof.
                    Some(a) if rec.mined_height == Some(mined) && a > mined => {
                        if f.proofs.version_working {
                            Step::Prove
                        } else {
                            Step::Wait
                        }
                    }
                    // W-10: the planned anchor whatever the payment's block, unless it came late;
                    // W-20: never past the last block of the payment's epoch.
                    _ => match anchor::anchor_height(rec.planned_anchor_height, draw, mined, f.payment_epoch_last) {
                        Some(anchor) => Step::ScheduleAnchor { mined, anchor },
                        None => Step::Abandon("paid, but its proof cannot land: the order will be refunded"),
                    },
                },
                // W-8: not mined as far as the server says, which proves nothing (a reorganization, a
                // server behind or another one): before its expiry height the payment is sent again;
                // after it, it never happened only if the wallet shows its inputs unspent. Otherwise
                // the maker waits, and never pays the order twice.
                _ => {
                    if !rec.expiry_height.is_some_and(|e| f.zcash_tip > e) {
                        Step::Rebroadcast
                    } else if f.inputs_unspent {
                        Step::PaymentExpired
                    } else {
                        Step::Wait
                    }
                }
            }
        }
        (Side::Buy, MakerStatus::Proven) => {
            if past_deadline {
                Step::Abandon("the proof fell after the deadline")
            } else {
                Step::Reprove
            }
        }
        (Side::Buy, MakerStatus::TakerPays) => Step::Wait,
    }
}

/// W-8: whether a wallet scan that covers blocks up to `scanned_to` shows every input of a payment
/// with expiry height `expiry` unspent: `inputs` are the payment's input nullifiers (hex) and
/// `notes` the wallet's notes (nullifier, spent). Then the payment was never mined, and never will
/// be: no block above its expiry height may hold it. A payment without recorded inputs shows
/// nothing.
pub fn inputs_unspent(
    scanned_to: u32,
    expiry: u32,
    inputs: &[String],
    notes: impl IntoIterator<Item = ([u8; 32], bool)>,
) -> bool {
    if scanned_to <= expiry || inputs.is_empty() {
        return false;
    }
    let notes: Vec<(String, bool)> = notes.into_iter().map(|(nf, spent)| (hex::encode(nf), spent)).collect();
    inputs.iter().all(|input| notes.iter().any(|(nf, spent)| nf == input && !spent))
}

/// What the maker knows when a destination arrives (`POST /swap/orders/:id/destination`).
#[derive(Clone, Copy, Debug)]
pub struct DestinationFacts {
    /// Base's clock.
    pub now: u64,
    /// W-7: whether a proof could land now.
    pub proofs: ProofPath,
    /// W-19: the zatoshi the maker can still pay: its scanned balance less what orders with a
    /// destination reserved (this order has none yet).
    pub zcash_free: u64,
    /// W-19: the taker's other open orders already given a destination (in payment).
    pub taker_paying: usize,
}

/// `POST /swap/orders/:id/destination`: whether the maker takes the taker's destination for `rec`
/// now. Only a Buy order still waiting for it (or given the same one again), with time left for
/// the whole pipeline before its deadline, and a destination that opens its commitment; a first
/// destination only while a proof could land (W-7).
pub fn accept_destination(
    cfg: &MakerConfig,
    network: &Network,
    rec: &OrderRecord,
    d: &DestinationFacts,
    receiver: &str,
    salt: &str,
) -> Result<Destination, ApiError> {
    if rec.side != Side::Buy {
        return Err(ApiError::NOT_BUY);
    }
    let first = match rec.status {
        MakerStatus::AwaitingDestination => true,
        MakerStatus::DestinationReceived => false,
        _ => return Err(ApiError::NOT_AWAITING),
    };
    if d.now.saturating_add(cfg.required_secs()) > rec.pay_deadline {
        return Err(ApiError::TOO_LATE);
    }
    let destination =
        check_destination(network, receiver, salt, rec.receiver_commitment).map_err(|_| ApiError::BAD_DESTINATION)?;
    if first {
        // W-7: no destination for a payment whose proof could not land; it may be given again later.
        if !d.proofs.version_working {
            return Err(ApiError::PROOFS_STOPPED);
        }
        if d.proofs.relay_lag > cfg.max_relay_lag_blocks {
            return Err(ApiError::RELAY_BEHIND);
        }
        // W-19: from here on the order reserves its ZEC: the maker must have it, and one taker's
        // orders in payment stay within its cap.
        if d.zcash_free < rec.zatoshi.saturating_add(PAYMENT_FEE_HEADROOM) {
            return Err(ApiError::NO_INVENTORY);
        }
        if d.taker_paying >= cfg.max_open_orders_per_taker {
            return Err(ApiError::TAKER_BUSY);
        }
    }
    Ok(destination)
}

/// The maker's Ironwood wallet: the file `zecured-wallet init` writes.
#[derive(Deserialize)]
struct WalletFile {
    spending_key: String,
    birthday: u32,
}

pub struct Wallet {
    pub sk: SpendingKey,
    pub fvk: FullViewingKey,
    pub birthday: u32,
    pub scan: Option<ScanState>,
}

impl Wallet {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let f: WalletFile =
            serde_json::from_slice(&std::fs::read(path).with_context(|| format!("read {}", path.display()))?)?;
        let bytes: [u8; 32] = hex::decode(&f.spending_key)?.try_into().map_err(|_| anyhow!("bad spending key"))?;
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes(bytes)).context("invalid spending key")?;
        Ok(Self {
            fvk: FullViewingKey::from(&sk),
            sk,
            birthday: f.birthday,
            scan: None,
        })
    }
}

/// The maker: its configuration, keys, chain access and books.
pub struct Maker {
    pub cfg: MakerConfig,
    /// The payments' Zcash network.
    pub zcash: ZcashNetwork,
    /// Its consensus parameters (encodings, branch ids by height).
    pub network: Network,
    pub signer: PrivateKeySigner,
    pub provider: DynProvider,
    pub domain: Eip712Domain,
    pub limits: ChainLimits,
    pub book: Mutex<Book>,
    pub wallet: Mutex<Wallet>,
    /// The wallet's spendable balance after the last scan (zatoshi).
    pub balance: AtomicU64,
    /// The last pass's Zcash tip, relay tip and when it read them (Unix seconds).
    zcash_tip: AtomicU64,
    relay_tip: AtomicU64,
    pass_at: AtomicU64,
    pub prover: Prover,
    /// W-7: the Swap version of the prover's guest and selector: the maker quotes, takes
    /// destinations, pays and proves only while it is working.
    pub version_id: B256,
    proving_key: tokio::sync::OnceCell<Arc<ProvingKey>>,
}

/// The system clock, Unix seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A new order of this maker, as Base opened it.
pub fn new_record(
    order_id: B256,
    side: Side,
    taker: Address,
    zatoshi: u64,
    usdc: alloy::primitives::U256,
    pay_deadline: u64,
    receiver_commitment: B256,
) -> OrderRecord {
    OrderRecord {
        order_id,
        side,
        taker,
        zatoshi,
        usdc,
        pay_deadline,
        receiver_commitment,
        status: match side {
            Side::Buy => MakerStatus::AwaitingDestination,
            Side::Sell => MakerStatus::TakerPays,
        },
        receiver: None,
        salt: None,
        pay_after_height: None,
        planned_anchor_height: None,
        anchor_draw: None,
        txid: None,
        opening: None,
        raw_tx: None,
        expiry_height: None,
        input_nullifiers: vec![],
        mined_height: None,
        anchor_height: None,
        note: None,
    }
}

impl Maker {
    pub fn new(
        cfg: MakerConfig,
        zcash: ZcashNetwork,
        signer: PrivateKeySigner,
        provider: DynProvider,
        domain: Eip712Domain,
        limits: ChainLimits,
        book: Book,
        wallet: Wallet,
        version_id: B256,
    ) -> Self {
        let prover = cfg.prover();
        Self {
            cfg,
            zcash,
            network: zcash.params(),
            signer,
            provider,
            domain,
            limits,
            book: Mutex::new(book),
            wallet: Mutex::new(wallet),
            balance: AtomicU64::new(0),
            zcash_tip: AtomicU64::new(0),
            relay_tip: AtomicU64::new(0),
            pass_at: AtomicU64::new(0),
            prover,
            version_id,
            proving_key: tokio::sync::OnceCell::new(),
        }
    }

    /// The last pass's Zcash tip, relay tip and when (Unix seconds).
    pub fn last_pass(&self) -> (u64, u64, u64) {
        (
            self.zcash_tip.load(Ordering::Relaxed),
            self.relay_tip.load(Ordering::Relaxed),
            self.pass_at.load(Ordering::Relaxed),
        )
    }

    /// W-7: whether a payment made now could be proven, as the API decides a quote or a
    /// destination: the version is read now, the relay's lag is the last pass's, which must be
    /// recent (the maker follows both chains).
    pub async fn proof_path(&self) -> Result<ProofPath, ApiError> {
        let (zcash_tip, relay_height, pass_at) = self.last_pass();
        if unix_now().saturating_sub(pass_at) > 3 * self.cfg.poll_secs + 60 {
            return Err(ApiError::UNAVAILABLE);
        }
        Ok(ProofPath {
            version_working: swap_version_working(&self.provider, self.cfg.swap, self.version_id).await?,
            relay_lag: zcash_tip.saturating_sub(relay_height),
        })
    }

    /// An open order of this maker read from Base (one the engine has not synced yet).
    pub async fn record_from_chain(&self, order_id: B256) -> Result<Option<OrderRecord>> {
        let o = self.swap().order(order_id).call().await?;
        if o.maker != self.signer.address() || OrderStatus::from_u8(o.status)? != OrderStatus::Open {
            return Ok(None);
        }
        Ok(Some(new_record(
            order_id,
            Side::from_u8(o.side)?,
            o.taker,
            o.zatoshi,
            o.usdc,
            o.payDeadline,
            o.receiverCommitment,
        )))
    }

    pub fn swap(&self) -> IZecuredSwap::IZecuredSwapInstance<&DynProvider> {
        IZecuredSwap::new(self.cfg.swap, &self.provider)
    }

    /// Base's clock: the latest block's timestamp.
    pub async fn now(&self) -> Result<u64> {
        Ok(self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("no latest Base block")?
            .header
            .timestamp)
    }

    async fn proving_key(&self) -> Result<Arc<ProvingKey>> {
        let pk = self
            .proving_key
            .get_or_try_init(|| async { tokio::task::spawn_blocking(spend::proving_key).await.map(Arc::new) })
            .await?;
        Ok(pk.clone())
    }

    /// Runs the engine forever.
    pub async fn run(self: Arc<Self>) {
        loop {
            if let Err(e) = self.tick().await {
                warn!("swap engine pass failed: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(self.cfg.poll_secs)).await;
        }
    }

    /// One pass over every open order.
    pub async fn tick(&self) -> Result<()> {
        let mut client = lwd::connect(&self.cfg.lwd).await?;
        lwd::check_network(&mut client, self.zcash).await?;
        let zcash_tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
        self.scan_wallet(&mut client, zcash_tip).await?;
        self.sync_orders().await?;
        let now = self.now().await?;
        let relay_height = relay_tip(&self.provider, self.cfg.swap).await?;
        // W-7: whether a proof could land, read once per pass, before any payment of the pass.
        let proofs = ProofPath {
            version_working: swap_version_working(&self.provider, self.cfg.swap, self.version_id).await?,
            relay_lag: u64::from(zcash_tip).saturating_sub(relay_height),
        };
        self.zcash_tip.store(u64::from(zcash_tip), Ordering::Relaxed);
        self.relay_tip.store(relay_height, Ordering::Relaxed);
        self.pass_at.store(unix_now(), Ordering::Relaxed);
        let ids: Vec<B256> = self.book.lock().await.open_orders().map(|o| o.order_id).collect();
        for id in ids {
            if let Err(e) = self.advance(&mut client, id, now, zcash_tip, relay_height, proofs).await {
                warn!(order = %id, "order step failed: {e:#}");
            }
        }
        Ok(())
    }

    async fn scan_wallet(&self, client: &mut lwd::LwdClient, tip: u32) -> Result<()> {
        let mut w = self.wallet.lock().await;
        if w.scan.is_none() {
            let state = ScanState::from_birthday(client, w.fvk.clone(), w.birthday).await?;
            w.scan = Some(state);
        }
        let state = w.scan.as_mut().expect("set above");
        if let Err(e) = state.scan_to(client, tip).await {
            // A reorg or a server change: start again from the birthday next pass.
            w.scan = None;
            return Err(e.context("scan the maker's wallet"));
        }
        self.balance.store(state.balance(), Ordering::Relaxed);
        Ok(())
    }

    /// Reads `OrderOpened` of this maker since the last pass into the book.
    async fn sync_orders(&self) -> Result<()> {
        let head = self.provider.get_block_number().await?;
        let from = {
            let book = self.book.lock().await;
            book.synced_to.max(self.cfg.from_block.saturating_sub(1)) + 1
        };
        if from > head {
            return Ok(());
        }
        let swap = self.cfg.swap;
        let provider = &self.provider;
        let logs = zecured_base::events::get_logs_chunked(from, head, |a, b| {
            let filter = Filter::new()
                .address(swap)
                .event_signature(IZecuredSwap::OrderOpened::SIGNATURE_HASH)
                .from_block(a)
                .to_block(b);
            async move { provider.get_logs(&filter).await.map_err(anyhow::Error::from) }
        })
        .await?;
        let me = self.signer.address();
        let mut book = self.book.lock().await;
        for log in logs.iter().filter(|l| !l.removed) {
            let ev = IZecuredSwap::OrderOpened::decode_log(&log.inner).context("OrderOpened")?;
            if ev.maker != me || book.orders.contains_key(&ev.orderId) {
                continue;
            }
            let side = Side::from_u8(ev.side)?;
            info!(order = %ev.orderId, ?side, zatoshi = ev.zatoshi, "order opened");
            book.orders.insert(
                ev.orderId,
                new_record(ev.orderId, side, ev.taker, ev.zatoshi, ev.usdc, ev.payDeadline, ev.receiverCommitment),
            );
        }
        book.synced_to = head;
        book.save(&self.cfg.state)
    }

    /// The facts about `rec` (chain reads only).
    async fn facts(
        &self,
        client: &mut lwd::LwdClient,
        rec: &OrderRecord,
        now: u64,
        zcash_tip: u32,
        relay_height: u64,
        proofs: ProofPath,
    ) -> Result<Facts> {
        let swap = self.swap();
        let order = swap.order(rec.order_id).call().await?;
        let status = OrderStatus::from_u8(order.status)?;
        let p = swap.pendingSettlement(rec.order_id).call().await?;
        let pending = if p.anchor == B256::ZERO {
            None
        } else {
            let stale = swap.settlementStale(rec.order_id).call().await?;
            Some(Pending {
                stale,
                ready: zecured_swap::chain::settlement_ready(&p, relay_height, now),
            })
        };
        // W-8: a transport error is no answer: `?` skips the order for this pass, so a payment is
        // never taken for unmined because the server could not be reached.
        let payment = match (rec.status, rec.txid.as_deref()) {
            (MakerStatus::Paid, Some(txid)) => {
                Some(payment_seen(client, &TxId::from_hex(txid).context("stored txid")?).await?)
            }
            _ => None,
        };
        let inputs_unspent = match (rec.status, rec.expiry_height) {
            (MakerStatus::Paid, Some(expiry)) => {
                let w = self.wallet.lock().await;
                w.scan.as_ref().is_some_and(|s| {
                    let notes = s.notes.iter().map(|n| (n.nullifier.to_bytes(), n.spent_at.is_some()));
                    inputs_unspent(s.scanned_to, expiry, &rec.input_nullifiers, notes)
                })
            }
            _ => false,
        };
        let other_payment_unmined = {
            let book = self.book.lock().await;
            book.open_orders()
                .any(|o| o.order_id != rec.order_id && o.status == MakerStatus::Paid && o.mined_height.is_none())
        };
        Ok(Facts {
            now,
            zcash_tip,
            status,
            pending,
            payment,
            inputs_unspent,
            other_payment_unmined,
            tip_epoch_last: anchor::epoch_last_height(self.zcash, zcash_tip),
            tip_epoch_provable: anchor::provable_epoch(self.zcash, zcash_tip),
            payment_epoch_last: match payment {
                Some(Seen::Mined(h)) => anchor::epoch_last_height(self.zcash, h),
                _ => None,
            },
            proofs,
        })
    }

    async fn advance(
        &self,
        client: &mut lwd::LwdClient,
        id: B256,
        now: u64,
        zcash_tip: u32,
        relay_height: u64,
        proofs: ProofPath,
    ) -> Result<()> {
        let Some(rec) = self.book.lock().await.orders.get(&id).cloned() else {
            return Ok(());
        };
        let f = self.facts(client, &rec, now, zcash_tip, relay_height, proofs).await?;
        let (pay_delay, draw) = anchor::draw(self.cfg.pay_delay_max_blocks);
        let step = decide(&self.cfg, &rec, &f, pay_delay, draw);
        let mut rec = rec;
        match step {
            Step::Wait => return Ok(()),
            Step::SchedulePayment {
                pay_after,
                planned_anchor,
                draw,
            } => {
                rec.pay_after_height = Some(pay_after);
                rec.planned_anchor_height = Some(planned_anchor);
                rec.anchor_draw = Some(draw);
            }
            Step::Pay => self.pay(client, &mut rec).await?,
            Step::Rebroadcast => {
                if let Some(raw) = rec.raw_tx.as_deref() {
                    // A known transaction is refused again: that is fine.
                    let _ = spend::broadcast_raw(client, hex::decode(raw)?).await;
                }
                return Ok(());
            }
            Step::PaymentExpired => {
                info!(order = %id, "the payment expired unmined, its inputs unspent; paying again if there is time");
                rec.status = MakerStatus::DestinationReceived;
                // Scheduled again, its anchor planned again (the order keeps its draw).
                rec.pay_after_height = None;
                rec.planned_anchor_height = None;
                rec.txid = None;
                rec.opening = None;
                rec.raw_tx = None;
                rec.expiry_height = None;
                rec.input_nullifiers.clear();
                rec.mined_height = None;
                rec.anchor_height = None;
            }
            Step::ScheduleAnchor { mined, anchor } => {
                rec.mined_height = Some(mined);
                rec.anchor_height = Some(anchor);
                // A payment from before anchors were planned keeps the draw its anchor used.
                rec.anchor_draw.get_or_insert(draw);
            }
            Step::Prove => {
                if !self.prove(client, &mut rec).await? {
                    return Ok(());
                }
            }
            Step::Reprove => {
                rec.status = MakerStatus::Paid;
            }
            Step::Drop => {
                let swap = self.swap();
                let call = swap.dropSettlement(id);
                let gas = gas_with_headroom(call.estimate_gas().await.context("estimate dropSettlement")?);
                let r = call.gas(gas).send().await?.get_receipt().await?;
                anyhow::ensure!(r.status(), "dropSettlement reverted");
                info!(order = %id, "dropped a stale settlement");
                if rec.side == Side::Buy && rec.status == MakerStatus::Proven {
                    rec.status = MakerStatus::Paid;
                }
            }
            Step::Finalize => {
                finalize_settlement(&self.provider, self.cfg.swap, id).await?;
                info!(order = %id, "settled");
                rec.finish(MakerStatus::Settled, None);
            }
            Step::Refund => {
                let swap = self.swap();
                let call = swap.refund(id);
                let gas = gas_with_headroom(call.estimate_gas().await.context("estimate refund")?);
                let r = call.gas(gas).send().await?.get_receipt().await?;
                anyhow::ensure!(r.status(), "refund reverted");
                info!(order = %id, "refunded");
                rec.finish(MakerStatus::Refunded, None);
            }
            Step::Finish(status) => rec.finish(status, None),
            Step::Abandon(why) => {
                warn!(order = %id, "abandoned: {why}");
                rec.status = MakerStatus::Abandoned;
                rec.note = Some(why.to_owned());
                rec.forget_private();
            }
        }
        let mut book = self.book.lock().await;
        // The API only touches orders waiting for their destination, and only before the engine
        // would refund them: keep its destination if it arrived meanwhile.
        if let Some(current) = book.orders.get(&id)
            && current.status == MakerStatus::DestinationReceived
            && rec.status == MakerStatus::AwaitingDestination
        {
            return Ok(());
        }
        book.orders.insert(id, rec);
        book.save(&self.cfg.state)
    }

    /// Pays a Buy order: one note of its zatoshi to the taker's receiver, the swap memo,
    /// `ovk = None`; checked against the payment statement before it is broadcast, and recorded
    /// (opening, txid, the raw transaction) before the broadcast.
    async fn pay(&self, client: &mut lwd::LwdClient, rec: &mut OrderRecord) -> Result<()> {
        let receiver: [u8; 43] = hex::decode(rec.receiver.as_deref().context("no destination")?)?
            .try_into()
            .map_err(|_| anyhow!("stored receiver"))?;
        let salt: [u8; 32] = hex::decode(rec.salt.as_deref().context("no salt")?)?
            .try_into()
            .map_err(|_| anyhow!("stored salt"))?;
        let recipient = Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(&receiver))
            .context("receiver")?;
        let pk = self.proving_key().await?;
        let (tx, opening, target, nullifiers) = {
            let w = self.wallet.lock().await;
            let state = w.scan.as_ref().context("the wallet is not scanned yet")?;
            let unspent = state.unspent();
            let inputs = select_inputs(&unspent, rec.zatoshi)?;
            let nullifiers: Vec<String> = inputs.iter().map(|n| hex::encode(n.nullifier.to_bytes())).collect();
            let target = state.scanned_to + 1;
            let unsigned = build_payment(
                &self.network,
                BlockHeight::from_u32(target),
                &w.fvk,
                state.anchor(),
                &inputs,
                recipient,
                rec.zatoshi,
                rec.order_id,
                salt,
            )?;
            let opening = unsigned.opening.clone();
            let sk = w.sk.clone();
            let tx = tokio::task::block_in_place(|| authorize(unsigned, &sk, &pk))?;
            (tx, opening, target, nullifiers)
        };
        check_payment_note(&tx, &opening, rec.order_id)?;
        let raw = spend::serialize(&tx)?;
        rec.txid = Some(tx.txid().to_string());
        rec.opening = Some(opening);
        rec.raw_tx = Some(hex::encode(&raw));
        rec.expiry_height = Some(target + spend::PAYOUT_EXPIRY_DELTA);
        rec.input_nullifiers = nullifiers;
        rec.status = MakerStatus::Paid;
        // Recorded before the broadcast: a crash right after it cannot lose the opening.
        {
            let mut book = self.book.lock().await;
            book.orders.insert(rec.order_id, rec.clone());
            book.save(&self.cfg.state)?;
        }
        // The server's refusal text is its node's, and may name the transaction: at debug level only.
        // The payment is recorded; the next pass sends it again.
        spend::broadcast_raw(client, raw).await.map_err(|e| {
            debug!("payment broadcast: {e:#}");
            anyhow!("the light wallet server refused the payment (details at debug level); it is sent again")
        })?;
        info!(order = %rec.order_id, zatoshi = rec.zatoshi, "paid");
        Ok(())
    }

    /// Proves a mined payment under the header at exactly its anchor height (W-10) and submits the
    /// proof; false while that header has not the order's tier of work on top of it on the relay.
    async fn prove(&self, client: &mut lwd::LwdClient, rec: &mut OrderRecord) -> Result<bool> {
        let anchor = rec.anchor_height.context("no anchor height")?;
        let Some(anchor_hash) = anchor_at(&self.provider, self.cfg.swap, anchor, rec.zatoshi).await? else {
            return Ok(false);
        };
        let (headers, network) = (self.cfg.headers.clone(), self.zcash);
        let have = tokio::task::spawn_blocking(move || p2p::extend_file(network, &headers, &p2p::seed_peers(network)))
            .await??;
        anyhow::ensure!(have >= anchor, "the P2P peers are behind the relay");
        let txid = TxId::from_hex(rec.txid.as_deref().context("no txid")?).context("stored txid")?;
        let opening = rec.opening.clone().context("no opening")?;
        let built = payment::build(
            client,
            self.zcash,
            &self.cfg.headers,
            &self.cfg.summaries,
            txid,
            &opening,
            Some(rec.order_id.0),
            Some(rec.receiver_commitment.0),
            anchor,
        )
        .await
        .map_err(|e| {
            // W-10: the witness builder's errors may name the payment's block; that never goes next
            // to the order at the default level.
            debug!("payment witness: {e:#}");
            anyhow!("the payment witness could not be built (details at debug level)")
        })?;
        anyhow::ensure!(
            B256::from(built.journal.anchor) == anchor_hash,
            "the P2P headers' block at the anchor height is not the relay's"
        );
        let prover = self.prover.clone();
        let witness = built.witness;
        let seal = tokio::task::spawn_blocking(move || prover.seal(&witness)).await??;
        prove_settlement(&self.provider, self.cfg.swap, rec.order_id, &seal).await?;
        info!(order = %rec.order_id, anchor, "payment proven on Base");
        rec.status = MakerStatus::Proven;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::U256;

    use super::*;
    use crate::config::tests::judge_like;

    fn rec(side: Side, status: MakerStatus) -> OrderRecord {
        OrderRecord {
            order_id: B256::repeat_byte(1),
            side,
            taker: Address::repeat_byte(2),
            zatoshi: 50_000_000,
            usdc: U256::from(20_000_000u64),
            pay_deadline: 1_000_000 + 21_600,
            receiver_commitment: B256::repeat_byte(3),
            status,
            receiver: None,
            salt: None,
            pay_after_height: None,
            planned_anchor_height: None,
            anchor_draw: None,
            txid: None,
            opening: None,
            raw_tx: None,
            expiry_height: None,
            input_nullifiers: vec![],
            mined_height: None,
            anchor_height: None,
            note: None,
        }
    }

    fn facts(now: u64) -> Facts {
        Facts {
            now,
            zcash_tip: 4_500_000,
            status: OrderStatus::Open,
            pending: None,
            payment: None,
            inputs_unspent: false,
            other_payment_unmined: false,
            tip_epoch_last: None,
            tip_epoch_provable: true,
            payment_epoch_last: None,
            proofs: ProofPath {
                version_working: true,
                relay_lag: 1,
            },
        }
    }

    fn dest(now: u64) -> DestinationFacts {
        DestinationFacts {
            now,
            proofs: ProofPath {
                version_working: true,
                relay_lag: 1,
            },
            zcash_free: 2_000_000_000,
            taker_paying: 0,
        }
    }

    #[test]
    fn a_buy_order_from_destination_to_proof() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::AwaitingDestination);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "no destination yet");
        r.status = MakerStatus::DestinationReceived;
        // The tip is 4,500,000: block c = 4,500,001 is being mined. The anchor first (c + 8 + 1 + r),
        // then the payment's delay of 3 blocks: broadcast at 4,500,003, mined from 4,500,004 on.
        assert_eq!(
            decide(&cfg, &r, &facts(1_000_000), 3, 2),
            Step::SchedulePayment {
                pay_after: 4_500_003,
                planned_anchor: 4_500_012,
                draw: 2
            }
        );
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_012);
        r.anchor_draw = Some(2);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "the privacy delay");
        let mut f = facts(1_000_100);
        f.zcash_tip = 4_500_003;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Pay);
        f.other_payment_unmined = true;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "one unmined payment at a time");
        r.status = MakerStatus::Paid;
        r.expiry_height = Some(4_500_123);
        r.input_nullifiers = vec!["ab".repeat(32)];
        let mut f = facts(1_000_200);
        f.payment = Some(Seen::NotMined);
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Rebroadcast);
        f.zcash_tip = 4_500_124;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "expired, its inputs not shown unspent");
        f.inputs_unspent = true;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::PaymentExpired);
        f.inputs_unspent = false;
        f.zcash_tip = 4_500_010;
        f.payment = Some(Seen::Mined(4_500_005));
        assert_eq!(
            decide(&cfg, &r, &f, 3, 2),
            Step::ScheduleAnchor {
                mined: 4_500_005,
                anchor: 4_500_012
            }
        );
        r.mined_height = Some(4_500_005);
        r.anchor_height = Some(4_500_012);
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Prove);
        r.status = MakerStatus::Proven;
        f.pending = Some(Pending {
            stale: false,
            ready: false,
        });
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "K3");
        f.pending = Some(Pending {
            stale: false,
            ready: true,
        });
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Finalize);
        f.pending = Some(Pending {
            stale: true,
            ready: true,
        });
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Drop, "a fallen anchor or a removed version");
        f.pending = None;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Reprove, "someone dropped it");
        f.status = OrderStatus::Settled;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Finish(MakerStatus::Settled));
    }

    /// W-8: one transport error of the light wallet server after the payment was mined never makes
    /// the maker forget the payment and pay a second time. The error itself skips the order for the
    /// pass (`payment_seen` returns it); whatever the server says afterwards ("not mined": a
    /// reorganization, a server behind), a payment seen mined expires only when the wallet's scan,
    /// past its expiry height, shows its inputs unspent.
    #[test]
    fn w8_a_transport_error_after_the_payment_was_mined_never_expires_it() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.expiry_height = Some(4_500_123);
        r.planned_anchor_height = Some(4_500_014);
        r.anchor_draw = Some(4);
        r.mined_height = Some(4_500_005);
        r.anchor_height = Some(4_500_014);
        r.input_nullifiers = vec!["ab".repeat(32)];
        let mut f = facts(1_000_200);
        f.zcash_tip = 4_500_124;
        f.payment = Some(Seen::NotMined);
        assert_ne!(decide(&cfg, &r, &f, 3, 2), Step::PaymentExpired, "a payment seen mined never expires");
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait);
        f.inputs_unspent = true;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::PaymentExpired, "its inputs unspent past its expiry: never mined");
        f.zcash_tip = 4_500_123;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Rebroadcast, "not expired yet: sent again");
        // Seen mined again: proven as scheduled.
        f.payment = Some(Seen::Mined(4_500_005));
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Prove);
        // Moved by a reorganization: its anchor is scheduled again (the planned one still).
        f.payment = Some(Seen::Mined(4_500_006));
        assert_eq!(
            decide(&cfg, &r, &f, 3, 2),
            Step::ScheduleAnchor {
                mined: 4_500_006,
                anchor: 4_500_014
            }
        );
    }

    /// W-10: the proof's anchor height does not depend on the payment's block: every block the
    /// payment may land in below the planned anchor gives the order that same anchor; it is the
    /// payment's next block only when the payment came in the window's last block, and a late
    /// payment anchors `r` blocks above its next one.
    #[test]
    fn w10_the_anchor_height_does_not_depend_on_the_payment_height() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.expiry_height = Some(4_500_123);
        // Scheduled at tip 4,500,000 (c = 4,500,001) with r = 5: the anchor is c + 8 + 1 + 5.
        r.planned_anchor_height = Some(4_500_015);
        r.anchor_draw = Some(5);
        let anchor_for = |mined: u32| {
            let mut f = facts(1_000_200);
            f.zcash_tip = 4_500_030;
            f.payment = Some(Seen::Mined(mined));
            match decide(&cfg, &r, &f, 3, 0) {
                Step::ScheduleAnchor { anchor, .. } => anchor,
                s => panic!("{s:?}"),
            }
        };
        let anchors: Vec<u32> = (4_500_002..4_500_015).map(anchor_for).collect();
        assert!(anchors.iter().all(|a| *a == 4_500_015), "the anchor follows the payment: {anchors:?}");
        assert_eq!(anchor_for(4_500_014), 4_500_015, "h + 1 only for the window's last block");
        assert_eq!(anchor_for(4_500_015), 4_500_021, "late: h + 1 + r, with the order's r");
        assert_eq!(anchor_for(4_500_020), 4_500_026);
    }

    /// W-10: a payment that could no longer land below its planned anchor (held by another unmined
    /// payment, say) is scheduled again from the current height, with the order's draw; a fresh
    /// order takes the draw it is given.
    #[test]
    fn w10_a_payment_held_past_its_anchor_is_planned_again() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_012);
        r.anchor_draw = Some(2);
        let mut f = facts(1_000_100);
        f.zcash_tip = 4_500_010;
        assert_eq!(decide(&cfg, &r, &f, 4, 7), Step::Pay, "still lands below the anchor");
        f.zcash_tip = 4_500_011;
        assert_eq!(
            decide(&cfg, &r, &f, 4, 7),
            Step::SchedulePayment {
                pay_after: 4_500_015,
                planned_anchor: 4_500_023,
                draw: 2
            },
            "planned again from c = 4,500,012, the order's r kept"
        );
        let fresh = rec(Side::Buy, MakerStatus::DestinationReceived);
        assert!(matches!(decide(&cfg, &fresh, &f, 4, 7), Step::SchedulePayment { draw: 7, .. }));
    }

    /// W-20: the anchor never passes the last block of the payment's epoch (the statement needs the
    /// payment and its anchor in one epoch); a payment mined in that very block cannot be proven.
    #[test]
    fn w20_the_anchor_never_leaves_the_payments_epoch() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.planned_anchor_height = Some(4_500_015);
        r.anchor_draw = Some(5);
        let mut f = facts(1_000_200);
        f.zcash_tip = 4_500_030;
        f.payment = Some(Seen::Mined(4_500_008));
        f.payment_epoch_last = Some(4_500_010);
        assert_eq!(
            decide(&cfg, &r, &f, 3, 0),
            Step::ScheduleAnchor {
                mined: 4_500_008,
                anchor: 4_500_010
            },
            "cut at the epoch's last block"
        );
        f.payment = Some(Seen::Mined(4_500_010));
        assert!(matches!(decide(&cfg, &r, &f, 3, 0), Step::Abandon(_)), "the epoch's last block: no anchor");
    }

    /// W-20: no payment while its anchor window and the order's tier of work would not fit before
    /// a known activation, nor in an epoch the payment statement does not prove on.
    #[test]
    fn w20_no_payment_near_a_known_activation() {
        let cfg = judge_like();
        // From the tip: 18 blocks of anchor window, 16 of work (1,200 s in 75-s blocks).
        assert_eq!(cfg.anchor_pipeline_blocks(), 18 + 16);
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        let mut f = facts(1_000_000);
        f.tip_epoch_last = Some(4_500_000 + 34 - 1);
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "the activation is too close");
        f.tip_epoch_last = Some(4_500_000 + 34);
        assert!(matches!(decide(&cfg, &r, &f, 3, 2), Step::SchedulePayment { .. }));
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_012);
        r.anchor_draw = Some(2);
        f.zcash_tip = 4_500_003;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "scheduled, but the activation came closer");
        f.tip_epoch_last = None;
        f.tip_epoch_provable = false;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "an epoch the statement does not prove on");
        f.tip_epoch_provable = true;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Pay);
    }

    /// The hash the relay stub gives the header at `height`.
    fn header_hash(height: u32) -> B256 {
        let mut h = [0xaa; 32];
        h[28..].copy_from_slice(&height.to_be_bytes());
        B256::from(h)
    }

    /// A Base RPC stub of ZecuredSwap and its relay, by function selector: the relay's best chain
    /// ends at `tip`, each header has one unit of work, and the order's tier asks for `min`.
    async fn relay_stub(tip: u32, min: u64) -> String {
        use alloy::sol_types::SolCall;
        use axum::{Json, Router, routing::post};
        use zecured_base::events::IZcashHeaderRelay;
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| async move {
                let call = &req["params"][0];
                let data = hex::decode(
                    call["input"].as_str().or(call["data"].as_str()).unwrap_or_default().trim_start_matches("0x"),
                )
                .unwrap_or_default();
                let (selector, arg) = (data.get(..4).unwrap_or_default(), data.get(4..36).unwrap_or_default());
                let word = |v: U256| v.to_be_bytes::<32>().to_vec();
                let height_of = |hash: &[u8]| u32::from_be_bytes(hash[28..32].try_into().unwrap());
                let out: Vec<u8> = if selector == IZecuredSwap::relayCall::SELECTOR {
                    word(U256::from_be_slice(Address::repeat_byte(0x77).as_slice()))
                } else if selector == IZecuredSwap::minConfirmationWorkForCall::SELECTOR {
                    word(U256::from(min))
                } else if selector == IZcashHeaderRelay::bestChainTipCall::SELECTOR {
                    [header_hash(tip).to_vec(), word(U256::from(tip)), word(U256::from(tip))].concat()
                } else if selector == IZcashHeaderRelay::bestBlockHashAtCall::SELECTOR {
                    let h = u32::try_from(U256::from_be_slice(arg)).unwrap();
                    if h <= tip { header_hash(h).to_vec() } else { vec![0; 32] }
                } else if selector == IZcashHeaderRelay::confirmationWorkCall::SELECTOR {
                    word(U256::from(tip - height_of(arg)))
                } else {
                    vec![]
                };
                Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": format!("0x{}", hex::encode(out)) }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// W-10: the proof anchors at exactly the planned header, once the order's tier of work is on
    /// top of it: never at the highest header that first has that work.
    #[tokio::test]
    async fn w10_the_proof_anchors_at_exactly_the_planned_height() {
        use alloy::providers::ProviderBuilder;
        let (planned, tip) = (4_500_020u32, 4_500_040u32);
        let rpc = relay_stub(tip, 3).await;
        let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap());
        let swap = Address::repeat_byte(0x11);
        let got = anchor_at(&provider, swap, planned, 50_000_000).await.unwrap();
        assert_eq!(
            got,
            Some(header_hash(planned)),
            "the anchor is the planned header, not the highest one with the work"
        );
        assert_eq!(anchor_at(&provider, swap, tip - 2, 50_000_000).await.unwrap(), None, "not the tier's work yet");
        assert_eq!(anchor_at(&provider, swap, tip - 3, 50_000_000).await.unwrap(), Some(header_hash(tip - 3)));
        assert_eq!(anchor_at(&provider, swap, tip + 1, 50_000_000).await.unwrap(), None, "above the relay's tip");
    }

    /// W-8: what the wallet's scan shows of an expired payment's inputs.
    #[test]
    fn w8_the_inputs_of_an_expired_payment() {
        let (a, b) = ([0xab; 32], [0xcd; 32]);
        let inputs = vec![hex::encode(a), hex::encode(b)];
        let notes = |a_spent, b_spent| vec![(a, a_spent), (b, b_spent), ([0xee; 32], true)];
        assert!(inputs_unspent(124, 123, &inputs, notes(false, false)));
        assert!(!inputs_unspent(123, 123, &inputs, notes(false, false)), "the scan must pass the expiry height");
        assert!(!inputs_unspent(124, 123, &inputs, notes(false, true)), "an input spent: by the payment or another");
        assert!(!inputs_unspent(124, 123, &inputs, vec![(a, false)]), "an input the wallet does not know");
        assert!(!inputs_unspent(124, 123, &[], notes(false, false)), "no recorded inputs: nothing shown");
    }

    /// The maker never pays when the pipeline would not fit before the deadline (docs/SWAP.md "The market maker").
    #[test]
    fn too_late_to_pay_or_prove() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        let late = r.pay_deadline - cfg.required_secs() + 1;
        assert!(matches!(decide(&cfg, &r, &facts(late), 3, 2), Step::Abandon(_)));
        r.status = MakerStatus::Paid;
        assert!(matches!(decide(&cfg, &r, &facts(r.pay_deadline + 1), 3, 2), Step::Abandon(_)));
        r.status = MakerStatus::Abandoned;
        assert_eq!(decide(&cfg, &r, &facts(r.pay_deadline), 3, 2), Step::Wait);
        assert_eq!(decide(&cfg, &r, &facts(r.pay_deadline + 1), 3, 2), Step::Refund, "the taker's USDC back");
    }

    #[test]
    fn a_sell_order() {
        let cfg = judge_like();
        let r = rec(Side::Sell, MakerStatus::TakerPays);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "the taker pays and proves");
        let mut f = facts(1_000_000);
        f.pending = Some(Pending {
            stale: false,
            ready: true,
        });
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Finalize, "the maker finalizes a ready settlement");
        assert_eq!(decide(&cfg, &r, &facts(r.pay_deadline + 1), 3, 2), Step::Refund, "unproven: the maker's USDC back");
        let mut f = facts(r.pay_deadline + 1);
        f.pending = Some(Pending {
            stale: false,
            ready: false,
        });
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "no refund while a proof stands");
        f.status = OrderStatus::Refunded;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Finish(MakerStatus::Refunded));
    }

    /// A Buy order awaiting the destination `(ua, salt)`, and the latest time it is taken.
    fn awaiting(cfg: &MakerConfig) -> (OrderRecord, String, [u8; 32], u64) {
        use orchard::keys::{Scope, SpendingKey};
        let sk = (0u8..=255).find_map(|i| Option::<SpendingKey>::from(SpendingKey::from_bytes([i; 32]))).unwrap();
        let addr = FullViewingKey::from(&sk).address_at(5u32, Scope::External);
        let ua = zecured_zcash_ops::keys::encode_ironwood_ua(&Network::TestNetwork, addr).unwrap();
        let salt = [0x11; 32];
        let mut r = rec(Side::Buy, MakerStatus::AwaitingDestination);
        r.receiver_commitment = zecured_swap::receiver_commitment(&salt, &addr.to_raw_address_bytes());
        let now = r.pay_deadline - cfg.required_secs();
        (r, ua, salt, now)
    }

    #[test]
    fn destinations() {
        let cfg = judge_like();
        let (r, ua, salt, now) = awaiting(&cfg);
        let d = accept_destination(&cfg, &Network::TestNetwork, &r, &dest(now), &ua, &hex::encode(salt)).unwrap();
        assert_eq!(zecured_swap::receiver_commitment(&d.salt, &d.receiver), r.receiver_commitment);
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &dest(now + 1), &ua, &hex::encode(salt)).err(),
            Some(ApiError::TOO_LATE)
        );
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &dest(now), &ua, &hex::encode([0x12; 32])).err(),
            Some(ApiError::BAD_DESTINATION)
        );
        let sell = rec(Side::Sell, MakerStatus::TakerPays);
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &sell, &dest(now), &ua, &hex::encode(salt)).err(),
            Some(ApiError::NOT_BUY)
        );
        let mut paid = r.clone();
        paid.status = MakerStatus::Paid;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &paid, &dest(now), &ua, &hex::encode(salt)).err(),
            Some(ApiError::NOT_AWAITING)
        );
    }

    /// W-7: the maker takes no destination for a payment whose proof could not land: its Swap
    /// version removed or its router's verifier stopped, or the relay behind. The same destination
    /// given again once taken is still acknowledged.
    #[test]
    fn w7_refuses_a_destination_when_the_router_is_stopped() {
        let cfg = judge_like();
        let (mut r, ua, salt, now) = awaiting(&cfg);
        let mut d = dest(now);
        d.proofs.version_working = false;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::PROOFS_STOPPED)
        );
        d.proofs = ProofPath {
            version_working: true,
            relay_lag: 6,
        };
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::RELAY_BEHIND)
        );
        r.status = MakerStatus::DestinationReceived;
        assert!(accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).is_ok());
    }

    /// W-19: a first destination the maker could not pay now is refused (its order reserves the ZEC
    /// from then on), and so is one of a taker whose other orders in payment reach its cap.
    #[test]
    fn w19_a_destination_the_maker_could_not_pay_is_refused() {
        let cfg = judge_like();
        let (r, ua, salt, now) = awaiting(&cfg);
        let mut d = dest(now);
        d.zcash_free = r.zatoshi + PAYMENT_FEE_HEADROOM - 1;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::NO_INVENTORY)
        );
        d.zcash_free = r.zatoshi + PAYMENT_FEE_HEADROOM;
        assert!(accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).is_ok());
        d.taker_paying = 3;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::TAKER_BUSY)
        );
    }

    /// W-7: the maker never pays while no proof could land (its Swap version removed or its
    /// router's verifier stopped, or the relay behind), and starts no proving run while the
    /// version is not working.
    #[test]
    fn w7_refuses_to_pay_when_the_version_is_not_working() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_012);
        r.anchor_draw = Some(2);
        let mut f = facts(1_000_100);
        f.zcash_tip = 4_500_003;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Pay);
        f.proofs.version_working = false;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "a removed version or a stopped router");
        f.proofs = ProofPath {
            version_working: true,
            relay_lag: 6,
        };
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "the relay lags");
        r.status = MakerStatus::Paid;
        r.expiry_height = Some(4_500_123);
        r.mined_height = Some(4_500_005);
        r.anchor_height = Some(4_500_012);
        let mut f = facts(1_000_200);
        f.payment = Some(Seen::Mined(4_500_005));
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Prove);
        f.proofs.version_working = false;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "no proving run that could not land");
    }
}
