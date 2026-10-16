//! Zecured coordinator: drives the signer network.
//!
//! It holds **no** key material for vaults: it relays DKG messages (sealed node-to-node),
//! scans vaults with their viewing keys, proves deposits (as collateral, or as refunds of what
//! cannot back a vault, T-02), builds and proves PCZTs, runs the FROST rounds with whichever
//! signers accept, aggregates, broadcasts, and relays deposit proofs, payout commitments and
//! confirmations to Base with its own gas key. It also keeps the Intake's pending activations
//! moving (a keyless keeper: anyone may finalize, drop or convert them). Every decision that moves
//! value is taken independently by at least three signer nodes.
//!
//! Errors returned to API callers carry a [`error::PublicError`] (fixed message and code);
//! details are logged at debug level only, scrubbed of identifiers and numbers.

pub mod error;
mod frost;
pub mod inflight;
pub mod nodes;
pub mod proofs;
pub mod queue;
pub mod select;
pub mod store;

use std::{
    collections::{BTreeMap, HashMap},
    panic::AssertUnwindSafe,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    sol_types::{Eip712Domain, SolStruct},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures::FutureExt;
use orchard::{circuit::ProvingKey, keys::FullViewingKey};
use reddsa::frost::redpallas::keys::PublicKeyPackage;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, error, info, warn};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, Network},
};
use zecured_base::{
    Destination, OpenVault, PayoutConfirmation, ViewKeyRequest, check_signed_by_all, claim_ids_hash,
    events::{self, ClaimChange, ISignerBond, IZecuredRegistry, IZecuredSlasher, RegistryEvent, VaultEvent},
    recover_typed, registry_domain, vault_opened,
    verify::VAULT_PENDING,
};
use zecured_node::{
    ConfirmIn, ConfirmResponse, DestinationIn, RecipientIn,
    chain::{BaseView, VaultNotes, ZcashView, refund_tags},
    config::NetworkConfig,
    dkg::{Round1Out, Round2In, Round2Out, Round3In, Round3Out, Sealed, VaultOpen},
    now,
    signing::{CommitRequest, CommitResponse, SignKind, SpendCommitRecord, main_lineage},
};
use zecured_policy::{
    classify::{self, Classification, DepositRules, NotePosition},
    derive::deposit_tag,
    ledger::{self, ClaimKind, ClaimState, NextPayout, PayoutInput, PayoutPlan, Recipient},
};
use zecured_zcash_ops::{
    keys,
    scan::ReceivedNote,
    spend::{self, Payment},
};

pub use zecured_threshold::MIN_SIGNERS;

pub use crate::store::{NodeSignature, NoteView};
use crate::{
    error::{PublicError, public_error, scrubbed},
    inflight::{Action, Inputs, Settled},
    nodes::{Answer, Nodes, is_refusal, shortfall, split},
    store::{CoordVault, Inflight, KeptCommitment, Store, VaultStatus},
};

const NETWORK: Network = Network::TestNetwork;
const ZATS_PER_ZEC: u64 = 100_000_000;

/// H-01: how long a request id that opened a vault (or is opening one) is remembered at most, to
/// refuse its stamp again; past its deadline the request is refused anyway, and once its vault is
/// open its owner is bound.
const OPEN_REQUEST_MEMORY_SECS: u64 = 24 * 60 * 60;
/// The vault birthday is set this many blocks below the coordinator's Zcash tip, so signers
/// whose tip lags slightly still accept it (nodes refuse a birthday above their own tip).
const BIRTHDAY_MARGIN: u32 = 3;
/// An expired, closed or cancelled vault is rescanned this often, so late deposits still show up
/// and are refunded through proofs (M-10, T-02).
pub const IDLE_SCAN_INTERVAL: Duration = Duration::from_secs(600);
/// Safety net against a hung provider: one vault's pass (scan, proving, both FROST rounds,
/// Base transactions) never takes this long when things work. Every step is safe to cancel:
/// a payout is recorded before it is broadcast.
const VAULT_PASS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BASE_SYNC_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Wait for a Base transaction's receipt at most this long (alloy's default is forever).
pub(crate) const RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);
/// H-11: seconds past `commit_depth_secs` the coordinator still waits for a commitment to be
/// deep enough (a node's provider may lag a little; `MAX_COMMIT_DEPTH_SECS` plus this, the
/// commitment's transaction and round 2 fit in a signing session).
const COMMIT_WAIT_MARGIN_SECS: u64 = 30;
/// T-05: how often the coordinator warns at most about one vault's payable claims getting old.
const LIVENESS_WARNING_INTERVAL: Duration = Duration::from_secs(30 * 60);

alloy::sol! {
    #[sol(rpc)]
    interface IRegistryClaims {
        function claim(bytes32 claimId) external view returns (bytes32 vaultId, uint8 kind, uint64 amount, bytes32 commitment, uint8 status);
    }
}

/// The app's request to open a vault (`OpenVault`, signed by the owner key).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenVaultRequest {
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub request_id: B256,
    pub deadline: u64,
    pub signature: Bytes,
    /// H-01: the nonce of the request's hashcash stamp (`zecured_base::stamp`).
    #[serde(default)]
    pub stamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenVaultResponse {
    pub vault_id: B256,
    pub address: String,
    pub zip321: String,
    pub amount: u64,
    pub expiry_height: u32,
    /// `keccak256` of the vault's tag key `K = BLAKE2b-256("Zecured_TagKey__", UFVK)`, as the
    /// signers signed it in `VaultOpened`: the owner needs it to check the signatures now and the
    /// viewing key later (it must derive this K).
    pub tag_key_hash: B256,
    /// Every signer's `VaultOpened` signature, ordered by node index (C-4).
    pub signatures: Vec<NodeSignature>,
}

/// Public view of a vault for the app: what its pages read, nothing more (L-10).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultView {
    pub vault_id: B256,
    pub status: VaultStatus,
    pub amount: u64,
    pub expiry_height: u32,
    pub outstanding: u64,
    pub claims: Vec<ClaimView>,
    /// Deposits that have not landed on Base: each is refunded once a proof lands it as a Refund
    /// claim (T-02), whose destination the vault's account then gives.
    pub non_conforming: Vec<NoteView>,
    pub inflight: bool,
    pub birthday_height: u32,
    /// Zcash tip at the last coordinator pass (0 before the first pass).
    pub tip_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimView {
    pub claim_id: B256,
    pub kind: ledger::ClaimKind,
    pub amount: u64,
    pub paid: bool,
    pub state: ClaimState,
    /// Whether the claim has a destination on Base (a refund gets one from its account, T-02).
    pub destination_set: bool,
}

/// The v3 contracts the Registry names (immutable): the Intake, the slasher and the bonds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct V3Contracts {
    pub intake: Address,
    /// Zero on a deployment without one.
    pub slasher: Address,
    pub bond: Address,
}

/// One signer's reaction to the demo attack.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainVote {
    pub index: u16,
    pub signed: bool,
    pub reason: Option<String>,
}

/// Result of the threshold demo: an attempt to sweep a vault to an arbitrary address.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainReport {
    pub vault_id: B256,
    pub amount: u64,
    pub votes: Vec<DrainVote>,
    /// True if at least three signers would have signed (the demo never broadcasts).
    pub threshold_reached: bool,
}

pub struct Coordinator {
    pub cfg: NetworkConfig,
    nodes: Nodes,
    store: Mutex<Store>,
    zcash: Mutex<ZcashView>,
    gas: PrivateKeySigner,
    pk: OnceLock<Arc<ProvingKey>>,
    /// Zcash tip seen by the last coordinator pass (for the app's countdowns).
    last_tip: AtomicU32,
    /// When each idle (expired or closed) vault was last scanned.
    idle_scans: std::sync::Mutex<HashMap<B256, Instant>>,
    /// Tier 2: activate vaults on deposit proofs made here instead of signer attestations.
    proofs: Option<proofs::Prover>,
    /// H-01: request ids whose stamp is opening a vault or opened one, and until when they are
    /// remembered (Unix seconds).
    open_requests: std::sync::Mutex<HashMap<B256, u64>>,
    /// The Intake, slasher and bonds, read once from the Registry.
    v3: OnceCell<V3Contracts>,
    /// T-05: the bonds' liveness period (seconds), read once; `None` without bonds.
    liveness_period: OnceCell<Option<u64>>,
    /// T-05: when the coordinator last warned about each vault's old payable claims.
    liveness_warned: std::sync::Mutex<HashMap<B256, Instant>>,
    /// TEST ONLY: see [`TEST_WITHHOLD_FLAG`].
    #[cfg(feature = "test-hooks")]
    withhold_flag: std::path::PathBuf,
}

/// H-01: a request id taken for one vault opening. Dropped without [`Self::keep`] (the opening
/// failed, or its caller went away), it is released so that the owner can retry the same request.
struct OpenRequestClaim<'a> {
    requests: &'a std::sync::Mutex<HashMap<B256, u64>>,
    id: B256,
    kept: bool,
}

impl OpenRequestClaim<'_> {
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for OpenRequestClaim<'_> {
    fn drop(&mut self) {
        if !self.kept {
            self.requests.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.id);
        }
    }
}

/// TEST ONLY (cargo feature `test-hooks`, off by default): while this file exists in the
/// coordinator's data directory, the next payout is signed and recorded but neither broadcast
/// nor rebroadcast, so it expires unmined and the rebuild path runs end to end
/// (docs/TESTING.md). The file is removed when used. Builds without the feature ignore it.
#[cfg(feature = "test-hooks")]
pub const TEST_WITHHOLD_FLAG: &str = "test-withhold-next-broadcast";

/// `zcash:` payment URI for `amount` zatoshis (ZIP 321).
pub fn zip321_uri(address: &str, amount: u64) -> String {
    let whole = amount / ZATS_PER_ZEC;
    let frac = amount % ZATS_PER_ZEC;
    let amount = if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", format!("{frac:08}").trim_end_matches('0'))
    };
    format!("zcash:{address}?amount={amount}")
}

fn fvk_of(v: &CoordVault) -> Result<FullViewingKey> {
    UnifiedFullViewingKey::decode(&NETWORK, &v.ufvk)
        .map_err(|e| anyhow!("stored UFVK: {e}"))?
        .orchard()
        .cloned()
        .context("UFVK has no Orchard-protocol key")
}

fn short(id: &B256) -> String {
    hex::encode(&id[..4])
}

/// Successful answers, or the shortfall error if fewer than `need` nodes accepted.
fn at_least<T>(answers: Vec<Answer<T>>, need: usize, what: &str) -> Result<Vec<(u16, T)>> {
    let (ok, failed) = split(answers);
    if ok.len() < need {
        return Err(shortfall(what, ok.len(), need, &failed));
    }
    Ok(ok)
}

/// Logs a failed step: its public code at warn level, the scrubbed chain at debug level.
fn log_failure(vault: &B256, what: &str, e: &anyhow::Error) {
    let code = public_error(e).unwrap_or(PublicError::Internal).code();
    warn!(vault = %short(vault), code, "{what} failed");
    debug!(vault = %short(vault), error = %scrubbed(e), "{what} failed");
}

/// True if an idle vault last scanned at `last` is due for another scan at `now`.
pub fn idle_scan_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= IDLE_SCAN_INTERVAL)
}

