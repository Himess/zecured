//! R-5: the bots arm the liveness clock of their own claims.
//!
//! The bonds pay an overdue claim to its claimant (T-05) only once the claim is armed with a
//! signer's receipt: `DestinationReceived(claimId, commitment, receivedAt)`, in which a signer
//! node states that it holds and has checked the claim's destination. The liquidator's claims
//! (liquidations and pre-liquidations) and the market maker's (the executor's claims of its closes
//! and deleverages) have the bot as their claimant. For each, the bot asks the backend for a
//! receipt (`POST /claims/:claimId/receipt`, the first valid one any signer gives; `404
//! no_receipt` until the nodes hold the destination, `429 rate_limited`), backing off while there
//! is none, and sends `SignerBondV2.armLiveness(claimId, receivedAt, signature)` from its own key:
//! the bots pay their own gas, never through the paymaster.
//!
//! Before sending, the claim and its arming are read on Base: a claim already armed for its
//! current commitment is left alone, one armed for an earlier commitment is armed again, and one
//! already paid (or paid by the bonds, written off, compensated) is dropped. The claims being
//! tracked are kept in a file (owner-readable only, like the bot's destinations), so a restart
//! resumes them. Only claim id prefixes are logged.

use std::{collections::BTreeMap, future::Future, path::PathBuf, time::Duration};

use alloy::{
    primitives::{Address, B256, Bytes, Log},
    providers::Provider,
    sol_types::SolEvent,
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use zecured_base::{
    events::{ISignerBond, IZecuredRegistry},
    verify::{CLAIM_DISPUTED, CLAIM_PENDING},
};

use crate::chain::NonceLock;

/// The first wait after the backend has no receipt yet, doubled on every further miss up to
/// [`RECEIPT_RETRY_MAX`] (the nodes hold the destination once they have seen the claim on Base).
pub const RECEIPT_RETRY_FIRST: u64 = 30;
pub const RECEIPT_RETRY_MAX: u64 = 600;
/// How often an armed claim is read again, to notice a new commitment or that it was paid.
pub const ARMED_RECHECK: u64 = 3_600;
/// How long receipt requests stop after the backend's rate limit (20 a minute) answered.
pub const RATE_LIMIT_PAUSE: u64 = 60;

/// A claim on Base, as far as its arming goes (`Registry.claimDetails`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimState {
    pub status: u8,
    pub bond_paid: bool,
    pub commitment: B256,
}

/// A liveness receipt as the backend serves it (its `signer` field is not needed: the bond
/// recovers the signer from the signature).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub claim_id: B256,
    pub commitment: B256,
    pub received_at: u64,
    pub signature: Bytes,
}

/// The backend's answer to a receipt request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptAnswer {
    Found(Receipt),
    /// `404 no_receipt`: no signer holds a validated destination for the claim yet.
    NotYet,
    /// `429 rate_limited`.
    RateLimited,
}

/// What arming needs from Base.
pub trait ArmingChain {
    fn claim(&self, claim_id: B256) -> impl Future<Output = Result<ClaimState>> + Send;
    /// `SignerBondV2.livenessArming`: the commitment the claim was armed for and its `payableAt`
    /// (zeros if never armed).
    fn arming(&self, claim_id: B256) -> impl Future<Output = Result<(B256, u64)>> + Send;
    /// Simulates, then sends `armLiveness` from the bot's key and waits for it; the transaction's
    /// hash.
    fn arm(&self, claim_id: B256, received_at: u64, signature: Bytes) -> impl Future<Output = Result<B256>> + Send;
}

/// Where receipts come from.
pub trait ReceiptSource {
    fn receipt(&self, claim_id: B256) -> impl Future<Output = Result<ReceiptAnswer>> + Send;
}

/// The backend's `POST /claims/:claimId/receipt`.
pub struct BackendReceipts {
    client: reqwest::Client,
    base: String,
}

impl BackendReceipts {
    pub fn new(backend: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?,
            base: backend.trim_end_matches('/').to_owned(),
        })
    }
}

