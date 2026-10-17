//! A Zecured signer node: one of five FROST participants per vault.
//!
//! The node never trusts the coordinator. It checks owner requests itself (P0), sorts a vault's
//! notes on its own Zcash view, reads Base events at the safe head (each one checked against the
//! Registry's state), recomputes every payout plan and verifies the PCZT against it before
//! contributing a signature share. It signs no deposit (every deposit is proven, Registry v3), and
//! refunds off-chain only a deposit that no guest of the deployment can land on Base (P5 under
//! I30): every other one leaves the vault as a Refund claim, through a committed payout.

pub mod api;
pub mod auth;
pub mod chain;
pub mod config;
pub mod dkg;
pub mod envelope;
pub mod signing;
pub mod store;

use std::{
    collections::{BTreeSet, HashMap},
    hash::Hash,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy::{
    eips::BlockId,
    primitives::{Address, B256, Bytes},
    providers::Provider,
    sol_types::SolStruct,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tracing::{debug, warn};
use zcash_protocol::TxId;
use zecured_base::{
    Destination, RefundRequest,
    events::{self, ISignerBond, IZecuredIntake, IZecuredRegistry, IZecuredSlasher, VaultEvent},
    operator::{bond_domain, destination_received},
    payout_confirmation, recover_typed, registry_domain, sign_typed, signer_bitmap,
    verify::{VAULT_PENDING, Verdict},
};
use zecured_policy::{
    classify::Landing,
    derive::{deposit_tag, payout_ref},
    ledger::{self, ClaimId, ClaimState, Recipient, commitment},
};

use crate::{
    chain::{VaultNotes, ZcashView, owns_receiver, vault_fvk},
    config::{Identity, NetworkConfig},
    signing::{
        CommitRequest, CommitResponse, Session, ShareRequest, ShareResponse, SignKind, SignedSpendCommit,
        SpendCommitRecord,
    },
    store::{ConfirmedPayout, RefundRecord, Store, VaultRecord},
};

/// Demo switch for the threshold demonstration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Enforce the policy.
    Normal,
    /// DEMO: sign any PCZT without checks (a compromised signer). Only a node built with the
    /// `demo` feature and started with `--demo-allow-compromise` can be switched to it.
    Compromised,
    /// DEMO: refuse to answer (a crashed or partitioned signer).
    Offline,
}

/// Why a mode switch was refused.
#[derive(Debug, thiserror::Error)]
pub enum ModeError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("this node cannot be switched to compromised mode (needs a demo build and --demo-allow-compromise)")]
    NotAllowed,
}

/// A payout this node signed and will confirm once mined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inflight {
    pub claim_ids: Vec<[u8; 32]>,
    pub inputs: Vec<[u8; 32]>,
    /// Id of the transaction this node signed (its shielded sighash, internal byte order).
    /// `None` only for records written before it was kept.
    #[serde(default)]
    pub txid: Option<[u8; 32]>,
    /// The transaction's expiry height: after it, it can no longer be mined.
    #[serde(default)]
    pub expiry_height: u32,
    /// The change it returns to the vault.
    #[serde(default)]
    pub change: u64,
    /// Tier 3 (T3-4): the payout's commitment and every signature the Registry recorded it with,
    /// so that this node can submit it again if Base lost it.
    #[serde(default)]
    pub spend_commit: Option<CommittedSpend>,
}

/// A commitment on Base and the signatures `commitSpend` used, in that order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedSpend {
    pub record: signing::SpendCommitRecord,
    pub sigs: Vec<Bytes>,
}

/// Operator settings of a node (beyond the shared network configuration).
#[derive(Clone, Debug, Default)]
pub struct NodeOptions {
    /// Token for the demo control panel's mode switch. Never set it on a production node.
    pub admin_token: Option<String>,
    /// DEMO: allow switching to [`Mode::Compromised`] (needs the `demo` feature too).
    pub demo_allow_compromise: bool,
    /// Development only: run although the network config names no coordinator, i.e. accept
    /// unauthenticated requests (L-1).
    pub insecure_no_auth: bool,
    /// Light wallet servers that must agree with ours on a deposit (C-1).
    pub lightwalletd_witnesses: Vec<String>,
    /// A Base RPC that must agree with ours on every Registry event (C-2).
    pub base_rpc_witness: Option<String>,
}

/// Signing sessions expire this long after round 1 (L-7).
pub const SESSION_TTL: Duration = Duration::from_secs(300);
/// Most signing sessions kept between the two rounds (L-7).
pub const MAX_SESSIONS: usize = 256;
/// Pending DKGs expire this long after round 1 (L-7).
pub const DKG_TTL: Duration = Duration::from_secs(300);
/// Most DKGs in progress at once (L-7).
pub const MAX_PENDING_DKGS: usize = 64;
/// Sync rounds a Registry event contradicted by the primary RPC's own state is retried before
/// it is skipped for good (C-2, M-9).
const MAX_EVENT_REJECTIONS: u32 = 3;
/// Blocks below both providers' latest block at which an event is checked when the state at its
/// own block is gone (non-archive nodes keep only the last ~128 states).
const RECENT_STATE_MARGIN: u64 = 2;

/// T3-4: Base blocks a payout's commitment must at least be under, on every provider, before this
/// node releases a share for the payout, unless the network config sets `commit_depth_blocks`.
pub const DEFAULT_COMMIT_DEPTH: u64 = 3;
/// Review H-11, T3-4: seconds a payout's commitment must have been on Base before this node
/// releases a share while Base's `safe` head has not reached it yet, unless the network config sets
/// `commit_depth_secs` (60 blocks at 2 s, 600 at 200 ms).
pub const DEFAULT_COMMIT_DEPTH_SECS: u64 = 120;

/// I6: signatures a commitment needs.
const COMMIT_THRESHOLD: usize = 3;
/// T-05: how often a node warns at most about one vault's payable claims that are getting old.
const LIVENESS_WARNING_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub struct Node {
    pub cfg: NetworkConfig,
    pub id: Identity,
    pub store: Mutex<Store>,
    pub zcash: Mutex<ZcashView>,
    pub pending_dkg: Mutex<HashMap<B256, dkg::Pending>>,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub mode: Mutex<Mode>,
    pub admin_token: Option<String>,
    demo_allow_compromise: bool,
    base_rpc_witness: Option<String>,
    /// Signed coordinator requests already accepted (L-1).
    pub replay: auth::ReplayCache,
    /// One Base sync at a time (background loop and `/v1/sync`).
    base_sync: Mutex<()>,
    /// Sync rounds each Registry event was refused, by `(block, log index)`.
    rejected_events: Mutex<HashMap<(u64, u64), u32>>,
    /// T-05: the bonds' liveness period (seconds), read once; `None` without bonds.
    liveness_period: OnceCell<Option<u64>>,
    /// T-05: when this node last warned about each vault's old payable claims.
    liveness_warned: Mutex<HashMap<B256, std::time::Instant>>,
    /// I30: what the deployment's guests can land on Base, read once (every provider agreeing).
    landing: OnceCell<Landing>,
    /// R-5: the Registry's bond contract (`signerBond()`, immutable), read once; zero without bonds.
    bond: OnceCell<Address>,
}

/// What a node plans a vault's payout from (O-4): the vault's claims as the planner counts them, the
/// unpaid claims a commitment on Base binds to the main lineage note (RT-3), the confirmed claims
/// counted unpaid again (rule 3) and the claims set aside near their liveness due time (rules 1 and
/// 2, R-10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planning {
    pub claims: Vec<ledger::Claim>,
    pub bound: Vec<ClaimId>,
    pub reverted: Vec<ClaimId>,
    pub deferred: Vec<ClaimId>,
}

