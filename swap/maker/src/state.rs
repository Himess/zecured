//! The maker's order book: its orders as Base opened them and how far each is, and the quotes it
//! gave whose orders it has not read yet (N-10), persisted (0600, atomically) so that a restart
//! neither pays twice nor forgets a payment to prove. A Buy order's destination (the taker's
//! receiver and salt), the payment's txid and the note's opening are kept only until the order is
//! settled, refunded or abandoned, then erased (docs/SWAP.md "The market maker").
//!
//! N-10, W-19: the maker has `max_open_orders` places, one taker at most
//! `max_open_orders_per_taker` of them. A quote takes one from the moment it is given until its
//! order is read from Base (or it can no longer be); an order then holds it, but a Buy order
//! without a destination only during its reservation (`reservation_secs`, given when the maker
//! reads it, within its taker's cap and while the maker has a place and the ZEC). Each place holds
//! its ZEC (Buy) or USDC (a Sell quote; a Sell order's USDC is locked on Base).

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zecured_swap::{Side, pay::PaymentOpening};

use crate::{config::MakerConfig, quoting::PAYMENT_FEE_HEADROOM};

/// Where an order is, from the maker's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MakerStatus {
    /// Buy: open on Base, waiting for the taker's destination.
    AwaitingDestination,
    /// Buy: the destination opened the commitment; the payment waits for its privacy delay.
    DestinationReceived,
    /// Buy: the payment is broadcast (or mined); its proof waits for the relay.
    Paid,
    /// Buy: the proof is on Base, the settlement pending (K3).
    Proven,
    /// Sell: the taker pays and proves; the maker waits (and refunds itself after the deadline).
    TakerPays,
    /// Buy: the maker will not pay or cannot prove any more (too late); the order waits for its
    /// refund, which the maker sends itself after the deadline.
    Abandoned,
    /// Done: the USDC went to the payer's counterparty.
    Settled,
    /// Done: the USDC went back to whoever locked it.
    Refunded,
}

impl MakerStatus {
    pub fn is_done(self) -> bool {
        matches!(self, MakerStatus::Settled | MakerStatus::Refunded)
    }
}

/// One order of this maker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderRecord {
    pub order_id: B256,
    pub side: Side,
    pub taker: Address,
    pub zatoshi: u64,
    pub usdc: U256,
    pub pay_deadline: u64,
    pub receiver_commitment: B256,
    pub status: MakerStatus,
    /// N-10: until when (Base's clock) a Buy order without a destination holds one of the maker's
    /// places and reserves its ZEC; `None` when it was given no reservation (its taker at its cap,
    /// or the maker full or short of ZEC when it read the order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_until: Option<u64>,
    /// Buy: the taker's raw receiver (hex) and salt (hex). Private; erased when done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,
    /// The Zcash height at or after which the payment is broadcast (H-05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pay_after_height: Option<u32>,
    /// W-10: the anchor height of the payment's proof, fixed when the payment was scheduled, before
    /// the payment existed: `c + pay_delay_max_blocks + 1 + 2 + r` (`zecured_swap::anchor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_anchor_height: Option<u32>,
    /// W-10: `r`, the anchor's random draw, drawn once for the order and kept across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_draw: Option<u32>,
    /// N-12: a late payment's own `r` (its anchor is `h + 1 + r`), drawn apart from `anchor_draw`
    /// the first time the payment is seen mined at or after its planned anchor, and kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_anchor_draw: Option<u32>,
    /// The payment's txid and the note's opening. Private; erased when done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<PaymentOpening>,
    /// The serialized payment, for a rebroadcast. Private; erased when done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_tx: Option<String>,
    /// The payment's expiry height (unmined after it, the payment never happened).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_height: Option<u32>,
    /// W-8: the nullifiers (hex) of the payment's input notes: still unspent in the wallet's scan
    /// past the expiry height, they show that the payment never happened. Private; erased when done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_nullifiers: Vec<String>,
    /// The payment's block, and the anchor height its proof uses (W-10: the planned one, or the
    /// late payment's). Erased when done, with the other Zcash heights of the payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mined_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_height: Option<u32>,
    /// Why the order stopped, if it did (never a receiver, salt, txid or Zcash height).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl OrderRecord {
    /// Erases what the maker only needed to pay and prove (docs/SWAP.md "The market maker"),
    /// and the Zcash heights that tie the order to its payment's block (W-10).
    pub fn forget_private(&mut self) {
        self.receiver = None;
        self.salt = None;
        self.txid = None;
        self.opening = None;
        self.raw_tx = None;
        self.input_nullifiers.clear();
        self.pay_after_height = None;
        self.planned_anchor_height = None;
        self.anchor_draw = None;
        self.late_anchor_draw = None;
        self.expiry_height = None;
        self.mined_height = None;
        self.anchor_height = None;
    }

    /// Marks the order done with `status`, erasing its private data.
    pub fn finish(&mut self, status: MakerStatus, note: Option<&str>) {
        self.status = status;
        self.note = note.map(str::to_owned);
        self.forget_private();
    }

    /// N-10: whether the order holds one of the maker's places at Base time `now`: an open order
    /// does, but a Buy order without a destination only during its reservation.
    pub fn holds_place(&self, now: u64) -> bool {
        match self.status {
            s if s.is_done() => false,
            MakerStatus::AwaitingDestination => self.reserved_until.is_some_and(|until| now < until),
            _ => true,
        }
    }

    /// The maker's zatoshi this order holds at `now`, which the wallet's scanned balance does not
    /// show as gone yet: a Buy order's, during its reservation (N-10), once its destination is in,
    /// and while its payment is not mined (its inputs still look unspent).
    pub fn reserved_zatoshi(&self, now: u64) -> u64 {
        let holds = match self.status {
            MakerStatus::AwaitingDestination => self.holds_place(now),
            MakerStatus::DestinationReceived => true,
            MakerStatus::Paid => self.mined_height.is_none(),
            _ => false,
        };
        if self.side == Side::Buy && holds {
            self.zatoshi
        } else {
            0
        }
    }
}