/// The end of a vault opening: every signer of the network answered round 3 for itself, all
/// derived the same keys and address, and each signed `VaultOpened` for them with its
/// configured attestation key (C-4; the owner checks the same signatures against its own
/// pinned signer set). Returns the vault record and the response for the owner.
fn finish_open(
    cfg: &NetworkConfig,
    domain: &Eip712Domain,
    open: &VaultOpen,
    round3: &[(u16, Round3Out)],
) -> Result<(CoordVault, OpenVaultResponse)> {
    let (_, first) = round3.first().context("no round-3 output")?;
    let agree = round3.iter().all(|(index, o)| {
        o.index == *index
            && o.ak == first.ak
            && o.ufvk == first.ufvk
            && o.address == first.address
            && o.public_key_package == first.public_key_package
    });
    if !agree {
        return Err(anyhow!("signers disagree on the vault keys").context(PublicError::SignersUnavailable));
    }
    let opened = vault_opened(
        open.vault_id,
        open.owner,
        open.account,
        open.amount,
        open.expiry_height,
        &first.address,
        &first.ufvk,
    );
    let signers: Vec<(u16, Address)> = cfg.nodes.iter().map(|n| (n.index, n.eth)).collect();
    let mut signatures: Vec<NodeSignature> = round3
        .iter()
        .map(|(index, o)| NodeSignature {
            index: *index,
            signature: o.signature.clone(),
        })
        .collect();
    signatures.sort_by_key(|s| s.index);
    let pairs: Vec<(u16, Bytes)> = signatures.iter().map(|s| (s.index, s.signature.clone())).collect();
    check_signed_by_all(domain, &opened, &signers, &pairs)
        .context("the signers' VaultOpened signatures do not check out")
        .context(PublicError::SignersUnavailable)?;
    let vault = CoordVault {
        vault_id: open.vault_id,
        owner: open.owner,
        account: open.account,
        amount: open.amount,
        birthday: open.birthday,
        expiry_height: open.expiry_height,
        ufvk: first.ufvk.clone(),
        address: first.address.clone(),
        public_key_package: first.public_key_package.clone(),
        status: VaultStatus::AwaitingDeposit,
        decisions: vec![],
        attest_tx: None,
        outstanding: 0,
        claims: vec![],
        inflight: None,
        attest_delay: None,
        used_view_nonces: vec![],
        non_conforming: vec![],
        opened_signatures: signatures.clone(),
        activated_tag: None,
        pending_tag: None,
        opened_at: now(),
    };
    let response = OpenVaultResponse {
        vault_id: vault.vault_id,
        zip321: zip321_uri(&vault.address, vault.amount),
        address: vault.address.clone(),
        amount: vault.amount,
        expiry_height: vault.expiry_height,
        tag_key_hash: opened.tagKeyHash,
        signatures,
    };
    Ok((vault, response))
}

/// Applies one Registry event to the coordinator's record, as the Registry's state machine moves
/// the vault. Idempotent: a chunk applied again after a crash (vault files are saved before the
/// cursor) changes nothing.
fn apply_event(v: &mut CoordVault, ev: &VaultEvent) {
    match &ev.event {
        RegistryEvent::Reserved { deposit_tag, .. } => {
            v.pending_tag = Some(deposit_tag.0);
            if matches!(v.status, VaultStatus::AwaitingDeposit | VaultStatus::Expired) {
                v.status = VaultStatus::DepositSeen;
            }
        }
        RegistryEvent::ReservationReleased => {
            v.pending_tag = None;
            if v.status == VaultStatus::DepositSeen {
                // A refund opened meanwhile keeps the vault on Base, cancelled.
                v.status = if v.claims.is_empty() {
                    VaultStatus::AwaitingDeposit
                } else {
                    VaultStatus::Cancelled
                };
            }
        }
        RegistryEvent::VaultActivated {
            amount, deposit_tag, ..
        } => {
            // Once only: re-applying it after claims would reset `outstanding`.
            if v.activated_tag.is_none() {
                v.status = VaultStatus::Active;
                v.outstanding = *amount;
                v.activated_tag = Some(deposit_tag.0);
                v.pending_tag = None;
            }
        }
        RegistryEvent::Claims(claims) => {
            for c in claims {
                if v.claims.iter().any(|x| x.id == c.id) {
                    continue;
                }
                if c.kind == ClaimKind::Refund {
                    // T-02: a refund of the pending deposit itself (converted), or of a deposit to
                    // a vault Base had neither activated nor reserved: the vault is cancelled for
                    // good. A refund while another deposit's activation is pending leaves it so.
                    let converted = c.refund_tag.is_some() && c.refund_tag == v.pending_tag;
                    if converted || v.pending_tag.is_none() {
                        v.pending_tag = None;
                        if v.activated_tag.is_none() {
                            v.status = VaultStatus::Cancelled;
                        }
                    }
                } else {
                    // Every other claim burns exactly its amount (D14: releases may be partial; the
                    // fee is minted less, T-06; a retirement burns what it retires, T-04).
                    v.outstanding = v.outstanding.saturating_sub(c.amount);
                }
                v.claims.push(c.clone());
            }
        }
        RegistryEvent::ClaimChanged { claim_id, change } => {
            if let Some(claim) = v.claims.iter_mut().find(|c| c.id == claim_id.0) {
                match change {
                    ClaimChange::RefundDestination(c) => {
                        claim.commitment = c.0;
                        claim.created_at = ev.timestamp;
                        if claim.state == ClaimState::WrittenOff {
                            claim.state = ClaimState::Unpaid;
                        }
                    }
                    ClaimChange::ReimburseDestination(c) => {
                        claim.commitment = c.0;
                        claim.bond_paid = true;
                    }
                    ClaimChange::BondPaid => {
                        claim.bond_paid = true;
                        claim.commitment = [0; 32];
                    }
                    ClaimChange::WrittenOff => {
                        if claim.state == ClaimState::Unpaid {
                            claim.state = ClaimState::WrittenOff;
                        }
                    }
                    ClaimChange::Compensated => {
                        if claim.state != ClaimState::Paid {
                            claim.state = ClaimState::Compensated;
                        }
                    }
                    ClaimChange::Disputed { .. } => claim.state = ClaimState::Disputed,
                    ClaimChange::Upheld { .. } => claim.state = ClaimState::Paid,
                    ClaimChange::Reopened => {
                        if claim.state == ClaimState::Disputed {
                            claim.state = ClaimState::Unpaid;
                        }
                    }
                }
            }
        }
        RegistryEvent::PayoutConfirmed(ids) => {
            for c in v.claims.iter_mut().filter(|c| ids.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
            }
        }
    }
    if v.status == VaultStatus::Active && v.outstanding == 0 && !v.claims.is_empty() && v.settled() {
        v.status = VaultStatus::Closed;
    }
}

impl Coordinator {
    pub fn new(cfg: NetworkConfig, data_dir: &Path, gas_key: &str) -> Result<Self> {
        let gas: PrivateKeySigner = gas_key.parse().context("coordinator gas key")?;
        Ok(Self {
            nodes: Nodes::new(&cfg, gas.clone())?,
            zcash: Mutex::new(ZcashView::new(&cfg.lightwalletd_url)),
            store: Mutex::new(Store::open(data_dir)?),
            gas,
            pk: OnceLock::new(),
            last_tip: AtomicU32::new(0),
            idle_scans: std::sync::Mutex::default(),
            proofs: None,
            open_requests: std::sync::Mutex::default(),
            v3: OnceCell::new(),
            liveness_period: OnceCell::new(),
            liveness_warned: std::sync::Mutex::default(),
            #[cfg(feature = "test-hooks")]
            withhold_flag: data_dir.join(TEST_WITHHOLD_FLAG),
            cfg,
        })
    }

    /// The Halo 2 proving key (built once, a few seconds).
    async fn proving_key(&self) -> Result<Arc<ProvingKey>> {
        if let Some(pk) = self.pk.get() {
            return Ok(pk.clone());
        }
        let pk = Arc::new(tokio::task::spawn_blocking(spend::proving_key).await?);
        Ok(self.pk.get_or_init(|| pk).clone())
    }

    fn domain(&self) -> Eip712Domain {
        registry_domain(self.cfg.chain_id, self.cfg.registry)
    }

    async fn vault(&self, id: &B256) -> Result<CoordVault> {
        self.store.lock().await.vaults.get(id).cloned().context(PublicError::UnknownVault)
    }

    /// Read-modify-write of a vault record under the store lock (L-5).
    async fn update_vault<R>(&self, id: &B256, f: impl FnOnce(&mut CoordVault) -> R) -> Result<R> {
        self.store.lock().await.update(id, f)
    }

    /// P0 checks the coordinator can do itself before involving the signers: the request is
    /// signed by the vault owner and not expired. Every node checks again (and the nonce).
    fn check_owner<T: SolStruct>(&self, v: &CoordVault, message: &T, signature: &[u8], deadline: u64) -> Result<()> {
        let signer = recover_typed(&self.domain(), message, signature).context(PublicError::NotOwner)?;
        ensure!(signer == v.owner, PublicError::NotOwner);
        ensure!(deadline >= now(), PublicError::Expired);
        Ok(())
    }

    // ------------------------------------------------------------------ vault opening

    /// H-01: whether the request carries a stamp with the network's zero bits. One hash: checked
    /// before anything else about the request, and before the API spends a rate-limit token on it.
    pub fn check_open_stamp(&self, req: &OpenVaultRequest) -> Result<()> {
        let bits = self.cfg.open_vault.stamp_bits;
        ensure!(zecured_base::stamp::is_valid(&req.request_id, &req.owner, req.stamp, bits), PublicError::BadStamp);
        Ok(())
    }

