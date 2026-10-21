//! The maker's order book: its orders as Base opened them and how far each is, persisted (0600,
//! atomically) so that a restart neither pays twice nor forgets a payment to prove. A Buy order's
//! destination (the taker's receiver and salt), the payment's txid and the note's opening are kept
//! only until the order is settled, refunded or abandoned, then erased (docs/SWAP.md "The market maker").

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
    /// Buy: the taker's raw receiver (hex) and salt (hex). Private; erased when done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,
    /// The Zcash height at or after which the payment is broadcast (H-05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pay_after_height: Option<u32>,
    /// W-10: the anchor height of the payment's proof, fixed when the payment was scheduled, before
    /// the payment existed: `c + pay_delay_max_blocks + 1 + r` (`zecured_swap::anchor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_anchor_height: Option<u32>,
    /// W-10: `r`, the anchor's random draw, drawn once for the order and kept across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_draw: Option<u32>,
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
}

/// The persisted book.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    /// The last Base block whose `OrderOpened` events were read.
    pub synced_to: u64,
    pub orders: BTreeMap<B256, OrderRecord>,
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

    /// Zatoshi the wallet's scanned balance does not show as gone yet: promised by Buy orders given
    /// a destination and not paid, or paid by a transaction not mined (its inputs still look
    /// unspent). W-19: an order without a destination reserves nothing, so orders opened and left
    /// without one cannot tie up the inventory; a destination the maker could not pay is refused.
    pub fn reserved_zatoshi(&self) -> u64 {
        self.open_orders()
            .filter(|o| match o.status {
                MakerStatus::DestinationReceived => true,
                MakerStatus::Paid => o.mined_height.is_none(),
                _ => false,
            })
            .map(|o| o.zatoshi)
            .sum()
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
            receiver: Some("ab".repeat(43)),
            salt: Some("5a".repeat(32)),
            pay_after_height: Some(10),
            planned_anchor_height: Some(25),
            anchor_draw: Some(5),
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

    /// W-19: a Buy order reserves the maker's ZEC only once its destination is in: orders opened and
    /// left without one (queue stuffing) leave the inventory alone. Sell orders reserve no ZEC but
    /// count as open orders (`max_open_orders` bounds the USDC the maker locks).
    #[test]
    fn w19_buy_orders_without_a_destination_reserve_no_zec() {
        let mut book = Book::default();
        let awaiting = record(MakerStatus::AwaitingDestination);
        book.orders.insert(awaiting.order_id, awaiting);
        assert_eq!(book.reserved_zatoshi(), 0, "no destination, no reservation");
        let mut received = record(MakerStatus::DestinationReceived);
        received.order_id = B256::repeat_byte(2);
        book.orders.insert(received.order_id, received);
        assert_eq!(book.reserved_zatoshi(), 50_000_000, "a destination reserves");
        let mut sell = record(MakerStatus::TakerPays);
        sell.side = Side::Sell;
        sell.order_id = B256::repeat_byte(3);
        book.orders.insert(sell.order_id, sell);
        assert_eq!(book.reserved_zatoshi(), 50_000_000, "a Sell order reserves no ZEC");
        assert_eq!(book.open_orders().count(), 3, "Sell orders count toward max_open_orders");
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
        let mut old = serde_json::to_value(&book).unwrap();
        let o = old["orders"].as_object_mut().unwrap().values_mut().next().unwrap().as_object_mut().unwrap();
        for k in [
            "planned_anchor_height",
            "anchor_draw",
            "anchor_height",
            "input_nullifiers",
        ] {
            o.remove(k);
        }
        o.insert("anchor_after_height".into(), serde_json::json!(13));
        let r: Book = serde_json::from_value(old).unwrap();
        assert_eq!(r.orders[&B256::repeat_byte(1)].anchor_draw, None);
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
        assert_eq!(back.reserved_zatoshi(), 50_000_000, "the unpaid Buy order reserves, a mined payment no more");
        let mut unmined = back.clone();
        unmined.orders.get_mut(&B256::repeat_byte(2)).unwrap().mined_height = None;
        assert_eq!(unmined.reserved_zatoshi(), 100_000_000, "a payment not mined yet still reserves");
        assert!(Book::load(&dir.join("missing.json")).unwrap().orders.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