/// N-10: a quote the maker gave whose order it has not read from Base yet: it holds one of the
/// maker's places, and the ZEC (Buy) or USDC (Sell) it would take, until `holds_until`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GivenQuote {
    pub side: Side,
    /// The taker it names (zero: an open Buy quote).
    pub taker: Address,
    pub zatoshi: u64,
    pub usdc: U256,
    /// Base's clock: the quote's deadline, and a pass to read its order should it be opened then.
    pub holds_until: u64,
}

/// The persisted book.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    /// The last Base block whose `OrderOpened` events were read.
    pub synced_to: u64,
    pub orders: BTreeMap<B256, OrderRecord>,
    /// N-10: the quotes given whose orders are not in the book yet, by the order id they open.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quotes: BTreeMap<B256, GivenQuote>,
}

impl Book {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Writes the book atomically, readable by its owner only.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let tmp: PathBuf = path.with_extension("tmp");
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    /// Orders not done.
    pub fn open_orders(&self) -> impl Iterator<Item = &OrderRecord> {
        self.orders.values().filter(|o| !o.status.is_done())
    }

    /// N-10: the quotes that hold a place at `now`: not past their time, and not opened and read.
    fn live_quotes(&self, now: u64) -> impl Iterator<Item = &GivenQuote> {
        self.quotes
            .iter()
            .filter(move |(id, q)| now <= q.holds_until && !self.orders.contains_key(*id))
            .map(|(_, q)| q)
    }

    /// N-10: the maker's places taken at `now`: by its open orders that hold one, and by its quotes
    /// whose orders it has not read yet.
    pub fn places_taken(&self, now: u64) -> usize {
        self.open_orders().filter(|o| o.holds_place(now)).count() + self.live_quotes(now).count()
    }

    /// N-10, W-19: the places of `taker` at `now`: its orders on Base that hold one (opened from a
    /// quote naming it or from an open Buy quote) and the quotes naming it; order `except` aside.
    pub fn taker_places(&self, taker: Address, now: u64, except: Option<B256>) -> usize {
        let orders = self
            .open_orders()
            .filter(|o| o.taker == taker && Some(o.order_id) != except && o.holds_place(now))
            .count();
        orders + self.live_quotes(now).filter(|q| q.taker == taker).count()
    }

