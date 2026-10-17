//! P-1, the demo path (DEMO: the judge deployment only, never Base mainnet). The backend's demo
//! wallet, an ordinary Ironwood wallet whose spending key comes from `DEMO_WALLET_SPENDING_KEY`
//! (never logged), pays an opened vault its full amount in testnet ZEC (TAZ), so that a judge
//! without TAZ still goes through a real deposit, proof and loan:
//!
//! - `POST /demo/vaults/:vaultId/pay` with the vault as `POST /vaults` returned it (owner, account,
//!   amount, expiry height, address, tag key hash and every signer's `VaultOpened` signature) and a
//!   hashcash stamp `sha256("zecured-demo-pay-v1" || vaultId || nonce (u64, big-endian))` with the
//!   network's open-vault zero bits (app/lib/demo.ts computes the same). The demo wallet pays only
//!   a vault that all five signers vouched for (C-4: the address is theirs) and that the coordinator
//!   still has waiting for this very deposit with time left before its expiry; once per vault
//!   (persisted), within a daily cap (UTC days). Its output to the vault uses `ovk = None`, as a
//!   third party's payment. `{ txid, amount }`, or one of the fixed refusals below.
//! - `GET /demo/status`: `{ enabled, returnAddress, dailyCapZat, paidTodayZat, lowBalance, stampBits }`.
//!   `returnAddress` is the demo wallet's Ironwood address, where the app sends a demo vault's exits
//!   when the user gives no address of their own.
//!
//! Low balance: below `DEMO_LOW_BALANCE_ZAT` the operators are alerted (a warning in the log and,
//! with `DEMO_ALARM_WEBHOOK`, a POST of `{ event, balanceZat, thresholdZat }`), at most once every
//! [`ALARM_EVERY`]. Nothing about any vault or user is logged or sent.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    future::Future,
    path::{Path as FsPath, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex as StdMutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, anyhow, ensure};
use axum::{
    Json,
    extract::{
        Path,
        rejection::{JsonRejection, PathRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use orchard::{
    circuit::ProvingKey,
    keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey},
};
use pczt::roles::signer::Signer;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use zcash_protocol::consensus::{BlockHeight, Network};
use zecured_base::{VaultOpened, check_signed_by_all, registry_domain, stamp::leading_zero_bits, text_hash};
use zecured_coordinator::{Coordinator, error::scrubbed, store::VaultStatus};
use zecured_zcash_ops::{
    keys::{encode_ironwood_ua, ironwood_receiver},
    lwd,
    scan::ScanState,
    spend::{self, OutputPrivacy, Payment},
};

/// The demo wallet pays TAZ: Zcash testnet only.
const NETWORK: Network = Network::TestNetwork;
/// Base mainnet: never a demo path there.
pub const BASE_MAINNET: u64 = 8_453;
/// Domain separation of the demo stamp's preimage.
pub const STAMP_DOMAIN: &[u8] = b"zecured-demo-pay-v1";
/// Zcash blocks a vault must have left before its expiry for the demo wallet to pay it: the payment's
/// proof, its mining and the deposit's confirmations fit in them.
pub const MIN_BLOCKS_LEFT: u32 = 24;
/// The least time between two low-balance alarms.
pub const ALARM_EVERY: Duration = Duration::from_secs(6 * 3_600);
/// How often the demo wallet's balance is read in the background.
pub const WATCH_EVERY: Duration = Duration::from_secs(600);
/// How long a payment waits for the wallet (another payment, or the first scan) before `demo_busy`.
const LOCK_WAIT: Duration = Duration::from_secs(90);
/// UTC days of totals the book keeps.
const DAYS_KEPT: u64 = 7;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The demo stamp of `nonce` for `vault`.
pub fn stamp_digest(vault: &B256, nonce: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(STAMP_DOMAIN);
    h.update(vault);
    h.update(nonce.to_be_bytes());
    h.finalize().into()
}

/// Whether `nonce` stamps `vault` with at least `bits` zero bits.
pub fn stamp_ok(vault: &B256, nonce: u64, bits: u32) -> bool {
    leading_zero_bits(&stamp_digest(vault, nonce)) >= bits
}

// ------------------------------------------------------------------------- refusals (L-2)

/// A fixed answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemoError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}

impl DemoError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self { status, code, message }
    }
}

impl IntoResponse for DemoError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response()
    }
}

pub const BAD_REQUEST: DemoError = DemoError::new(StatusCode::BAD_REQUEST, "bad_request", "Invalid request.");
pub const DEMO_OFF: DemoError =
    DemoError::new(StatusCode::NOT_FOUND, "demo_off", "The demo wallet is off on this deployment.");
pub const BAD_STAMP: DemoError =
    DemoError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_stamp", "The request's proof of work does not check out.");
pub const UNVOUCHED: DemoError =
    DemoError::new(StatusCode::FORBIDDEN, "demo_unvouched", "The signers' signatures on this vault do not check out.");
pub const NOT_AWAITING: DemoError = DemoError::new(
    StatusCode::CONFLICT,
    "demo_not_awaiting",
    "This vault does not wait for this deposit, or expires too soon.",
);
pub const ALREADY_PAID: DemoError =
    DemoError::new(StatusCode::CONFLICT, "demo_already_paid", "The demo wallet already paid this vault.");