impl ReceiptSource for BackendReceipts {
    fn receipt(&self, claim_id: B256) -> impl Future<Output = Result<ReceiptAnswer>> + Send {
        async move {
            let resp = self
                .client
                .post(format!("{}/claims/{claim_id}/receipt", self.base))
                .send()
                .await
                .context("backend unreachable")?;
            match resp.status() {
                StatusCode::OK => Ok(ReceiptAnswer::Found(resp.json().await.context("the backend's receipt")?)),
                StatusCode::NOT_FOUND => Ok(ReceiptAnswer::NotYet),
                StatusCode::TOO_MANY_REQUESTS => Ok(ReceiptAnswer::RateLimited),
                status => {
                    let v: Value = resp.json().await.unwrap_or(Value::Null);
                    bail!("backend refused the receipt ({status}, {})", v["code"].as_str().unwrap_or("no code"))
                }
            }
        }
    }
}

/// Base through an RPC provider with the bot's key: the Registry (claims) and the bond (arming).
pub struct ProviderChain<'a, P> {
    pub p: &'a P,
    pub registry: Address,
    pub bond: Address,
    /// The key's nonce lock when other transactions of the same key are sent concurrently (the
    /// market maker's closes and deleverages); none when they are not (the liquidator's single
    /// loop, whose provider fills the nonces).
    pub nonces: Option<&'a NonceLock>,
}

/// A bot's receipt waits are bounded like its liquidations' (alloy's default is forever).
const ARM_RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);

impl<P: Provider> ArmingChain for ProviderChain<'_, P> {
    fn claim(&self, claim_id: B256) -> impl Future<Output = Result<ClaimState>> + Send {
        async move {
            let c = IZecuredRegistry::new(self.registry, self.p).claimDetails(claim_id).call().await?;
            Ok(ClaimState {
                status: c.status,
                bond_paid: c.bondPaid,
                commitment: c.commitment,
            })
        }
    }

    fn arming(&self, claim_id: B256) -> impl Future<Output = Result<(B256, u64)>> + Send {
        async move {
            let a = ISignerBond::new(self.bond, self.p).livenessArming(claim_id).call().await?;
            Ok((a.commitment, a.payableAt))
        }
    }

    fn arm(&self, claim_id: B256, received_at: u64, signature: Bytes) -> impl Future<Output = Result<B256>> + Send {
        async move {
            let bond = ISignerBond::new(self.bond, self.p);
            let call = bond.armLiveness(claim_id, received_at, signature);
            call.call().await.context("armLiveness would revert")?;
            let gas = zecured_base::gas_with_headroom(call.estimate_gas().await?);
            let call = call.gas(gas);
            let pending = match self.nonces {
                Some(lock) => {
                    lock.send_with(self.p, |nonce| async move { Ok(call.nonce(nonce).send().await?) }).await?
                }
                None => call.send().await?,
            };
            let receipt = pending.with_timeout(Some(ARM_RECEIPT_TIMEOUT)).get_receipt().await?;
            ensure!(receipt.status(), "armLiveness reverted");
            Ok(receipt.transaction_hash)
        }
    }
}

/// The claims a transaction of the bot created whose claimant is the bot, from its logs: every
/// `LiquidationRelease` of the Registry (a liquidation, a pre-liquidation or a deleverage pays the
/// caller), and the market maker's claim of a `CloseRelease` (the other is the borrower's).
pub fn own_claims<'a>(logs: impl IntoIterator<Item = &'a Log>, registry: Address) -> Vec<B256> {
    logs.into_iter()
        .filter(|l| l.address == registry)
        .filter_map(|l| {
            if let Ok(e) = IZecuredRegistry::LiquidationRelease::decode_log(l) {
                return Some(e.claimId);
            }
            IZecuredRegistry::CloseRelease::decode_log(l).ok().map(|e| e.mmClaimId)
        })
        .collect()
}

/// What the bot keeps about a claim it tracks.
#[derive(Clone, Debug, Default)]
struct Tracked {
    /// The commitment the bot last armed the claim for (or found it armed for).
    armed_for: Option<B256>,
    attempts: u32,
    next_try_at: u64,
}