    /// H-01: one stamp per request id. The id is taken while its vault opens and kept once it has
    /// opened (until the request's deadline, a day at most); a failed opening releases it.
    fn claim_open_request(&self, id: B256, deadline: u64) -> Result<OpenRequestClaim<'_>> {
        let now = now();
        let mut requests = self.open_requests.lock().unwrap_or_else(|p| p.into_inner());
        requests.retain(|_, until| *until >= now);
        ensure!(!requests.contains_key(&id), PublicError::NonceUsed);
        requests.insert(id, deadline.min(now.saturating_add(OPEN_REQUEST_MEMORY_SECS)));
        Ok(OpenRequestClaim {
            requests: &self.open_requests,
            id,
            kept: false,
        })
    }

    pub async fn open_vault(&self, req: OpenVaultRequest) -> Result<OpenVaultResponse> {
        ensure!(self.cfg.denominations.contains(&req.amount), PublicError::NotDenomination);
        self.check_open_stamp(&req)?;
        // Checked here too (every node checks it), so a forged or stale request costs no DKG.
        let msg = OpenVault {
            owner: req.owner,
            account: req.account,
            amount: req.amount,
            requestId: req.request_id,
            deadline: req.deadline,
        };
        let signer = recover_typed(&self.domain(), &msg, &req.signature).context(PublicError::NotOwner)?;
        ensure!(signer == req.owner, PublicError::NotOwner);
        ensure!(req.deadline >= now(), PublicError::Expired);
        {
            let store = self.store.lock().await;
            ensure!(!store.is_bound(req.account, req.owner), PublicError::AlreadyBound);
            // H-01: a vault without a deposit holds its slot for `pending_slot_secs`, not until it
            // expires (about two hours), so filling every slot needs a stamp per slot every half hour.
            let limits = &self.cfg.open_vault;
            ensure!(store.holding_slots(now(), limits.pending_slot_secs) < limits.max_pending, PublicError::Busy);
        }
        let claim = self.claim_open_request(req.request_id, req.deadline)?;
        let tip = self.zcash.lock().await.tip().await.context(PublicError::ChainUnavailable)?;
        let birthday = tip.saturating_sub(BIRTHDAY_MARGIN).max(1);
        let mut id = [0u8; 32];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut id);
        let open = VaultOpen {
            vault_id: B256::from(id),
            owner: req.owner,
            account: req.account,
            amount: req.amount,
            request_id: req.request_id,
            deadline: req.deadline,
            signature: req.signature,
            birthday,
            expiry_height: birthday.checked_add(self.cfg.vault_expiry_blocks).context("expiry height overflows")?,
        };
        let all = self.nodes.indices();
        let n = all.len();

        let r1: Vec<Round1Out> = at_least(self.nodes.post_many(&all, "/v1/dkg/round1", &open).await, n, "DKG round 1")?
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        let r2in = Round2In {
            vault_id: open.vault_id,
            round1: r1.clone(),
        };
        let r2: Vec<(u16, Round2Out)> =
            at_least(self.nodes.post_many(&all, "/v1/dkg/round2", &r2in).await, n, "DKG round 2")?;
        let mut to: BTreeMap<u16, BTreeMap<u16, Sealed>> = BTreeMap::new();
        for (_, out) in r2 {
            for (recipient, sealed) in out.sealed {
                to.entry(recipient).or_default().insert(out.index, sealed);
            }
        }
        let bodies: Vec<(u16, Round3In)> = all
            .iter()
            .map(|&i| {
                (
                    i,
                    Round3In {
                        vault_id: open.vault_id,
                        round1: r1.clone(),
                        sealed: to.remove(&i).unwrap_or_default(),
                    },
                )
            })
            .collect();
        // DKG needs all five signers, and so does the owner: each must vouch for the vault.
        let r3: Vec<(u16, Round3Out)> =
            at_least(self.nodes.post_each(&bodies, "/v1/dkg/round3").await, n, "DKG round 3")?;
        let (vault, resp) = finish_open(&self.cfg, &self.domain(), &open, &r3)?;
        // Kept with the vault, so the signatures can be served again.
        self.store.lock().await.save(vault)?;
        claim.keep();
        info!(vault = %short(&resp.vault_id), "vault opened");
        Ok(resp)
    }

    // ------------------------------------------------------------------ recipients

    fn recipient_from_ua(ua: &str, salt: [u8; 32]) -> Result<Recipient> {
        let addr = keys::ironwood_receiver(&NETWORK, ua).context(PublicError::InvalidReceiver)?;
        Ok(Recipient {
            receiver: addr.to_raw_address_bytes(),
            salt,
        })
    }

    fn recipient_in(r: &Recipient) -> RecipientIn {
        RecipientIn {
            receiver: hex::encode(r.receiver),
            salt: hex::encode(r.salt),
        }
    }

    /// Third-party recipient (liquidator / market maker): reveal to the signers.
    pub async fn add_recipient(&self, ua: &str, salt: [u8; 32]) -> Result<B256> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let answers = self.nodes.post_many(&self.nodes.indices(), "/v1/recipients", &Self::recipient_in(&r)).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "recipient")?;
        Ok(B256::from(self.store.lock().await.add_recipient(r)?))
    }

    /// The owner's payout destination, validated before any Base transaction burns zZEC.
    pub async fn add_destination(
        &self,
        vault_id: B256,
        ua: &str,
        salt: [u8; 32],
        nonce: u64,
        deadline: u64,
        signature: Bytes,
    ) -> Result<B256> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let v = self.vault(&vault_id).await?;
        let msg = Destination {
            vaultId: vault_id,
            commitment: B256::from(r.commitment()),
            nonce,
            deadline,
        };
        self.check_owner(&v, &msg, &signature, deadline)?;
        let body = DestinationIn {
            recipient: Self::recipient_in(&r),
            nonce,
            deadline,
            signature,
        };
        let path = format!("/v1/vaults/{vault_id}/destination");
        let answers = self.nodes.post_many(&self.nodes.indices(), &path, &body).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "destination")?;
        Ok(B256::from(self.store.lock().await.add_recipient(r)?))
    }

    /// The vault's viewing key, for the owner only.
    pub async fn viewing_key(&self, vault_id: B256, nonce: u64, deadline: u64, signature: &[u8]) -> Result<String> {
        let mut store = self.store.lock().await;
        let v = store.vaults.get(&vault_id).context(PublicError::UnknownVault)?;
        let msg = ViewKeyRequest {
            vaultId: vault_id,
            nonce,
            deadline,
        };
        self.check_owner(v, &msg, signature, deadline)?;
        ensure!(!v.used_view_nonces.contains(&nonce), PublicError::NonceUsed);
        store.update(&vault_id, |v| {
            v.used_view_nonces.push(nonce);
            v.ufvk.clone()
        })
    }

    /// The app's view of a vault, from the store alone: no Zcash lock, no scan (L-10). The
    /// deposits waiting for a refund proof are the ones persisted by the last scan (M-10).
    pub async fn view(&self, vault_id: B256) -> Result<VaultView> {
        let store = self.store.lock().await;
        let v = store.vaults.get(&vault_id).context(PublicError::UnknownVault)?;
        Ok(VaultView {
            vault_id,
            status: v.status,
            amount: v.amount,
            expiry_height: v.expiry_height,
            outstanding: v.outstanding,
            claims: v
                .claims
                .iter()
                .map(|c| ClaimView {
                    claim_id: B256::from(c.id),
                    kind: c.kind,
                    amount: c.amount,
                    paid: c.state == ClaimState::Paid,
                    state: c.state,
                    destination_set: c.commitment != [0; 32],
                })
                .collect(),
            non_conforming: v.non_conforming.clone(),
            inflight: v.inflight.is_some(),
            birthday_height: v.birthday,
            tip_height: self.last_tip.load(Ordering::Relaxed),
        })
    }

    /// The Zcash tip height the last coordinator pass saw (0 before the first one).
    pub fn zcash_tip(&self) -> u32 {
        self.last_tip.load(Ordering::Relaxed)
    }

    // ------------------------------------------------------------------ threshold demo

    /// DEMO: builds a transaction sweeping every unspent note of the vault to `to` and asks
    /// all signers for round-1 commitments. Honest signers refuse (the PCZT matches no
    /// plan); signers switched to "compromised" mode accept. Never broadcasts anything.
    pub async fn demo_drain(&self, vault_id: B256, to: &str) -> Result<DrainReport> {
        let (pczt, _, total) = self.sweep_pczt(vault_id, to).await?;
        let mut sid = [0u8; 16];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut sid);
        let req = CommitRequest {
            session_id: format!("demo-{}", hex::encode(sid)),
            vault_id,
            kind: SignKind::Payout { claim_ids: vec![] },
            pczt: pczt.serialize().map_err(|e| anyhow!("serialize PCZT: {e:?}"))?,
        };
        let answers: Vec<Answer<CommitResponse>> =
            self.nodes.post_many(&self.nodes.indices(), "/v1/sign/commit", &req).await;
        let votes: Vec<DrainVote> = answers
            .into_iter()
            .map(|(index, a)| match a {
                Ok(_) => DrainVote {
                    index,
                    signed: true,
                    reason: None,
                },
                Err(e) => DrainVote {
                    index,
                    signed: false,
                    reason: Some(scrubbed(&e)),
                },
            })
            .collect();
        let signed = votes.iter().filter(|v| v.signed).count();
        info!(vault = %short(&vault_id), signed, "threshold demo: sweep attempt");
        Ok(DrainReport {
            vault_id,
            amount: total,
            threshold_reached: signed >= usize::from(MIN_SIGNERS),
            votes,
        })
    }

    /// DEMO ONLY (`demo-tier3`, the separate theft demo, review T3-6): sweeps every unspent note
    /// of the vault to `to` with the signatures of three compromised nodes and broadcasts it,
    /// with no commitment on Base. That is an uncommitted spend (F1): the vault's watcher proves
    /// it and the slasher takes all five bonds. Returns the transaction id.
    #[cfg(feature = "demo-tier3")]
    pub async fn demo_steal(&self, vault_id: B256, to: &str) -> Result<String> {
        let v = self.vault(&vault_id).await?;
        let (pczt, mut indices, total) = self.sweep_pczt(vault_id, to).await?;
        indices.sort_unstable();
        let (sighash, requests) = spend::signing_requests(&pczt, &indices)?;
        let pczt_bytes = pczt.clone().serialize().map_err(|e| anyhow!("serialize PCZT: {e:?}"))?;
        let pkp: PublicKeyPackage = serde_json::from_value(v.public_key_package.clone())?;
        let signed = frost::demo_sign_uncommitted(
            &self.nodes,
            &frost::Payload {
                vault_id,
                kind: &SignKind::Payout { claim_ids: vec![] },
                pczt: &pczt_bytes,
                sighash,
                requests: &requests,
                public_key_package: &pkp,
            },
        )
        .await?;
        let tx = spend::extract(spend::apply_signatures(pczt, &signed.signatures)?)?;
        let txid = tx.txid().to_string();
        self.broadcast(spend::serialize(&tx)?).await?;
        warn!(vault = %short(&vault_id), amount = total, signers = ?signed.signers, "THEFT DEMO: uncommitted sweep broadcast");
        Ok(txid)
    }

    /// DEMO: the transaction sweeping every unspent note of the vault to `to` (proved, unsigned),
    /// the action indices of its vault spends, and the value it sweeps.
    async fn sweep_pczt(&self, vault_id: B256, to: &str) -> Result<(pczt::Pczt, Vec<usize>, u64)> {
        let attacker = keys::ironwood_receiver(&NETWORK, to).context(PublicError::InvalidReceiver)?;
        let v = self.vault(&vault_id).await?;
        let mut decisions = v.decisions.clone();
        let notes = self.notes(&v, &mut decisions).await?;
        let mut inputs: Vec<ReceivedNote> = notes.lineage.clone();
        inputs.extend(notes.refunds.iter().map(|(_, n)| n.clone()));
        inputs.extend(notes.unlanded.iter().map(|(_, n)| n.clone()));
        ensure!(!inputs.is_empty(), PublicError::BadRequest);
        let total: u64 = inputs.iter().map(|n| n.value()).sum();
        let fee = spend::zip317_fee(inputs.len(), 1);
        let payments = [Payment {
            recipient: attacker,
            value: total.checked_sub(fee).context(PublicError::BadRequest)?,
            privacy: spend::OutputPrivacy::NoOvk,
        }];
        let fvk = fvk_of(&v)?;
        let anchor = self.zcash.lock().await.scan(&vault_id).context("vault not scanned")?.anchor();
        let refs: Vec<&ReceivedNote> = inputs.iter().collect();
        let unsigned = spend::build(
            &NETWORK,
            BlockHeight::from_u32(notes.tip.checked_add(1).context("tip overflows")?),
            &fvk,
            anchor,
            &refs,
            &payments,
        )?;
        let pk = self.proving_key().await?;
        let indices = unsigned.spend_action_indices.clone();
        let pczt = tokio::task::spawn_blocking(move || spend::finalize_and_prove(unsigned.pczt, &pk)).await??;
        Ok((pczt, indices, total))
    }

    /// Forwards a demo mode switch to one node. The request is signed with the coordinator
    /// key like every node request (D16); which mode the caller may set on which node is the
    /// caller's policy (the backend allows "compromised" on two fixed nodes at most).
    pub async fn set_node_mode(&self, index: u16, mode: &str, token: &str) -> Result<serde_json::Value> {
        ensure!(self.nodes.contains(index), PublicError::BadRequest);
        let body = serde_json::json!({ "mode": mode, "token": token });
        self.nodes.post(index, "/v1/admin/mode", &body).await.map_err(|e| {
            let public = if is_refusal(&e) {
                PublicError::SignersRefused
            } else {
                PublicError::SignersUnavailable
            };
            e.context(public)
        })
    }

    /// Health of every node, under the index it was asked as (unreachable nodes are reported as
    /// such).
    pub async fn node_health(&self) -> Vec<serde_json::Value> {
        let mut out = vec![];
        for i in self.nodes.indices() {
            out.push(match self.nodes.get_json(i, "/v1/health").await {
                Ok(serde_json::Value::Object(mut v)) => {
                    v.insert("index".into(), i.into());
                    serde_json::Value::Object(v)
                }
                _ => serde_json::json!({ "index": i, "mode": "unreachable" }),
            });
        }
        out
    }

    // ------------------------------------------------------------------ main loop

    /// One pass over Base events and every vault. Errors of one vault do not stop others.
    pub async fn tick(&self) -> Result<()> {
        match tokio::time::timeout(BASE_SYNC_TIMEOUT, self.sync_base()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(code = public_error(&e).unwrap_or(PublicError::Internal).code(), "base sync failed");
                debug!(error = %scrubbed(&e), "base sync failed");
            }
            Err(_) => warn!("base sync timed out"),
        }
        let ids: Vec<B256> = self.store.lock().await.vaults.keys().copied().collect();
        for id in ids {
            // A vault whose pass panics must not starve the vaults after it.
            let pass = AssertUnwindSafe(tokio::time::timeout(VAULT_PASS_TIMEOUT, self.process(&id))).catch_unwind();
            match pass.await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => log_failure(&id, "vault pass", &e),
                Ok(Err(_)) => warn!(vault = %short(&id), "vault pass timed out"),
                Err(_) => error!(vault = %short(&id), "vault pass panicked"),
            }
        }
        Ok(())
    }

    /// Applies Registry events up to Base's safe head. No lock is held while the RPC answers.
    async fn sync_base(&self) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let safe = events::safe_head(&provider).await?;
        let cursor = self.store.lock().await.base_cursor;
        let start = cursor.max(self.cfg.registry_from_block.saturating_sub(1)) + 1;
        // H-11: as many blocks per step as `eth_getLogs` is allowed (`ZECURED_LOG_RANGE`).
        let step = events::log_range_ceiling();
        let mut from = start;
        while from <= safe {
            let to = (from + step - 1).min(safe);
            let evs = events::fetch(&provider, self.cfg.registry, from, to).await?;
            let mut store = self.store.lock().await;
            for ev in evs {
                if store.vaults.contains_key(&ev.vault_id) {
                    store.update(&ev.vault_id, |v| apply_event(v, &ev))?;
                }
            }
            store.set_base_cursor(to)?;
            from = to + 1;
        }
        Ok(())
    }

    async fn notes(&self, v: &CoordVault, decisions: &mut Vec<(NotePosition, Classification)>) -> Result<VaultNotes> {
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations: self.cfg.confirmations,
        };
        let fvk = fvk_of(v)?;
        let refunds = refund_tags(&v.claims);
        let base = BaseView {
            ufvk: &v.ufvk,
            address: &v.address,
            activated_tag: v.activated_tag,
            refund_tags: &refunds,
        };
        self.zcash.lock().await.notes(&v.vault_id, &fvk, v.birthday, &rules, decisions, base).await
    }

    fn idle_scan_is_due(&self, id: &B256) -> bool {
        let scans = self.idle_scans.lock().unwrap_or_else(|p| p.into_inner());
        idle_scan_due(scans.get(id).copied(), Instant::now())
    }

    fn mark_idle_scanned(&self, id: &B256) {
        self.idle_scans.lock().unwrap_or_else(|p| p.into_inner()).insert(*id, Instant::now());
    }

    async fn process(&self, id: &B256) -> Result<()> {
        let v = self.vault(id).await?;
        // Expired, closed and cancelled vaults with nothing to pay are only watched for late
        // deposits, which are refunded through proofs (T-02).
        let idle = v.is_idle();
        if idle && !self.idle_scan_is_due(id) {
            return Ok(());
        }
        let mut decisions = v.decisions.clone();
        let mut notes = self.notes(&v, &mut decisions).await?;
        self.last_tip.fetch_max(notes.tip, Ordering::Relaxed);
        let unlanded: Vec<NoteView> = notes
            .unlanded
            .iter()
            .map(|(note_id, n)| NoteView {
                note_id: B256::from(*note_id),
                amount: n.value(),
            })
            .collect();
        // Only what the scan produced is written; everything else stays as it is now.
        let v = self
            .update_vault(id, |cur| {
                cur.decisions = decisions;
                cur.non_conforming = unlanded;
                cur.clone()
            })
            .await?;
        self.warn_overdue(&v).await;
        if idle {
            self.mark_idle_scanned(id);
            return self.prove_refunds(&v, &notes).await;
        }
        // Plan from the same lineage as the signers (H-2): the activated deposit or the change
        // worth exactly the vault's book value; foreign and zero-value notes are ignored.
        notes.retain_planned_change(ledger::book_value(v.outstanding, &v.claims));

        if let Some(inflight) = v.inflight.clone() {
            return self.check_inflight(id, inflight, notes.tip).await;
        }
        match v.status {
            VaultStatus::AwaitingDeposit => {
                if let Some(conforming) = &notes.conforming {
                    // L5 (D15): a random number of extra confirmations hides when the deposit
                    // landed.
                    let delay = self.attest_delay(id, &v).await?;
                    let confs = classify::confirmations(conforming.position.height, notes.tip);
                    if confs < self.cfg.confirmations + delay {
                        return Ok(());
                    }
                    if self.proves_deposits() {
                        let target = proofs::Target::activation(conforming, v.amount);
                        self.prove_deposit(id, &target).await?;
                    }
                    // Otherwise the borrower proves it (`POST /vaults/:id/deposit-proof`, Y10).
                } else if notes.tip > v.expiry_height + self.cfg.confirmations {
                    self.update_vault(id, |cur| {
                        if cur.status == VaultStatus::AwaitingDeposit {
                            cur.status = VaultStatus::Expired;
                        }
                    })
                    .await?;
                }
            }
            // K3: a deposit proof waits on Base until the relay has moved on.
            VaultStatus::DepositSeen => self.finalize_pending(id).await?,
            VaultStatus::Active | VaultStatus::Closed | VaultStatus::Cancelled | VaultStatus::Expired => {
                if self.pay_next(&v, &notes).await? {
                    return Ok(());
                }
            }
        }
        let v = self.vault(id).await?;
        if v.inflight.is_none() {
            self.prove_refunds(&v, &notes).await?;
        }
        Ok(())
    }

    /// T-05: warns about a vault whose payable claims are older than half the bonds' liveness
    /// period: unpaid for the whole period, a claimant may take its claim's value from the bonds.
    /// A vault is named at most every half hour, by the first bytes of its id and a count only.
    async fn warn_overdue(&self, v: &CoordVault) {
        let period = match self.liveness_period().await {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(e) => {
                debug!(error = %scrubbed(&e), "liveness period unknown");
                return;
            }
        };
        let late = ledger::overdue(&v.claims, now(), period / 2).len();
        if late == 0 {
            return;
        }
        let mut warned = self.liveness_warned.lock().unwrap_or_else(|p| p.into_inner());
        if warned.get(&v.vault_id).is_some_and(|at| at.elapsed() < LIVENESS_WARNING_INTERVAL) {
            return;
        }
        warned.insert(v.vault_id, Instant::now());
        warn!(
            vault = %short(&v.vault_id),
            claims = late,
            "payable claims older than half the bonds' liveness period: pay them before their claimants can take their value from the bonds (T-05)"
        );
    }

    /// The Intake, the slasher and the bonds the Registry names (immutable, read once).
    pub(crate) async fn v3(&self) -> Result<V3Contracts> {
        let contracts = self
            .v3
            .get_or_try_init(|| async {
                let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
                let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
                anyhow::Ok(V3Contracts {
                    intake: registry.intake().call().await.context("Registry.intake")?,
                    slasher: registry.slasher().call().await.context("Registry.slasher")?,
                    bond: registry.signerBond().call().await.context("Registry.signerBond")?,
                })
            })
            .await?;
        Ok(*contracts)
    }

    /// T-05: the bonds' liveness period, read once; `None` on a deployment without bonds.
    async fn liveness_period(&self) -> Result<Option<u64>> {
        let period = self
            .liveness_period
            .get_or_try_init(|| async {
                let bond = self.v3().await?.bond;
                if bond == Address::ZERO {
                    return anyhow::Ok(None);
                }
                let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
                let period =
                    ISignerBond::new(bond, &provider).livenessPeriod().call().await.context("bond.livenessPeriod")?;
                anyhow::Ok(Some(period))
            })
            .await?;
        Ok(*period)
    }

    /// L23: Zcash blocks between a payout's commitment and its broadcast, drawn per payout from
    /// 1 to the maximum (0 when the maximum is 0), counted from the tip once the payout is signed
    /// (RT-13): a draw of 0 would go out right after the commitment.
    fn payout_delay(&self) -> u32 {
        let max = self.cfg.payout_delay_max_blocks;
        if max == 0 {
            0
        } else {
            1 + rand_core_06::RngCore::next_u32(&mut rand_core_06::OsRng) % max
        }
    }

    /// D15: extra confirmations before proving the deposit, drawn once per vault.
    async fn attest_delay(&self, id: &B256, v: &CoordVault) -> Result<u32> {
        if let Some(d) = v.attest_delay {
            return Ok(d);
        }
        let max = self.cfg.attest_delay_max_blocks;
        let d = if max == 0 {
            0
        } else {
            rand_core_06::RngCore::next_u32(&mut rand_core_06::OsRng) % (max + 1)
        };
        self.update_vault(id, |cur| *cur.attest_delay.get_or_insert(d)).await
    }

    // ------------------------------------------------------------------ payouts

    /// Returns true if a payout was started: the vault's next payout, from its main lineage or from
    /// the note of a refunded deposit, planned exactly as the signers plan it.
    async fn pay_next(&self, v: &CoordVault, notes: &VaultNotes) -> Result<bool> {
        if !v.claims.iter().any(|c| c.payable()) {
            return Ok(false);
        }
        // K3 and T-04: nothing is paid from a pending or compromised vault (the Registry refuses
        // both); checking first saves proving a transaction.
        if !self.payable_on_base(&v.vault_id).await? {
            return Ok(false);
        }
        // RT-3: the claims a commitment on Base binds to the lineage note come first, as the nodes plan.
        let bound = self.bound_claims(v, notes).await?;
        // The nodes never pay a vault's own addresses (security review H-2): plan the same way.
        let fvk = fvk_of(v)?;
        let (main, i10) = main_lineage(v.activated_tag.is_some(), v.outstanding, &v.claims, &notes.lineage, &bound);
        if let Err(e) = &i10
            && v.activated_tag.is_some()
            && v.claims.iter().any(|c| c.kind.main_lineage() && c.payable())
        {
            warn!(vault = %short(&v.vault_id), "the vault's main lineage does not add up (I10): nothing is paid from it");
            debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!("{e}")), "I10");
        }
        let next = {
            let store = self.store.lock().await;
            let recipient =
                |c: &_| store.recipient(c).filter(|r| !zecured_node::chain::owns_receiver(&fvk, &r.receiver));
            ledger::next_vault_payout(&v.claims, recipient, main, &notes.refund_values(), now())?
        };
        let NextPayout::Pay(plan) = next else { return Ok(false) };
        let inputs: Vec<ReceivedNote> = match plan.input {
            PayoutInput::Main => notes.lineage.clone(),
            PayoutInput::Refund(tag) => {
                notes.refunds.iter().filter(|(t, _)| *t == tag).map(|(_, n)| n.clone()).collect()
            }
        };
        // RT-4: a note pays only the claim list of its first commitment.
        if !self.claim_set_allows(v, &inputs, &plan).await? {
            debug!(vault = %short(&v.vault_id), "the input's first commitment named other claims; not paying");
            return Ok(false);
        }
        let kind = SignKind::Payout {
            claim_ids: plan.claims.iter().map(|c| B256::from(*c)).collect(),
        };
        self.execute(&v.vault_id, kind, &plan, &inputs, notes.tip).await?;
        Ok(true)
    }

    /// K3 and T-04: whether Base lets the vault's claims be paid now: its activation is not
    /// pending, and the slasher has not marked it compromised.
    async fn payable_on_base(&self, id: &B256) -> Result<bool> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let status = IZecuredRegistry::new(self.cfg.registry, &provider)
            .vault(*id)
            .call()
            .await
            .context("Registry.vault")?
            .status;
        if status == VAULT_PENDING {
            return Ok(false);
        }
        let slasher = self.v3().await?.slasher;
        if slasher == Address::ZERO {
            return Ok(true);
        }
        let compromised = IZecuredSlasher::new(slasher, &provider)
            .isCompromised(*id)
            .call()
            .await
            .context("slasher.isCompromised")?;
        Ok(!compromised)
    }

    /// RT-4: whether the Registry takes `plan`'s claim list from its one input: the list of the
    /// input's first commitment, if it has one.
    async fn claim_set_allows(&self, v: &CoordVault, inputs: &[ReceivedNote], plan: &PayoutPlan) -> Result<bool> {
        let [input] = inputs else {
            return Ok(false); // the nodes sign payouts of exactly one note
        };
        let cmx = orchard::note::ExtractedNoteCommitment::from(input.note.commitment()).to_bytes();
        let tag = B256::from(deposit_tag(&v.ufvk, &cmx));
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let set = IZecuredRegistry::new(self.cfg.registry, &provider)
            .inputClaimSet(tag)
            .call()
            .await
            .context("Registry.inputClaimSet")?;
        Ok(set == B256::ZERO || set.0 == ledger::claim_set_hash(&plan.claims))
    }

    /// RT-3: the unpaid claims that a commitment on Base binds to the vault's main lineage note.
    async fn bound_claims(&self, v: &CoordVault, notes: &VaultNotes) -> Result<Vec<[u8; 32]>> {
        let [input] = notes.lineage.as_slice() else {
            return Ok(vec![]);
        };
        let cmx = orchard::note::ExtractedNoteCommitment::from(input.note.commitment()).to_bytes();
        let tag = B256::from(deposit_tag(&v.ufvk, &cmx));
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let mut bound = vec![];
        for c in v.claims.iter().filter(|c| c.state == ClaimState::Unpaid) {
            if registry.claimInput(B256::from(c.id)).call().await.context("Registry.claimInput")? == tag {
                bound.push(c.id);
            }
        }
        Ok(bound)
    }

    /// TEST ONLY: consumes the withhold flag if this build has the `test-hooks` feature.
    fn take_withhold_flag(&self) -> bool {
        #[cfg(feature = "test-hooks")]
        {
            std::fs::remove_file(&self.withhold_flag).is_ok()
        }
        #[cfg(not(feature = "test-hooks"))]
        {
            false
        }
    }

    /// Builds, proves, gets signed (3 of 5) and broadcasts the transaction for `plan`.
    async fn execute(
        &self,
        id: &B256,
        kind: SignKind,
        plan: &PayoutPlan,
        inputs: &[ReceivedNote],
        tip: u32,
    ) -> Result<()> {
        let v = self.vault(id).await?;
        let fvk = fvk_of(&v)?;
        let anchor = self.zcash.lock().await.scan(id).context("vault not scanned")?.anchor();
        let payments: Vec<Payment> = plan
            .payments
            .iter()
            .map(|p| {
                Ok(Payment {
                    recipient: Option::from(orchard::Address::from_raw_address_bytes(&p.receiver))
                        .context("invalid receiver")?,
                    value: p.value,
                    privacy: p.privacy,
                })
            })
            .collect::<Result<_>>()?;
        let refs: Vec<&ReceivedNote> = inputs.iter().collect();
        let unsigned = spend::build(
            &NETWORK,
            BlockHeight::from_u32(tip.checked_add(1).context("tip overflows")?),
            &fvk,
            anchor,
            &refs,
            &payments,
        )?;
        ensure!(
            unsigned.fee == plan.fee && unsigned.change == plan.change,
            "built transaction does not match the plan"
        );
        let pk = self.proving_key().await?;
        let pczt = tokio::task::spawn_blocking(move || spend::finalize_and_prove(unsigned.pczt, &pk)).await??;
        let mut indices = unsigned.spend_action_indices.clone();
        indices.sort_unstable();
        let (sighash, requests) = spend::signing_requests(&pczt, &indices)?;
        let expiry = *pczt.global().expiry_height();
        let pczt_bytes = pczt.clone().serialize().map_err(|e| anyhow!("serialize PCZT: {e:?}"))?;
        let pkp: PublicKeyPackage = serde_json::from_value(v.public_key_package.clone())?;

        let signed = frost::sign(
            &self.nodes,
            &frost::Payload {
                vault_id: *id,
                kind: &kind,
                pczt: &pczt_bytes,
                sighash,
                requests: &requests,
                public_key_package: &pkp,
            },
            self,
        )
        .await?;
        let pczt = spend::apply_signatures(pczt, &signed.signatures)?;
        let tx = spend::extract(pczt)?;
        let raw = spend::serialize(&tx)?;

        // Record the payout before broadcasting it: if the broadcast fails or its response is
        // lost, the same transaction is rebroadcast until it is mined or provably dead, and
        // nothing else is built for these claims in the meantime.
        let withhold = self.take_withhold_flag();
        let SignKind::Payout { claim_ids } = &kind;
        let claim_ids = claim_ids.clone();
        // L23: a committed payout waits a random number of blocks before its broadcast.
        let delay = self.payout_delay();
        // RT-13: counted from the tip now that the payout is signed, not from when it was planned.
        let signed_tip = if delay == 0 {
            tip
        } else {
            self.fetch_zcash_tip().await.unwrap_or(tip).max(tip)
        };
        let record = Inflight {
            claim_ids,
            txid: tx.txid().to_string(),
            expiry_height: expiry,
            raw_tx: (!withhold).then(|| hex::encode(&raw)),
            inputs: inputs.iter().map(|n| B256::from(n.nullifier.to_bytes())).collect(),
            broadcast_from: if delay == 0 {
                0
            } else {
                signed_tip.saturating_add(delay)
            },
            spend_commit: signed.spend_commit.clone().map(|(record, sigs)| KeptCommitment { record, sigs }),
        };
        self.update_vault(id, |cur| cur.inflight = Some(record)).await?;
        if withhold {
            warn!(vault = %short(id), "TEST: payout withheld; it will expire unmined");
            return Ok(());
        }
        if delay > 0 {
            info!(vault = %short(id), blocks = delay, "payout signed; broadcast held back (L23)");
            return Ok(());
        }
        match self.broadcast(raw).await {
            Ok(()) => info!(vault = %short(id), signers = ?signed.signers, "payout broadcast"),
            Err(e) => log_failure(id, "payout broadcast (will retry)", &e),
        }
        Ok(())
    }

    /// The Zcash tip now, from the light wallet server.
    async fn fetch_zcash_tip(&self) -> Result<u32> {
        let mut client = zecured_zcash_ops::lwd::connect(&self.cfg.lightwalletd_url).await?;
        Ok(u32::try_from(zecured_zcash_ops::lwd::latest_block(&mut client).await?.height)?)
    }

    async fn broadcast(&self, raw: Vec<u8>) -> Result<()> {
        let mut client = zecured_zcash_ops::lwd::connect(&self.cfg.lightwalletd_url).await?;
        spend::broadcast_raw(&mut client, raw).await
    }

    /// Our own scan's view of the payout's vault inputs.
    async fn inputs_state(&self, id: &B256, inflight: &Inflight) -> Inputs {
        let nullifiers = if !inflight.inputs.is_empty() {
            inflight.inputs.clone()
        } else {
            // Records written before `inputs` existed: the transaction's own nullifiers.
            match inflight.raw_tx.as_deref().map(|raw| inflight::nullifiers_of(raw, &NETWORK, inflight.expiry_height)) {
                Some(Ok(n)) => n,
                _ => return Inputs::Unknown,
            }
        };
        let zcash = self.zcash.lock().await;
        let Some(scan) = zcash.scan(id) else {
            return Inputs::Unknown;
        };
        let seen: Vec<bool> = nullifiers
            .iter()
            .filter_map(|nf| scan.notes.iter().find(|n| n.nullifier.to_bytes() == nf.0).map(|n| n.spent_at.is_some()))
            .collect();
        if seen.is_empty() {
            Inputs::Unknown
        } else if seen.contains(&true) {
            Inputs::Spent
        } else {
            Inputs::Unspent
        }
    }

    /// Follows a broadcast payout: confirms it on Base once it has enough confirmations,
    /// rebroadcasts it while it can still be mined, and drops it (to be rebuilt) only once it
    /// is provably dead (I-5).
    async fn check_inflight(&self, id: &B256, inflight: Inflight, tip: u32) -> Result<()> {
        // RT-5: a payout that may still be mined (or was) must keep its commitment on Base.
        if let Some(kept) = &inflight.spend_commit {
            self.keep_commitment(kept).await?;
        }
        let txid = TxId::from_hex(&inflight.txid).context("stored txid")?;
        let lookup = match zecured_zcash_ops::lwd::connect(&self.cfg.lightwalletd_url).await {
            Ok(mut client) => inflight::lookup(&mut client, &NETWORK, &txid).await,
            Err(e) => Err(e),
        };
        let inputs = self.inputs_state(id, &inflight).await;
        let action = inflight::decide(&lookup, tip, inflight.expiry_height, self.cfg.confirmations, inputs);
        if let Err(e) = &lookup {
            debug!(vault = %short(id), error = %scrubbed(e), "payout status unknown");
        }
        match action {
            Action::Confirm => self.settle(id, &inflight).await,
            Action::Wait => Ok(()),
            Action::Rebroadcast if tip < inflight.broadcast_from => Ok(()), // L23: held back
            Action::Rebroadcast => {
                // "Already in the mempool" rejections are expected and harmless.
                if let Some(raw) = inflight.raw_tx.as_deref().and_then(|h| hex::decode(h).ok()) {
                    let _ = self.broadcast(raw).await;
                }
                Ok(())
            }
            Action::Rebuild => {
                warn!(vault = %short(id), "payout expired unmined; will rebuild");
                self.update_vault(id, |cur| {
                    if cur.inflight.as_ref().is_some_and(|f| f.txid == inflight.txid) {
                        cur.inflight = None;
                    }
                })
                .await
            }
            Action::Investigate => {
                error!(
                    vault = %short(id),
                    "payout inputs are spent by a transaction the light wallet server does not report: \
                     not rebuilding; investigate"
                );
                Ok(())
            }
        }
    }

    /// The mined payout is confirmed on Base (unless it already is) and its record closed.
    async fn settle(&self, id: &B256, inflight: &Inflight) -> Result<()> {
        if !inflight.claim_ids.is_empty() {
            match self.claims_settled(id, &inflight.claim_ids).await? {
                // H-5: confirmPayout went through but its receipt was lost (or another
                // coordinator pass sent it): sending it again would revert forever.
                Settled::All => info!(vault = %short(id), "payout already confirmed on Base"),
                Settled::None => self.confirm_on_base(id, inflight).await?,
                Settled::Partial => bail!("the claims of one payout are partly paid on Base; investigate"),
            }
        }
        self.update_vault(id, |v| {
            for c in v.claims.iter_mut().filter(|c| inflight.claim_ids.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
            }
            if v.status == VaultStatus::Active && v.outstanding == 0 && v.settled() {
                v.status = VaultStatus::Closed;
            }
            if v.inflight.as_ref().is_some_and(|f| f.txid == inflight.txid) {
                v.inflight = None;
            }
        })
        .await
    }

    /// Whether the Registry already marks a payout's claims paid (H-5).
    async fn claims_settled(&self, id: &B256, claim_ids: &[B256]) -> Result<Settled> {
        let known_paid = {
            let store = self.store.lock().await;
            store.vaults.get(id).is_some_and(|v| {
                claim_ids.iter().all(|c| v.claims.iter().any(|x| x.id == c.0 && x.state == ClaimState::Paid))
            })
        };
        if known_paid {
            return Ok(Settled::All); // PayoutConfirmed already seen at the safe head
        }
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IRegistryClaims::new(self.cfg.registry, &provider);
        let mut states = Vec::with_capacity(claim_ids.len());
        for c in claim_ids {
            let r = registry.claim(*c).call().await.context("Registry.claim")?;
            states.push((r.vaultId, r.status));
        }
        Ok(inflight::settled(id, &states))
    }

    async fn confirm_on_base(&self, id: &B256, inflight: &Inflight) -> Result<()> {
        let path = format!("/v1/vaults/{id}/confirm");
        let body = ConfirmIn {
            claim_ids: inflight.claim_ids.clone(),
            txid: inflight.txid.clone(),
        };
        let answers = self.nodes.post_many::<_, ConfirmResponse>(&self.nodes.indices(), &path, &body).await;
        let (answers, failed) = split(answers);
        let msg = PayoutConfirmation {
            vaultId: *id,
            claimIdsHash: claim_ids_hash(&inflight.claim_ids),
        };
        let need = usize::from(MIN_SIGNERS);
        let Some(sigs) = select::confirmation(&self.cfg.nodes, &self.domain(), &msg, &answers, need) else {
            return Err(if answers.len() >= need {
                anyhow!("fewer than {need} valid confirmation signatures").context(PublicError::SignersUnavailable)
            } else {
                shortfall("payout confirmation", answers.len(), need, &failed)
            });
        };
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let call = registry.confirmPayout(*id, inflight.claim_ids.clone(), sigs);
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate confirmPayout")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send confirmPayout")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("confirmPayout receipt")?;
        ensure!(receipt.status(), "confirmPayout reverted");
        info!(vault = %short(id), "payout confirmed on Base");
        Ok(())
    }

    /// Tier 3 (docs/TIER3.md § 5): records a payout's commitment on Base with the round-1
    /// signatures of the chosen signers, each checked against its node's attestation address and
    /// sent in signer order, then waits until it is as deep as the nodes check before they release
    /// a share: under Base's `safe` head, or `commit_depth_secs` old (review H-11), and in any case
    /// `commit_depth_blocks` deep. A commitment already on Base comes from an earlier
    /// session whose signatures this coordinator no longer has: the payout is built again (a new
    /// transaction, so a new commitment).
    async fn commit_spend(&self, record: &SpendCommitRecord, signed: &[(u16, Bytes)]) -> Result<Vec<Bytes>> {
        let msg = record.message();
        let domain = self.domain();
        let valid: Vec<(Address, Bytes)> = signed
            .iter()
            .filter_map(|(index, sig)| {
                let eth = self.cfg.nodes.iter().find(|n| n.index == *index)?.eth;
                select::signed_by(&domain, &msg, sig, eth).then(|| (eth, sig.clone()))
            })
            .collect();
        let sigs = select::sorted_threshold(valid, usize::from(MIN_SIGNERS))
            .context("fewer than three valid commitment signatures")?;
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let existing = registry.spendRecord(record.payout_ref).call().await.context("Registry.spendRecord")?;
        ensure!(existing.vaultId == B256::ZERO, "this payout was committed by an earlier session; building it again");
        let call = registry.commitSpend((&msg).into(), sigs.clone());
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate commitSpend")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send commitSpend")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("commitSpend receipt")?;
        ensure!(receipt.status(), "commitSpend reverted");
        let block = receipt.block_number.context("commitSpend receipt without a block")?;
        let committed_at = provider
            .get_block_by_number(block.into())
            .await
            .context("Base block of the commitment")?
            .context("Base block of the commitment not found")?
            .header
            .timestamp;
        let wait = Duration::from_secs(self.cfg.commit_depth_secs + COMMIT_WAIT_MARGIN_SECS);
        let started = Instant::now();
        loop {
            let (latest, now_on_base) = events::latest_block(&provider).await.context("Base latest block")?;
            let safe = events::safe_head(&provider).await.ok();
            if ready_for_round_two(&self.cfg, block, committed_at, latest, now_on_base, safe) {
                break;
            }
            ensure!(started.elapsed() < wait, "Base did not take the commitment deep enough in time");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        info!(vault = %short(&record.vault_id), "payout committed on Base");
        Ok(sigs)
    }

    /// RT-5: Base still holds the payout's commitment, or it is submitted again with the
    /// signatures it was first recorded with (a Base reorg dropped it). Anyone may submit a
    /// commitment, and a slashing case for the payout is dismissed once it is back.
    async fn keep_commitment(&self, kept: &KeptCommitment) -> Result<()> {
        let msg = kept.record.message();
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let on_base = registry.spendRecord(kept.record.payout_ref).call().await.context("Registry.spendRecord")?;
        if !commitment_missing(&on_base, msg.eip712_hash_struct())? {
            return Ok(());
        }
        warn!(vault = %short(&kept.record.vault_id), "a payout's commitment is missing on Base: submitting it again");
        let call = registry.commitSpend((&msg).into(), kept.sigs.clone());
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate commitSpend")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send commitSpend")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("commitSpend receipt")?;
        ensure!(receipt.status(), "commitSpend reverted");
        Ok(())
    }
}