    /// Zatoshi the wallet's scanned balance does not show as gone yet: held by Buy orders (during
    /// their reservation, once their destination is in, while their payment is not mined) and by
    /// Buy quotes (N-10). An order left without a destination holds nothing after its reservation
    /// (W-19), and a destination the maker could not pay is refused.
    pub fn reserved_zatoshi(&self, now: u64) -> u64 {
        let orders: u64 = self.open_orders().map(|o| o.reserved_zatoshi(now)).sum();
        let quotes: u64 = self.live_quotes(now).filter(|q| q.side == Side::Buy).map(|q| q.zatoshi).sum();
        orders.saturating_add(quotes)
    }

    /// N-10: the USDC the maker's Sell quotes would lock, opened (a Sell order's is locked on Base).
    pub fn quoted_usdc(&self, now: u64) -> U256 {
        self.live_quotes(now).filter(|q| q.side == Side::Sell).map(|q| q.usdc).sum()
    }

    /// N-10: records `rec`, an order read from Base at `now`, in place of its quote. A Buy order
    /// still waiting for its destination gets its reservation (`reservation_secs`) only while its
    /// taker is under its cap, the maker has a place, and `balance` (the wallet's) covers its ZEC on
    /// top of what is held already; otherwise it holds nothing (and its destination is checked as a
    /// new claim on the maker).
    pub fn record_opened(&mut self, mut rec: OrderRecord, now: u64, balance: u64, cfg: &MakerConfig) {
        self.quotes.remove(&rec.order_id);
        if rec.side == Side::Buy && rec.status == MakerStatus::AwaitingDestination {
            let room = self.taker_places(rec.taker, now, None) < cfg.max_open_orders_per_taker
                && self.places_taken(now) < cfg.max_open_orders
                && balance.saturating_sub(self.reserved_zatoshi(now))
                    >= rec.zatoshi.saturating_add(PAYMENT_FEE_HEADROOM);
            rec.reserved_until = room.then(|| now.saturating_add(cfg.reservation_secs));
        }
        self.orders.insert(rec.order_id, rec);
    }