pub const DAILY_CAP: DemoError =
    DemoError::new(StatusCode::TOO_MANY_REQUESTS, "demo_daily_cap", "Today's demo budget is spent.");
pub const BUSY: DemoError = DemoError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "demo_busy",
    "The demo wallet's funds are on their way in other payments. Try again in a few minutes.",
);
pub const LOW_BALANCE: DemoError =
    DemoError::new(StatusCode::SERVICE_UNAVAILABLE, "demo_low_balance", "The demo wallet is nearly empty.");
pub const UNAVAILABLE: DemoError = DemoError::new(
    StatusCode::BAD_GATEWAY,
    "demo_wallet_unavailable",
    "The demo wallet could not pay right now. Try again later.",
);

/// Accepts decimal strings or JSON numbers.
fn num(v: &Value) -> Result<u64, DemoError> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
    .ok_or(BAD_REQUEST)
}

// ------------------------------------------------------------------------- the wallet and the vaults

/// Why the demo wallet did not pay.
#[derive(Debug)]
pub enum PayError {
    /// Its free notes do not cover the payment now; its payments in flight will (their change).
    Busy,
    /// It cannot cover the payment: its balance estimate.
    LowBalance(u64),
    /// Nothing was broadcast (the light wallet server, the scan, the build or the proof failed).
    NotSent(anyhow::Error),
    /// The broadcast failed: the payment may still have reached the network.
    Unknown(anyhow::Error),
}

/// The demo wallet.
pub trait Payer: Send + Sync {
    /// Pays `amount` zatoshis to `to`: the transaction id (display order) and the balance left.
    fn pay(&self, to: orchard::Address, amount: u64) -> BoxFuture<'_, Result<(String, u64), PayError>>;
    /// Its balance estimate, read from the chain now: free notes plus the change of payments in flight.
    fn balance(&self) -> BoxFuture<'_, anyhow::Result<u64>>;
    /// Its Ironwood-only unified address.
    fn return_address(&self) -> String;
}

/// The coordinator's view of a vault.
pub trait Vaults: Send + Sync {
    /// The amount and expiry height of a vault still waiting for its deposit (none seen); `None`
    /// for an unknown vault or one past that.
    fn awaiting(&self, vault: B256) -> BoxFuture<'_, Option<(u64, u32)>>;
    /// The Zcash tip of the coordinator's last pass.
    fn zcash_tip(&self) -> u32;
}

impl Vaults for Coordinator {
    fn awaiting(&self, vault: B256) -> BoxFuture<'_, Option<(u64, u32)>> {
        Box::pin(async move {
            let v = self.view(vault).await.ok()?;
            (v.status == VaultStatus::AwaitingDeposit && v.non_conforming.is_empty())
                .then_some((v.amount, v.expiry_height))
        })
    }

    fn zcash_tip(&self) -> u32 {
        Coordinator::zcash_tip(self)
    }
}

// ------------------------------------------------------------------------- the demo path

/// What the demo path runs with.
pub struct DemoConfig {
    pub chain_id: u64,
    pub registry: Address,
    /// The signer set (node index, attestation address) every vault's `VaultOpened` must come from.
    pub signers: Vec<(u16, Address)>,
    pub stamp_bits: u32,
    pub daily_cap_zat: u64,
    pub low_balance_zat: u64,
    pub alarm_webhook: Option<String>,
    /// The book of paid vaults and daily totals (`<coordinator data>/demo-payer.json`).
    pub book: PathBuf,
}

/// Vaults paid (or being paid) and the zatoshi paid per UTC day (days since the Unix epoch).
#[derive(Default, Serialize, Deserialize)]
struct Book {
    paid: BTreeSet<B256>,
    days: BTreeMap<u64, u64>,
}

fn load_book(path: &FsPath) -> anyhow::Result<Book> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw).context("the demo payer's book is malformed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
        Err(e) => Err(e).context("read the demo payer's book"),
    }
}

fn save_book(path: &FsPath, book: &Book) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(book)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The low-balance alarm.
struct Alarm {
    webhook: Option<String>,
    http: reqwest::Client,
    last: StdMutex<Option<Instant>>,
}

impl Alarm {
    /// Alerts the operators unless it did within [`ALARM_EVERY`]; whether it did now.
    fn fire(&self, balance: u64, threshold: u64) -> bool {
        {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|t| t.elapsed() < ALARM_EVERY) {
                return false;
            }
            *last = Some(Instant::now());
        }
        warn!(balance_zat = balance, threshold_zat = threshold, "DEMO: the demo wallet runs low on TAZ; top it up");
        if let Some(url) = self.webhook.clone() {
            let http = self.http.clone();
            let body = json!({ "event": "demo_wallet_low", "balanceZat": balance.to_string(), "thresholdZat": threshold.to_string() });
            tokio::spawn(async move {
                if http.post(url).json(&body).send().await.is_err() {
                    warn!("DEMO: the low-balance webhook could not be reached");
                }
            });
        }
        true
    }
}

struct Demo {
    cfg: DemoConfig,
    vaults: Arc<dyn Vaults>,
    payer: Arc<dyn Payer>,
    book: Mutex<Book>,
    balance: StdMutex<Option<u64>>,
    alarm: Alarm,
}

/// The demo path: off, or the demo wallet with its book.
#[derive(Clone)]
pub struct DemoPath(Option<Arc<Demo>>);