/// The tracked claims as the file keeps them (the retry schedule starts afresh after a restart).
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    claims: Vec<SavedClaim>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedClaim {
    claim_id: B256,
    armed_for: Option<B256>,
}

/// What one pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Claims armed by this pass.
    pub armed: u32,
    /// Claims found armed for their current commitment: nothing sent.
    pub already_armed: u32,
    /// Claims waiting (no receipt yet, a dispute, an error, or the rate limit's pause).
    pub waiting: u32,
    /// Claims dropped: paid, paid by the bonds, written off or compensated.
    pub done: u32,
    pub rate_limited: bool,
}

/// What one claim's step decided.
enum Step {
    Done,
    Armed(B256),
    AlreadyArmed(B256),
    Wait,
    RateLimited,
}

/// Arms the liveness of the bot's claims, pass after pass.
pub struct Armer<C, R> {
    chain: C,
    receipts: R,
    path: Option<PathBuf>,
    claims: BTreeMap<B256, Tracked>,
    paused_until: u64,
}

fn short(id: &B256) -> String {
    hex::encode(&id[..4])
}

impl<C: ArmingChain + Sync, R: ReceiptSource + Sync> Armer<C, R> {
    /// An armer whose tracked claims are kept in `path` (and loaded from it, if it exists).
    pub fn new(chain: C, receipts: R, path: Option<PathBuf>) -> Result<Self> {
        let mut claims = BTreeMap::new();
        if let Some(p) = &path {
            match std::fs::read(p) {
                Ok(raw) => {
                    let saved: Saved = serde_json::from_slice(&raw).context("the bot's claims file")?;
                    for c in saved.claims {
                        claims.insert(
                            c.claim_id,
                            Tracked {
                                armed_for: c.armed_for,
                                ..Tracked::default()
                            },
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("read the bot's claims file"),
            }
        }
        Ok(Self {
            chain,
            receipts,
            path,
            claims,
            paused_until: 0,
        })
    }

    /// Starts tracking a claim of the bot (idempotent).
    pub fn track(&mut self, claim_id: B256) -> Result<()> {
        if self.claims.contains_key(&claim_id) {
            return Ok(());
        }
        self.claims.insert(claim_id, Tracked::default());
        info!(claim = %short(&claim_id), "tracking the claim's liveness arming");
        self.save()
    }

    /// The claims being tracked, with the commitment each was armed for.
    pub fn tracked(&self) -> Vec<(B256, Option<B256>)> {
        self.claims.iter().map(|(id, t)| (*id, t.armed_for)).collect()
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let saved = Saved {
            claims: self
                .claims
                .iter()
                .map(|(id, t)| SavedClaim {
                    claim_id: *id,
                    armed_for: t.armed_for,
                })
                .collect(),
        };
        crate::chain::write_private(path, &serde_json::to_vec_pretty(&saved)?)
    }

    /// Checks every claim that is due at `now` (Unix seconds): arms it, waits for its receipt, or
    /// drops it once nothing is left to arm.
    pub async fn pass(&mut self, now: u64) -> PassReport {
        let mut report = PassReport::default();
        let due: Vec<B256> = self.claims.iter().filter(|(_, t)| t.next_try_at <= now).map(|(id, _)| *id).collect();
        let mut changed = false;
        for id in due {
            let step = self.step(id, now).await;
            let Some(t) = self.claims.get_mut(&id) else {
                continue;
            };
            match step {
                Ok(Step::Done) => {
                    self.claims.remove(&id);
                    changed = true;
                    report.done += 1;
                }
                Ok(Step::Armed(commitment)) => {
                    *t = Tracked {
                        armed_for: Some(commitment),
                        attempts: 0,
                        next_try_at: now + ARMED_RECHECK,
                    };
                    changed = true;
                    report.armed += 1;
                }
                Ok(Step::AlreadyArmed(commitment)) => {
                    changed |= t.armed_for != Some(commitment);
                    *t = Tracked {
                        armed_for: Some(commitment),
                        attempts: 0,
                        next_try_at: now + ARMED_RECHECK,
                    };
                    report.already_armed += 1;
                }
                Ok(Step::Wait) => {
                    backoff(t, now);
                    report.waiting += 1;
                }
                Ok(Step::RateLimited) => {
                    backoff(t, now);
                    self.paused_until = now + RATE_LIMIT_PAUSE;
                    report.waiting += 1;
                    report.rate_limited = true;
                    warn!(claim = %short(&id), "the backend's rate limit: receipt requests pause");
                    break;
                }
                Err(e) => {
                    backoff(t, now);
                    report.waiting += 1;
                    warn!(claim = %short(&id), "liveness arming failed: {e:#}");
                }
            }
        }
        if changed && let Err(e) = self.save() {
            warn!("keeping the bot's claims failed: {e:#}");
        }
        report
    }

    async fn step(&self, id: B256, now: u64) -> Result<Step> {
        let c = self.chain.claim(id).await?;
        // Paid (on Zcash or by the bonds), written off or compensated: nothing left to arm.
        if c.bond_paid || (c.status != CLAIM_PENDING && c.status != CLAIM_DISPUTED) {
            return Ok(Step::Done);
        }
        // Disputed (payable again only once reopened), or without a destination: later.
        if c.status != CLAIM_PENDING || c.commitment == B256::ZERO {
            return Ok(Step::Wait);
        }
        let (armed_for, payable_at) = self.chain.arming(id).await?;
        if armed_for == c.commitment && payable_at != 0 {
            return Ok(Step::AlreadyArmed(c.commitment));
        }
        if now < self.paused_until {
            return Ok(Step::Wait);
        }
        match self.receipts.receipt(id).await? {
            ReceiptAnswer::NotYet => Ok(Step::Wait),
            ReceiptAnswer::RateLimited => Ok(Step::RateLimited),
            // A receipt for an earlier destination (the backend's books lag Base) arms nothing.
            ReceiptAnswer::Found(r) if r.claim_id != id || r.commitment != c.commitment => Ok(Step::Wait),
            ReceiptAnswer::Found(r) => {
                let tx = self.chain.arm(id, r.received_at, r.signature).await?;
                let again = if armed_for == B256::ZERO {
                    ""
                } else {
                    " again, for its new commitment"
                };
                info!(claim = %short(&id), %tx, "liveness armed{again} (R-5)");
                Ok(Step::Armed(c.commitment))
            }
        }
    }
}

/// The next try of a claim that is waiting: 30 s, then twice as long each time, at most 10 minutes.
fn backoff(t: &mut Tracked, now: u64) {
    t.attempts = t.attempts.saturating_add(1);
    let delay = RECEIPT_RETRY_FIRST.saturating_mul(1 << (t.attempts - 1).min(8)).min(RECEIPT_RETRY_MAX);
    t.next_try_at = now + delay;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use alloy::primitives::LogData;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode as HttpStatus,
        response::{IntoResponse, Response},
        routing::post,
    };
    use serde_json::json;
    use zecured_base::verify::{CLAIM_COMPENSATED, CLAIM_PAID, CLAIM_WRITTEN_OFF};

    use super::*;

    const CLAIM: B256 = B256::repeat_byte(0xc1);
    const COMMITMENT: B256 = B256::repeat_byte(0xa1);
    const NEW_COMMITMENT: B256 = B256::repeat_byte(0xa2);

    /// A MOCK of Base: the claims, the bond's armings, and every `armLiveness` sent.
    #[derive(Default)]
    struct StubChain {
        claims: Mutex<HashMap<B256, ClaimState>>,
        armings: Mutex<HashMap<B256, (B256, u64)>>,
        sent: Mutex<Vec<(B256, u64, Bytes)>>,
    }

    impl StubChain {
        fn with_claim(id: B256, status: u8, bond_paid: bool, commitment: B256) -> Self {
            let s = Self::default();
            s.claims.lock().unwrap().insert(
                id,
                ClaimState {
                    status,
                    bond_paid,
                    commitment,
                },
            );
            s
        }

        fn sent(&self) -> usize {
            self.sent.lock().unwrap().len()
        }
    }

    impl ArmingChain for &StubChain {
        fn claim(&self, id: B256) -> impl Future<Output = Result<ClaimState>> + Send {
            let c = self.claims.lock().unwrap().get(&id).copied();
            async move { c.context("unknown claim") }
        }

        fn arming(&self, id: B256) -> impl Future<Output = Result<(B256, u64)>> + Send {
            let a = self.armings.lock().unwrap().get(&id).copied().unwrap_or_default();
            async move { Ok(a) }
        }

        fn arm(&self, id: B256, received_at: u64, signature: Bytes) -> impl Future<Output = Result<B256>> + Send {
            // What the bond does: the arming is for the claim's current commitment.
            let commitment = self.claims.lock().unwrap()[&id].commitment;
            self.armings.lock().unwrap().insert(id, (commitment, received_at));
            self.sent.lock().unwrap().push((id, received_at, signature));
            async move { Ok(B256::repeat_byte(0xee)) }
        }
    }

    /// A MOCK backend: `misses` answers of `404 no_receipt` (or `429` with `limited`), then the
    /// receipt for `commitment`.
    #[derive(Clone)]
    struct StubBackend {
        calls: Arc<AtomicUsize>,
        misses: usize,
        limited: bool,
        commitment: Arc<Mutex<B256>>,
    }

    async fn receipt_route(State(b): State<StubBackend>, Path(claim): Path<B256>) -> Response {
        let n = b.calls.fetch_add(1, Ordering::SeqCst);
        if n < b.misses {
            let (status, code) = if b.limited {
                (HttpStatus::TOO_MANY_REQUESTS, "rate_limited")
            } else {
                (HttpStatus::NOT_FOUND, "no_receipt")
            };
            return (status, Json(json!({ "error": "x", "code": code }))).into_response();
        }
        let commitment = *b.commitment.lock().unwrap();
        Json(json!({
            "claimId": claim,
            "commitment": commitment,
            "receivedAt": 1_000,
            "signer": Address::repeat_byte(0x55),
            "signature": Bytes::from(vec![0x51; 65]),
        }))
        .into_response()
    }

    async fn backend(misses: usize, limited: bool, commitment: B256) -> (String, StubBackend) {
        let state = StubBackend {
            calls: Arc::default(),
            misses,
            limited,
            commitment: Arc::new(Mutex::new(commitment)),
        };
        let app = Router::new().route("/claims/{claim}/receipt", post(receipt_route)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), state)
    }

    fn calls(b: &StubBackend) -> usize {
        b.calls.load(Ordering::SeqCst)
    }

    /// The receipt arrives late: the backend has none until the nodes hold the destination; the
    /// bot backs off (30 s, then 60 s) and arms the claim once it gets one; then sends nothing more.
    #[tokio::test]
    async fn a_late_receipt_is_waited_for_then_armed() {
        let chain = StubChain::with_claim(CLAIM, CLAIM_PENDING, false, COMMITMENT);
        let (url, b) = backend(2, false, COMMITMENT).await;
        let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
        armer.track(CLAIM).unwrap();

        let r = armer.pass(1_000).await;
        assert_eq!((r.waiting, r.armed, calls(&b), chain.sent()), (1, 0, 1, 0), "404: wait");
        let r = armer.pass(1_029).await;
        assert_eq!((r, calls(&b)), (PassReport::default(), 1), "not due before 30 s");
        let r = armer.pass(1_030).await;
        assert_eq!((r.waiting, calls(&b), chain.sent()), (1, 2, 0), "404 again: wait 60 s");
        assert_eq!(armer.pass(1_089).await, PassReport::default());
        let r = armer.pass(1_090).await;
        assert_eq!((r.armed, calls(&b), chain.sent()), (1, 3, 1), "armed with the receipt");
        assert_eq!(chain.sent.lock().unwrap()[0], (CLAIM, 1_000, Bytes::from(vec![0x51; 65])));
        assert_eq!(armer.tracked(), vec![(CLAIM, Some(COMMITMENT))]);
        // Rechecked an hour later: armed for its commitment, nothing sent or asked.
        let r = armer.pass(1_090 + ARMED_RECHECK).await;
        assert_eq!((r.already_armed, calls(&b), chain.sent()), (1, 3, 1));
    }

    /// A claim already armed for its current commitment (by the bot before a restart, or by anyone):
    /// no receipt is asked for and no transaction sent.
    #[tokio::test]
    async fn an_armed_claim_sends_nothing() {
        let chain = StubChain::with_claim(CLAIM, CLAIM_PENDING, false, COMMITMENT);
        chain.armings.lock().unwrap().insert(CLAIM, (COMMITMENT, 900));
        let (url, b) = backend(0, false, COMMITMENT).await;
        let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
        armer.track(CLAIM).unwrap();
        let r = armer.pass(1_000).await;
        assert_eq!((r.already_armed, r.armed, calls(&b), chain.sent()), (1, 0, 0, 0));
        assert_eq!(armer.tracked(), vec![(CLAIM, Some(COMMITMENT))]);
    }

    /// The claim's commitment changed since it was armed: the old arming no longer counts, and the
    /// bot arms it again with a receipt for the new commitment (one for the old one arms nothing).
    #[tokio::test]
    async fn a_changed_commitment_is_armed_again() {
        let chain = StubChain::with_claim(CLAIM, CLAIM_PENDING, false, NEW_COMMITMENT);
        chain.armings.lock().unwrap().insert(CLAIM, (COMMITMENT, 900));
        let (url, b) = backend(0, false, COMMITMENT).await;
        let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
        armer.track(CLAIM).unwrap();
        // The backend still serves the old destination's receipt: wait.
        let r = armer.pass(1_000).await;
        assert_eq!((r.waiting, calls(&b), chain.sent()), (1, 1, 0));
        // Then the new one's: armed again.
        *b.commitment.lock().unwrap() = NEW_COMMITMENT;
        let r = armer.pass(1_030).await;
        assert_eq!((r.armed, calls(&b), chain.sent()), (1, 2, 1));
        assert_eq!(chain.armings.lock().unwrap()[&CLAIM], (NEW_COMMITMENT, 1_000));
        assert_eq!(armer.tracked(), vec![(CLAIM, Some(NEW_COMMITMENT))]);
    }

    /// Paid, paid by the bonds, written off or compensated: skipped and dropped, without asking the
    /// backend or sending anything. A disputed claim waits.
    #[tokio::test]
    async fn paid_claims_are_skipped() {
        for (status, bond_paid) in [
            (CLAIM_PAID, false),
            (CLAIM_PENDING, true),
            (CLAIM_WRITTEN_OFF, false),
            (CLAIM_COMPENSATED, false),
        ] {
            let chain = StubChain::with_claim(CLAIM, status, bond_paid, COMMITMENT);
            let (url, b) = backend(0, false, COMMITMENT).await;
            let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
            armer.track(CLAIM).unwrap();
            let r = armer.pass(1_000).await;
            assert_eq!((r.done, calls(&b), chain.sent()), (1, 0, 0), "status {status}, bond paid {bond_paid}");
            assert!(armer.tracked().is_empty());
        }
        let chain = StubChain::with_claim(CLAIM, CLAIM_DISPUTED, false, COMMITMENT);
        let (url, b) = backend(0, false, COMMITMENT).await;
        let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
        armer.track(CLAIM).unwrap();
        let r = armer.pass(1_000).await;
        assert_eq!((r.waiting, calls(&b), chain.sent(), armer.tracked().len()), (1, 0, 0, 1));
    }

    /// `429`: the pass stops and receipt requests pause for a minute, for every claim.
    #[tokio::test]
    async fn the_rate_limit_pauses_receipt_requests() {
        let chain = StubChain::with_claim(CLAIM, CLAIM_PENDING, false, COMMITMENT);
        let other = B256::repeat_byte(0xc2);
        chain.claims.lock().unwrap().insert(
            other,
            ClaimState {
                status: CLAIM_PENDING,
                bond_paid: false,
                commitment: COMMITMENT,
            },
        );
        let (url, b) = backend(1, true, COMMITMENT).await;
        let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), None).unwrap();
        armer.track(CLAIM).unwrap();
        armer.track(other).unwrap();
        let r = armer.pass(1_000).await;
        assert!(r.rate_limited);
        assert_eq!(calls(&b), 1, "the pass stopped at the first 429");
        // Both claims are due, but no request goes out before the pause ends.
        let r = armer.pass(1_030).await;
        assert_eq!((r.waiting, calls(&b)), (2, 1));
        // After it, each claim gets its receipt when it is next due.
        let r = armer.pass(1_060).await;
        assert_eq!((r.armed, calls(&b)), (1, 2));
        let r = armer.pass(1_090).await;
        assert_eq!((r.armed, calls(&b), chain.sent()), (1, 3, 2));
    }

    /// The tracked claims survive a restart, with what each was armed for; the file is the bot's
    /// own (0600 on Unix).
    #[tokio::test]
    async fn tracked_claims_survive_a_restart() {
        let path = std::env::temp_dir().join(format!("zecured-bots-claims-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let chain = StubChain::with_claim(CLAIM, CLAIM_PENDING, false, COMMITMENT);
        let other = B256::repeat_byte(0xc3);
        let (url, _) = backend(0, false, COMMITMENT).await;
        {
            let mut armer = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), Some(path.clone())).unwrap();
            armer.track(CLAIM).unwrap();
            armer.track(other).unwrap();
            armer.track(other).unwrap(); // idempotent
            chain.claims.lock().unwrap().insert(
                other,
                ClaimState {
                    status: CLAIM_PENDING,
                    bond_paid: false,
                    commitment: B256::ZERO,
                },
            );
            armer.pass(1_000).await;
        }
        let restarted = Armer::new(&chain, BackendReceipts::new(&url).unwrap(), Some(path.clone())).unwrap();
        let mut tracked = restarted.tracked();
        tracked.sort();
        assert_eq!(tracked, vec![(CLAIM, Some(COMMITMENT)), (other, None)]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// The bot's own claims in its transactions' logs: every `LiquidationRelease` and the market
    /// maker's claim of a `CloseRelease`, from the Registry only.
    #[test]
    fn own_claims_come_from_the_registrys_events() {
        let registry = Address::repeat_byte(0x0e);
        let log = |address: Address, data: LogData| Log { address, data };
        let liquidation = IZecuredRegistry::LiquidationRelease {
            vaultId: B256::repeat_byte(1),
            claimId: B256::repeat_byte(2),
            amount: 100_000,
            recipientCommitment: COMMITMENT,
        };
        let close = IZecuredRegistry::CloseRelease {
            vaultId: B256::repeat_byte(1),
            userClaimId: B256::repeat_byte(3),
            mmClaimId: B256::repeat_byte(4),
            userAmount: 1,
            mmAmount: 2,
            userCommitment: COMMITMENT,
            mmCommitment: NEW_COMMITMENT,
        };
        let logs = [
            log(registry, liquidation.encode_log_data()),
            log(Address::repeat_byte(0x0f), liquidation.encode_log_data()), // not the Registry's
            log(registry, close.encode_log_data()),
            log(registry, LogData::new_unchecked(vec![B256::repeat_byte(9)], Bytes::new())), // another event
        ];
        assert_eq!(own_claims(&logs, registry), vec![B256::repeat_byte(2), B256::repeat_byte(4)]);
    }

    #[test]
    fn the_backoff_doubles_up_to_ten_minutes() {
        let mut t = Tracked::default();
        let waits: Vec<u64> = (0..7)
            .map(|_| {
                backoff(&mut t, 0);
                t.next_try_at
            })
            .collect();
        assert_eq!(waits, vec![30, 60, 120, 240, 480, 600, 600]);
    }
}
