//! The order engine: follows the maker's orders on Base and moves each one along. Buy: wait for
//! the taker's destination, pay after a random privacy delay (H-05), wait for the payment's block
//! and for the order's tier of work on the relay (plus a random number of extra blocks, H-05),
//! build the payment witness, prove it, submit the proof, finalize once ready. Sell: the taker pays
//! and proves; the maker finalizes a ready settlement and refunds itself after the deadline. Every
//! decision is the pure function [`decide`] of an order and the facts read for it.

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
use rand_core_06::{OsRng, RngCore};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, Network},
};
use zecured_base::gas_with_headroom;
use zecured_deposit_witness::{p2p, payment};
use zecured_swap::{
    OrderStatus, Side,
    abi::IZecuredSwap,
    chain::{finalize_settlement, payment_anchor, prove_settlement, relay_tip},
    pay::{authorize, build_payment, check_payment_note, select_inputs},
    seal::Prover,
};
use zecured_zcash_ops::{lwd, scan::ScanState, spend};

use crate::{
    api::ApiError,
    config::MakerConfig,
    gate::{Destination, check_destination},
    quoting::ChainLimits,
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
    /// Paid: the payment's block, once mined.
    pub mined_height: Option<u32>,
    /// Another payment of the maker is broadcast and not mined yet (one at a time: its inputs are
    /// not marked spent before it is mined).
    pub other_payment_unmined: bool,
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
    /// Set the payment's privacy delay: broadcast at or after this Zcash height.
    SchedulePayment(u32),
    Pay,
    /// The payment is broadcast and unmined: send it again (a server may have dropped it).
    Rebroadcast,
    /// The payment expired unmined: it never happened; pay again if there is time.
    PaymentExpired,
    /// Record the payment's block and the least anchor height of its proof.
    ScheduleAnchor {
        mined: u32,
        anchor_after: u32,
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

/// The next step for `rec` given `f`. `pay_delay` (1 to `pay_delay_max_blocks`) and
/// `prove_delay` (0 to `prove_delay_max_blocks`) are the random privacy delays, drawn by the
/// caller.
pub fn decide(cfg: &MakerConfig, rec: &OrderRecord, f: &Facts, pay_delay: u32, prove_delay: u32) -> Step {
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
            match rec.pay_after_height {
                None => Step::SchedulePayment(f.zcash_tip.saturating_add(pay_delay.max(1))),
                Some(h) if f.zcash_tip >= h && !f.other_payment_unmined => Step::Pay,
                Some(_) => Step::Wait,
            }
        }
        (Side::Buy, MakerStatus::Paid) => {
            if past_deadline {
                return Step::Abandon("paid, but not proven before the deadline");
            }
            match (f.mined_height, rec.mined_height, rec.anchor_after_height) {
                (None, _, _) => {
                    if rec.expiry_height.is_some_and(|e| f.zcash_tip > e) {
                        Step::PaymentExpired
                    } else {
                        Step::Rebroadcast
                    }
                }
                (Some(mined), _, None) => Step::ScheduleAnchor {
                    mined,
                    anchor_after: mined.saturating_add(prove_delay),
                },
                (Some(_), _, Some(_)) => Step::Prove,
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

/// `POST /swap/orders/:id/destination`: whether the maker takes the taker's destination for `rec`
/// now. Only a Buy order still waiting for it (or given the same one again), with time left for
/// the whole pipeline before its deadline, and a destination that opens its commitment.
pub fn accept_destination(
    cfg: &MakerConfig,
    network: &Network,
    rec: &OrderRecord,
    now: u64,
    receiver: &str,
    salt: &str,
) -> Result<Destination, ApiError> {
    if rec.side != Side::Buy {
        return Err(ApiError::NOT_BUY);
    }
    match rec.status {
        MakerStatus::AwaitingDestination | MakerStatus::DestinationReceived => {}
        _ => return Err(ApiError::NOT_AWAITING),
    }
    if now.saturating_add(cfg.required_secs()) > rec.pay_deadline {
        return Err(ApiError::TOO_LATE);
    }
    check_destination(network, receiver, salt, rec.receiver_commitment).map_err(|_| ApiError::BAD_DESTINATION)
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
    proving_key: tokio::sync::OnceCell<Arc<ProvingKey>>,
}

fn random_below(n: u32) -> u32 {
    if n == 0 { 0 } else { OsRng.next_u32() % n }
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
        txid: None,
        opening: None,
        raw_tx: None,
        expiry_height: None,
        mined_height: None,
        anchor_after_height: None,
        note: None,
    }
}

impl Maker {
    pub fn new(
        cfg: MakerConfig,
        network: Network,
        signer: PrivateKeySigner,
        provider: DynProvider,
        domain: Eip712Domain,
        limits: ChainLimits,
        book: Book,
        wallet: Wallet,
    ) -> Self {
        let prover = Prover {
            host: cfg.host.clone(),
            mock: cfg.mock_seal,
            work: cfg.state.with_file_name("proofs"),
        };
        Self {
            cfg,
            network,
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
        let zcash_tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
        self.scan_wallet(&mut client, zcash_tip).await?;
        self.sync_orders().await?;
        let now = self.now().await?;
        let relay_height = relay_tip(&self.provider, self.cfg.swap).await?;
        self.zcash_tip.store(u64::from(zcash_tip), Ordering::Relaxed);
        self.relay_tip.store(relay_height, Ordering::Relaxed);
        self.pass_at.store(unix_now(), Ordering::Relaxed);
        let ids: Vec<B256> = self.book.lock().await.open_orders().map(|o| o.order_id).collect();
        for id in ids {
            if let Err(e) = self.advance(&mut client, id, now, zcash_tip, relay_height).await {
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
        let mined_height = match (rec.status, rec.txid.as_deref()) {
            (MakerStatus::Paid, Some(txid)) => {
                let txid = TxId::from_hex(txid).context("stored txid")?;
                match lwd::raw_transaction(client, &txid).await {
                    Ok((_, h)) if h > 0 => Some(u32::try_from(h)?),
                    _ => None,
                }
            }
            _ => None,
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
            mined_height,
            other_payment_unmined,
        })
    }

    async fn advance(
        &self,
        client: &mut lwd::LwdClient,
        id: B256,
        now: u64,
        zcash_tip: u32,
        relay_height: u64,
    ) -> Result<()> {
        let Some(rec) = self.book.lock().await.orders.get(&id).cloned() else {
            return Ok(());
        };
        let f = self.facts(client, &rec, now, zcash_tip, relay_height).await?;
        let pay_delay = 1 + random_below(self.cfg.pay_delay_max_blocks);
        let prove_delay = random_below(self.cfg.prove_delay_max_blocks + 1);
        let step = decide(&self.cfg, &rec, &f, pay_delay, prove_delay);
        let mut rec = rec;
        match step {
            Step::Wait => return Ok(()),
            Step::SchedulePayment(h) => rec.pay_after_height = Some(h),
            Step::Pay => self.pay(client, &mut rec).await?,
            Step::Rebroadcast => {
                if let Some(raw) = rec.raw_tx.as_deref() {
                    // A known transaction is refused again: that is fine.
                    let _ = spend::broadcast_raw(client, hex::decode(raw)?).await;
                }
                return Ok(());
            }
            Step::PaymentExpired => {
                info!(order = %id, "the payment expired unmined; paying again if there is time");
                rec.status = MakerStatus::DestinationReceived;
                rec.pay_after_height = None;
                rec.txid = None;
                rec.opening = None;
                rec.raw_tx = None;
                rec.expiry_height = None;
            }
            Step::ScheduleAnchor { mined, anchor_after } => {
                rec.mined_height = Some(mined);
                rec.anchor_after_height = Some(anchor_after);
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
        let (tx, opening, target) = {
            let w = self.wallet.lock().await;
            let state = w.scan.as_ref().context("the wallet is not scanned yet")?;
            let unspent = state.unspent();
            let inputs = select_inputs(&unspent, rec.zatoshi)?;
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
            (tx, opening, target)
        };
        check_payment_note(&tx, &opening, rec.order_id)?;
        let raw = spend::serialize(&tx)?;
        rec.txid = Some(tx.txid().to_string());
        rec.opening = Some(opening);
        rec.raw_tx = Some(hex::encode(&raw));
        rec.expiry_height = Some(target + spend::PAYOUT_EXPIRY_DELTA);
        rec.status = MakerStatus::Paid;
        // Recorded before the broadcast: a crash right after it cannot lose the opening.
        {
            let mut book = self.book.lock().await;
            book.orders.insert(rec.order_id, rec.clone());
            book.save(&self.cfg.state)?;
        }
        spend::broadcast_raw(client, raw).await?;
        info!(order = %rec.order_id, zatoshi = rec.zatoshi, "paid");
        Ok(())
    }

    /// Proves a mined payment and submits the proof; false while its anchor is not on the relay yet.
    async fn prove(&self, client: &mut lwd::LwdClient, rec: &mut OrderRecord) -> Result<bool> {
        let mined = rec.mined_height.context("unmined")?;
        let floor = rec.anchor_after_height.unwrap_or(mined).saturating_sub(1).max(mined);
        let Some(anchor) = payment_anchor(&self.provider, self.cfg.swap, floor, rec.zatoshi).await? else {
            return Ok(false);
        };
        let headers = self.cfg.headers.clone();
        let have = tokio::task::spawn_blocking(move || p2p::extend_file(&headers, &p2p::seed_peers())).await??;
        anyhow::ensure!(have >= anchor, "the P2P peers are behind the relay");
        let txid = TxId::from_hex(rec.txid.as_deref().context("no txid")?).context("stored txid")?;
        let opening = rec.opening.clone().context("no opening")?;
        let built = payment::build(
            client,
            &self.cfg.headers,
            &self.cfg.summaries,
            txid,
            &opening,
            Some(rec.order_id.0),
            Some(rec.receiver_commitment.0),
            anchor,
        )
        .await?;
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
            txid: None,
            opening: None,
            raw_tx: None,
            expiry_height: None,
            mined_height: None,
            anchor_after_height: None,
            note: None,
        }
    }

    fn facts(now: u64) -> Facts {
        Facts {
            now,
            zcash_tip: 4_500_000,
            status: OrderStatus::Open,
            pending: None,
            mined_height: None,
            other_payment_unmined: false,
        }
    }

    #[test]
    fn a_buy_order_from_destination_to_proof() {
        let cfg = judge_like();
        let mut r = rec(Side::Buy, MakerStatus::AwaitingDestination);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "no destination yet");
        r.status = MakerStatus::DestinationReceived;
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::SchedulePayment(4_500_003));
        r.pay_after_height = Some(4_500_003);
        assert_eq!(decide(&cfg, &r, &facts(1_000_000), 3, 2), Step::Wait, "the privacy delay");
        let mut f = facts(1_000_100);
        f.zcash_tip = 4_500_003;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Pay);
        f.other_payment_unmined = true;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Wait, "one unmined payment at a time");
        r.status = MakerStatus::Paid;
        r.expiry_height = Some(4_500_123);
        let mut f = facts(1_000_200);
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::Rebroadcast);
        f.zcash_tip = 4_500_124;
        assert_eq!(decide(&cfg, &r, &f, 3, 2), Step::PaymentExpired);
        f.zcash_tip = 4_500_010;
        f.mined_height = Some(4_500_005);
        assert_eq!(
            decide(&cfg, &r, &f, 3, 2),
            Step::ScheduleAnchor {
                mined: 4_500_005,
                anchor_after: 4_500_007
            }
        );
        r.mined_height = Some(4_500_005);
        r.anchor_after_height = Some(4_500_007);
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

    /// The maker never pays when the pipeline would not fit before the deadline (work order 5.6).
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

    #[test]
    fn destinations() {
        use orchard::keys::{Scope, SpendingKey};
        let cfg = judge_like();
        let sk = (0u8..=255).find_map(|i| Option::<SpendingKey>::from(SpendingKey::from_bytes([i; 32]))).unwrap();
        let addr = FullViewingKey::from(&sk).address_at(5u32, Scope::External);
        let ua = zecured_zcash_ops::keys::encode_ironwood_ua(&Network::TestNetwork, addr).unwrap();
        let salt = [0x11; 32];
        let mut r = rec(Side::Buy, MakerStatus::AwaitingDestination);
        r.receiver_commitment = zecured_swap::receiver_commitment(&salt, &addr.to_raw_address_bytes());
        let now = r.pay_deadline - cfg.required_secs();
        let d = accept_destination(&cfg, &Network::TestNetwork, &r, now, &ua, &hex::encode(salt)).unwrap();
        assert_eq!(d.receiver, addr.to_raw_address_bytes());
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, now + 1, &ua, &hex::encode(salt)).err(),
            Some(ApiError::TOO_LATE)
        );
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &r, now, &ua, &hex::encode([0x12; 32])).err(),
            Some(ApiError::BAD_DESTINATION)
        );
        let sell = rec(Side::Sell, MakerStatus::TakerPays);
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &sell, now, &ua, &hex::encode(salt)).err(),
            Some(ApiError::NOT_BUY)
        );
        let mut paid = r.clone();
        paid.status = MakerStatus::Paid;
        assert_eq!(
            accept_destination(&cfg, &Network::TestNetwork, &paid, now, &ua, &hex::encode(salt)).err(),
            Some(ApiError::NOT_AWAITING)
        );
    }
}