#[derive(Deserialize)]
pub struct SignatureBody {
    index: u16,
    signature: Bytes,
}

/// `POST /demo/vaults/:vaultId/pay`'s body (app/lib/api.ts `demoPay`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayBody {
    owner: Address,
    account: Address,
    amount: Value,
    expiry_height: u32,
    address: String,
    tag_key_hash: B256,
    signatures: Vec<SignatureBody>,
    stamp: Value,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl DemoPath {
    pub fn off() -> Self {
        Self(None)
    }

    /// The demo path with `payer`, refused on Base mainnet.
    pub fn new(cfg: DemoConfig, vaults: Arc<dyn Vaults>, payer: Arc<dyn Payer>) -> anyhow::Result<Self> {
        ensure!(cfg.chain_id != BASE_MAINNET, "the demo path never runs on Base mainnet");
        ensure!(!cfg.signers.is_empty(), "the demo path needs the signer set");
        let book = load_book(&cfg.book)?;
        let alarm = Alarm {
            webhook: cfg.alarm_webhook.clone().filter(|u| !u.is_empty()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
            last: StdMutex::new(None),
        };
        Ok(Self(Some(Arc::new(Demo {
            cfg,
            vaults,
            payer,
            book: Mutex::new(book),
            balance: StdMutex::new(None),
            alarm,
        }))))
    }

    #[cfg(test)]
    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Reads the demo wallet's balance now and every [`WATCH_EVERY`] (which also keeps its scan warm),
    /// alarming when it runs low.
    pub fn watch(&self) {
        let Some(d) = self.0.clone() else { return };
        tokio::spawn(async move {
            loop {
                match d.payer.balance().await {
                    Ok(b) => d.note_balance(b),
                    Err(e) => {
                        warn!("DEMO: the demo wallet's balance could not be read");
                        debug!(error = %scrubbed(&e), "demo wallet");
                    }
                }
                tokio::time::sleep(WATCH_EVERY).await;
            }
        });
    }
}

impl Demo {
    fn note_balance(&self, balance: u64) {
        *self.balance.lock().unwrap_or_else(PoisonError::into_inner) = Some(balance);
        if balance < self.cfg.low_balance_zat {
            self.alarm.fire(balance, self.cfg.low_balance_zat);
        }
    }

    async fn status(&self, now: u64) -> Value {
        let paid_today = self.book.lock().await.days.get(&(now / 86_400)).copied().unwrap_or(0);
        let balance = *self.balance.lock().unwrap_or_else(PoisonError::into_inner);
        json!({
            "enabled": true,
            "returnAddress": self.payer.return_address(),
            "dailyCapZat": self.cfg.daily_cap_zat.to_string(),
            "paidTodayZat": paid_today.to_string(),
            "lowBalance": balance.is_some_and(|b| b < self.cfg.low_balance_zat),
            "stampBits": self.cfg.stamp_bits,
        })
    }

    /// Books the payment before it is made: once per vault, within the day's cap.
    async fn reserve(&self, vault: B256, amount: u64, day: u64) -> Result<(), DemoError> {
        let mut book = self.book.lock().await;
        if book.paid.contains(&vault) {
            return Err(ALREADY_PAID);
        }
        let today = book.days.get(&day).copied().unwrap_or(0);
        if today.saturating_add(amount) > self.cfg.daily_cap_zat {
            return Err(DAILY_CAP);
        }
        let before = std::mem::take(&mut *book);
        book.paid = before.paid.clone();
        book.paid.insert(vault);
        book.days = before.days.iter().filter(|(d, _)| **d + DAYS_KEPT > day).map(|(d, t)| (*d, *t)).collect();
        book.days.insert(day, today + amount);
        if save_book(&self.cfg.book, &book).is_err() {
            *book = before;
            warn!("DEMO: the demo payer's book could not be saved");
            return Err(UNAVAILABLE);
        }
        Ok(())
    }

    /// Undoes [`Self::reserve`] for a payment that was not broadcast.
    async fn release(&self, vault: B256, amount: u64, day: u64) {
        let mut book = self.book.lock().await;
        book.paid.remove(&vault);
        if let Some(t) = book.days.get_mut(&day) {
            *t = t.saturating_sub(amount);
        }
        if save_book(&self.cfg.book, &book).is_err() {
            warn!("DEMO: the demo payer's book could not be saved");
        }
    }

    async fn pay(&self, vault: B256, b: PayBody, now: u64) -> Result<Value, DemoError> {
        let amount = num(&b.amount)?;
        let nonce = num(&b.stamp)?;
        if b.signatures.len() > 2 * self.cfg.signers.len() || b.address.len() > 1_000 {
            return Err(BAD_REQUEST);
        }
        if !stamp_ok(&vault, nonce, self.cfg.stamp_bits) {
            return Err(BAD_STAMP);
        }
        // C-4: the vault exactly as all five signers vouched for it, its address included.
        let message = VaultOpened {
            vaultId: vault,
            owner: b.owner,
            account: b.account,
            amount,
            expiryHeight: b.expiry_height,
            addressHash: text_hash(&b.address),
            tagKeyHash: b.tag_key_hash,
        };
        let signatures: Vec<(u16, Bytes)> = b.signatures.into_iter().map(|s| (s.index, s.signature)).collect();
        let domain = registry_domain(self.cfg.chain_id, self.cfg.registry);
        check_signed_by_all(&domain, &message, &self.cfg.signers, &signatures).map_err(|_| UNVOUCHED)?;
        let recipient = ironwood_receiver(&NETWORK, &b.address).map_err(|_| UNVOUCHED)?;
        // Still waiting for this very deposit, with time left to confirm it.
        let (want, expiry) = self.vaults.awaiting(vault).await.ok_or(NOT_AWAITING)?;
        let tip = self.vaults.zcash_tip();
        if want != amount || expiry != b.expiry_height || expiry <= tip.saturating_add(MIN_BLOCKS_LEFT) {
            return Err(NOT_AWAITING);
        }
        let day = now / 86_400;
        self.reserve(vault, amount, day).await?;
        match self.payer.pay(recipient, amount).await {
            Ok((txid, balance)) => {
                info!("DEMO: the demo wallet paid a vault");
                self.note_balance(balance);
                Ok(json!({ "txid": txid, "amount": amount.to_string() }))
            }
            Err(PayError::Busy) => {
                self.release(vault, amount, day).await;
                Err(BUSY)
            }
            Err(PayError::LowBalance(balance)) => {
                self.release(vault, amount, day).await;
                self.note_balance(balance);
                Err(LOW_BALANCE)
            }
            Err(PayError::NotSent(e)) => {
                self.release(vault, amount, day).await;
                warn!("DEMO: a demo payment failed before its broadcast");
                debug!(error = %scrubbed(&e), "demo payment");
                Err(UNAVAILABLE)
            }
            Err(PayError::Unknown(e)) => {
                warn!("DEMO: a demo payment's broadcast failed; its vault stays booked as paid");
                debug!(error = %scrubbed(&e), "demo payment");
                Err(UNAVAILABLE)
            }
        }
    }
}

/// `GET /demo/status`.
pub async fn status(axum::Extension(d): axum::Extension<DemoPath>) -> Json<Value> {
    match &d.0 {
        Some(demo) => Json(demo.status(now_secs()).await),
        None => Json(json!({ "enabled": false })),
    }
}

/// `POST /demo/vaults/:vaultId/pay`.
pub async fn pay(
    axum::Extension(d): axum::Extension<DemoPath>,
    id: Result<Path<B256>, PathRejection>,
    body: Result<Json<PayBody>, JsonRejection>,
) -> Result<Json<Value>, DemoError> {
    let demo = d.0.as_ref().ok_or(DEMO_OFF)?;
    let Path(vault) = id.map_err(|_| BAD_REQUEST)?;
    let Json(b) = body.map_err(|_| BAD_REQUEST)?;
    demo.pay(vault, b, now_secs()).await.map(Json)
}

// ------------------------------------------------------------------------- the real wallet

/// How many of `values` (free notes, in the wallet's P1 order) pay `amount` and the ZIP 317 fee of
/// a payment with change: the first ones until they cover it; `None` when all of them do not.
pub fn select(values: &[u64], amount: u64) -> Option<usize> {
    let mut total = 0u64;
    for (i, v) in values.iter().enumerate() {
        total = total.saturating_add(*v);
        if total >= amount.saturating_add(spend::zip317_fee(i + 1, 2)) {
            return Some(i + 1);
        }
    }
    None
}

/// A broadcast payment not yet seen mined: its inputs stay booked until then or its expiry.
struct InFlight {
    nullifiers: Vec<[u8; 32]>,
    expiry: u32,
    change: u64,
}

#[derive(Default)]
struct WalletState {
    scan: Option<ScanState>,
    in_flight: Vec<InFlight>,
    pk: Option<Arc<ProvingKey>>,
}

impl WalletState {
    fn booked(&self) -> HashSet<[u8; 32]> {
        self.in_flight.iter().flat_map(|p| p.nullifiers.iter().copied()).collect()
    }

    /// Free notes plus the change of the payments in flight.
    fn estimate(&self) -> u64 {
        let booked = self.booked();
        let free: u64 = self.scan.as_ref().map_or(0, |s| {
            s.unspent().iter().filter(|n| !booked.contains(&n.nullifier.to_bytes())).map(|n| n.value()).sum()
        });
        free + self.in_flight.iter().map(|p| p.change).sum::<u64>()
    }
}

/// The demo wallet on Zcash testnet: one Orchard-protocol spending key, Ironwood notes only, scanned
/// through the network's light wallet server.
pub struct LwdPayer {
    lwd_url: String,
    fvk: FullViewingKey,
    ask: SpendAuthorizingKey,
    birthday: u32,
    return_address: String,
    state: Mutex<WalletState>,
}

impl LwdPayer {
    /// `spending_key`: 32 bytes of hex (`zecured-wallet init`'s `spending_key`); never echoed.
    pub fn new(lwd_url: String, spending_key: &str, birthday: u32) -> anyhow::Result<Self> {
        let bytes: [u8; 32] = hex::decode(spending_key.trim().trim_start_matches("0x"))
            .ok()
            .and_then(|b| b.try_into().ok())
            .context("DEMO_WALLET_SPENDING_KEY must be 32 bytes of hex")?;
        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes(bytes))
            .context("DEMO_WALLET_SPENDING_KEY is not a valid spending key")?;
        ensure!(birthday > 0, "DEMO_WALLET_BIRTHDAY must be the demo wallet's birthday height");
        let fvk = FullViewingKey::from(&sk);
        let return_address = encode_ironwood_ua(&NETWORK, fvk.address_at(0u32, Scope::External))?;
        Ok(Self {
            lwd_url,
            ask: SpendAuthorizingKey::from(&sk),
            fvk,
            birthday,
            return_address,
            state: Mutex::new(WalletState::default()),
        })
    }

    /// Scans to the server's tip (from the birthday the first time, or again after a reorg) and
    /// forgets the payments in flight whose inputs were seen spent or that expired.
    async fn sync(&self, st: &mut WalletState) -> anyhow::Result<lwd::LwdClient> {
        let mut client = lwd::connect(&self.lwd_url).await?;
        let tip = u32::try_from(lwd::latest_block(&mut client).await?.height)?;
        if st.scan.is_none() {
            st.scan = Some(ScanState::from_birthday(&mut client, self.fvk.clone(), self.birthday).await?);
        }
        let scan = st.scan.as_mut().context("no scan state")?;
        if let Err(e) = scan.scan_to(&mut client, tip).await {
            st.scan = None;
            return Err(e);
        }
        let unspent: HashSet<[u8; 32]> = scan.unspent().iter().map(|n| n.nullifier.to_bytes()).collect();
        st.in_flight.retain(|p| p.expiry >= tip && p.nullifiers.iter().all(|nf| unspent.contains(nf)));
        Ok(client)
    }

    async fn pay_now(&self, to: orchard::Address, amount: u64) -> Result<(String, u64), PayError> {
        let mut st = tokio::time::timeout(LOCK_WAIT, self.state.lock()).await.map_err(|_| PayError::Busy)?;
        let mut client = self.sync(&mut st).await.map_err(PayError::NotSent)?;
        let booked = st.booked();
        let scan = st.scan.as_ref().ok_or_else(|| PayError::NotSent(anyhow!("no scan state")))?;
        let free: Vec<_> = scan.unspent().into_iter().filter(|n| !booked.contains(&n.nullifier.to_bytes())).collect();
        let values: Vec<u64> = free.iter().map(|n| n.value()).collect();
        let Some(count) = select(&values, amount) else {
            let in_flight: u64 = st.in_flight.iter().map(|p| p.change).sum();
            let coming = values.iter().sum::<u64>() + in_flight;
            return Err(if coming >= amount + spend::zip317_fee(1, 2) {
                PayError::Busy
            } else {
                PayError::LowBalance(st.estimate())
            });
        };
        let inputs: Vec<_> = free[..count].iter().map(|n| (*n).clone()).collect();
        let target = scan.scanned_to + 1;
        let refs: Vec<_> = inputs.iter().collect();
        let payment = Payment {
            recipient: to,
            value: amount,
            privacy: OutputPrivacy::NoOvk,
        };
        let unsigned =
            spend::build(&NETWORK, BlockHeight::from_u32(target), &self.fvk, scan.anchor(), &refs, &[payment])
                .map_err(PayError::NotSent)?;
        let pk = match &st.pk {
            Some(pk) => pk.clone(),
            None => {
                let pk = Arc::new(
                    tokio::task::spawn_blocking(spend::proving_key)
                        .await
                        .map_err(|e| PayError::NotSent(anyhow!("proving key task: {e}")))?,
                );
                st.pk = Some(pk.clone());
                pk
            }
        };
        let (pczt, indices, change) = (unsigned.pczt, unsigned.spend_action_indices, unsigned.change);
        let ask = self.ask.clone();
        let tx = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let pczt = spend::finalize_and_prove(pczt, &pk)?;
            let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
            for i in &indices {
                signer.sign_ironwood(rand_010::rng(), *i, &ask).map_err(|e| anyhow!("sign: {e:?}"))?;
            }
            spend::extract(signer.finish())
        })
        .await
        .map_err(|e| PayError::NotSent(anyhow!("prover task: {e}")))?
        .map_err(PayError::NotSent)?;
        let txid = spend::broadcast(&mut client, &tx).await.map_err(PayError::Unknown)?;
        st.in_flight.push(InFlight {
            nullifiers: inputs.iter().map(|n| n.nullifier.to_bytes()).collect(),
            expiry: target + spend::PAYOUT_EXPIRY_DELTA,
            change,
        });
        Ok((txid.to_string(), st.estimate()))
    }
}