/// H-11, T3-4: whether a commitment mined in Base block `block` (at Unix time `committed_at`) is
/// as deep as the nodes check before they release a share (`zecured_node::commitment_deep_enough`),
/// with Base's latest block `latest` at `now_on_base` and its safe head `safe` (if the provider has
/// one): at least `commit_depth_blocks` deep, and under the safe head or `commit_depth_secs` old.
/// The age carries the slack of the nodes' search for a block that old and a second for a node
/// whose provider lags.
fn ready_for_round_two(
    cfg: &NetworkConfig,
    block: u64,
    committed_at: u64,
    latest: u64,
    now_on_base: u64,
    safe: Option<u64>,
) -> bool {
    let floor = latest > block.saturating_add(cfg.commit_depth_blocks);
    let under_safe = safe.is_some_and(|s| s >= block);
    let margin = cfg.commit_depth_secs.saturating_add(events::SECS_DEEP_SLACK + 1);
    let old_enough = now_on_base >= committed_at.saturating_add(margin);
    floor && (under_safe || old_enough)
}

/// RT-5: whether Base lacks the commitment whose struct hash is `record_hash` (so it must be
/// submitted again). Another record under the same payout reference cannot be fixed by resubmitting.
fn commitment_missing(on_base: &IZecuredRegistry::SpendRecord, record_hash: B256) -> Result<bool> {
    if on_base.recordHash == record_hash {
        return Ok(false);
    }
    ensure!(on_base.vaultId == B256::ZERO, "another commitment holds this payout's reference; investigate");
    Ok(true)
}