    /// N-10: forgets the quotes that hold nothing any more (past their time, or opened and read).
    pub fn prune_quotes(&mut self, now: u64) {
        let orders = &self.orders;
        self.quotes.retain(|id, q| now <= q.holds_until && !orders.contains_key(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: MakerStatus) -> OrderRecord {
        OrderRecord {
            order_id: B256::repeat_byte(1),
            side: Side::Buy,
            taker: Address::repeat_byte(2),
            zatoshi: 50_000_000,
            usdc: U256::from(20_000_000u64),
            pay_deadline: 1_760_021_600,
            receiver_commitment: B256::repeat_byte(3),
            status,
            reserved_until: None,
            receiver: Some("ab".repeat(43)),
            salt: Some("5a".repeat(32)),
            pay_after_height: Some(10),
            planned_anchor_height: Some(25),
            anchor_draw: Some(5),
            late_anchor_draw: Some(2),
            txid: Some("cd".repeat(32)),
            opening: Some(PaymentOpening {
                action_index: 1,
                recipient: vec![0xab; 43],
                value: 50_000_000,
                rseed: [0x42; 32],
                salt: [0x5a; 32],
            }),
            raw_tx: Some("00".into()),
            expiry_height: Some(130),
            input_nullifiers: vec!["ab".repeat(32)],
            mined_height: Some(11),
            anchor_height: Some(25),
            note: None,
        }
    }

    #[test]
    fn finishing_forgets_the_private_data() {
        let mut r = record(MakerStatus::Proven);
        r.finish(MakerStatus::Settled, None);
        assert!(r.status.is_done());
        assert!(r.receiver.is_none() && r.salt.is_none() && r.txid.is_none() && r.opening.is_none());
        assert!(r.raw_tx.is_none() && r.input_nullifiers.is_empty());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("receiver\"") && !json.contains("salt") && !json.contains("txid"));
        assert!(!json.contains("input_nullifiers"));
        assert!(!json.contains("height") && !json.contains("anchor"), "no Zcash height of the payment: {json}");
    }

    /// W-19: a Buy order without a destination reserves none of the maker's ZEC (but during its
    /// reservation, N-10): orders opened and left without one (queue stuffing) leave the inventory
    /// alone. Sell orders reserve no ZEC but hold places (`max_open_orders` bounds the USDC the maker
    /// locks).
    #[test]
    fn w19_buy_orders_without_a_destination_reserve_no_zec() {
        let now = 1_000_000;
        let mut book = Book::default();
        let awaiting = record(MakerStatus::AwaitingDestination);
        book.orders.insert(awaiting.order_id, awaiting);
        assert_eq!(book.reserved_zatoshi(now), 0, "no destination, no reservation");
        let mut received = record(MakerStatus::DestinationReceived);
        received.order_id = B256::repeat_byte(2);
        book.orders.insert(received.order_id, received);
        assert_eq!(book.reserved_zatoshi(now), 50_000_000, "a destination reserves");
        let mut sell = record(MakerStatus::TakerPays);
        sell.side = Side::Sell;
        sell.order_id = B256::repeat_byte(3);
        book.orders.insert(sell.order_id, sell);
        assert_eq!(book.reserved_zatoshi(now), 50_000_000, "a Sell order reserves no ZEC");
        assert_eq!(book.places_taken(now), 2, "Sell orders hold places, an order left without a destination none");
    }

    /// N-10: a Buy order read from Base holds a place, and reserves its ZEC, for `reservation_secs`
    /// while it waits for its destination; only within its taker's cap (orders opened from open Buy
    /// quotes included) and while the maker has a place and the ZEC. Given quotes hold places too,
    /// until their orders are read or their time is over.
    #[test]
    fn n10_reservations_places_and_quotes() {
        let cfg = crate::config::tests::judge_like();
        let (now, taker) = (1_000_000, Address::repeat_byte(0x33));
        let buy = |id: u8, taker: Address| {
            let mut r = record(MakerStatus::AwaitingDestination);
            r.order_id = B256::repeat_byte(id);
            r.taker = taker;
            r
        };
        let mut book = Book::default();
        book.record_opened(buy(1, taker), now, 2_000_000_000, &cfg);
        let r = &book.orders[&B256::repeat_byte(1)];
        assert_eq!(r.reserved_until, Some(now + 600));
        assert!(r.holds_place(now + 599) && !r.holds_place(now + 600), "ten minutes");
        assert_eq!((book.places_taken(now), book.reserved_zatoshi(now)), (1, 50_000_000));
        assert_eq!((book.places_taken(now + 600), book.reserved_zatoshi(now + 600)), (0, 0), "left without one");
        // One taker's cap: its fourth order, from an open quote, gets no reservation.
        for id in 2..=4 {
            book.record_opened(buy(id, taker), now, 2_000_000_000, &cfg);
        }
        assert_eq!(book.orders[&B256::repeat_byte(4)].reserved_until, None, "beyond the taker's cap");
        assert_eq!(book.taker_places(taker, now, None), 3);
        assert_eq!(book.taker_places(taker, now, Some(B256::repeat_byte(1))), 2);
        // Quotes hold places: the maker full, another taker's order gets no reservation.
        let quote = |side, taker| GivenQuote {
            side,
            taker,
            zatoshi: 50_000_000,
            usdc: U256::from(19_800_000u64),
            holds_until: now + 225,
        };
        for id in 10..17 {
            book.quotes.insert(B256::repeat_byte(id), quote(Side::Buy, Address::ZERO));
        }
        assert_eq!(book.places_taken(now), cfg.max_open_orders);
        assert_eq!(book.reserved_zatoshi(now), 10 * 50_000_000, "three orders' and seven quotes'");
        book.record_opened(buy(5, Address::repeat_byte(0x44)), now, 2_000_000_000, &cfg);
        assert_eq!(book.orders[&B256::repeat_byte(5)].reserved_until, None, "no place left");
        // An opened quote's order takes its place; a quote past its time holds nothing.
        book.record_opened(buy(10, Address::repeat_byte(0x44)), now, 2_000_000_000, &cfg);
        assert!(!book.quotes.contains_key(&B256::repeat_byte(10)));
        assert_eq!(book.orders[&B256::repeat_byte(10)].reserved_until, Some(now + 600), "the quote's place");
        assert_eq!(book.places_taken(now + 226), 4, "the quotes' time is over");
        book.prune_quotes(now + 226);
        assert!(book.quotes.is_empty());
        // Short of ZEC: no reservation.
        book.record_opened(buy(6, Address::repeat_byte(0x55)), now, 4 * 50_000_000, &cfg);
        assert_eq!(book.orders[&B256::repeat_byte(6)].reserved_until, None, "the ZEC is held already");
        // Sell quotes hold their USDC, and the taker they name a place.
        book.quotes.insert(B256::repeat_byte(20), quote(Side::Sell, Address::repeat_byte(0x55)));
        assert_eq!(book.quoted_usdc(now), U256::from(19_800_000u64));
        assert_eq!(book.taker_places(Address::repeat_byte(0x55), now, None), 1);
        // Kept across a restart; a book written before N-10 loads.
        let mut old = serde_json::to_value(&book).unwrap();
        let back: Book = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(back.quotes, book.quotes);
        old.as_object_mut().unwrap().remove("quotes");
        assert!(serde_json::from_value::<Book>(old).unwrap().quotes.is_empty());
    }

    /// W-10: the anchor's draw and the planned anchor survive a restart (they are drawn once per
    /// order); a book written before them still loads.
    #[test]
    fn w10_the_anchor_draw_survives_a_restart() {
        let dir =
            std::env::temp_dir().join(format!("zecured-maker-test-{}", hex::encode(zecured_swap::random_bytes32())));
        let path = dir.join("orders.json");
        let mut book = Book::default();
        book.orders.insert(B256::repeat_byte(1), record(MakerStatus::DestinationReceived));
        book.save(&path).unwrap();
        let back = Book::load(&path).unwrap();
        let r = &back.orders[&B256::repeat_byte(1)];
        assert_eq!((r.planned_anchor_height, r.anchor_draw, r.anchor_height), (Some(25), Some(5), Some(25)));
        assert_eq!(r.late_anchor_draw, Some(2), "N-12: a late payment's own draw too");
        let mut old = serde_json::to_value(&book).unwrap();
        let o = old["orders"].as_object_mut().unwrap().values_mut().next().unwrap().as_object_mut().unwrap();
        for k in [
            "planned_anchor_height",
            "anchor_draw",
            "late_anchor_draw",
            "anchor_height",
            "input_nullifiers",
        ] {
            o.remove(k);
        }
        o.insert("anchor_after_height".into(), serde_json::json!(13));
        let r: Book = serde_json::from_value(old).unwrap();
        assert_eq!(r.orders[&B256::repeat_byte(1)].anchor_draw, None);
        assert_eq!(r.orders[&B256::repeat_byte(1)].late_anchor_draw, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_book_persists_and_reserves() {
        let dir =
            std::env::temp_dir().join(format!("zecured-maker-test-{}", hex::encode(zecured_swap::random_bytes32())));
        let path = dir.join("orders.json");
        let mut book = Book::default();
        book.synced_to = 42;
        book.orders.insert(B256::repeat_byte(1), record(MakerStatus::DestinationReceived));
        let mut paid = record(MakerStatus::Paid);
        paid.order_id = B256::repeat_byte(2);
        book.orders.insert(paid.order_id, paid);
        let mut done = record(MakerStatus::Settled);
        done.order_id = B256::repeat_byte(3);
        book.orders.insert(done.order_id, done);
        book.save(&path).unwrap();
        let back = Book::load(&path).unwrap();
        assert_eq!(back.synced_to, 42);
        assert_eq!(back.orders.len(), 3);
        assert_eq!(back.open_orders().count(), 2);
        assert_eq!(back.reserved_zatoshi(0), 50_000_000, "the unpaid Buy order reserves, a mined payment no more");
        let mut unmined = back.clone();
        unmined.orders.get_mut(&B256::repeat_byte(2)).unwrap().mined_height = None;
        assert_eq!(unmined.reserved_zatoshi(0), 100_000_000, "a payment not mined yet still reserves");
        assert!(Book::load(&dir.join("missing.json")).unwrap().orders.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