/// R-5: the coordinator asks for a claim's liveness receipt (`POST /v1/receipts`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptIn {
    pub claim_id: B256,
}

/// R-5: `DestinationReceived(claimId, commitment, receivedAt)` signed by this node's attestation
/// key under the bond's domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptOut {
    pub claim_id: B256,
    pub commitment: B256,
    pub received_at: u64,
    pub signature: Bytes,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Base64 (standard alphabet) serde helpers for binary fields in the JSON API.
/// The plan's claims with the salts of their recipients (Tier 3 claim refs), in plan order.
fn plan_salts(store: &Store, v: &VaultRecord, claims: &[[u8; 32]]) -> Result<Vec<([u8; 32], [u8; 32])>> {
    claims
        .iter()
        .map(|id| {
            let claim = v.claims.iter().find(|c| c.id == *id).context("a planned claim is unknown")?;
            let recipient = store.recipient(&claim.commitment).context("a planned claim has no recipient")?;
            Ok((*id, recipient.salt))
        })
        .collect()
}

pub mod b64 {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecipientIn {
    /// Raw 43-byte Orchard-protocol receiver, hex.
    pub receiver: String,
    /// 32-byte salt, hex.
    pub salt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationIn {
    #[serde(flatten)]
    pub recipient: RecipientIn,
    pub nonce: u64,
    pub deadline: u64,
    pub signature: Bytes,
}

/// P5: the owner's request to refund a deposit that no guest of the deployment can land on Base.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundIn {
    pub note_id: B256,
    #[serde(flatten)]
    pub recipient: RecipientIn,
    pub nonce: u64,
    pub deadline: u64,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmIn {
    pub claim_ids: Vec<B256>,
    pub txid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmResponse {
    pub index: u16,
    pub signature: Bytes,
}

fn parse_recipient(r: &RecipientIn) -> Result<Recipient> {
    let receiver: [u8; 43] = hex::decode(r.receiver.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("receiver must be 43 bytes"))?;
    ensure!(
        bool::from(orchard::Address::from_raw_address_bytes(&receiver).is_some()),
        "not a valid Orchard-protocol (Ironwood) receiver"
    );
    let salt: [u8; 32] = hex::decode(r.salt.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("salt must be 32 bytes"))?;
    Ok(Recipient { receiver, salt })
}

/// Inserts `value` under `key` after dropping the entries older than `ttl`; refuses a new key
/// once `cap` entries are kept (L-7: signing sessions and pending DKGs, with their secrets, do
/// not pile up in memory).
fn insert_bounded<K: Eq + Hash, V>(
    map: &mut HashMap<K, V>,
    key: K,
    value: V,
    age: impl Fn(&V) -> Duration,
    ttl: Duration,
    cap: usize,
) -> Result<()> {
    map.retain(|_, v| age(v) < ttl);
    ensure!(map.len() < cap || map.contains_key(&key), "too many requests in progress");
    map.insert(key, value);
    Ok(())
}

/// Records a confirmed payout on its vault: its claims are paid and the confirmation is kept
/// (H-5). The in-flight record is cleared only if it is still this payout (L-6: never drop a
/// newer one).
fn settle_payout(v: &mut VaultRecord, paid: &Inflight, txid: [u8; 32]) {
    for claim in v.claims.iter_mut().filter(|x| paid.claim_ids.contains(&x.id)) {
        claim.state = ClaimState::Paid;
    }
    if v.inflight.as_ref() == Some(paid) {
        v.inflight = None;
    }
    let record = ConfirmedPayout {
        claim_ids: paid.claim_ids.clone(),
        txid,
    };
    if !v.confirmed.contains(&record) {
        v.confirmed.push(record);
    }
}

/// R-15: the largest amount the Intake's work tiers admit (its last tier's `maxAmountZat`; tiers
/// ascend, the largest is taken whatever the order).
pub fn largest_tier(tiers: &[IZecuredIntake::WorkTier]) -> Result<u64> {
    tiers.iter().map(|t| t.maxAmountZat).max().context("the Intake has no work tier")
}

/// The claims Base has pending (a commitment may bind them, RT-3).
pub fn unpaid_ids(claims: &[ledger::Claim]) -> Vec<ClaimId> {
    claims.iter().filter(|c| c.state == ClaimState::Unpaid).map(|c| c.id).collect()
}

/// The claims confirmed paid and not proven: O-4 rule 3 counts those whose input is still unspent
/// unpaid again.
pub fn confirmed_ids(claims: &[ledger::Claim]) -> Vec<ClaimId> {
    claims.iter().filter(|c| c.state == ClaimState::Paid && !c.proven).map(|c| c.id).collect()
}

/// Whether `wanted` is a non-empty subsequence of `all` (same order, each once): the claims of one
/// payout a confirmation may name (R-1: the payout's claims still pending on Base).
pub fn confirmable_subset(wanted: &[ClaimId], all: &[ClaimId]) -> bool {
    if wanted.is_empty() {
        return false;
    }
    let mut rest = all.iter();
    wanted.iter().all(|w| rest.any(|a| a == w))
}

/// D18: the nodes that may be started with `--demo-allow-compromise`. The demo build allows two
/// (4 and 5), never a signing threshold; only the separate theft demo's build (`demo-tier3`,
/// T3-6) allows a third (3), so that a theft can really be signed there.
pub fn may_be_compromised(index: u16) -> bool {
    let first = if cfg!(feature = "demo-tier3") { 3 } else { 4 };
    (first..=5).contains(&index)
}

/// H-2: a vault never pays itself. A destination at one of its own addresses would come back as a
/// note outside its books.
fn ensure_not_own_receiver(v: &VaultRecord, receiver: &[u8; 43]) -> Result<()> {
    ensure!(!owns_receiver(&vault_fvk(v)?, receiver), "a vault cannot pay one of its own addresses");
    Ok(())
}

impl Node {
    pub fn open(cfg: NetworkConfig, id: Identity, data_dir: &Path, opts: NodeOptions) -> Result<Self> {
        // L-1: without a coordinator address every request is unauthenticated (D16).
        ensure!(
            cfg.coordinator.is_some() || opts.insecure_no_auth,
            "the network config names no coordinator, so the node API would accept unauthenticated requests; \
             set `coordinator`, or pass --insecure-no-auth (local development only)"
        );
        ensure!(
            !opts.demo_allow_compromise || cfg!(feature = "demo"),
            "--demo-allow-compromise needs a build with the `demo` feature"
        );
        ensure!(
            !opts.demo_allow_compromise || may_be_compromised(id.index),
            "--demo-allow-compromise: node {} can never be compromised in this build (D18, T3-6)",
            id.index
        );
        #[cfg(feature = "demo-tier3")]
        {
            let mut signers: Vec<alloy::primitives::Address> = cfg.nodes.iter().map(|n| n.eth).collect();
            signers.push(id.eth.address());
            zecured_base::theft_demo_guard::check(cfg.registry, &signers)?;
        }
        store::private_dir(data_dir)?;
        let store = Store::open(&data_dir.join("store"))?;
        let zcash = ZcashView::with_witnesses(&cfg.lightwalletd_url, opts.lightwalletd_witnesses);
        Ok(Self {
            cfg,
            id,
            store: Mutex::new(store),
            zcash: Mutex::new(zcash),
            pending_dkg: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            mode: Mutex::new(Mode::Normal),
            admin_token: opts.admin_token,
            demo_allow_compromise: opts.demo_allow_compromise,
            base_rpc_witness: opts.base_rpc_witness,
            replay: auth::ReplayCache::default(),
            base_sync: Mutex::new(()),
            rejected_events: Mutex::new(HashMap::new()),
            liveness_period: OnceCell::new(),
            liveness_warned: Mutex::new(HashMap::new()),
            landing: OnceCell::new(),
            bond: OnceCell::new(),
        })
    }

    pub async fn mode(&self) -> Mode {
        *self.mode.lock().await
    }

    fn domain(&self) -> alloy::sol_types::Eip712Domain {
        registry_domain(self.cfg.chain_id, self.cfg.registry)
    }

    /// Scans a vault without holding the store lock (M-7: a slow light wallet server must not
    /// freeze the node), then records new P1 decisions and returns the store, locked, with the
    /// vault's current record and its notes. The main lineage is every unspent lineage member: the
    /// planning step ([`Node::planning`]) keeps only the planned change (H-2), once it knows which
    /// claims count unpaid (O-4 rule 3).
    async fn scan_vault(&self, vault_id: &B256) -> Result<(MutexGuard<'_, Store>, VaultRecord, VaultNotes)> {
        // I30: unknown (Base unreachable), nothing is refundable off-chain; payouts go on.
        let landing = match self.landing().await {
            Ok(l) => Some(l),
            Err(e) => {
                debug!("the deployment's guests are unknown, no off-chain refund: {e:#}");
                None
            }
        };
        let mut snapshot = self.store.lock().await.vaults.get(vault_id).context("unknown vault")?.clone();
        let notes = self.zcash.lock().await.vault_notes(&mut snapshot, self.cfg.confirmations, landing).await?;
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(vault_id).context("unknown vault")?.clone();
        if chain::merge_decisions(&mut v.decisions, &snapshot.decisions)? {
            store.save_vault(v.clone())?;
        }
        Ok((store, v, notes))
    }

    // ----- vault creation -----

    pub async fn dkg_round1(&self, open: dkg::VaultOpen) -> Result<dkg::Round1Out> {
        let tip = self.zcash.lock().await.tip().await?;
        {
            let store = self.store.lock().await;
            dkg::check_open(&self.cfg, &store, &open, now(), tip)?;
        }
        let vault_id = open.vault_id;
        let mut all = self.pending_dkg.lock().await;
        // L-9: one opening at a time per account and per owner; completed vaults are refused by
        // `check_open` (and again before round 3 saves the vault).
        ensure!(
            !all.iter().any(|(id, p)| *id != vault_id && p.age() < DKG_TTL && p.same_parties(&open)),
            "account or owner already has a vault being opened"
        );
        let (pending, out) = dkg::round1(self.id.index, open)?;
        insert_bounded(&mut all, vault_id, pending, dkg::Pending::age, DKG_TTL, MAX_PENDING_DKGS)?;
        Ok(out)
    }

    pub async fn dkg_round2(&self, input: dkg::Round2In) -> Result<dkg::Round2Out> {
        let mut pending = self.pending_dkg.lock().await;
        let p = pending.get_mut(&input.vault_id).context("no DKG in progress for this vault")?;
        ensure!(p.age() < DKG_TTL, "this vault opening expired");
        dkg::round2(&self.cfg, &self.id.x25519, self.id.index, p, &input)
    }

    pub async fn dkg_round3(&self, input: dkg::Round3In) -> Result<dkg::Round3Out> {
        let p = self.pending_dkg.lock().await.remove(&input.vault_id).context("no DKG in progress for this vault")?;
        ensure!(p.age() < DKG_TTL, "this vault opening expired");
        let mut store = self.store.lock().await;
        dkg::round3(&self.cfg, &self.id.x25519, &self.id.eth, self.id.index, p, &input, &mut store)
    }

    // ----- recipients and owner requests (P0) -----

    pub async fn add_recipient(&self, r: RecipientIn) -> Result<B256> {
        let recipient = parse_recipient(&r)?;
        let mut store = self.store.lock().await;
        // H-2: a third party's recipient may not be any vault's own address.
        for v in store.vaults.values() {
            ensure_not_own_receiver(v, &recipient.receiver)?;
        }
        Ok(B256::from(store.add_recipient(recipient)?))
    }

    pub async fn destination(&self, vault_id: B256, d: DestinationIn) -> Result<B256> {
        let recipient = parse_recipient(&d.recipient)?;
        let c = recipient.commitment();
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        let msg = Destination {
            vaultId: vault_id,
            commitment: B256::from(c),
            nonce: d.nonce,
            deadline: d.deadline,
        };
        ensure!(recover_typed(&self.domain(), &msg, &d.signature)? == v.owner, "not signed by the vault owner");
        ensure!(d.deadline >= now(), "request expired");
        ensure_not_own_receiver(&v, &recipient.receiver)?;
        ensure!(v.use_nonce(d.nonce), "nonce already used");
        store.save_vault(v)?;
        store.add_recipient(recipient)?;
        Ok(B256::from(c))
    }

    /// P5 under I30: the owner asks for a deposit back that no guest of the deployment can land on
    /// Base (another address of the vault; under journal v1 also another amount, a late payment or
    /// a transaction with other pools; under v2 a note below the Intake's refund floor). Any other
    /// deposit leaves the vault only as a Refund claim, once a proof lands it: refunding it here
    /// would turn into an uncommitted spend of a lineage note (F1) the day someone proves it.
    pub async fn refund_request(&self, vault_id: B256, r: RefundIn) -> Result<()> {
        let recipient = parse_recipient(&r.recipient)?;
        // The owner's signature is checked before anything is scanned.
        {
            let store = self.store.lock().await;
            let v = store.vaults.get(&vault_id).context("unknown vault")?;
            let msg = RefundRequest {
                vaultId: vault_id,
                noteId: r.note_id,
                commitment: B256::from(commitment(&recipient.salt, &recipient.receiver)),
                nonce: r.nonce,
                deadline: r.deadline,
            };
            ensure!(recover_typed(&self.domain(), &msg, &r.signature)? == v.owner, "not signed by the vault owner");
            ensure!(r.deadline >= now(), "request expired");
            ensure_not_own_receiver(v, &recipient.receiver)?;
        }
        let (v, note) = {
            let (_store, v, notes) = self.scan_vault(&vault_id).await?;
            let (_, note) = notes.refundable.iter().find(|(id, _)| *id == r.note_id.0).context(
                "not a deposit that may be refunded off-chain: one a guest can land on Base is refunded as a Refund claim (I30)",
            )?;
            (v, note.clone())
        };
        self.check_refund_on_base(&v, &note).await?;
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        ensure!(v.use_nonce(r.nonce), "nonce already used");
        v.refunds.retain(|x| x.note_id != r.note_id.0);
        v.refunds.push(RefundRecord {
            note_id: r.note_id.0,
            recipient,
        });
        store.save_vault(v)?;
        Ok(())
    }

    /// I30: what the deployment's guests can land on Base, from the Intake the Registry names: its
    /// journal version, for v2 its refund floor, and its largest work tier (R-15: nothing above it
    /// lands). Read once, on every Base provider, which must agree: a lying provider must never
    /// make a note look unprovable.
    pub async fn landing(&self) -> Result<Landing> {
        let landing = self
            .landing
            .get_or_try_init(|| async {
                let mut agreed: Option<Landing> = None;
                for provider in self.base_providers()? {
                    let intake = IZecuredRegistry::new(self.cfg.registry, &provider)
                        .intake()
                        .call()
                        .await
                        .context("Registry.intake")?;
                    ensure!(intake != Address::ZERO, "the Registry names no Intake");
                    let intake = IZecuredIntake::new(intake, &provider);
                    let largest_tier = largest_tier(&intake.workTiers().call().await.context("Intake.workTiers")?)?;
                    let landing = if intake.journalV2().call().await.context("Intake.journalV2")? {
                        Landing::WithRefunds {
                            min_refund: intake.minRefundZat().call().await.context("Intake.minRefundZat")?,
                            largest_tier,
                        }
                    } else {
                        Landing::ActivateOnly { largest_tier }
                    };
                    match agreed {
                        Some(first) => ensure!(first == landing, "Base providers disagree on what the Intake takes"),
                        None => agreed = Some(landing),
                    }
                }
                agreed.context("no Base provider")
            })
            .await?;
        Ok(*landing)
    }

    /// Before accepting or signing an off-chain refund, on every Base provider: the note is no
    /// lineage note of any vault (RT-9: a change returned to the external address by colluding
    /// signers looks like a deposit to refund, and is not) and has never landed (its tag unused).
    /// That no guest can land it at all is checked on the scan ([`chain::refundable_offchain`]).
    async fn check_refund_on_base(&self, v: &VaultRecord, note: &zecured_zcash_ops::scan::ReceivedNote) -> Result<()> {
        let cmx = orchard::note::ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
        let tag = B256::from(deposit_tag(&v.ufvk, &cmx));
        for provider in self.base_providers()? {
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
            let lineage = registry.lineageOf(tag).call().await.context("Registry.lineageOf")?;
            ensure!(lineage.vaultId == B256::ZERO, "this note is a lineage note on Base");
            let used = registry.depositTagUsed(tag).call().await.context("Registry.depositTagUsed")?;
            ensure!(!used, "this note landed on Base");
        }
        Ok(())
    }

    // ----- signing -----

    pub async fn sign_commit(&self, req: CommitRequest) -> Result<CommitResponse> {
        #[cfg(feature = "demo")]
        if self.mode().await == Mode::Compromised {
            let (session, commitments) = {
                let store = self.store.lock().await;
                let v = store.vaults.get(&req.vault_id).context("unknown vault")?;
                signing::commit_unchecked(v, &req)?
            };
            return self.start_session(&req, session, &commitments, None).await;
        }

        let (store, v, mut notes) = self.scan_vault(&req.vault_id).await?;
        drop(store);
        if let SignKind::Refund { .. } = &req.kind {
            // P5 (I30): a deposit no guest can land is no lineage note, so there is nothing to
            // commit on Base; it is checked there again, on every provider, before signing.
            let (plan, inputs) = {
                let store = self.store.lock().await;
                signing::expected_plan(&store, &v, &v.claims, &notes, &req.kind, now(), &[])?
            };
            for note in &inputs {
                self.check_refund_on_base(&v, note).await?;
            }
            let (session, commitments) =
                signing::commit(&v, &plan, &inputs, notes.tip, &req, v.inflight.as_ref(), &notes.unspent)?;
            return self.start_session(&req, session, &commitments, None).await;
        }
        // O-4 and RT-3: the claims as every node and the coordinator plan from them, the claims a
        // commitment on Base binds to the main lineage note first (Base calls never hold the store
        // lock, M-7).
        let planning = self.planning(&v, &mut notes, now()).await?;
        let store = self.store.lock().await;
        let (plan, inputs) =
            signing::expected_plan(&store, &v, &planning.claims, &notes, &req.kind, now(), &planning.bound)?;
        // Tier 3: the salts of the plan's recipients, for the claim refs.
        let claims = plan_salts(&store, &v, &plan.claims)?;
        drop(store);
        // H-1 (inside `commit`): no other payout while one this node signed can still be mined.
        let (mut session, commitments) =
            signing::commit(&v, &plan, &inputs, notes.tip, &req, v.inflight.as_ref(), &notes.unspent)?;
        // Tier 3 (docs/TIER3.md § 5): sign the payout's commitment; the shares wait until Base has it.
        let [input] = inputs.as_slice() else {
            bail!("a payout spends exactly one lineage note ({} here)", inputs.len());
        };
        let input_cmx = orchard::note::ExtractedNoteCommitment::from(input.note.commitment()).to_bytes();
        let record = signing::spend_commit(v.vault_id, &v.ufvk, &input_cmx, &claims, &session.verified, plan.change)?;
        self.check_commit_preconditions(&v, &record).await?;
        let signature = sign_typed(&self.id.eth, &self.domain(), &record.message())?;
        session.spend_commit = Some(record.clone());
        let spend_commit = Some(SignedSpendCommit { record, signature });
        self.start_session(&req, session, &commitments, spend_commit).await
    }

    /// The claims this node plans the vault's next payout from, and the claims a commitment on Base
    /// binds to its main lineage note (RT-3), read from every Base provider, which must agree, and
    /// planned by the rules every node and the coordinator apply alike (O-4):
    /// - rule 3: claims confirmed paid (not proven) whose input is still an unspent lineage note of
    ///   the vault count unpaid again ([`ledger::revert_unlanded`]); then the lineage is restricted
    ///   to the planned change of that book value (H-2);
    /// - rules 1 and 2 (R-10): a claim the bonds may pay within one payout lifetime is not committed
    ///   ([`ledger::defer_near_due`], with the bond's `livenessDue`).
    async fn planning(&self, v: &VaultRecord, notes: &mut VaultNotes, now: u64) -> Result<Planning> {
        let tag_of = |n: &zecured_zcash_ops::scan::ReceivedNote| B256::from(deposit_tag(&v.ufvk, &chain::note_cmx(n)));
        let candidates: Vec<B256> = notes.lineage.iter().map(tag_of).collect();
        let mut claims = v.claims.clone();
        let mut inputs = match candidates.is_empty() {
            true => vec![],
            false => self.claim_inputs(&unpaid_ids(&claims)).await?,
        };
        // Rule 3 only when the books do not add up as they are (an input still holds the value of
        // claims Base shows paid): the confirmed claims' inputs are read then, not on every plan.
        let mut reverted = vec![];
        if !candidates.is_empty() && !notes.books_add_up(v.outstanding, &claims) {
            let confirmed = self.claim_inputs(&confirmed_ids(&claims)).await?;
            let unlanded: Vec<ClaimId> = confirmed
                .iter()
                .filter(|(_, input)| *input != B256::ZERO && candidates.contains(input))
                .map(|(id, _)| *id)
                .collect();
            reverted = ledger::revert_unlanded(&mut claims, &unlanded);
            inputs.extend(confirmed);
        }
        if !reverted.is_empty() {
            warn!(
                vault = %hex::encode(&v.vault_id[..4]),
                claims = reverted.len(),
                "claims confirmed paid whose input is still unspent: their payout is planned again, to be proven (O-4)"
            );
        }
        notes.retain_planned_change(ledger::book_value(v.outstanding, &claims));
        let bound = match notes.lineage.as_slice() {
            [input] => {
                let tag = tag_of(input);
                let unpaid = |id: &ClaimId| claims.iter().any(|c| c.id == *id && c.state == ClaimState::Unpaid);
                inputs.iter().filter(|(id, input)| *input == tag && unpaid(id)).map(|(id, _)| *id).collect()
            }
            _ => vec![], // no single lineage note: the plan is refused anyway
        };
        let dues = self.liveness_dues(&claims).await?;
        let due_of = |id: &ClaimId| dues.iter().find(|(c, _)| c == id).map_or(0, |(_, d)| *d);
        let deferred = ledger::defer_near_due(&mut claims, now, self.cfg.payout_lifetime(), due_of);
        if !deferred.is_empty() {
            debug!(
                vault = %hex::encode(&v.vault_id[..4]),
                claims = deferred.len(),
                "claims within one payout lifetime of their liveness due time are not committed (R-10)"
            );
        }
        Ok(Planning {
            claims,
            bound,
            reverted,
            deferred,
        })
    }

    /// The lineage note each of `claims` is bound to on Base (`claimInput`; zero: none yet), read on
    /// every Base provider, which must agree.
    async fn claim_inputs(&self, claims: &[ClaimId]) -> Result<Vec<(ClaimId, B256)>> {
        if claims.is_empty() {
            return Ok(vec![]);
        }
        let mut agreed: Option<Vec<(ClaimId, B256)>> = None;
        for provider in self.base_providers()? {
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
            let mut inputs = Vec::with_capacity(claims.len());
            for id in claims {
                inputs.push((*id, registry.claimInput(B256::from(*id)).call().await.context("Registry.claimInput")?));
            }
            match &agreed {
                Some(first) => ensure!(*first == inputs, "Base providers disagree on the claims' inputs"),
                None => agreed = Some(inputs),
            }
        }
        Ok(agreed.unwrap_or_default())
    }

    /// R-10: when the bonds may pay each payable claim (`livenessDue`; 0: never, or unreadable, as
    /// every node and the coordinator count it), on every Base provider, which must agree. Nothing
    /// without bonds.
    async fn liveness_dues(&self, claims: &[ledger::Claim]) -> Result<Vec<(ClaimId, u64)>> {
        let bond = self.bond().await?;
        let payable: Vec<ClaimId> =
            claims.iter().filter(|c| c.payable() && c.kind.liveness_covered()).map(|c| c.id).collect();
        if bond == Address::ZERO || payable.is_empty() {
            return Ok(vec![]);
        }
        let mut agreed: Option<Vec<(ClaimId, u64)>> = None;
        for provider in self.base_providers()? {
            let bond = ISignerBond::new(bond, &provider);
            let mut dues = Vec::with_capacity(payable.len());
            for id in &payable {
                // A claim the bond cannot price now (a stale feed, R-9) cannot be paid from the bonds
                // either: 0, as everywhere.
                let due = bond.livenessDue(B256::from(*id)).call().await.map(|d| d.dueAt).unwrap_or(0);
                dues.push((*id, due));
            }
            match &agreed {
                Some(first) => ensure!(*first == dues, "Base providers disagree on the claims' liveness due times"),
                None => agreed = Some(dues),
            }
        }
        Ok(agreed.unwrap_or_default())
    }

    /// The Registry's bond contract (`signerBond()`, immutable), read once; zero without bonds.
    async fn bond(&self) -> Result<Address> {
        let bond = self
            .bond
            .get_or_try_init(|| async {
                let provider = chain::base_provider(&self.cfg.base_rpc_url)?;
                IZecuredRegistry::new(self.cfg.registry, &provider)
                    .signerBond()
                    .call()
                    .await
                    .context("Registry.signerBond")
            })
            .await?;
        Ok(*bond)
    }

    /// Tier 3 checks before signing a commitment, on every Base provider: the vault is neither
    /// pending (K3: its reservation can still be released) nor compromised (T-04: the compensator
    /// settles its claims), the input is a lineage note of this vault on Base (else this node's
    /// lineage differs from the Registry's), and the payout keeps every pending claim already
    /// committed against that note (T3-5). The Registry refuses such commitments too; checking
    /// first saves the signing session.
    async fn check_commit_preconditions(&self, v: &VaultRecord, record: &SpendCommitRecord) -> Result<()> {
        let unpaid: Vec<[u8; 32]> = v.claims.iter().filter(|c| c.state == ClaimState::Unpaid).map(|c| c.id).collect();
        let planned: Vec<[u8; 32]> = record.claim_ids.iter().map(|c| c.0).collect();
        for provider in self.base_providers()? {
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
            let status = registry.vault(v.vault_id).call().await.context("Registry.vault")?.status;
            ensure!(status != VAULT_PENDING, "the vault's activation is pending on Base (K3)");
            let slasher = registry.slasher().call().await.context("Registry.slasher")?;
            if slasher != Address::ZERO {
                let compromised = IZecuredSlasher::new(slasher, &provider)
                    .isCompromised(v.vault_id)
                    .call()
                    .await
                    .context("slasher.isCompromised")?;
                ensure!(!compromised, "the vault is compromised (T-04): the compensator settles its claims");
            }
            let lineage = registry.lineageOf(record.input_tag).call().await.context("Registry.lineageOf")?;
            ensure!(lineage.vaultId == v.vault_id, "the payout's input is not a lineage note of this vault on Base");
            let mut bound = vec![];
            for id in &unpaid {
                let input = registry.claimInput(B256::from(*id)).call().await.context("Registry.claimInput")?;
                if input == record.input_tag {
                    bound.push(*id);
                }
            }
            signing::check_bound_claims(&planned, &bound)?;
            // RT-4: the Registry accepts from a note only the claim list of its first commitment.
            let set = registry.inputClaimSet(record.input_tag).call().await.context("Registry.inputClaimSet")?;
            let ours = alloy::primitives::keccak256(planned.concat());
            ensure!(set == B256::ZERO || set == ours, "a commitment from this lineage note carries another claim list");
        }
        Ok(())
    }

    /// T3-4: the shares of a payout leave this node only if it holds every signature its
    /// commitment was recorded with (so it can submit it again) and every Base provider it uses
    /// shows that very record deep enough: at its `safe` head, or, while that lags, in a block
    /// `commit_depth_secs` old (review H-11), and in any case `commit_depth_blocks` deep.
    async fn check_commitment_on_base(&self, record: &SpendCommitRecord, sigs: &[Bytes]) -> Result<()> {
        let msg = record.message();
        let domain = self.domain();
        let mut set: Vec<Address> = self.cfg.nodes.iter().map(|n| n.eth).collect();
        set.sort();
        let mut signers: Vec<Address> = Vec::with_capacity(sigs.len());
        for sig in sigs {
            let signer = recover_typed(&domain, &msg, sig)?;
            ensure!(set.contains(&signer), "a commitment signature is not a signer's");
            ensure!(signers.last().is_none_or(|prev| *prev < signer), "commitment signatures out of signer order");
            signers.push(signer);
        }
        ensure!(signers.len() >= COMMIT_THRESHOLD, "fewer than {COMMIT_THRESHOLD} commitment signatures");
        let bitmap = signer_bitmap(&set, &signers);
        let hash = msg.eip712_hash_struct();
        for provider in self.base_providers()? {
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
            let recorded_at = |block: u64| {
                let call = registry.spendRecord(record.payout_ref).block(BlockId::number(block));
                async move {
                    let on_base = call.call().await.context("Registry.spendRecord")?;
                    anyhow::Ok(
                        on_base.recordHash == hash
                            && on_base.vaultId == record.vault_id
                            && on_base.signerBitmap == bitmap,
                    )
                }
            };
            let latest = events::latest_block(&provider).await.context("Base latest block")?;
            let safe = events::safe_head(&provider).await.ok();
            let timestamp_of = |number: u64| {
                let provider = &provider;
                async move {
                    Ok(provider
                        .get_block_by_number(number.into())
                        .await
                        .context("get block")?
                        .context("block not found")?
                        .header
                        .timestamp)
                }
            };
            let deep = commitment_deep_enough(&self.cfg, latest, safe, timestamp_of, recorded_at).await?;
            ensure!(
                deep,
                "the payout's commitment is on Base neither under the safe head nor {} s deep with these signatures",
                self.cfg.commit_depth_secs
            );
        }
        Ok(())
    }

    fn base_providers(&self) -> Result<Vec<impl Provider + use<>>> {
        let mut providers = vec![chain::base_provider(&self.cfg.base_rpc_url)?];
        if let Some(w) = self.base_rpc_witness.as_deref() {
            providers.push(chain::base_provider(w)?);
        }
        Ok(providers)
    }

    async fn start_session(
        &self,
        req: &CommitRequest,
        session: Session,
        commitments: &[reddsa::frost::redpallas::round1::SigningCommitments],
        spend_commit: Option<SignedSpendCommit>,
    ) -> Result<CommitResponse> {
        let commitments = commitments.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?;
        let mut sessions = self.sessions.lock().await;
        let age = |s: &Session| s.created.elapsed();
        insert_bounded(&mut sessions, req.session_id.clone(), session, age, SESSION_TTL, MAX_SESSIONS)?;
        Ok(CommitResponse {
            index: self.id.index,
            commitments,
            spend_commit,
        })
    }

    pub async fn sign_share(&self, req: ShareRequest) -> Result<ShareResponse> {
        let session =
            self.sessions.lock().await.remove(&req.session_id).context("unknown or finished signing session")?;
        ensure!(session.created.elapsed() < SESSION_TTL, "signing session expired");
        let mut inflight = session.inflight();
        if inflight.is_some() {
            // Tier 3 (T3-4), before the store lock: Base calls must never hold it (M-7).
            let record = session.spend_commit.as_ref().context("payout without a signed commitment")?;
            self.check_commitment_on_base(record, &req.spend_commit_sigs).await?;
            if let Some(next) = inflight.as_mut() {
                next.spend_commit = Some(CommittedSpend {
                    record: record.clone(),
                    sigs: req.spend_commit_sigs.clone(),
                });
            }
        }
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&session.vault_id).context("unknown vault")?.clone();
        if let Some(next) = &inflight {
            // H-1: the payout in flight must still be the one round 1 saw (or this one).
            signing::check_release(v.inflight.as_ref(), session.prior_inflight, session.verified.sighash)?;
            ensure!(next.txid.is_some(), "payout without a transaction id");
        }
        let shares = signing::share(self.id.index, &v, &session, &req)?;
        // Recorded before any share leaves the node; a restart keeps it.
        if let Some(next) = inflight {
            v.inflight = Some(next);
            store.save_vault(v)?;
        }
        Ok(ShareResponse {
            index: self.id.index,
            shares,
        })
    }

    /// Signs `PayoutConfirmation(vaultId, payoutRef, claimIdsHash)` once the payout this node signed
    /// is mined and confirmed (R-1: the payout reference is this node's own, derived from the very
    /// transaction it signed). It names some or all of the payout's claims, in the payout's order:
    /// the coordinator leaves out those Base no longer has pending, and never a reopened claim,
    /// which only a proof of its payout settles again.
    pub async fn confirm(&self, vault_id: B256, c: ConfirmIn) -> Result<ConfirmResponse> {
        let wanted: Vec<[u8; 32]> = c.claim_ids.iter().map(|x| x.0).collect();
        // The coordinator sends the display form (byte-reversed hex); compare internal bytes.
        let txid = TxId::from_hex(&c.txid).context("invalid txid")?;
        let txid_bytes = *txid.as_ref();
        let (inflight, ufvk) = {
            let store = self.store.lock().await;
            let v = store.vaults.get(&vault_id).context("unknown vault")?;
            ensure!(
                !wanted.iter().any(|c| v.reopened.contains(c)),
                "a reopened claim is settled only by a proof of its payout, never by a confirmation (R-1)"
            );
            // H-5: a confirmation already given is given again (submitting it to Base may
            // have failed after the payout was cleared here).
            if v.confirmed.iter().any(|p| p.txid == txid_bytes && confirmable_subset(&wanted, &p.claim_ids)) {
                return self.payout_confirmation(vault_id, &v.ufvk, &txid_bytes, &c.claim_ids);
            }
            (v.inflight.clone().context("no payout in flight for this vault")?, v.ufvk.clone())
        };
        // H-1: only the very transaction this node signed confirms its claims.
        signing::check_confirmation(&inflight, &wanted, &txid_bytes)?;
        if inflight.txid.is_none() {
            warn!("confirming a payout recorded before transaction ids were kept");
        }
        let (confs, all_spent) = self.zcash.lock().await.spends(&txid, &inflight.inputs).await?;
        ensure!(all_spent, "transaction does not spend the vault notes of this payout");
        ensure!(confs >= self.cfg.confirmations, "payout has {confs} confirmations");

        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        settle_payout(&mut v, &inflight, txid_bytes);
        store.save_vault(v)?;
        drop(store);
        self.payout_confirmation(vault_id, &ufvk, &txid_bytes, &c.claim_ids)
    }

    fn payout_confirmation(
        &self,
        vault_id: B256,
        ufvk: &str,
        txid: &[u8; 32],
        claim_ids: &[B256],
    ) -> Result<ConfirmResponse> {
        let msg = payout_confirmation(vault_id, B256::from(payout_ref(ufvk, txid)), claim_ids);
        Ok(ConfirmResponse {
            index: self.id.index,
            signature: sign_typed(&self.id.eth, &self.domain(), &msg)?,
        })
    }

    /// R-5: this node's liveness receipt for a claim: `DestinationReceived(claimId, commitment,
    /// receivedAt)` under the bond's domain, once it holds the claim's destination preimage and has
    /// validated it (a valid Ironwood receiver that is none of the vault's own addresses, whose
    /// commitment is the claim's current one in this node's books). `receivedAt` is when it first
    /// held that recipient. A changed destination (a refund's, given again) needs a new receipt:
    /// the bond checks the receipt against the claim's current commitment.
    pub async fn receipt(&self, claim_id: B256) -> Result<ReceiptOut> {
        let bond = self.bond().await?;
        ensure!(bond != Address::ZERO, "this deployment has no bonds: no liveness, no receipt");
        let mut store = self.store.lock().await;
        self.issue_receipt(&mut store, claim_id, bond)
    }

    /// [`Node::receipt`] once the bond's address is known.
    pub fn issue_receipt(&self, store: &mut Store, claim_id: B256, bond: Address) -> Result<ReceiptOut> {
        let v = store
            .vaults
            .values()
            .find(|v| v.claims.iter().any(|c| c.id == claim_id.0))
            .context("unknown claim")?
            .clone();
        let claim = v.claims.iter().find(|c| c.id == claim_id.0).context("unknown claim")?;
        ensure!(claim.kind.liveness_covered(), "the bonds do not cover this kind of claim");
        ensure!(claim.state == ClaimState::Unpaid && !claim.bond_paid, "the claim is not pending");
        ensure!(claim.commitment != [0; 32], "the claim has no destination yet");
        let recipient =
            store.recipient(&claim.commitment).context("this node does not hold the claim's destination")?;
        ensure!(recipient.commitment() == claim.commitment, "the destination does not match the claim's commitment");
        ensure!(
            bool::from(orchard::Address::from_raw_address_bytes(&recipient.receiver).is_some()),
            "the destination is not a valid Ironwood receiver"
        );
        ensure_not_own_receiver(&v, &recipient.receiver)?;
        let received_at = store.held_since(&claim.commitment, now())?;
        let commitment = B256::from(claim.commitment);
        let msg = destination_received(claim_id, commitment, received_at);
        let signature = sign_typed(&self.id.eth, &bond_domain(self.cfg.chain_id, bond), &msg)?;
        Ok(ReceiptOut {
            claim_id,
            commitment,
            received_at,
            signature,
        })
    }

    // ----- background -----

    /// Applies new Registry events up to Base's safe head. Every event is checked against the
    /// Registry's state, on the witness RPC too if one is configured (C-2); network calls never
    /// hold the store lock (M-7). Returns the number of vault updates.
    pub async fn sync_base(&self) -> Result<usize> {
        let _one_sync_at_a_time = self.base_sync.lock().await;
        let (cursor, known): (u64, BTreeSet<B256>) = {
            let store = self.store.lock().await;
            (store.base_cursor, store.vaults.keys().copied().collect())
        };
        let primary = chain::base_provider(&self.cfg.base_rpc_url)?;
        let witness = self.base_rpc_witness.as_deref().map(chain::base_provider).transpose()?;
        let mut safe = events::safe_head(&primary).await?;
        if let Some(w) = &witness {
            safe = safe.min(events::safe_head(w).await.context("witness Base RPC")?);
        }
        // H-11: as many blocks per step as `eth_getLogs` is allowed (`ZECURED_LOG_RANGE`).
        let step = events::log_range_ceiling();
        let mut from = cursor.max(self.cfg.registry_from_block.saturating_sub(1)).saturating_add(1);
        let mut applied = 0;
        while from <= safe {
            // Where events are checked if the state at their own block is gone: a recent block both
            // providers still have the state of. Taken again for every step (H-11): at 200 ms
            // blocks, a non-archive provider's ~128 recent states last about 25 seconds.
            let mut latest = primary.get_block_number().await.context("Base block number")?;
            if let Some(w) = &witness {
                latest = latest.min(w.get_block_number().await.context("witness Base RPC")?);
            }
            let recent = latest.saturating_sub(RECENT_STATE_MARGIN).max(safe);
            let to = from.saturating_add(step - 1).min(safe);
            let mut proven: Vec<VaultEvent> = vec![];
            let mut refused = None;
            for ev in events::fetch(&primary, self.cfg.registry, from, to).await? {
                if !known.contains(&ev.vault_id) || !ev.event.for_signers() {
                    continue;
                }
                match chain::verify_event(&primary, witness.as_ref(), self.cfg.registry, &ev, recent).await? {
                    // What is applied carries the Registry's own values (C-2).
                    Verdict::Proven(applied) => proven.push(applied),
                    // The Registry changed the claim again since; that later event carries its state.
                    Verdict::Stale(why) => debug!(
                        block = ev.block,
                        log_index = ev.log_index,
                        "skipping a Registry event its later events supersede: {why}"
                    ),
                    Verdict::Forged(why) if self.give_up_on(&ev).await => warn!(
                        block = ev.block,
                        log_index = ev.log_index,
                        "skipping a Registry event that the Registry state contradicts: {why}"
                    ),
                    Verdict::Forged(why) => {
                        warn!(block = ev.block, log_index = ev.log_index, "Registry state contradicts an event: {why}");
                        refused = Some(anyhow!("a Registry event is contradicted by the Registry state; retrying"));
                        break;
                    }
                    // Fail closed: one of the two providers lies or is broken.
                    Verdict::Disputed(why) => {
                        refused = Some(anyhow!(
                            "Base RPCs disagree on the Registry event at block {} ({}): {why}",
                            ev.block,
                            ev.log_index
                        ));
                        break;
                    }
                }
            }
            // Events before a refused one are applied; the cursor stays before this range so
            // the refused event is checked again next time (applied events are not re-applied).
            let mut store = self.store.lock().await;
            applied += chain::apply_all(&mut store, &proven);
            if let Some(e) = refused {
                return Err(e);
            }
            if to > store.base_cursor {
                store.set_base_cursor(to)?;
            }
            drop(store);
            from = to.saturating_add(1);
        }
        Ok(applied)
    }

    /// Counts one more refusal of `ev`; true once it should be skipped for good.
    async fn give_up_on(&self, ev: &VaultEvent) -> bool {
        let mut rejected = self.rejected_events.lock().await;
        if rejected.len() > 1_024 {
            rejected.clear();
        }
        let count = rejected.entry((ev.block, ev.log_index)).or_insert(0);
        *count += 1;
        let give_up = *count >= MAX_EVENT_REJECTIONS;
        if give_up {
            rejected.remove(&(ev.block, ev.log_index));
        }
        give_up
    }

    /// T-05: the bonds' liveness period, read once from the Registry's bond contract; `None` on a
    /// deployment without bonds.
    async fn liveness_period(&self) -> Result<Option<u64>> {
        let period = self
            .liveness_period
            .get_or_try_init(|| async {
                let provider = chain::base_provider(&self.cfg.base_rpc_url)?;
                let bond = self.bond().await?;
                if bond == Address::ZERO {
                    return anyhow::Ok(None);
                }
                let period =
                    ISignerBond::new(bond, &provider).livenessPeriod().call().await.context("bond.livenessPeriod")?;
                anyhow::Ok(Some(period))
            })
            .await?;
        Ok(*period)
    }

    /// T-05: warns about every vault with a payable claim older than half the bonds' liveness
    /// period: unpaid for the whole period, its claimant may take the claim's value from the
    /// bonds. A vault is named at most every half hour; nothing but its id and a count is logged.
    /// Returns the number of such vaults.
    pub async fn check_liveness(&self) -> Result<usize> {
        let Some(period) = self.liveness_period().await? else {
            return Ok(0);
        };
        let now_secs = now();
        let late: Vec<(B256, usize)> = {
            let store = self.store.lock().await;
            store
                .vaults
                .values()
                .map(|v| (v.vault_id, ledger::overdue(&v.claims, now_secs, period / 2).len()))
                .filter(|(_, n)| *n > 0)
                .collect()
        };
        let mut warned = self.liveness_warned.lock().await;
        for (vault, claims) in &late {
            if warned.get(vault).is_some_and(|at| at.elapsed() < LIVENESS_WARNING_INTERVAL) {
                continue;
            }
            warned.insert(*vault, std::time::Instant::now());
            warn!(
                vault = %hex::encode(&vault[..4]),
                claims,
                "payable claims older than half the bonds' liveness period: pay them before their claimants can take their value from the bonds (T-05)"
            );
        }
        Ok(late.len())
    }

    /// Switches the demo mode (C-3). The token is compared in constant time; "compromised"
    /// needs a demo build and `--demo-allow-compromise`.
    pub async fn set_mode(&self, mode: Mode, token: &str) -> Result<Mode, ModeError> {
        match &self.admin_token {
            Some(expected) if auth::token_matches(expected, token) => {}
            _ => return Err(ModeError::Unauthorized),
        }
        if mode == Mode::Compromised && !(cfg!(feature = "demo") && self.demo_allow_compromise) {
            return Err(ModeError::NotAllowed);
        }
        *self.mode.lock().await = mode;
        Ok(mode)
    }
}

/// Review H-11, T3-4: whether a payout's commitment is deep enough on one Base provider, whose
/// latest block is `latest` (number, timestamp) and safe head `safe` (if it has one):
/// `recorded_at(n)` tells whether block `n`'s state holds the very record. It must, at Base's safe
/// head (derived from L1 data, so a Base reorg can no longer drop it) or, while that lags, in a
/// block `commit_depth_secs` old (`timestamp_of` finds one), and in any case `commit_depth_blocks`
/// below the latest block, the minimum from before H-11.
pub async fn commitment_deep_enough<T, TF, R, RF>(
    cfg: &NetworkConfig,
    latest: (u64, u64),
    safe: Option<u64>,
    timestamp_of: T,
    mut recorded_at: R,
) -> Result<bool>
where
    T: FnMut(u64) -> TF,
    TF: std::future::Future<Output = Result<u64>>,
    R: FnMut(u64) -> RF,
    RF: std::future::Future<Output = Result<bool>>,
{
    let floor = latest.0.saturating_sub(cfg.commit_depth_blocks);
    if let Some(safe) = safe
        && recorded_at(safe.min(floor)).await?
    {
        return Ok(true);
    }
    let old = events::block_secs_deep_by(latest, cfg.commit_depth_secs, timestamp_of).await?;
    recorded_at(old.min(floor)).await
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use zecured_policy::ledger::{Claim, ClaimKind};

    use super::*;

    fn depth_config(blocks: u64, secs: u64) -> NetworkConfig {
        serde_json::from_value(serde_json::json!({
            "lightwalletd_url": "http://127.0.0.1:1", "base_rpc_url": "http://127.0.0.1:1", "chain_id": 84_532,
            "registry": Address::repeat_byte(0x11), "adapter": Address::repeat_byte(0x22), "registry_from_block": 1,
            "confirmations": 3, "vault_expiry_blocks": 288, "denominations": [1_000_000], "nodes": [],
            "commit_depth_blocks": blocks, "commit_depth_secs": secs,
        }))
        .unwrap()
    }

    /// Whether a commitment recorded in block `committed` is deep enough when the chain's latest
    /// block is `latest`, its safe head `safe`, and block `n` has timestamp `chain(n)`.
    async fn deep(cfg: &NetworkConfig, chain: fn(u64) -> u64, committed: u64, latest: u64, safe: Option<u64>) -> bool {
        let timestamp_of = |n: u64| async move { Ok(chain(n)) };
        let recorded_at = |n: u64| async move { Ok(n >= committed) };
        commitment_deep_enough(cfg, (latest, chain(latest)), safe, timestamp_of, recorded_at).await.unwrap()
    }

    /// H-11, T3-4: a share is released once the commitment is under the safe head, or, while that
    /// lags or is missing, once it is `commit_depth_secs` old, whatever the block time; never before
    /// it is `commit_depth_blocks` deep.
    #[tokio::test]
    async fn a_commitment_must_be_under_the_safe_head_or_old_enough() {
        let cfg = depth_config(3, 120);
        let two_secs: fn(u64) -> u64 = |n| 1_700_000_000 + 2 * n;
        let fifth: fn(u64) -> u64 = |n| 1_700_000_000 + n / 5;
        let c = 5_000_000;
        // Under the safe head and 3 blocks deep: released.
        assert!(deep(&cfg, two_secs, c, c + 3, Some(c)).await);
        // Under the safe head but not yet 3 blocks deep: not yet.
        assert!(!deep(&cfg, two_secs, c, c + 2, Some(c + 2)).await);
        // The safe head lags (or the provider has none): released once 120 s old, at 2 s blocks
        // (60 blocks) and at 200 ms blocks (600) alike, not before.
        for safe in [Some(c - 1_000), None] {
            assert!(!deep(&cfg, two_secs, c, c + 59, safe).await);
            assert!(deep(&cfg, two_secs, c, c + 61, safe).await);
            assert!(!deep(&cfg, fifth, c, c + 590, safe).await);
            assert!(deep(&cfg, fifth, c, c + 610, safe).await);
        }
        // Not recorded at all: never.
        assert!(!deep(&cfg, two_secs, c + 10_000, c + 5_000, Some(c + 4_000)).await);
        // An operator's larger block minimum still holds (here 100 blocks).
        let strict = depth_config(100, 120);
        assert!(!deep(&strict, two_secs, c, c + 61, None).await);
        assert!(deep(&strict, two_secs, c, c + 100, None).await);
    }

    /// D18, T3-6: never a signing threshold of compromisable nodes, except in the theft demo's
    /// own build, where exactly three can be.
    #[test]
    fn compromisable_nodes_stay_below_the_threshold_outside_the_theft_demo() {
        let allowed: Vec<u16> = (0..=6).filter(|i| may_be_compromised(*i)).collect();
        if cfg!(feature = "demo-tier3") {
            assert_eq!(allowed, vec![3, 4, 5]);
        } else {
            assert_eq!(allowed, vec![4, 5]);
        }
    }

    #[test]
    fn bounded_maps_expire_and_cap_their_entries() {
        let ttl = Duration::from_secs(300);
        let age = |v: &Duration| *v;
        let mut map: HashMap<u32, Duration> = HashMap::new();
        insert_bounded(&mut map, 1, Duration::from_secs(400), age, ttl, 2).unwrap();
        insert_bounded(&mut map, 2, Duration::from_secs(10), age, ttl, 2).unwrap();
        // Entry 1 has expired: room for 3.
        insert_bounded(&mut map, 3, Duration::from_secs(0), age, ttl, 2).unwrap();
        assert_eq!(map.len(), 2);
        assert!(!map.contains_key(&1));
        // Full: a new key is refused, an existing one can be replaced.
        assert!(insert_bounded(&mut map, 4, Duration::from_secs(0), age, ttl, 2).is_err());
        insert_bounded(&mut map, 3, Duration::from_secs(1), age, ttl, 2).unwrap();
    }

    fn vault_with(claims: &[[u8; 32]], inflight: Option<Inflight>) -> VaultRecord {
        VaultRecord {
            vault_id: B256::repeat_byte(1),
            owner: Address::repeat_byte(2),
            account: Address::repeat_byte(3),
            amount: 1_000_000,
            birthday: 1,
            expiry_height: 289,
            key_package: serde_json::Value::Null,
            public_key_package: serde_json::Value::Null,
            sk: String::new(),
            ak: String::new(),
            ufvk: String::new(),
            address: String::new(),
            decisions: vec![],
            activated: true,
            outstanding: 0,
            claims: claims
                .iter()
                .map(|id| Claim {
                    id: *id,
                    kind: ClaimKind::Liquidation,
                    amount: 100_000,
                    commitment: *id,
                    order: (1, 0),
                    created_at: 0,
                    partner: None,
                    state: ClaimState::Unpaid,
                    refund_tag: None,
                    bond_paid: false,
                    proven: false,
                    confirmed_at: 0,
                })
                .collect(),
            refunds: vec![],
            used_nonces: vec![],
            inflight,
            confirmed: vec![],
            last_event: None,
            activated_tag: None,
            reopened: vec![],
        }
    }

    fn payout(claim: [u8; 32], txid: u8) -> Inflight {
        Inflight {
            claim_ids: vec![claim],
            inputs: vec![[9; 32]],
            txid: Some([txid; 32]),
            expiry_height: 100,
            change: 0,
            spend_commit: None,
        }
    }

    /// L-6: a confirmation that raced with a newer signed payout must not drop it.
    #[test]
    fn settling_a_payout_keeps_a_newer_one_in_flight() {
        let (a, b) = ([1; 32], [2; 32]);
        let old = payout(a, 11);
        let newer = payout(b, 12);
        let mut v = vault_with(&[a, b], Some(newer.clone()));
        settle_payout(&mut v, &old, [11; 32]);
        assert_eq!(v.inflight, Some(newer.clone()), "the newer payout stays in flight");
        assert_eq!(v.claims[0].state, ClaimState::Paid);
        assert_eq!(v.claims[1].state, ClaimState::Unpaid);
        // The usual case: the confirmed payout is the one in flight.
        settle_payout(&mut v, &newer, [12; 32]);
        assert_eq!(v.inflight, None);
        assert_eq!(v.confirmed.len(), 2);
        // Settling again changes nothing.
        settle_payout(&mut v, &newer, [12; 32]);
        assert_eq!(v.confirmed.len(), 2);
    }
}
