//! A Zecured signer node: one of five FROST participants per vault.
//!
//! The node never trusts the coordinator. It checks owner requests itself (P0), classifies
//! deposits on its own Zcash view (P1), reads Base events at the safe head (each one checked
//! against the Registry's state), recomputes every payout plan and verifies the PCZT against
//! it before contributing a signature share.

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
use tokio::sync::{Mutex, MutexGuard};
use tracing::warn;
use zcash_protocol::TxId;
use zecured_base::{
    DepositAttestation, Destination, PayoutConfirmation, RefundRequest, VaultCancellation, claim_ids_hash,
    events::{self, IZecuredRegistry, VaultEvent},
    recover_typed, registry_domain, sign_typed, signer_bitmap,
    verify::{STATUS_NONE, VAULT_CANCELLED, Verdict},
};
use zecured_policy::{
    classify::Classification,
    derive::{deposit_tag, note_id},
    ledger::{self, ClaimState, Recipient, commitment},
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
    /// Light wallet servers that must agree with ours before a deposit is attested (C-1).
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
/// Largest `eth_getLogs` block range (public RPCs refuse larger ones).
const MAX_LOG_RANGE: u64 = 2_000;
/// Blocks below both providers' latest block at which an event is checked when the state at its
/// own block is gone (non-archive nodes keep only the last ~128 states).
const RECENT_STATE_MARGIN: u64 = 2;

/// T3-4: Base blocks a payout's commitment must be under, on every provider, before this node
/// releases a share for the payout (about 6 seconds).
pub const COMMIT_DEPTH: u64 = 3;

/// I6: signatures a commitment needs.
const COMMIT_THRESHOLD: usize = 3;

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
pub struct AttestResponse {
    pub index: u16,
    pub account: alloy::primitives::Address,
    pub amount: u64,
    pub deposit_tag: B256,
    pub signature: Bytes,
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

/// H-2: a vault never pays itself. A destination or refund to one of its own addresses would
/// come back as a note outside its books.
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
    /// vault's current record and its notes (lineage restricted to the planned change, H-2).
    async fn scan_vault(&self, vault_id: &B256) -> Result<(MutexGuard<'_, Store>, VaultRecord, VaultNotes)> {
        let mut snapshot = self.store.lock().await.vaults.get(vault_id).context("unknown vault")?.clone();
        let mut notes = self.zcash.lock().await.vault_notes(&mut snapshot, self.cfg.confirmations).await?;
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(vault_id).context("unknown vault")?.clone();
        if chain::merge_decisions(&mut v.decisions, &snapshot.decisions)? {
            store.save_vault(v.clone())?;
        }
        notes.retain_planned_change(ledger::book_value(v.outstanding, &v.claims));
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

    // ----- P1 -----

    pub async fn attest(&self, vault_id: B256) -> Result<AttestResponse> {
        let (note, recorded) = {
            let (_store, v, notes) = self.scan_vault(&vault_id).await?;
            let note = notes.conforming.context("no conforming deposit (yet)")?;
            let recorded = v.attestation.as_ref().is_some_and(|a| a.nullifier == note.nullifier.to_bytes());
            (note, recorded)
        };
        // C-1: independent light wallet servers must agree on the deposit before this node
        // signs for it for the first time (a later signature is the same, deterministic one).
        if !recorded {
            self.zcash.lock().await.check_deposit_witnesses(&note).await?;
        }

        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        ensure!(!v.cancelled, "the vault was cancelled on Base");
        let nullifier = note.nullifier.to_bytes();
        let cmx = orchard::note::ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
        let tag = deposit_tag(&v.ufvk, &cmx);
        let msg = DepositAttestation {
            vaultId: vault_id,
            account: v.account,
            amount: v.amount,
            depositTag: B256::from(tag),
        };
        let signature = sign_typed(&self.id.eth, &self.domain(), &msg)?;
        if v.attestation.is_none() {
            v.attestation = Some(store::AttestationRecord {
                position: v
                    .decisions
                    .iter()
                    .find(|(_, c)| *c == Classification::Conforming)
                    .map(|(p, _)| *p)
                    .context("conforming decision missing")?,
                nullifier,
                deposit_tag: tag,
            });
            store.save_vault(v.clone())?;
        }
        Ok(AttestResponse {
            index: self.id.index,
            account: v.account,
            amount: v.amount,
            deposit_tag: B256::from(tag),
            signature,
        })
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
            let (_, note) = notes
                .non_conforming
                .iter()
                .find(|(id, _)| *id == r.note_id.0)
                .context("note is not an unspent non-conforming deposit")?;
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

    /// Tier 2 review: a note of the vault's amount may back a mint that never went through this
    /// node's classification (a deposit proof needs no signer), and the classification itself
    /// rests on the light wallet server's answers and on the absence of reorgs deeper than the
    /// confirmation depth. So before accepting or signing the refund of such a note, the node
    /// asks the Registry, on every Base provider it is configured with: the vault must be
    /// cancelled (D23), or activated against another note (this note's deposit tag unused, D24).
    /// Never while the vault can still be activated. Notes of any other value cannot back a mint.
    async fn check_refund_on_base(&self, v: &VaultRecord, note: &zecured_zcash_ops::scan::ReceivedNote) -> Result<()> {
        if note.value() != v.amount {
            return Ok(());
        }
        let cmx = orchard::note::ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
        let tag = B256::from(deposit_tag(&v.ufvk, &cmx));
        let mut providers = vec![chain::base_provider(&self.cfg.base_rpc_url)?];
        if let Some(w) = self.base_rpc_witness.as_deref() {
            providers.push(chain::base_provider(w)?);
        }
        for provider in &providers {
            let registry = IZecuredRegistry::new(self.cfg.registry, provider);
            let status = registry.vault(v.vault_id).call().await.context("Registry.vault")?.status;
            let tag_used = registry.depositTagUsed(tag).call().await.context("Registry.depositTagUsed")?;
            refund_allowed_by_registry(status, tag_used)?;
        }
        Ok(())
    }

    // ----- cancellation (D23, L-8) -----

    /// Signs `VaultCancellation` so that the owner can get back the conforming deposit of a
    /// vault that was never activated, and records the owner's `RefundRequest` for that deposit
    /// (the consent, and the refund's destination). Only past the deposit window, so that no
    /// attestation of ours is about to land, and only while the Registry has not activated the
    /// vault (on every configured Base provider). A cancellation and an activation exclude each
    /// other on-chain; the deposit becomes refundable only once `VaultCancelled` is applied.
    pub async fn cancel(&self, vault_id: B256, r: RefundIn) -> Result<ConfirmResponse> {
        let recipient = parse_recipient(&r.recipient)?;
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
            ensure!(!v.activated, "the vault is active");
            ensure_not_own_receiver(v, &recipient.receiver)?;
        }
        {
            let (_store, v, notes) = self.scan_vault(&vault_id).await?;
            let deposit = notes.conforming.as_ref().context("no conforming deposit")?;
            ensure!(deposit.spent_at.is_none(), "the deposit was spent");
            let id = note_id(&vault_id.0, &chain::to_policy_position(&deposit.position));
            ensure!(id == r.note_id.0, "not the vault's conforming deposit");
            ensure!(
                notes.tip > v.expiry_height.saturating_add(self.cfg.confirmations),
                "the deposit window is still open"
            );
        }
        let mut providers = vec![chain::base_provider(&self.cfg.base_rpc_url)?];
        if let Some(w) = self.base_rpc_witness.as_deref() {
            providers.push(chain::base_provider(w)?);
        }
        for provider in &providers {
            let status = IZecuredRegistry::new(self.cfg.registry, provider)
                .vault(vault_id)
                .call()
                .await
                .context("Registry.vault")?
                .status;
            ensure!(status == STATUS_NONE || status == VAULT_CANCELLED, "the Registry has activated the vault");
        }
        {
            let mut store = self.store.lock().await;
            let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
            ensure!(!v.activated, "the vault is active");
            ensure!(v.use_nonce(r.nonce), "nonce already used");
            v.refunds.retain(|x| x.note_id != r.note_id.0);
            v.refunds.push(RefundRecord {
                note_id: r.note_id.0,
                recipient,
            });
            store.save_vault(v)?;
        }
        Ok(ConfirmResponse {
            index: self.id.index,
            signature: sign_typed(&self.id.eth, &self.domain(), &VaultCancellation { vaultId: vault_id })?,
        })
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

        let (store, v, notes) = self.scan_vault(&req.vault_id).await?;
        let (plan, inputs) = signing::expected_plan(&store, &v, &notes, &req.kind, now())?;
        // Tier 3: the salts of the plan's recipients, for the claim refs.
        let salts = match &req.kind {
            SignKind::Payout { .. } => Some(plan_salts(&store, &v, &plan.claims)?),
            SignKind::Refund { .. } => None,
        };
        drop(store);
        if matches!(req.kind, SignKind::Refund { .. }) {
            for note in &inputs {
                self.check_refund_on_base(&v, note).await?;
            }
        }
        // H-1 (inside `commit`): no other payout while one this node signed can still be mined.
        let (mut session, commitments) =
            signing::commit(&v, &plan, &inputs, notes.tip, &req, v.inflight.as_ref(), &notes.unspent)?;
        // Tier 3 (docs/TIER3.md § 5): sign the payout's commitment; the shares wait until Base has it.
        let spend_commit = match salts {
            Some(claims) => {
                let [input] = inputs.as_slice() else {
                    bail!("a payout spends exactly one lineage note ({} here)", inputs.len());
                };
                let input_cmx = orchard::note::ExtractedNoteCommitment::from(input.note.commitment()).to_bytes();
                let record =
                    signing::spend_commit(v.vault_id, &v.ufvk, &input_cmx, &claims, &session.verified, plan.change)?;
                self.check_commit_preconditions(&v, &record).await?;
                let signature = sign_typed(&self.id.eth, &self.domain(), &record.message())?;
                session.spend_commit = Some(record.clone());
                Some(SignedSpendCommit { record, signature })
            }
            None => None,
        };
        self.start_session(&req, session, &commitments, spend_commit).await
    }

    /// Tier 3 checks before signing a commitment, on every Base provider: the input is a lineage
    /// note of this vault on Base (else this node's lineage differs from the Registry's), and the
    /// payout keeps every pending claim already committed against that note (T3-5).
    async fn check_commit_preconditions(&self, v: &VaultRecord, record: &SpendCommitRecord) -> Result<()> {
        let unpaid: Vec<[u8; 32]> = v.claims.iter().filter(|c| c.state == ClaimState::Unpaid).map(|c| c.id).collect();
        let planned: Vec<[u8; 32]> = record.claim_ids.iter().map(|c| c.0).collect();
        for provider in self.base_providers()? {
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
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
        }
        Ok(())
    }

    /// T3-4: the shares of a payout leave this node only if it holds every signature its
    /// commitment was recorded with (so it can submit it again) and every Base provider it uses
    /// shows that very record, `COMMIT_DEPTH` blocks deep.
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
            let latest = provider.get_block_number().await.context("Base block number")?;
            let at = BlockId::number(latest.saturating_sub(COMMIT_DEPTH));
            let on_base = IZecuredRegistry::new(self.cfg.registry, &provider)
                .spendRecord(record.payout_ref)
                .block(at)
                .call()
                .await
                .context("Registry.spendRecord")?;
            ensure!(
                on_base.recordHash == hash && on_base.vaultId == record.vault_id && on_base.signerBitmap == bitmap,
                "the payout's commitment is not on Base {COMMIT_DEPTH} blocks deep with these signatures"
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

    /// Signs `PayoutConfirmation` once the payout this node signed is mined and confirmed.
    pub async fn confirm(&self, vault_id: B256, c: ConfirmIn) -> Result<ConfirmResponse> {
        let wanted: Vec<[u8; 32]> = c.claim_ids.iter().map(|x| x.0).collect();
        // The coordinator sends the display form (byte-reversed hex); compare internal bytes.
        let txid = TxId::from_hex(&c.txid).context("invalid txid")?;
        let txid_bytes = *txid.as_ref();
        let inflight = {
            let store = self.store.lock().await;
            let v = store.vaults.get(&vault_id).context("unknown vault")?;
            // H-5: a confirmation already given is given again (submitting it to Base may
            // have failed after the payout was cleared here).
            if v.confirmed.iter().any(|p| p.claim_ids == wanted && p.txid == txid_bytes) {
                return self.payout_confirmation(vault_id, &c.claim_ids);
            }
            v.inflight.clone().context("no payout in flight for this vault")?
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
        self.payout_confirmation(vault_id, &c.claim_ids)
    }

    fn payout_confirmation(&self, vault_id: B256, claim_ids: &[B256]) -> Result<ConfirmResponse> {
        let msg = PayoutConfirmation {
            vaultId: vault_id,
            claimIdsHash: claim_ids_hash(claim_ids),
        };
        Ok(ConfirmResponse {
            index: self.id.index,
            signature: sign_typed(&self.id.eth, &self.domain(), &msg)?,
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
        let mut latest = primary.get_block_number().await.context("Base block number")?;
        if let Some(w) = &witness {
            safe = safe.min(events::safe_head(w).await.context("witness Base RPC")?);
            latest = latest.min(w.get_block_number().await.context("witness Base RPC")?);
        }
        // Where events are checked if the state at their own block is gone: a recent block both
        // providers still have the state of.
        let recent = latest.saturating_sub(RECENT_STATE_MARGIN).max(safe);
        let mut from = cursor.max(self.cfg.registry_from_block.saturating_sub(1)).saturating_add(1);
        let mut applied = 0;
        while from <= safe {
            let to = from.saturating_add(MAX_LOG_RANGE - 1).min(safe);
            let mut proven: Vec<VaultEvent> = vec![];
            let mut refused = None;
            for ev in events::fetch(&primary, self.cfg.registry, from, to).await? {
                if !known.contains(&ev.vault_id) {
                    continue;
                }
                match chain::verify_event(&primary, witness.as_ref(), self.cfg.registry, &ev, recent).await? {
                    Verdict::Proven => proven.push(ev),
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

/// K2: whether one Base provider's view of the Registry allows refunding a note of the vault's
/// amount: the vault is cancelled (D23), or activated or closed against another note (this
/// note's deposit tag unused, D24). Never while the vault can still be activated.
fn refund_allowed_by_registry(status: u8, tag_used: bool) -> Result<()> {
    match status {
        VAULT_CANCELLED => Ok(()),
        STATUS_NONE => bail!("the vault can still be activated: it must be cancelled before this refund"),
        _ => {
            ensure!(!tag_used, "this note backs the vault on Base");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use zecured_policy::ledger::{Claim, ClaimKind};

    use super::*;

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

    #[test]
    fn a_note_of_the_vault_amount_is_refunded_only_if_it_cannot_back_the_vault() {
        use zecured_base::verify::{VAULT_ACTIVE, VAULT_CLOSED};
        // Cancelled (D23): the conforming deposit goes back, whatever its tag.
        assert!(refund_allowed_by_registry(VAULT_CANCELLED, false).is_ok());
        assert!(refund_allowed_by_registry(VAULT_CANCELLED, true).is_ok());
        // Not activated yet: a deposit proof may still activate the vault with this note; and E3,
        // a proof is pending (the vault reads None, the tag reads used): never refundable either.
        assert!(refund_allowed_by_registry(STATUS_NONE, false).is_err());
        assert!(refund_allowed_by_registry(STATUS_NONE, true).is_err());
        // Activated or closed: only a note other than the one that backs the vault.
        for status in [VAULT_ACTIVE, VAULT_CLOSED] {
            assert!(refund_allowed_by_registry(status, false).is_ok());
            assert!(refund_allowed_by_registry(status, true).is_err());
        }
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
            attestation: None,
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
                })
                .collect(),
            refunds: vec![],
            used_nonces: vec![],
            inflight,
            confirmed: vec![],
            last_event: None,
            cancelled: false,
            activated_tag: None,
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
