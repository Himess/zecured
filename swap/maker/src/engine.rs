//! The order engine: follows the maker's orders on Base and moves each one along. Buy: wait for
//! the taker's destination, fix the height of the proof's anchor and pay after a random privacy
//! delay (W-10, H-05; `zecured_swap::anchor`), wait for the payment's block and for the order's
//! tier of work on top of the header at exactly that height, build the payment witness, prove it,
//! submit the proof, finalize once ready. Sell: the taker pays and proves; the maker finalizes a
//! ready settlement and refunds itself after the deadline. Every decision is the pure function
//! [`decide`] of an order and the facts read for it. N-4: a payment, once built, is decided again
//! on facts read right before its broadcast, and is sent only if that is still a payment; a proof
//! runs in a task of its own, so a pass never waits for one.

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
    /// W-7: whether a proof could land (read once per pass, and again right before a broadcast).
    pub proofs: ProofPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub stale: bool,
    pub ready: bool,
}

/// What both chains say at one moment, as a decision needs it: read once per pass, and again
/// right before a payment's broadcast (N-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moment {
    /// Base's clock (the latest block's timestamp).
    pub now: u64,
    /// The light wallet server's Zcash tip, and the relay's.
    pub zcash_tip: u32,
    pub relay_height: u64,
    /// W-7: whether a proof could land.
    pub proofs: ProofPath,
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
    /// late payment's), and a late payment's own `r` (N-12), kept once drawn.
    ScheduleAnchor {
        mined: u32,
        anchor: u32,
        late_draw: Option<u32>,
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
/// `pay_delay_max_blocks`, an anchor's `r`) are the random draws (`zecured_swap::anchor::draw`),
/// made by the caller; an order keeps the first `draw` it is scheduled with, and a late payment
/// the first `draw` its late anchor takes (N-12).
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
    let order_draw = rec.anchor_draw.unwrap_or(draw);
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
                        planned_anchor: anchor::planned_anchor(c, cfg.pay_delay_max_blocks, order_draw),
                        draw: order_draw,
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
                    // W-10: the planned anchor whatever the payment's block, unless it came late (or
                    // was paid before anchors were planned): then `h + 1 + r` with an `r` of its own
                    // (N-12), never the order's, which would tie the late anchor to the plan. W-20:
                    // never past the last block of the payment's epoch.
                    _ => {
                        let late = !rec.planned_anchor_height.is_some_and(|p| p > mined);
                        let late_draw = late.then(|| rec.late_anchor_draw.unwrap_or(draw));
                        // On time, the planned anchor: `r` is not used.
                        let r = late_draw.unwrap_or_default();
                        match anchor::anchor_height(rec.planned_anchor_height, r, mined, f.payment_epoch_last) {
                            Some(anchor) => Step::ScheduleAnchor {
                                mined,
                                anchor,
                                late_draw,
                            },
                            None => Step::Abandon("paid, but its proof cannot land: the order will be refunded"),
                        }
                    }
                },
                // W-8: not mined as far as the server says, which proves nothing (a reorganization, a
                // server behind or another one): before its expiry height the payment is sent again;
                // after it, it never happened only if the wallet shows its inputs unspent, and (N-12)
                // only once the tip is EXPIRY_MARGIN_BLOCKS past it. Otherwise the maker waits, and
                // never pays the order twice.
                _ => match rec.expiry_height {
                    Some(e) if f.zcash_tip > e => {
                        if f.inputs_unspent && f.zcash_tip > e.saturating_add(EXPIRY_MARGIN_BLOCKS) {
                            Step::PaymentExpired
                        } else {
                            Step::Wait
                        }
                    }
                    _ => Step::Rebroadcast,
                },
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

/// N-12 (W-8): blocks past its expiry height before an unmined payment counts as expired: a short
/// reorganization, or a light wallet server a block or two behind, could still show it mined at or
/// below that height.
pub const EXPIRY_MARGIN_BLOCKS: u32 = 3;