impl frost::SpendCommitter for Coordinator {
    async fn commit(&self, record: &SpendCommitRecord, signed: &[(u16, Bytes)]) -> Result<Vec<Bytes>> {
        self.commit_spend(record, signed).await
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use zecured_base::sign_typed;
    use zecured_policy::ledger::{Claim, ClaimKind};

    use super::*;
    use crate::{
        nodes::tests::config,
        store::tests::{TempDir, vault},
    };

    #[test]
    fn zip321_amounts() {
        assert_eq!(zip321_uri("utest1x", 1_000_000), "zcash:utest1x?amount=0.01");
        assert_eq!(zip321_uri("utest1x", 5_000_000), "zcash:utest1x?amount=0.05");
        assert_eq!(zip321_uri("utest1x", 100_000_000), "zcash:utest1x?amount=1");
        assert_eq!(zip321_uri("utest1x", 123_456_789), "zcash:utest1x?amount=1.23456789");
    }

    /// A coordinator whose nodes, light wallet server and RPC are all unreachable: anything
    /// that reaches them fails, so passing checks prove the request never left.
    fn offline(tag: &str) -> (Coordinator, TempDir) {
        let dir = TempDir::new(tag);
        let urls: Vec<String> = (1..=5).map(|_| "http://127.0.0.1:9".to_owned()).collect();
        let key = hex::encode(PrivateKeySigner::random().to_bytes());
        (Coordinator::new(config(&urls), &dir.0, &key).unwrap(), dir)
    }

    fn claim(id: u8, amount: u64) -> Claim {
        Claim {
            id: [id; 32],
            kind: ClaimKind::Liquidation,
            amount,
            commitment: [id; 32],
            order: (10, u64::from(id)),
            created_at: 0,
            partner: None,
            state: ClaimState::Unpaid,
            refund_tag: None,
            bond_paid: false,
        }
    }

    /// `event` at block 10, at Unix time 20.
    fn at(event: RegistryEvent) -> VaultEvent {
        VaultEvent {
            vault_id: B256::repeat_byte(1),
            block: 10,
            log_index: 0,
            block_hash: None,
            timestamp: 20,
            event,
        }
    }

    /// A refund of `amount` paid from the note `tag`, without a destination yet.
    fn refund(id: u8, amount: u64, tag: u8) -> Claim {
        Claim {
            kind: ClaimKind::Refund,
            commitment: [0; 32],
            refund_tag: Some([tag; 32]),
            ..claim(id, amount)
        }
    }

    /// Registry v3 (T-02, K3): the coordinator's record follows the vault on Base. A deposit to a
    /// vault Base does not know, or a pending activation converted, lands as a refund and cancels
    /// the vault; a refund of a second deposit leaves a pending or active vault as it is; a
    /// released reservation sends the vault back to waiting (or cancelled, with a refund).
    #[test]
    fn the_vault_follows_its_status_on_base() {
        let reserved = |tag: u8| {
            at(RegistryEvent::Reserved {
                account: Address::repeat_byte(101),
                amount: 1_000_000,
                deposit_tag: B256::repeat_byte(tag),
            })
        };
        // A late deposit to an expired vault, refunded by proof: cancelled.
        let mut v = vault(1);
        v.status = VaultStatus::Expired;
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![refund(1, 300_000, 9)])));
        assert_eq!((v.status, v.outstanding), (VaultStatus::Cancelled, 0));
        assert!(v.is_idle(), "nothing payable until the account gives a destination");
        let destination = at(RegistryEvent::ClaimChanged {
            claim_id: B256::repeat_byte(1),
            change: ClaimChange::RefundDestination(B256::repeat_byte(5)),
        });
        apply_event(&mut v, &destination);
        assert!(!v.is_idle() && v.claims[0].payable());
        assert_eq!(v.claims[0].created_at, 20, "payable from its destination on (T-05)");

        // A pending activation: a refund of another note keeps it pending; the conversion of its
        // own note cancels it.
        let mut v = vault(1);
        apply_event(&mut v, &reserved(7));
        assert_eq!((v.status, v.pending_tag), (VaultStatus::DepositSeen, Some([7; 32])));
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![refund(1, 300_000, 9)])));
        assert_eq!(v.status, VaultStatus::DepositSeen);
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![refund(2, 1_000_000, 7)])));
        assert_eq!((v.status, v.pending_tag), (VaultStatus::Cancelled, None));

        // A released reservation: waiting again, or cancelled if a refund landed meanwhile.
        let mut v = vault(1);
        apply_event(&mut v, &reserved(7));
        apply_event(&mut v, &at(RegistryEvent::ReservationReleased));
        assert_eq!((v.status, v.pending_tag), (VaultStatus::AwaitingDeposit, None));
        apply_event(&mut v, &reserved(8));
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![refund(1, 300_000, 9)])));
        apply_event(&mut v, &at(RegistryEvent::ReservationReleased));
        assert_eq!(v.status, VaultStatus::Cancelled);

        // An active vault: a refund of a second deposit does not touch its outstanding; the fee
        // (T-06) is minted less.
        let mut v = vault(1);
        let activated = at(RegistryEvent::VaultActivated {
            account: v.account,
            amount: 5_000_000,
            deposit_tag: B256::repeat_byte(0x7a),
        });
        apply_event(&mut v, &activated);
        let fee = Claim {
            kind: ClaimKind::Fee,
            ..claim(3, 25_000)
        };
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![fee, refund(4, 300_000, 9)])));
        assert_eq!((v.status, v.outstanding), (VaultStatus::Active, 4_975_000));
    }

    /// T-05 (a): a disputed confirmation is unpaid again until it is upheld or reopened.
    #[test]
    fn disputes_move_claims_back_and_forth() {
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        v.activated_tag = Some([0x7a; 32]);
        v.outstanding = 0;
        let mut paid = claim(1, 400_000);
        paid.state = ClaimState::Paid;
        v.claims = vec![paid];
        let change = |change: ClaimChange| {
            at(RegistryEvent::ClaimChanged {
                claim_id: B256::repeat_byte(1),
                change,
            })
        };
        apply_event(&mut v, &change(ClaimChange::Disputed { due_at: 99 }));
        assert_eq!(v.claims[0].state, ClaimState::Disputed);
        assert!(!v.is_idle() && !v.settled());
        apply_event(&mut v, &change(ClaimChange::Reopened));
        assert_eq!(v.claims[0].state, ClaimState::Unpaid);
        apply_event(&mut v, &change(ClaimChange::Disputed { due_at: 199 }));
        apply_event(
            &mut v,
            &change(ClaimChange::Upheld {
                payout_ref: B256::repeat_byte(2),
            }),
        );
        assert_eq!(v.claims[0].state, ClaimState::Paid);
    }

    /// M-9 (coordinator side): re-applying a chunk after a crash must not reset `outstanding`.
    #[test]
    fn registry_events_apply_once() {
        let mut v = vault(1);
        v.status = VaultStatus::DepositSeen;
        let activated = at(RegistryEvent::VaultActivated {
            account: v.account,
            amount: 1_000_000,
            deposit_tag: B256::repeat_byte(0x7a),
        });
        apply_event(&mut v, &activated);
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![claim(1, 400_000)])));
        // The same chunk again.
        apply_event(&mut v, &activated);
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![claim(1, 400_000)])));
        assert_eq!(v.status, VaultStatus::Active);
        assert_eq!(v.outstanding, 600_000);
        assert_eq!(v.claims.len(), 1);
        apply_event(&mut v, &at(RegistryEvent::Claims(vec![claim(2, 600_000)])));
        apply_event(&mut v, &at(RegistryEvent::PayoutConfirmed(vec![B256::repeat_byte(1), B256::repeat_byte(2)])));
        assert_eq!(v.status, VaultStatus::Closed);
    }

    #[test]
    fn idle_vaults_are_rescanned_at_a_low_frequency() {
        let t0 = Instant::now();
        assert!(idle_scan_due(None, t0), "first pass after a restart");
        assert!(!idle_scan_due(Some(t0), t0 + Duration::from_secs(30)));
        assert!(idle_scan_due(Some(t0), t0 + IDLE_SCAN_INTERVAL));
    }

    /// L-10 / M-10: the view needs neither the Zcash lock nor a scan, and shows the deposits
    /// waiting for a refund proof persisted by the last scan (e.g. of an expired vault after a
    /// restart).
    #[tokio::test]
    async fn the_view_reads_the_store_only() {
        let (c, _dir) = offline("view");
        let mut v = vault(1);
        v.status = VaultStatus::Expired;
        v.non_conforming = vec![NoteView {
            note_id: B256::repeat_byte(9),
            amount: 20_000,
        }];
        c.store.lock().await.save(v).unwrap();
        let _scan_in_progress = c.zcash.lock().await;
        let view = tokio::time::timeout(Duration::from_secs(2), c.view(B256::repeat_byte(1)))
            .await
            .expect("the view does not wait for the Zcash lock")
            .unwrap();
        assert_eq!(view.non_conforming.len(), 1);
        let e = c.view(B256::repeat_byte(2)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::UnknownVault));
    }

    fn open_request(owner: &PrivateKeySigner, c: &Coordinator, deadline: u64) -> OpenVaultRequest {
        let request_id = B256::repeat_byte(3);
        let msg = OpenVault {
            owner: owner.address(),
            account: Address::repeat_byte(0xee),
            amount: 1_000_000,
            requestId: request_id,
            deadline,
        };
        stamped(
            OpenVaultRequest {
                owner: owner.address(),
                account: msg.account,
                amount: msg.amount,
                request_id,
                deadline,
                signature: sign_typed(owner, &c.domain(), &msg).unwrap(),
                stamp: 0,
            },
            c,
        )
    }

    /// `req` with a stamp of the coordinator's bits for its request id and owner.
    fn stamped(mut req: OpenVaultRequest, c: &Coordinator) -> OpenVaultRequest {
        req.stamp = zecured_base::stamp::solve(&req.request_id, &req.owner, c.cfg.open_vault.stamp_bits, 0);
        req
    }

    /// A coordinator as `offline`, with an 8-bit stamp (quick to compute in tests).
    fn offline_with_stamps(tag: &str) -> (Coordinator, TempDir) {
        let (mut c, dir) = offline(tag);
        c.cfg.open_vault.stamp_bits = 8;
        (c, dir)
    }

    const UFVK: &str = "uviewtest1vaultkey";

    /// A five-node network whose attestation keys the test holds.
    fn keyed_network() -> (NetworkConfig, Vec<PrivateKeySigner>) {
        let urls: Vec<String> = (1..=5).map(|_| "http://127.0.0.1:9".to_owned()).collect();
        let mut cfg = config(&urls);
        let keys: Vec<PrivateKeySigner> = (0..5).map(|_| PrivateKeySigner::random()).collect();
        for (node, key) in cfg.nodes.iter_mut().zip(&keys) {
            node.eth = key.address();
        }
        (cfg, keys)
    }

    fn vault_open() -> VaultOpen {
        VaultOpen {
            vault_id: B256::repeat_byte(7),
            owner: Address::repeat_byte(0xaa),
            account: Address::repeat_byte(0xee),
            amount: 1_000_000,
            request_id: B256::repeat_byte(3),
            deadline: u64::MAX,
            signature: Bytes::new(),
            birthday: 4_486_000,
            expiry_height: 4_486_288,
        }
    }

    /// `VaultOpened` for `open` and `address`, signed by `key`.
    fn opened_signature(cfg: &NetworkConfig, key: &PrivateKeySigner, open: &VaultOpen, address: &str) -> Bytes {
        let msg = vault_opened(open.vault_id, open.owner, open.account, open.amount, open.expiry_height, address, UFVK);
        sign_typed(key, &registry_domain(cfg.chain_id, cfg.registry), &msg).unwrap()
    }

    /// Round 3 as five honest nodes answer it.
    fn round3_answers(cfg: &NetworkConfig, keys: &[PrivateKeySigner], open: &VaultOpen) -> Vec<(u16, Round3Out)> {
        cfg.nodes
            .iter()
            .zip(keys)
            .map(|(node, key)| {
                let out = Round3Out {
                    index: node.index,
                    ak: "ab".repeat(32),
                    ufvk: UFVK.into(),
                    address: "utest1vault".into(),
                    public_key_package: serde_json::json!({ "group": "key" }),
                    signature: opened_signature(cfg, key, open, "utest1vault"),
                };
                (node.index, out)
            })
            .collect()
    }

    /// C-4: the address and viewing key reach the owner only with every signer's `VaultOpened`
    /// signature, checked against that node's configured attestation address, ordered by index
    /// and kept with the vault.
    #[test]
    fn a_vault_opens_with_every_signers_signature() {
        let (cfg, keys) = keyed_network();
        let domain = registry_domain(cfg.chain_id, cfg.registry);
        let open = vault_open();
        let mut answers = round3_answers(&cfg, &keys, &open);
        answers.reverse(); // the order nodes answer in does not matter
        let (vault, resp) = finish_open(&cfg, &domain, &open, &answers).unwrap();
        assert_eq!(resp.signatures.iter().map(|s| s.index).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert_eq!(resp.tag_key_hash, zecured_base::tag_key_hash(UFVK));
        assert_eq!((resp.address.as_str(), resp.amount, resp.expiry_height), ("utest1vault", 1_000_000, 4_486_288));
        assert_eq!(resp.zip321, "zcash:utest1vault?amount=0.01");
        assert_eq!(vault.opened_signatures, resp.signatures);
        assert_eq!(vault.ufvk, UFVK);

        // Persisted with the vault, so they can be served again after a restart.
        let dir = TempDir::new("opened");
        Store::open(&dir.0).unwrap().save(vault).unwrap();
        assert_eq!(Store::open(&dir.0).unwrap().vaults[&open.vault_id].opened_signatures, resp.signatures);
    }

    #[test]
    fn a_vault_without_every_signers_signature_is_refused() {
        let (cfg, keys) = keyed_network();
        let domain = registry_domain(cfg.chain_id, cfg.registry);
        let open = vault_open();
        let honest = round3_answers(&cfg, &keys, &open);
        let refused = |answers: &[(u16, Round3Out)]| {
            let e = finish_open(&cfg, &domain, &open, answers).unwrap_err();
            assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable), "{e:#}");
        };
        // Node 3 signed another address (say, one a compromised coordinator would hand out).
        let mut other = honest.clone();
        other[2].1.signature = opened_signature(&cfg, &keys[2], &open, "utest1attacker");
        refused(&other);
        // Node 2 relays node 5's signature; node 4 signs with a key that is not its own.
        let mut relayed = honest.clone();
        relayed[1].1.signature = honest[4].1.signature.clone();
        refused(&relayed);
        let mut foreign = honest.clone();
        foreign[3].1.signature = opened_signature(&cfg, &PrivateKeySigner::random(), &open, "utest1vault");
        refused(&foreign);
        // A signature for another expiry height than the one the coordinator chose.
        let mut late = open.clone();
        late.expiry_height += 1;
        let mut expiry = honest.clone();
        expiry[0].1.signature = opened_signature(&cfg, &keys[0], &late, "utest1vault");
        refused(&expiry);
        // Four of five, a node answering as another, nodes disagreeing on the address.
        refused(&honest[..4]);
        let mut renamed = honest.clone();
        renamed[0].1.index = 2;
        refused(&renamed);
        let mut split = honest.clone();
        split[4].1.address = "utest1other".into();
        refused(&split);
    }

    /// M-8: forged, expired or excess vault openings are refused before any DKG work.
    #[tokio::test]
    async fn open_vault_refuses_before_reaching_the_signers() {
        let (mut c, _dir) = offline_with_stamps("open");
        c.cfg.open_vault.max_pending = 4;
        let owner = PrivateKeySigner::random();

        let mut forged = open_request(&owner, &c, now() + 600);
        forged.owner = Address::repeat_byte(0x99);
        let e = c.open_vault(stamped(forged, &c)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NotOwner));

        let e = c.open_vault(open_request(&owner, &c, now() - 1)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::Expired));

        // H-01: the slots are held by vaults opened in the last `pending_slot_secs`.
        for i in 1..=4u8 {
            let mut v = vault(i);
            v.opened_at = now();
            c.store.lock().await.save(v).unwrap();
        }
        let e = c.open_vault(open_request(&owner, &c, now() + 600)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::Busy));
        // Half an hour later, without deposits, they hold none: the request gets past the cap (and
        // fails at the unreachable light wallet server).
        let slot = c.cfg.open_vault.pending_slot_secs;
        for i in 1..=4u8 {
            c.store.lock().await.update(&B256::repeat_byte(i), |v| v.opened_at = now() - slot).unwrap();
        }
        let e = c.open_vault(open_request(&owner, &c, now() + 600)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::ChainUnavailable));
        assert!(c.store.lock().await.vaults.values().all(|v| v.status == VaultStatus::AwaitingDeposit));
    }

    /// H-01: the stamp is checked first, with the network's bits, over the request id and the owner.
    #[tokio::test]
    async fn open_vault_needs_a_stamp() {
        let (c, _dir) = offline_with_stamps("stamp");
        let owner = PrivateKeySigner::random();
        let good = open_request(&owner, &c, now() + 600);
        assert!(c.check_open_stamp(&good).is_ok());
        let bits = |r: &OpenVaultRequest| {
            zecured_base::stamp::leading_zero_bits(&zecured_base::stamp::digest(&r.request_id, &r.owner, r.stamp))
        };
        assert!(bits(&good) >= 8);
        let mut weak = good.clone();
        weak.stamp = (0..)
            .find(|n| {
                bits(&OpenVaultRequest {
                    stamp: *n,
                    ..good.clone()
                }) < 8
            })
            .unwrap();
        // Refused for its stamp before anything else is looked at (here: an expired, unsigned request).
        let mut unsigned = weak.clone();
        unsigned.signature = Bytes::new();
        unsigned.deadline = 0;
        for req in [weak, unsigned] {
            let e = c.open_vault(req).await.unwrap_err();
            assert_eq!(public_error(&e), Some(PublicError::BadStamp));
        }
        // A stamp is for one request id and one owner.
        let mut other_id = good.clone();
        other_id.request_id = B256::repeat_byte(4);
        let mut other_owner = good.clone();
        other_owner.owner = Address::repeat_byte(0x99);
        assert!(bits(&other_id) < 8 || bits(&other_owner) < 8);
        // Without stamps (0 bits), any nonce passes.
        let (mut free, _dir2) = offline("no-stamp");
        free.cfg.open_vault.stamp_bits = 0;
        assert!(
            free.check_open_stamp(&OpenVaultRequest {
                stamp: 0,
                ..good.clone()
            })
            .is_ok()
        );
        assert_eq!(PublicError::BadStamp.status(), 422);
    }

    /// H-01: one stamp per request id. While the request's vault opens, and once it has opened, the
    /// same request is refused; a failed opening releases it, so the owner can retry it as is.
    #[tokio::test]
    async fn a_stamp_opens_one_vault() {
        let (c, _dir) = offline_with_stamps("replay");
        let owner = PrivateKeySigner::random();
        let req = open_request(&owner, &c, now() + 600);
        // The light wallet server is unreachable: the opening fails after taking the request id,
        // and releases it.
        for _ in 0..2 {
            let e = c.open_vault(req.clone()).await.unwrap_err();
            assert_eq!(public_error(&e), Some(PublicError::ChainUnavailable));
        }
        // While an opening of the request runs, the request is refused.
        let running = c.claim_open_request(req.request_id, req.deadline).unwrap();
        let e = c.open_vault(req.clone()).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NonceUsed));
        // Its vault opened: still refused, until the request's deadline.
        running.keep();
        let e = c.open_vault(req.clone()).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NonceUsed));
        let remembered = c.open_requests.lock().unwrap()[&req.request_id];
        assert_eq!(remembered, req.deadline);
        // A request with a far deadline is remembered a day at most (its owner is bound by then).
        let far = B256::repeat_byte(9);
        c.claim_open_request(far, u64::MAX).unwrap().keep();
        assert!(c.open_requests.lock().unwrap()[&far] <= now() + OPEN_REQUEST_MEMORY_SECS);
    }

    /// H-11: the coordinator moves on to round 2 once the commitment is under Base's safe head or
    /// `commit_depth_secs` old (with the nodes' slack), and never before `commit_depth_blocks`.
    #[test]
    fn a_commitment_is_deep_enough_under_the_safe_head_or_after_its_seconds() {
        let (c, _dir) = offline("depth");
        let cfg = &c.cfg;
        assert_eq!((cfg.commit_depth_blocks, cfg.commit_depth_secs), (3, 120));
        let (block, at) = (1_000_000, 1_800_000_000);
        // Just mined: neither safe nor old.
        assert!(!ready_for_round_two(cfg, block, at, block + 1, at + 2, Some(block - 500)));
        // Under the safe head, but not yet `commit_depth_blocks` deep.
        assert!(!ready_for_round_two(cfg, block, at, block + 3, at + 6, Some(block)));
        // Under the safe head and deep enough: go.
        assert!(ready_for_round_two(cfg, block, at, block + 4, at + 8, Some(block)));
        // The safe head lags (or the provider has none): after 120 s and the slack, at 2 s or at
        // 200 ms blocks alike.
        for (blocks, provider_safe) in [(60, Some(block - 1)), (600, None)] {
            assert!(!ready_for_round_two(cfg, block, at, block + blocks, at + 121, provider_safe));
            assert!(ready_for_round_two(cfg, block, at, block + blocks, at + 122, provider_safe));
        }
    }

    /// M-8: an unsigned destination is refused without asking five nodes.
    #[tokio::test]
    async fn owner_requests_are_checked_before_forwarding() {
        let (c, _dir) = offline("owner");
        let owner = PrivateKeySigner::random();
        let mut v = vault(1);
        v.owner = owner.address();
        c.store.lock().await.save(v).unwrap();
        let id = B256::repeat_byte(1);
        // Any valid Ironwood UA will do.
        let ua = keys::encode_ironwood_ua(&NETWORK, test_address()).unwrap();
        let stranger = PrivateKeySigner::random();
        let salt = [5u8; 32];
        let r = Coordinator::recipient_from_ua(&ua, salt).unwrap();
        let msg = Destination {
            vaultId: id,
            commitment: B256::from(r.commitment()),
            nonce: 1,
            deadline: now() + 600,
        };
        let sig = sign_typed(&stranger, &c.domain(), &msg).unwrap();
        let e = c.add_destination(id, &ua, salt, 1, msg.deadline, sig).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NotOwner));

        let e = c.add_destination(id, "not an address", salt, 1, msg.deadline, Bytes::new()).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::InvalidReceiver));

        // Correctly signed: it does reach the (unreachable) nodes.
        let sig = sign_typed(&owner, &c.domain(), &msg).unwrap();
        let e = c.add_destination(id, &ua, salt, 1, msg.deadline, sig).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
        assert!(!format!("{e:#}").contains("127.0.0.1"), "no node URL in errors: {e:#}");
    }

    /// RT-5: a commitment dropped by a Base reorg is submitted again; one still there is left alone.
    #[test]
    fn a_commitment_missing_on_base_is_submitted_again() {
        let record = |hash: u8, vault: u8| IZecuredRegistry::SpendRecord {
            recordHash: B256::repeat_byte(hash),
            vaultId: B256::repeat_byte(vault),
            inputTag: B256::ZERO,
            changeTag: B256::ZERO,
            changeValue: 0,
            claimCount: 1,
            signerBitmap: 0x15,
        };
        let ours = B256::repeat_byte(7);
        assert!(!commitment_missing(&record(7, 1), ours).unwrap(), "still on Base");
        assert!(commitment_missing(&record(0, 0), ours).unwrap(), "gone: submit it again");
        assert!(commitment_missing(&record(8, 1), ours).is_err(), "another record: investigate");
    }

    fn test_address() -> orchard::Address {
        let sk = (0u8..=255)
            .map(|i| [i; 32])
            .find_map(|b| Option::<orchard::keys::SpendingKey>::from(orchard::keys::SpendingKey::from_bytes(b)))
            .unwrap();
        FullViewingKey::from(&sk).address_at(0u32, orchard::keys::Scope::External)
    }

    /// A Base RPC stub whose Registry reports every claim of `vault` with `status`.
    async fn registry_stub(vault: B256, status: u8) -> String {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| async move {
                let result = match req["method"].as_str() {
                    Some("eth_chainId") => serde_json::json!("0x14a34"),
                    Some("eth_call") => {
                        // claim(bytes32) returns (vaultId, kind, amount, commitment, status).
                        let mut ret = vault.to_vec();
                        ret.extend([0u8; 32]);
                        ret.extend(alloy::primitives::U256::from(500_000u64).to_be_bytes::<32>());
                        ret.extend([7u8; 32]);
                        ret.extend(alloy::primitives::U256::from(status).to_be_bytes::<32>());
                        serde_json::json!(format!("0x{}", hex::encode(ret)))
                    }
                    m => panic!("unexpected {m:?}"),
                };
                Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
            }),
        );
        crate::nodes::tests::serve(app).await
    }

    fn payout_in_flight(claims: &[u8]) -> Inflight {
        Inflight {
            claim_ids: claims.iter().map(|c| B256::repeat_byte(*c)).collect(),
            txid: "00".repeat(32),
            expiry_height: 500,
            raw_tx: None,
            inputs: vec![],
            broadcast_from: 0,
            spend_commit: None,
        }
    }

    /// H-5: a payout whose `confirmPayout` already went through (its receipt was lost) is
    /// recognized on Base and settled, instead of being re-confirmed forever (the nodes here
    /// are unreachable: asking them would fail).
    #[tokio::test]
    async fn a_payout_already_confirmed_on_base_is_settled() {
        let id = B256::repeat_byte(1);
        let (mut c, _dir) = offline("settled");
        c.cfg.base_rpc_url = registry_stub(id, inflight::CLAIM_PAID).await;
        let inflight = payout_in_flight(&[1, 2]);
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        v.claims = vec![claim(1, 400_000), claim(2, 600_000)];
        v.inflight = Some(inflight.clone());
        c.store.lock().await.save(v).unwrap();

        c.settle(&id, &inflight).await.unwrap();
        let v = c.vault(&id).await.unwrap();
        assert!(v.inflight.is_none(), "the vault is not wedged");
        assert!(v.claims.iter().all(|x| x.state == ClaimState::Paid));
        assert_eq!(v.status, VaultStatus::Closed);
    }

    /// H-5: when the claims are not paid yet and confirming fails, the payout stays in flight
    /// to be confirmed again on the next pass.
    #[tokio::test]
    async fn a_failed_confirmation_is_retried_later() {
        let id = B256::repeat_byte(1);
        let (mut c, _dir) = offline("retry");
        c.cfg.base_rpc_url = registry_stub(id, 1).await; // Pending
        let inflight = payout_in_flight(&[1]);
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        v.outstanding = 600_000;
        v.claims = vec![claim(1, 400_000)];
        v.inflight = Some(inflight.clone());
        c.store.lock().await.save(v).unwrap();

        let e = c.settle(&id, &inflight).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
        let v = c.vault(&id).await.unwrap();
        assert!(v.inflight.is_some());
        assert_eq!(v.claims[0].state, ClaimState::Unpaid);
    }

    /// L-12: the withhold flag is inert unless the build has the `test-hooks` feature.
    #[tokio::test]
    async fn the_withhold_flag_needs_the_test_hooks_feature() {
        let (c, dir) = offline("hooks");
        let flag = dir.0.join("test-withhold-next-broadcast");
        std::fs::write(&flag, b"").unwrap();
        let taken = c.take_withhold_flag();
        if cfg!(feature = "test-hooks") {
            assert!(taken && !flag.exists());
        } else {
            assert!(!taken && flag.exists(), "a default build ignores the flag");
        }
    }
}