impl Payer for LwdPayer {
    fn pay(&self, to: orchard::Address, amount: u64) -> BoxFuture<'_, Result<(String, u64), PayError>> {
        Box::pin(self.pay_now(to, amount))
    }

    fn balance(&self) -> BoxFuture<'_, anyhow::Result<u64>> {
        Box::pin(async move {
            let mut st = self.state.lock().await;
            self.sync(&mut st).await?;
            Ok(st.estimate())
        })
    }

    fn return_address(&self) -> String {
        self.return_address.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::signers::local::PrivateKeySigner;
    use axum::{Router, extract::State, routing::post};
    use zecured_base::sign_typed;

    use super::*;

    /// The first ZIP 2005 use_qsk vector's Ironwood-only testnet address (app/lib/signers.test.ts).
    const TESTNET_UA: &str = "utest1yv0hq77h4th2c882kdmcgt47xrm5zmhkfnfpw6dcack8e6kgfmg5new7vxed0trhkc67cmf2tjea8pnd09key4v85kmqdnz33yuas40f";
    const REGISTRY: Address = Address::repeat_byte(0x11);
    const CHAIN: u64 = 84_532;
    const BITS: u32 = 8;
    const DAY: u64 = 20_748 * 86_400;

    /// Shared vectors with app/lib/demo.test.ts (also recomputed there with Node's crypto).
    #[test]
    fn the_stamp_matches_the_apps() {
        let vault = B256::repeat_byte(0x55);
        assert_eq!(
            hex::encode(stamp_digest(&vault, 7)),
            "e5bfd308ea2e37fdf9dce75a209820c1411f1e640760f0f94195861d0337fc33"
        );
        assert_eq!(
            hex::encode(stamp_digest(&vault, 0)),
            "9e61a4bba0470454cb96a998eb45e91794e816e3c18c38374ad4affd6daa7468"
        );
        assert_eq!((0u64..).find(|n| stamp_ok(&vault, *n, 12)), Some(21_216));
        assert!(stamp_ok(&vault, 0, 0));
    }

    #[test]
    fn notes_are_taken_in_order_until_they_cover_the_payment_and_its_fee() {
        assert_eq!(select(&[5_000_000], 1_000_000), Some(1));
        assert_eq!(select(&[1_000_000], 1_000_000), None, "the fee is not covered");
        assert_eq!(select(&[1_010_000], 1_000_000), Some(1));
        assert_eq!(select(&[600_000, 500_000, 9_000_000], 1_000_000), Some(2));
        assert_eq!(select(&[600_000, 405_000], 1_000_000), None);
        assert_eq!(select(&[], 1), None);
        // Three inputs cost 15,000 zatoshis (ZIP 317: three logical actions).
        assert_eq!(select(&[400_000, 400_000, 210_000], 1_000_000), None);
        assert_eq!(select(&[400_000, 400_000, 215_000], 1_000_000), Some(3));
    }

    struct FakeVaults {
        waiting: HashMap<B256, (u64, u32)>,
        tip: u32,
    }

    impl Vaults for FakeVaults {
        fn awaiting(&self, vault: B256) -> BoxFuture<'_, Option<(u64, u32)>> {
            let v = self.waiting.get(&vault).copied();
            Box::pin(async move { v })
        }

        fn zcash_tip(&self) -> u32 {
            self.tip
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Outcome {
        Paid,
        Busy,
        Low,
        NotSent,
        Unknown,
    }

    struct FakePayer {
        outcome: StdMutex<Outcome>,
        paid: StdMutex<Vec<(orchard::Address, u64)>>,
    }

    impl FakePayer {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                outcome: StdMutex::new(Outcome::Paid),
                paid: StdMutex::default(),
            })
        }

        fn set(&self, o: Outcome) {
            *self.outcome.lock().unwrap() = o;
        }
    }

    impl Payer for FakePayer {
        fn pay(&self, to: orchard::Address, amount: u64) -> BoxFuture<'_, Result<(String, u64), PayError>> {
            let outcome = *self.outcome.lock().unwrap();
            let r = match outcome {
                Outcome::Paid => {
                    self.paid.lock().unwrap().push((to, amount));
                    Ok(("ab".repeat(32), 90_000_000))
                }
                Outcome::Busy => Err(PayError::Busy),
                Outcome::Low => Err(PayError::LowBalance(5_000)),
                Outcome::NotSent => Err(PayError::NotSent(anyhow!("lightwalletd at 10.0.0.7 refused"))),
                Outcome::Unknown => Err(PayError::Unknown(anyhow!("SendTransaction timed out"))),
            };
            Box::pin(async move { r })
        }

        fn balance(&self) -> BoxFuture<'_, anyhow::Result<u64>> {
            Box::pin(async { Ok(90_000_000) })
        }

        fn return_address(&self) -> String {
            TESTNET_UA.into()
        }
    }

    struct Net {
        signers: Vec<PrivateKeySigner>,
        book: PathBuf,
    }

    impl Net {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("zecured-demo-test-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let book = dir.join("demo-payer.json");
            let _ = std::fs::remove_file(&book);
            Self {
                signers: (0..5).map(|_| PrivateKeySigner::random()).collect(),
                book,
            }
        }

        fn config(&self, daily_cap_zat: u64) -> DemoConfig {
            DemoConfig {
                chain_id: CHAIN,
                registry: REGISTRY,
                signers: self.signers.iter().zip(1u16..).map(|(s, i)| (i, s.address())).collect(),
                stamp_bits: BITS,
                daily_cap_zat,
                low_balance_zat: 20_000_000,
                alarm_webhook: None,
                book: self.book.clone(),
            }
        }

        fn demo(&self, cap: u64, vaults: FakeVaults, payer: Arc<FakePayer>) -> Arc<Demo> {
            DemoPath::new(self.config(cap), Arc::new(vaults), payer).unwrap().0.unwrap()
        }

        /// The body the app sends for `vault`, signed by the signers (or with `forged` as node 3).
        fn body(&self, vault: B256, amount: u64, expiry: u32, forged: Option<&PrivateKeySigner>) -> PayBody {
            let (owner, account) = (Address::repeat_byte(0x0a), Address::repeat_byte(0x0b));
            let message = VaultOpened {
                vaultId: vault,
                owner,
                account,
                amount,
                expiryHeight: expiry,
                addressHash: text_hash(TESTNET_UA),
                tagKeyHash: B256::repeat_byte(0x7a),
            };
            let domain = registry_domain(CHAIN, REGISTRY);
            let signatures = self
                .signers
                .iter()
                .zip(1u16..)
                .map(|(s, index)| {
                    let key = if index == 3 { forged.unwrap_or(s) } else { s };
                    SignatureBody {
                        index,
                        signature: sign_typed(key, &domain, &message).unwrap(),
                    }
                })
                .collect();
            let stamp = (0u64..).find(|n| stamp_ok(&vault, *n, BITS)).unwrap();
            PayBody {
                owner,
                account,
                amount: json!(amount.to_string()),
                expiry_height: expiry,
                address: TESTNET_UA.into(),
                tag_key_hash: B256::repeat_byte(0x7a),
                signatures,
                stamp: json!(stamp.to_string()),
            }
        }
    }

    fn waiting(vaults: &[(B256, u64, u32)], tip: u32) -> FakeVaults {
        FakeVaults {
            waiting: vaults.iter().map(|(v, a, e)| (*v, (*a, *e))).collect(),
            tip,
        }
    }

    #[tokio::test]
    async fn pays_a_vouched_waiting_vault_its_full_amount_once() {
        let net = Net::new("once");
        let vault = B256::repeat_byte(1);
        let payer = FakePayer::new();
        let demo = net.demo(50_000_000, waiting(&[(vault, 1_000_000, 4_500_000)], 4_499_000), payer.clone());
        let r = demo.pay(vault, net.body(vault, 1_000_000, 4_500_000, None), DAY).await.unwrap();
        assert_eq!(r, json!({ "txid": "ab".repeat(32), "amount": "1000000" }));
        let paid = payer.paid.lock().unwrap().clone();
        assert_eq!(paid.len(), 1);
        assert_eq!((paid[0].0, paid[0].1), (ironwood_receiver(&NETWORK, TESTNET_UA).unwrap(), 1_000_000));
        let status = demo.status(DAY + 60).await;
        assert_eq!(status["paidTodayZat"], "1000000");
        assert_eq!(status["lowBalance"], false);
        assert_eq!(status["returnAddress"], TESTNET_UA);
        assert_eq!((&status["dailyCapZat"], &status["stampBits"]), (&json!("50000000"), &json!(BITS)));
        // Once per vault, across restarts.
        assert_eq!(demo.pay(vault, net.body(vault, 1_000_000, 4_500_000, None), DAY).await, Err(ALREADY_PAID));
        let again = net.demo(50_000_000, waiting(&[(vault, 1_000_000, 4_500_000)], 4_499_000), payer.clone());
        assert_eq!(again.pay(vault, net.body(vault, 1_000_000, 4_500_000, None), DAY).await, Err(ALREADY_PAID));
        assert_eq!(payer.paid.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pays_nothing_the_signers_and_the_coordinator_do_not_vouch_for() {
        let net = Net::new("vouch");
        let (vault, other) = (B256::repeat_byte(1), B256::repeat_byte(2));
        let payer = FakePayer::new();
        let vaults = waiting(&[(vault, 1_000_000, 4_500_000), (other, 5_000_000, 4_500_000)], 4_499_000);
        let demo = net.demo(50_000_000, vaults, payer.clone());
        let mut b = net.body(vault, 1_000_000, 4_500_000, None);
        b.stamp = json!((0u64..).find(|n| !stamp_ok(&vault, *n, BITS)).unwrap());
        assert_eq!(demo.pay(vault, b, DAY).await, Err(BAD_STAMP));
        let forger = PrivateKeySigner::random();
        assert_eq!(demo.pay(vault, net.body(vault, 1_000_000, 4_500_000, Some(&forger)), DAY).await, Err(UNVOUCHED));
        // Signed for another vault, or for another amount than the body says.
        let elsewhere = net.body(other, 1_000_000, 4_500_000, None);
        let mut moved = net.body(vault, 1_000_000, 4_500_000, None);
        moved.signatures = elsewhere.signatures;
        assert_eq!(demo.pay(vault, moved, DAY).await, Err(UNVOUCHED));
        let mut more = net.body(vault, 1_000_000, 4_500_000, None);
        more.amount = json!("5000000");
        assert_eq!(demo.pay(vault, more, DAY).await, Err(UNVOUCHED));
        // Vouched for, but not what the coordinator waits for.
        let unknown = B256::repeat_byte(3);
        assert_eq!(demo.pay(unknown, net.body(unknown, 1_000_000, 4_500_000, None), DAY).await, Err(NOT_AWAITING));
        assert_eq!(demo.pay(other, net.body(other, 1_000_000, 4_500_000, None), DAY).await, Err(NOT_AWAITING));
        assert_eq!(demo.pay(vault, net.body(vault, 1_000_000, 4_500_001, None), DAY).await, Err(NOT_AWAITING));
        // Too close to its expiry.
        let late =
            net.demo(50_000_000, waiting(&[(vault, 1_000_000, 4_500_000)], 4_500_000 - MIN_BLOCKS_LEFT), payer.clone());
        assert_eq!(late.pay(vault, net.body(vault, 1_000_000, 4_500_000, None), DAY).await, Err(NOT_AWAITING));
        assert!(payer.paid.lock().unwrap().is_empty());
        let mut bad = net.body(vault, 1_000_000, 4_500_000, None);
        bad.amount = json!("ten");
        assert_eq!(demo.pay(vault, bad, DAY).await, Err(BAD_REQUEST));
    }

    #[tokio::test]
    async fn the_daily_cap_and_failed_payments() {
        let net = Net::new("cap");
        let v: Vec<B256> = (1..=5u8).map(B256::repeat_byte).collect();
        let payer = FakePayer::new();
        let all: Vec<(B256, u64, u32)> = v.iter().map(|id| (*id, 1_000_000, 4_500_000)).collect();
        let demo = net.demo(1_500_000, waiting(&all, 4_499_000), payer.clone());
        let pay = |id: B256, now: u64| {
            let demo = demo.clone();
            let body = net.body(id, 1_000_000, 4_500_000, None);
            async move { demo.pay(id, body, now).await }
        };
        assert!(pay(v[0], DAY).await.is_ok());
        assert_eq!(pay(v[1], DAY + 3_600).await, Err(DAILY_CAP));
        assert!(pay(v[1], DAY + 86_400).await.is_ok(), "a new UTC day");
        // Not broadcast: booked back, the vault can try again.
        payer.set(Outcome::Busy);
        assert_eq!(pay(v[2], DAY + 2 * 86_400).await, Err(BUSY));
        payer.set(Outcome::NotSent);
        assert_eq!(pay(v[2], DAY + 2 * 86_400).await, Err(UNAVAILABLE));
        payer.set(Outcome::Low);
        assert_eq!(pay(v[2], DAY + 2 * 86_400).await, Err(LOW_BALANCE));
        assert_eq!(demo.status(DAY + 2 * 86_400).await["lowBalance"], true);
        payer.set(Outcome::Paid);
        assert!(pay(v[2], DAY + 2 * 86_400).await.is_ok());
        // A broadcast that may have gone out keeps the vault booked: it is never paid twice.
        payer.set(Outcome::Unknown);
        assert_eq!(pay(v[3], DAY + 3 * 86_400).await, Err(UNAVAILABLE));
        payer.set(Outcome::Paid);
        assert_eq!(pay(v[3], DAY + 3 * 86_400).await, Err(ALREADY_PAID));
        assert_eq!(demo.status(DAY + 3 * 86_400).await["paidTodayZat"], "1000000");
        assert_eq!(payer.paid.lock().unwrap().len(), 3);
        // The book keeps a week of daily totals.
        assert!(pay(v[4], DAY + 30 * 86_400).await.is_ok());
        let book: Value = serde_json::from_slice(&std::fs::read(&net.book).unwrap()).unwrap();
        assert_eq!(book["paid"].as_array().unwrap().len(), 5);
        assert_eq!(book["days"], json!({ (DAY / 86_400 + 30).to_string(): 1_000_000 }));
    }

    #[tokio::test]
    async fn off_and_on_base_mainnet() {
        let net = Net::new("off");
        let off = DemoPath::off();
        assert!(!off.enabled());
        assert_eq!(status(axum::Extension(off.clone())).await.0, json!({ "enabled": false }));
        let body = Json(net.body(B256::repeat_byte(1), 1_000_000, 4_500_000, None));
        assert_eq!(pay(axum::Extension(off), Ok(Path(B256::repeat_byte(1))), Ok(body)).await.unwrap_err(), DEMO_OFF);
        let mut cfg = net.config(1);
        cfg.chain_id = BASE_MAINNET;
        assert!(DemoPath::new(cfg, Arc::new(waiting(&[], 0)), FakePayer::new()).is_err());
    }

    /// The webhook gets the balance and the threshold, nothing else, at most once a period.
    #[tokio::test]
    async fn the_low_balance_alarm() {
        let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
        async fn hook(State(seen): State<Arc<StdMutex<Vec<Value>>>>, Json(b): Json<Value>) {
            seen.lock().unwrap().push(b);
        }
        let app = Router::new().route("/hook", post(hook)).with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let alarm = Alarm {
            webhook: Some(format!("http://{addr}/hook")),
            http: reqwest::Client::new(),
            last: StdMutex::new(None),
        };
        assert!(alarm.fire(5_000, 20_000_000));
        assert!(!alarm.fire(4_000, 20_000_000), "once per period");
        for _ in 0..100 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let body = seen.lock().unwrap().clone();
        assert_eq!(body, vec![json!({ "event": "demo_wallet_low", "balanceZat": "5000", "thresholdZat": "20000000" })]);
    }

    #[test]
    fn the_wallet_key_is_checked_and_never_echoed() {
        let secret = "zz".repeat(32);
        let e = LwdPayer::new("http://127.0.0.1:9".into(), &secret, 1).err().unwrap();
        assert!(!format!("{e:#}").contains(&secret));
        let valid = (0u8..=255).map(|i| [i; 32]).find(|b| bool::from(SpendingKey::from_bytes(*b).is_some())).unwrap();
        assert!(LwdPayer::new("http://127.0.0.1:9".into(), &hex::encode(valid), 0).is_err(), "a birthday is needed");
        let p = LwdPayer::new("http://127.0.0.1:9".into(), &hex::encode(valid), 4_400_000).unwrap();
        assert!(p.return_address().starts_with("utest1"));
        assert_eq!(ironwood_receiver(&NETWORK, &p.return_address()).unwrap(), p.fvk.address_at(0u32, Scope::External));
    }
}