/// W-8: whether a wallet scan that covers blocks up to `scanned_to` shows every input of a payment
/// with expiry height `expiry` unspent: `inputs` are the payment's input nullifiers (hex) and
/// `notes` the wallet's notes (nullifier, spent). Then the payment was never mined, and never will
/// be: no block above its expiry height may hold it. The scan must reach `EXPIRY_MARGIN_BLOCKS`
/// past that height (N-12). A payment without recorded inputs shows nothing.
pub fn inputs_unspent(
    scanned_to: u32,
    expiry: u32,
    inputs: &[String],
    notes: impl IntoIterator<Item = ([u8; 32], bool)>,
) -> bool {
    if scanned_to <= expiry.saturating_add(EXPIRY_MARGIN_BLOCKS) || inputs.is_empty() {
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
    /// W-19: the zatoshi the maker can still pay this order: its scanned balance less what other
    /// orders and quotes hold (N-10: this order's own reservation back in).
    pub zcash_free: u64,
    /// N-10: whether this order holds one of the maker's places (its reservation lasts), and the
    /// places taken by the others.
    pub holds_place: bool,
    pub places_taken: usize,
    /// W-19, N-10: the places of the order's taker on Base, this order's aside.
    pub taker_places: usize,
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
        // W-19: from here on the order holds its ZEC: the maker must have it (N-10: its reservation,
        // while it lasts, kept it for it), and one taker's places stay within its cap. N-10: an
        // order that no longer holds a place (its reservation lapsed, or none was given) takes one
        // again only if there is one.
        if d.zcash_free < rec.zatoshi.saturating_add(PAYMENT_FEE_HEADROOM) {
            return Err(ApiError::NO_INVENTORY);
        }
        if !d.holds_place && d.places_taken >= cfg.max_open_orders {
            return Err(ApiError::BUSY);
        }
        if d.taker_places >= cfg.max_open_orders_per_taker {
            return Err(ApiError::TAKER_BUSY);
        }
    }
    Ok(destination)
}

/// N-8: a destination given again for `rec` once the maker took one (the order waits for its
/// payment, or is paid or proven): answered from the book alone, `Ok` when it opens the order's
/// commitment (only the destination taken does). `None` while `rec` has taken none, or is done.
pub fn destination_again(
    network: &Network,
    rec: &OrderRecord,
    receiver: &str,
    salt: &str,
) -> Option<Result<(), ApiError>> {
    let taken = matches!(rec.status, MakerStatus::DestinationReceived | MakerStatus::Paid | MakerStatus::Proven);
    (rec.side == Side::Buy && taken).then(|| {
        check_destination(network, receiver, salt, rec.receiver_commitment)
            .map(|_| ())
            .map_err(|_| ApiError::BAD_DESTINATION)
    })
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
    /// The last pass's Zcash tip, relay tip and when it read them (Unix seconds), and Base's clock
    /// then.
    zcash_tip: AtomicU64,
    relay_tip: AtomicU64,
    pass_at: AtomicU64,
    pass_base_now: AtomicU64,
    pub prover: Prover,
    /// W-7: the Swap version of the prover's guest and selector: the maker quotes, takes
    /// destinations, pays and proves only while it is working.
    pub version_id: B256,
    proving_key: tokio::sync::OnceCell<Arc<ProvingKey>>,
    /// N-4: the order whose payment is being proven, off the pass (one proof at a time).
    proving: std::sync::Mutex<Option<B256>>,
    /// Held while one of the maker's Base transactions is sent and mined: the pass and the proving
    /// task send from one key.
    base_tx: Mutex<()>,
}

/// N-4: frees the proving slot when the proving task ends, however it ends.
struct ProvingSlot<'a>(&'a std::sync::Mutex<Option<B256>>);

impl Drop for ProvingSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut proving) = self.0.lock() {
            *proving = None;
        }
    }
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
        reserved_until: None,
        receiver: None,
        salt: None,
        pay_after_height: None,
        planned_anchor_height: None,
        anchor_draw: None,
        late_anchor_draw: None,
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
            pass_base_now: AtomicU64::new(0),
            prover,
            version_id,
            proving_key: tokio::sync::OnceCell::new(),
            proving: std::sync::Mutex::new(None),
            base_tx: Mutex::new(()),
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

    /// Base's clock at the last pass.
    pub fn last_pass_base_now(&self) -> u64 {
        self.pass_base_now.load(Ordering::Relaxed)
    }

    /// W-7: whether a payment made now could be proven, as the API decides a quote or a
    /// destination: the version is read now, the relay's lag is the last pass's, which must be
    /// recent (the maker follows both chains).
    pub async fn proof_path(&self) -> Result<ProofPath, ApiError> {
        let (zcash_tip, relay_height, pass_at) = self.last_pass();
        if unix_now().saturating_sub(pass_at) > self.cfg.pass_stale_secs() {
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

    /// One pass over every open order. It never waits for a proof (N-4: proofs run in a task of
    /// their own), so its view of the chains, which the API decides on, stays recent.
    pub async fn tick(self: &Arc<Self>) -> Result<()> {
        let mut client = lwd::connect(&self.cfg.lwd).await?;
        lwd::check_network(&mut client, self.zcash).await?;
        let zcash_tip = lwd_tip(&mut client).await?;
        self.scan_wallet(&mut client, zcash_tip).await?;
        self.sync_orders().await?;
        // W-7: whether a proof could land, read once per pass before any payment of the pass, and
        // again right before each broadcast (N-4).
        let m = self.moment(zcash_tip).await?;
        self.zcash_tip.store(u64::from(zcash_tip), Ordering::Relaxed);
        self.relay_tip.store(m.relay_height, Ordering::Relaxed);
        self.pass_at.store(unix_now(), Ordering::Relaxed);
        self.pass_base_now.store(m.now, Ordering::Relaxed);
        // N-10: the quotes that hold no place any more are forgotten.
        self.book.lock().await.prune_quotes(m.now);
        let ids: Vec<B256> = self.book.lock().await.open_orders().map(|o| o.order_id).collect();
        for id in ids {
            if let Err(e) = self.advance(&mut client, id, &m).await {
                warn!(order = %id, "order step failed: {e:#}");
            }
        }
        Ok(())
    }

    /// What Base says now, the Zcash tip being `zcash_tip`: its clock, the relay's tip, and whether
    /// a proof of the maker's Swap version could land.
    async fn moment(&self, zcash_tip: u32) -> Result<Moment> {
        let now = self.now().await?;
        let relay_height = relay_tip(&self.provider, self.cfg.swap).await?;
        let proofs = ProofPath {
            version_working: swap_version_working(&self.provider, self.cfg.swap, self.version_id).await?,
            relay_lag: u64::from(zcash_tip).saturating_sub(relay_height),
        };
        Ok(Moment {
            now,
            zcash_tip,
            relay_height,
            proofs,
        })
    }

    /// N-4: the step [`decide`] takes for `rec` on facts read now, the Zcash tip being `zcash_tip`:
    /// a built payment is broadcast only while this is still [`Step::Pay`].
    pub async fn decide_now(&self, rec: &OrderRecord, zcash_tip: u32) -> Result<Step> {
        let m = self.moment(zcash_tip).await?;
        let f = self.facts(rec, &m, None).await?;
        // The draws only shape a step that is not a payment, which is not taken from here.
        Ok(decide(&self.cfg, rec, &f, 1, 0))
    }

    /// N-4: whether the payment of order `id` is being proven now.
    pub fn proving_now(&self, id: B256) -> bool {
        self.proving.lock().is_ok_and(|p| *p == Some(id))
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
        // N-10: a new Buy order's reservation runs on Base's clock, read once there is one.
        let now = if logs.is_empty() { 0 } else { self.now().await? };
        let balance = self.balance.load(Ordering::Relaxed);
        let mut book = self.book.lock().await;
        for log in logs.iter().filter(|l| !l.removed) {
            let ev = IZecuredSwap::OrderOpened::decode_log(&log.inner).context("OrderOpened")?;
            if ev.maker != me || book.orders.contains_key(&ev.orderId) {
                continue;
            }
            let side = Side::from_u8(ev.side)?;
            info!(order = %ev.orderId, ?side, zatoshi = ev.zatoshi, "order opened");
            let rec =
                new_record(ev.orderId, side, ev.taker, ev.zatoshi, ev.usdc, ev.payDeadline, ev.receiverCommitment);
            book.record_opened(rec, now, balance, &self.cfg);
        }
        book.synced_to = head;
        book.save(&self.cfg.state)
    }

    /// The facts about `rec` at the moment `m` (Base reads; `payment`, what the light wallet server
    /// says of its payment, is the caller's to read).
    async fn facts(&self, rec: &OrderRecord, m: &Moment, payment: Option<Seen>) -> Result<Facts> {
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
                ready: zecured_swap::chain::settlement_ready(&p, m.relay_height, m.now),
            })
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
            now: m.now,
            zcash_tip: m.zcash_tip,
            status,
            pending,
            payment,
            inputs_unspent,
            other_payment_unmined,
            tip_epoch_last: anchor::epoch_last_height(self.zcash, m.zcash_tip),
            tip_epoch_provable: anchor::provable_epoch(self.zcash, m.zcash_tip),
            payment_epoch_last: match payment {
                Some(Seen::Mined(h)) => anchor::epoch_last_height(self.zcash, h),
                _ => None,
            },
            proofs: m.proofs,
        })
    }

    async fn advance(self: &Arc<Self>, client: &mut lwd::LwdClient, id: B256, m: &Moment) -> Result<()> {
        // N-4: an order whose payment is being proven is the proving task's until it ends (it
        // writes the order before it ends, so the record read below is never older than its own).
        if self.proving_now(id) {
            return Ok(());
        }
        let Some(rec) = self.book.lock().await.orders.get(&id).cloned() else {
            return Ok(());
        };
        // W-8: a transport error is no answer: `?` skips the order for this pass, so a payment is
        // never taken for unmined because the server could not be reached.
        let payment = match (rec.status, rec.txid.as_deref()) {
            (MakerStatus::Paid, Some(txid)) => {
                Some(payment_seen(client, &TxId::from_hex(txid).context("stored txid")?).await?)
            }
            _ => None,
        };
        let f = self.facts(&rec, m, payment).await?;
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
                rec.late_anchor_draw = None;
            }
            Step::ScheduleAnchor {
                mined,
                anchor,
                late_draw,
            } => {
                rec.mined_height = Some(mined);
                rec.anchor_height = Some(anchor);
                // N-12: a late payment keeps the draw its anchor took (a reorganization may move the
                // payment again).
                if let Some(r) = late_draw {
                    rec.late_anchor_draw.get_or_insert(r);
                }
            }
            Step::Prove => {
                // N-4: off the pass; the order is marked proven once its proof is on Base.
                self.start_proving(rec);
                return Ok(());
            }
            Step::Reprove => {
                rec.status = MakerStatus::Paid;
            }
            Step::Drop => {
                let _tx = self.base_tx.lock().await;
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
                let _tx = self.base_tx.lock().await;
                finalize_settlement(&self.provider, self.cfg.swap, id).await?;
                info!(order = %id, "settled");
                rec.finish(MakerStatus::Settled, None);
            }
            Step::Refund => {
                let _tx = self.base_tx.lock().await;
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
    /// `ovk = None`; checked against the payment statement before it is broadcast, decided again on
    /// facts read right before the broadcast (N-4), and recorded (opening, txid, the raw
    /// transaction) before the broadcast.
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
        // N-4: the decision to pay was taken on the pass's facts, and building the payment took
        // time: the tip, Base's clock, the relay and the Swap version are read again, and the
        // payment goes out only while `decide` still pays (it still lands below its planned anchor,
        // a proof could still land, there is still time). Otherwise it is dropped unsent, and the
        // next pass decides afresh (it waits, plans the anchor again or abandons the order).
        let tip = lwd_tip(client).await?;
        if self.decide_now(rec, tip).await? != Step::Pay {
            info!(order = %rec.order_id, "the payment was not sent: the chains moved on while it was built");
            return Ok(());
        }
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

    /// N-4: proves `rec`'s payment in a task of its own, so the pass goes on with the other orders
    /// (a payment one of them makes meanwhile is decided on what the chains say then) and the
    /// maker's view of the chains, which the API decides on, stays recent. One proof at a time:
    /// another order's waits for a later pass.
    fn start_proving(self: &Arc<Self>, rec: OrderRecord) {
        {
            let Ok(mut proving) = self.proving.lock() else {
                return;
            };
            if proving.is_some() {
                return;
            }
            *proving = Some(rec.order_id);
        }
        let maker = self.clone();
        tokio::spawn(async move {
            let _slot = ProvingSlot(&maker.proving);
            let id = rec.order_id;
            if let Err(e) = maker.prove_and_record(rec).await {
                warn!(order = %id, "proof failed: {e:#}");
            }
        });
    }

    /// The proving task: proves the payment and, once the proof is on Base, marks the order proven
    /// (still paid with the same payment: the pass leaves an order being proven alone).
    async fn prove_and_record(&self, mut rec: OrderRecord) -> Result<()> {
        if !self.prove(&mut rec).await? {
            return Ok(());
        }
        let mut book = self.book.lock().await;
        if let Some(current) = book.orders.get_mut(&rec.order_id)
            && current.status == MakerStatus::Paid
            && current.txid == rec.txid
        {
            current.status = MakerStatus::Proven;
            book.save(&self.cfg.state)?;
        }
        Ok(())
    }

    /// Proves a mined payment under the header at exactly its anchor height (W-10) and submits the
    /// proof; false while that header has not the order's tier of work on top of it on the relay.
    async fn prove(&self, rec: &mut OrderRecord) -> Result<bool> {
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
        let mut client = lwd::connect(&self.cfg.lwd).await?;
        lwd::check_network(&mut client, self.zcash).await?;
        let built = payment::build(
            &mut client,
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
        {
            let _tx = self.base_tx.lock().await;
            prove_settlement(&self.provider, self.cfg.swap, rec.order_id, &seal).await?;
        }
        info!(order = %rec.order_id, anchor, "payment proven on Base");
        rec.status = MakerStatus::Proven;
        Ok(true)
    }
}

/// The light wallet server's Zcash tip.
async fn lwd_tip(client: &mut lwd::LwdClient) -> Result<u32> {
    Ok(u32::try_from(lwd::latest_block(client).await?.height)?)
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
            reserved_until: None,
            receiver: None,
            salt: None,
            pay_after_height: None,
            planned_anchor_height: None,
            anchor_draw: None,
            late_anchor_draw: None,
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

    /// An order holding its place (its reservation lasts), the maker otherwise idle.
    fn dest(now: u64) -> DestinationFacts {
        DestinationFacts {
            now,
            proofs: ProofPath {
                version_working: true,
                relay_lag: 1,
            },
            zcash_free: 2_000_000_000,
            holds_place: true,
            places_taken: 0,
            taker_places: 0,
        }
    }

    #[test]
    fn a_buy_order_from_destination_to_proof() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::AwaitingDestination);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "no destination yet");
        r.status = MakerStatus::DestinationReceived;
        // The tip is 4,500,000: block c = 4,500,001 is being mined. The anchor first (c + 8 + 1 + 2 +
        // r: two blocks of headroom, N-4), then the payment's delay of 3 blocks: broadcast at
        // 4,500,003, mined from 4,500,004 on.
        assert_eq!(
            decide(&cfg, &r, &facts(1_000_000), 3, 2),
            Step::SchedulePayment {
                pay_after: 4_500_003,
                planned_anchor: 4_500_014,
                draw: 2
            }
        );
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_014);
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
        f.zcash_tip = 4_500_127;
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
                anchor: 4_500_014,
                late_draw: None
            }
        );
        r.mined_height = Some(4_500_005);
        r.anchor_height = Some(4_500_014);
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
        f.zcash_tip = 4_500_127;
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
                anchor: 4_500_014,
                late_draw: None
            }
        );
    }

    /// W-10: the proof's anchor height does not depend on the payment's block: every block the
    /// payment may land in below the planned anchor gives the order that same anchor; it is the
    /// payment's next block only when the payment came in the window's last block, and a late
    /// payment anchors its own `r` (N-12) blocks above its next one.
    #[test]
    fn w10_the_anchor_height_does_not_depend_on_the_payment_height() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.expiry_height = Some(4_500_123);
        // Scheduled at tip 4,500,000 (c = 4,500,001) with r = 5: the anchor is c + 8 + 1 + 2 + 5.
        r.planned_anchor_height = Some(4_500_017);
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
        let anchors: Vec<u32> = (4_500_002..4_500_017).map(anchor_for).collect();
        assert!(anchors.iter().all(|a| *a == 4_500_017), "the anchor follows the payment: {anchors:?}");
        assert_eq!(anchor_for(4_500_016), 4_500_017, "h + 1 only for the window's last block");
        assert_eq!(anchor_for(4_500_017), 4_500_018, "late: h + 1 + its own r (the draw given, 0)");
        assert_eq!(anchor_for(4_500_022), 4_500_023);
    }

    /// N-12: a late payment's anchor (`h + 1 + r`) takes an `r` of its own, the draw the engine gives
    /// when the payment is first seen late, never the order's: that one would tie the late anchor to
    /// the plan and narrow the blocks the payment hides among.
    #[test]
    fn n12_a_late_payment_anchors_with_an_r_of_its_own() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.expiry_height = Some(4_500_123);
        r.planned_anchor_height = Some(4_500_017);
        r.anchor_draw = Some(5);
        let mut f = facts(1_000_200);
        f.zcash_tip = 4_500_030;
        f.payment = Some(Seen::Mined(4_500_017));
        let step = decide(&cfg, &r, &f, 3, 0);
        assert!(matches!(step, Step::ScheduleAnchor { anchor: 4_500_018, .. }), "h + 1 + 0, not + 5: {step:?}");
        // The engine keeps that draw: moved by a reorganization, the payment keeps it.
        assert_eq!(
            step,
            Step::ScheduleAnchor {
                mined: 4_500_017,
                anchor: 4_500_018,
                late_draw: Some(0)
            }
        );
        r.late_anchor_draw = Some(3);
        f.payment = Some(Seen::Mined(4_500_019));
        assert!(matches!(decide(&cfg, &r, &f, 3, 7), Step::ScheduleAnchor { anchor: 4_500_023, .. }), "kept");
        // On time, no draw of its own: the planned anchor.
        f.payment = Some(Seen::Mined(4_500_009));
        assert!(matches!(
            decide(&cfg, &r, &f, 3, 7),
            Step::ScheduleAnchor {
                anchor: 4_500_017,
                late_draw: None,
                ..
            }
        ));
    }

    /// W-10: a payment that could no longer land below its planned anchor (held by another unmined
    /// payment, say) is scheduled again from the current height, with the order's draw; a fresh
    /// order takes the draw it is given.
    #[test]
    fn w10_a_payment_held_past_its_anchor_is_planned_again() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_014);
        r.anchor_draw = Some(2);
        let mut f = facts(1_000_100);
        f.zcash_tip = 4_500_012;
        assert_eq!(decide(&cfg, &r, &f, 4, 7), Step::Pay, "still lands below the anchor");
        f.zcash_tip = 4_500_013;
        assert_eq!(
            decide(&cfg, &r, &f, 4, 7),
            Step::SchedulePayment {
                pay_after: 4_500_017,
                planned_anchor: 4_500_027,
                draw: 2
            },
            "planned again from c = 4,500,014, the order's r kept"
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
                anchor: 4_500_010,
                late_draw: None
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
        // From the tip: 20 blocks of anchor window (two of headroom), 16 of work (1,200 s in 75-s
        // blocks).
        assert_eq!(cfg.anchor_pipeline_blocks(), 20 + 16);
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        let mut f = facts(1_000_000);
        f.tip_epoch_last = Some(4_500_000 + 36 - 1);
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "the activation is too close");
        f.tip_epoch_last = Some(4_500_000 + 36);
        assert!(matches!(decide(&cfg, &r, &f, 3, 2), Step::SchedulePayment { .. }));
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_014);
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

    /// N-4: a payment is decided again on what the chains say right before its broadcast. Another
    /// order's proof runs (off the pass) while the Swap version is removed: the payment the pass
    /// decided on its earlier facts is not sent. Neither is one whose tip passed its plan while it
    /// was built.
    #[tokio::test]
    async fn n4_no_payment_while_the_version_was_removed_during_another_proof() {
        use crate::testkit::{Chain, base_stub, maker, open_order};
        let chain = std::sync::Arc::new(std::sync::Mutex::new(Chain {
            now: 1_000_100,
            relay_height: 4_500_002,
            ..Chain::default()
        }));
        let maker = maker(&base_stub(chain.clone()).await, judge_like(), Book::default());
        let me = maker.signer.address();
        // Order B: scheduled at tip 4,500,000 and due; order A: paid, mined, its anchor scheduled.
        let mut b = rec(Side::Buy, MakerStatus::DestinationReceived);
        b.pay_after_height = Some(4_500_003);
        b.planned_anchor_height = Some(4_500_014);
        b.anchor_draw = Some(2);
        let mut a = rec(Side::Buy, MakerStatus::Paid);
        a.order_id = B256::repeat_byte(9);
        a.txid = Some("cd".repeat(32));
        a.mined_height = Some(4_500_001);
        a.anchor_height = Some(4_500_012);
        {
            let mut c = chain.lock().unwrap();
            for o in [&a, &b] {
                c.orders.insert(o.order_id, open_order(me, 0, o.taker, o.zatoshi, o.pay_deadline));
            }
        }
        maker.book.lock().await.orders.extend([(a.order_id, a.clone()), (b.order_id, b.clone())]);
        assert_eq!(maker.decide_now(&b, 4_500_003).await.unwrap(), Step::Pay, "the pass would pay B");
        // A's proof starts, and the version is removed while it runs.
        *maker.proving.lock().unwrap() = Some(a.order_id);
        assert!(maker.proving_now(a.order_id) && !maker.proving_now(b.order_id));
        chain.lock().unwrap().version_working = false;
        assert_eq!(maker.decide_now(&b, 4_500_003).await.unwrap(), Step::Wait, "B's broadcast is refused");
        // The tip passed B's plan while it was built: its anchor is planned again, nothing is sent.
        {
            let mut c = chain.lock().unwrap();
            c.version_working = true;
            c.relay_height = 4_500_012;
        }
        assert_eq!(maker.decide_now(&b, 4_500_012).await.unwrap(), Step::Pay, "the last tip below the plan");
        assert!(matches!(maker.decide_now(&b, 4_500_013).await.unwrap(), Step::SchedulePayment { .. }));
        // So is a payment once too little time is left.
        chain.lock().unwrap().now = b.pay_deadline - maker.cfg.required_secs() + 1;
        assert!(matches!(maker.decide_now(&b, 4_500_003).await.unwrap(), Step::Abandon(_)));
    }

    /// N-10's makers: on the Base stub, the engine just passed (its view of the chains is recent),
    /// with 20 ZEC to pay from.
    async fn passed_maker(chain: crate::testkit::Shared, cfg: MakerConfig, book: Book) -> Arc<Maker> {
        let m = crate::testkit::maker(&crate::testkit::base_stub(chain).await, cfg, book);
        m.pass_at.store(unix_now(), Ordering::Relaxed);
        m.balance.store(2_000_000_000, Ordering::Relaxed);
        m
    }

    /// One request to the maker's API: its status and JSON body.
    async fn call(
        m: &Arc<Maker>,
        req: axum::http::Request<axum::body::Body>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let r = crate::api::router(m.clone()).oneshot(req).await.unwrap();
        let status = r.status();
        let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    fn get(uri: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::get(uri).body(axum::body::Body::empty()).unwrap()
    }

    /// The taker's destination `(ua, salt)` of an order, and the commitment it opens.
    fn destination(index: u32) -> (String, [u8; 32], B256) {
        use orchard::keys::Scope;
        let sk = (1u8..).find_map(|b| Option::<SpendingKey>::from(SpendingKey::from_bytes([b; 32]))).unwrap();
        let address = FullViewingKey::from(&sk).address_at(index, Scope::External);
        let salt = [index as u8; 32];
        let ua = zecured_zcash_ops::keys::encode_ironwood_ua(&Network::TestNetwork, address).unwrap();
        (ua, salt, zecured_swap::receiver_commitment(&salt, &address.to_raw_address_bytes()))
    }

    /// N-10 (a): a Buy order left without a destination holds one of the maker's places only for
    /// a short time after it opened: orders opened and left (queue stuffing) leave the maker free
    /// to quote again.
    #[tokio::test]
    async fn n10_an_order_left_without_a_destination_stops_holding_a_place() {
        let cfg = judge_like();
        let mut book = Book::default();
        for i in 0..cfg.max_open_orders as u8 {
            let r = new_record(
                B256::repeat_byte(0x40 + i),
                Side::Buy,
                Address::repeat_byte(0x30 + i),
                50_000_000,
                U256::from(20_000_000u64),
                1_000_000 + 21_600,
                B256::repeat_byte(3),
            );
            book.orders.insert(r.order_id, r);
        }
        let chain = std::sync::Arc::new(std::sync::Mutex::new(crate::testkit::Chain::default()));
        let m = passed_maker(chain, cfg, book).await;
        let (status, body) = call(&m, get("/swap/quote?side=buy&zatoshi=50000000")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        crate::testkit::forget(&m);
    }

    /// N-10 (b): the per-taker cap applies to an order's taker on Base, orders opened from open Buy
    /// quotes included: beyond its cap, a taker's order holds no place of the maker's and takes no
    /// destination.
    #[tokio::test]
    async fn n10_the_per_taker_cap_applies_to_the_orders_taker_on_base() {
        use crate::testkit::{Chain, open_order, order_opened};
        let mut cfg = judge_like();
        cfg.max_open_orders = 4;
        let chain = std::sync::Arc::new(std::sync::Mutex::new(Chain::default()));
        let m = passed_maker(chain.clone(), cfg, Book::default()).await;
        // Four orders of one taker, opened from open Buy quotes, read from Base by the engine.
        let taker = Address::repeat_byte(0x33);
        let (ua, salt, commitment) = destination(4);
        let ids: Vec<B256> = (1..=4u8).map(B256::repeat_byte).collect();
        {
            let mut c = chain.lock().unwrap();
            for id in &ids {
                let mut o = open_order(m.signer.address(), 0, taker, 50_000_000, 1_000_000 + 21_600);
                o.receiverCommitment = commitment;
                c.logs.push(order_opened(*id, &o));
                c.orders.insert(*id, o);
            }
        }
        m.sync_orders().await.unwrap();
        assert_eq!(m.book.lock().await.orders.len(), 4);
        // Another taker is still quoted: the fourth order holds no place.
        let (status, body) = call(&m, get("/swap/quote?side=buy&zatoshi=50000000")).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        // And the fourth order takes no destination.
        let post = |id: B256| {
            let body = serde_json::json!({ "receiver": ua, "salt": hex::encode(salt) }).to_string();
            axum::http::Request::post(format!("/swap/orders/{id}/destination"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap()
        };
        let (status, body) = call(&m, post(ids[3])).await;
        assert_eq!((status, body["code"].as_str()), (axum::http::StatusCode::SERVICE_UNAVAILABLE, Some("busy")));
        // Its first three take theirs: their reservations kept their places and their ZEC.
        for id in &ids[..3] {
            assert_eq!(m.book.lock().await.orders[id].reserved_until, Some(1_000_000 + 600));
            let (status, body) = call(&m, post(*id)).await;
            assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{body}");
        }
        crate::testkit::forget(&m);
    }

    /// N-10 (c): quotes given and not opened yet hold the maker's places, and those naming a taker
    /// count toward its cap: a burst of quotes cannot outrun the engine reading their orders.
    #[tokio::test]
    async fn n10_quotes_given_count_toward_the_makers_places() {
        let mut cfg = judge_like();
        cfg.max_open_orders = 4;
        let chain = std::sync::Arc::new(std::sync::Mutex::new(crate::testkit::Chain::default()));
        let m = passed_maker(chain, cfg, Book::default()).await;
        let sell = format!("/swap/quote?side=sell&zatoshi=50000000&taker={}", Address::repeat_byte(0x33));
        for _ in 0..3 {
            let (status, body) = call(&m, get(&sell)).await;
            assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        }
        let (status, body) = call(&m, get(&sell)).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, Some("busy")),
            "the taker's cap"
        );
        let buy = "/swap/quote?side=buy&zatoshi=50000000";
        assert_eq!(call(&m, get(buy)).await.0, axum::http::StatusCode::OK, "the maker's last place");
        let (status, body) = call(&m, get(buy)).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, Some("busy")),
            "no place left"
        );
        crate::testkit::forget(&m);
    }

    /// W-8: what the wallet's scan shows of an expired payment's inputs.
    #[test]
    fn w8_the_inputs_of_an_expired_payment() {
        let (a, b) = ([0xab; 32], [0xcd; 32]);
        let inputs = vec![hex::encode(a), hex::encode(b)];
        let notes = |a_spent, b_spent| vec![(a, a_spent), (b, b_spent), ([0xee; 32], true)];
        assert!(inputs_unspent(127, 123, &inputs, notes(false, false)));
        assert!(!inputs_unspent(126, 123, &inputs, notes(false, false)), "the scan must pass the expiry height");
        assert!(!inputs_unspent(127, 123, &inputs, notes(false, true)), "an input spent: by the payment or another");
        assert!(!inputs_unspent(127, 123, &inputs, vec![(a, false)]), "an input the wallet does not know");
        assert!(!inputs_unspent(127, 123, &[], notes(false, false)), "no recorded inputs: nothing shown");
    }

    /// N-9: without delays (`pay_delay_max_blocks` 0, a local fork) the maker still pays: its
    /// payment, due once the tip reaches the schedule, lands below the planned anchor.
    #[test]
    fn n9_a_payment_without_delays_is_paid() {
        let mut cfg = judge_like();
        cfg.local_fork = true;
        cfg.mock_seal = true;
        cfg.pay_delay_max_blocks = 0;
        cfg.check().unwrap();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        let (pay_delay, draw) = anchor::draw(0);
        let Step::SchedulePayment {
            pay_after,
            planned_anchor,
            draw,
        } = decide(&cfg, &r, &facts(1_000_000), pay_delay, draw)
        else {
            panic!("not scheduled")
        };
        r.pay_after_height = Some(pay_after);
        r.planned_anchor_height = Some(planned_anchor);
        r.anchor_draw = Some(draw);
        let mut f = facts(1_000_100);
        f.zcash_tip = pay_after;
        assert_eq!(
            decide(&cfg, &r, &f, pay_delay, draw),
            Step::Pay,
            "due at {pay_after}, anchored at {planned_anchor}"
        );
    }

    /// N-12 (W-8): an unmined payment counts as expired only three blocks after its expiry height:
    /// before that, a short reorganization or a light wallet server a block or two behind could
    /// still show it mined at or below that height, and the order would be paid twice.
    #[test]
    fn n12_a_payment_expires_only_three_blocks_after_its_expiry_height() {
        let (a, b) = ([0xab; 32], [0xcd; 32]);
        let inputs = vec![hex::encode(a), hex::encode(b)];
        let notes = vec![(a, false), (b, false)];
        for scanned_to in 124..=126 {
            assert!(!inputs_unspent(scanned_to, 123, &inputs, notes.clone()), "scanned to {scanned_to}");
        }
        assert!(inputs_unspent(127, 123, &inputs, notes));
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::Paid);
        r.expiry_height = Some(4_500_123);
        r.input_nullifiers = inputs;
        let mut f = facts(1_000_200);
        f.payment = Some(Seen::NotMined);
        f.inputs_unspent = true;
        for tip in 4_500_124..=4_500_126 {
            f.zcash_tip = tip;
            assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "tip {tip}: not expired yet");
        }
        f.zcash_tip = 4_500_127;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::PaymentExpired);
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
        d.taker_places = 3;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::TAKER_BUSY)
        );
        // N-10: an order whose reservation lapsed takes a place again only if one is free.
        d.taker_places = 2;
        d.holds_place = false;
        d.places_taken = cfg.max_open_orders;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).err(),
            Some(ApiError::BUSY)
        );
        d.places_taken -= 1;
        assert!(accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).is_ok());
        d.holds_place = true;
        d.places_taken = cfg.max_open_orders;
        assert!(accept_destination(&cfg, &Network::TestNetwork, &r, &d, &ua, &hex::encode(salt)).is_ok(), "its own");
    }

    /// W-7: the maker never pays while no proof could land (its Swap version removed or its
    /// router's verifier stopped, or the relay behind), and starts no proving run while the
    /// version is not working.
    #[test]
    fn w7_refuses_to_pay_when_the_version_is_not_working() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::DestinationReceived);
        r.pay_after_height = Some(4_500_003);
        r.planned_anchor_height = Some(4_500_014);
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
        r.anchor_height = Some(4_500_014);
        let mut f = facts(1_000_200);
        f.payment = Some(Seen::Mined(4_500_005));
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Prove);
        f.proofs.version_working = false;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "no proving run that could not land");
    }
}
