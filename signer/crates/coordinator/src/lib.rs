//! Zecured coordinator: drives the signer network.
//!
//! It holds **no** key material for vaults: it relays DKG messages (sealed node-to-node),
//! scans vaults with their viewing keys, proves deposits (as collateral, or as refunds of what
//! cannot back a vault, T-02), builds and proves PCZTs, runs the FROST rounds with whichever
//! signers accept, aggregates, broadcasts, and relays deposit proofs, payout commitments and
//! confirmations to Base with its own gas key. It also keeps the Intake's pending activations and
//! refunds moving (a keyless keeper: anyone may finalize, drop or convert them), and proves the
//! delivery of every payout, which makes its claims final on Base (section 3; a disputed one first,
//! T-05 (a)). A deposit no guest of the deployment can land on Base is refunded off-chain at its
//! owner's request (P5 under I30). Every decision that moves value is taken independently by at
//! least three signer nodes.
//!
//! Errors returned to API callers carry a [`error::PublicError`] (fixed message and code);
//! details are logged at debug level only, scrubbed of identifiers and numbers.

mod delivery;
pub mod error;
mod frost;
pub mod inflight;
pub mod invites;
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
        atomic::{AtomicBool, AtomicU32, Ordering},
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
    Destination, OpenVault, ProveRefund, RefundRequest, ViewKeyRequest, check_signed_by_all,
    events::{
        self, ClaimChange, Contracts, ISignerBond, IZecuredIntake, IZecuredRegistry, IZecuredSlasher, RegistryEvent,
        VaultEvent,
    },
    operator::{bond_domain, destination_received},
    payout_confirmation, recover_typed, registry_domain, vault_opened,
    verify::{STATUS_NONE, VAULT_PENDING},
};
use zecured_node::{
    ConfirmIn, ConfirmResponse, DestinationIn, Planning, ReceiptIn, ReceiptOut, RecipientIn, RefundIn,
    chain::{BaseView, VaultNotes, ZcashView, note_cmx, refund_tags},
    config::NetworkConfig,
    dkg::{Round1Out, Round2In, Round2Out, Round3In, Round3Out, Sealed, VaultOpen},
    largest_tier, now,
    signing::{CommitRequest, CommitResponse, SignKind, SpendCommitRecord, main_lineage},
};
use zecured_policy::{
    classify::{self, Classification, DepositRules, Landing, NotePosition},
    derive::{deposit_tag, payout_ref},
    ledger::{self, ClaimId, ClaimKind, ClaimState, NextPayout, PayoutInput, PayoutPlan, Recipient},
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
    inflight::{Action, Inputs},
    invites::{InviteGate, OpeningsStatus},
    nodes::{Answer, Nodes, is_refusal, shortfall, split},
    store::{Armed, CommittedPayout, CoordVault, Inflight, KeptCommitment, PendingRefund, Refund, Store, VaultStatus},
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
/// O-2: seconds the coordinator waits for the signers to apply a vault's newest Base event before
/// it asks them to sign anyway (and retries on the next pass if they refuse, as before).
pub const NODE_CATCH_UP_SECS: u64 = 60;
/// O-2: how long the nodes' reported Base blocks are reused before they are asked again.
const NODE_STATUS_TTL: Duration = Duration::from_secs(5);
/// R-5: seconds a node's `receivedAt` may lie ahead of the coordinator's clock.
const RECEIPT_CLOCK_SKEW_SECS: u64 = 60;
/// R-5: receipts kept in memory at most.
const MAX_CACHED_RECEIPTS: usize = 4_096;

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
    /// Mainnet beta (section 6): the invite code, when openings are invite-only. Never logged.
    #[serde(default)]
    pub invite: Option<String>,
}

/// R-5: a claim's liveness receipt as the backend serves it: one signer's `DestinationReceived`
/// for the claim's current destination, which `SignerBondV2.armLiveness` takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimReceipt {
    pub claim_id: B256,
    pub commitment: B256,
    pub received_at: u64,
    pub signer: Address,
    pub signature: Bytes,
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
    /// Section 3: its payout's delivery is proven on Base (final, never disputable again).
    pub proven: bool,
    /// Confirmed paid by the signers, not proven yet: its claimant may dispute the confirmation
    /// until this time (Unix seconds; `None` otherwise, or while the dispute window is unknown).
    pub disputable_until: Option<u64>,
    /// R-5: its liveness clock is armed on the bond for its current destination.
    pub liveness_armed: bool,
}

/// The word `GET /vaults/:id` gives a claim's state (its `status`, as app/lib/api.ts reads it).
pub fn claim_status(state: ClaimState) -> &'static str {
    match state {
        ClaimState::Unpaid => "pending",
        ClaimState::Paid => "paid",
        ClaimState::WrittenOff => "written_off",
        ClaimState::Compensated => "compensated",
        ClaimState::Disputed => "disputed",
    }
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
    /// I30: what the deployment's guests can land on Base, read once from the Intake.
    landing: OnceCell<Landing>,
    /// T-05 (a): when the coordinator last warned about each vault's disputed confirmations.
    dispute_warned: std::sync::Mutex<HashMap<B256, Instant>>,
    /// O-2: each node's last applied Base block, as its status reported it, and when it was read.
    node_blocks: std::sync::Mutex<Option<(Instant, Vec<(u16, u64)>)>>,
    /// Mainnet beta (section 6): the invite gate of `POST /vaults`; `None`: openings are open.
    invites: Option<InviteGate>,
    /// R-5: receipts served, by claim (each for the claim's commitment at the time).
    receipts: std::sync::Mutex<HashMap<B256, ClaimReceipt>>,
    /// The Registry's `disputeWindow` (seconds), once read: what the vault view's
    /// `disputable_until` adds to a confirmation's time.
    dispute_window: OnceLock<u64>,
    /// R-10: whether the deployment's `commitGrace` was compared with the payout lifetime.
    grace_checked: AtomicBool,
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
        refunds: vec![],
        paid_here: vec![],
        reopened: vec![],
        attest_delay: None,
        used_view_nonces: vec![],
        non_conforming: vec![],
        opened_signatures: signatures.clone(),
        activated_tag: None,
        pending_tag: None,
        opened_at: now(),
        commitments: vec![],
        pending_refunds: vec![],
        armed: vec![],
        last_event_block: 0,
        last_event_at: 0,
        refund_requested: false,
        used_request_nonces: vec![],
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

/// Applies one event of the deployment to the coordinator's record, as the contracts' state
/// machines move the vault. Idempotent: a chunk applied again after a crash (vault files are saved
/// before the cursor) changes nothing.
fn apply_event(v: &mut CoordVault, ev: &VaultEvent) {
    // O-2: the signers act on the vault once their own view reaches this block.
    if ev.block > v.last_event_block {
        v.last_event_block = ev.block;
        v.last_event_at = now();
    }
    match &ev.event {
        RegistryEvent::SpendCommitted {
            payout_ref,
            input_tag,
            claims,
        } => {
            if !v.commitments.iter().any(|c| c.payout_ref == *payout_ref) {
                v.commitments.push(CommittedPayout {
                    payout_ref: *payout_ref,
                    input_tag: *input_tag,
                    claims: claims.iter().map(|c| c.0).collect(),
                });
            }
        }
        RegistryEvent::RefundPending {
            deposit_tag,
            ready_height,
            ready_time,
            ..
        } => {
            if !v.pending_refunds.iter().any(|p| p.deposit_tag == deposit_tag.0) {
                v.pending_refunds.push(PendingRefund {
                    deposit_tag: deposit_tag.0,
                    ready_height: *ready_height,
                    ready_time: *ready_time,
                });
            }
        }
        RegistryEvent::PendingRefundDropped { deposit_tag } => {
            v.pending_refunds.retain(|p| p.deposit_tag != deposit_tag.0);
        }
        RegistryEvent::LivenessArmed {
            claim_id,
            commitment,
            payable_at,
        } => {
            v.armed.retain(|a| a.claim_id != claim_id.0);
            v.armed.push(Armed {
                claim_id: claim_id.0,
                commitment: commitment.0,
                payable_at: *payable_at,
            });
        }
        RegistryEvent::PreLiquidated { .. } => {
            // Section 4: the Registry's `LiquidationRelease` of the same transaction creates the
            // claim; this only says it was a pre-liquidation. Nothing but the vault's id prefix.
            info!(vault = %short(&v.vault_id), "a pre-liquidation of the vault: its claim is paid as a liquidation");
        }
        RegistryEvent::ChangeTagOverridden { .. } => {
            error!(
                vault = %short(&v.vault_id),
                "a deposit of another vault took a change tag of this vault's commitment (R-16): that commitment's signers registered a false change"
            );
        }
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
                    // R-12: a pending refund opens its claim when it is finalized.
                    v.pending_refunds.retain(|p| Some(p.deposit_tag) != c.refund_tag);
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
                    // T-05 (a): Base's state; a claim this coordinator saw paid stays paid in its
                    // books ([`CoordVault::books`]), and it proves that payout's inclusion.
                    ClaimChange::Disputed { .. } => claim.state = ClaimState::Disputed,
                    // Section 3: final; R-3: so is a confirmation nobody disputed in time (no proof
                    // of its payout is needed any more).
                    ClaimChange::Upheld { .. } | ClaimChange::Proven { .. } | ClaimChange::Expired => {
                        claim.state = ClaimState::Paid;
                        claim.proven = true;
                    }
                    // R-11: out of the coverage cap until its destination is given.
                    ClaimChange::Dormant => {}
                    ClaimChange::Reopened => {
                        if claim.state == ClaimState::Disputed {
                            claim.state = ClaimState::Unpaid;
                        }
                        if !v.reopened.contains(&claim.id) {
                            v.reopened.push(claim.id);
                        }
                        if v.paid_here.contains(&claim.id) {
                            warn!(
                                vault = %short(&v.vault_id),
                                "a confirmation of a payout this coordinator saw mined was reopened: it still proves the payout's inclusion (D57); meanwhile its claimant may take the claim's value from the bonds, and the signers pay nothing twice (T-05)"
                            );
                        }
                    }
                }
            }
        }
        RegistryEvent::PayoutConfirmed { claims, .. } => {
            for c in v.claims.iter_mut().filter(|c| claims.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
                if !c.proven {
                    c.confirmed_at = ev.timestamp;
                }
            }
        }
    }
    if v.status == VaultStatus::Active && v.outstanding == 0 && !v.claims.is_empty() && v.settled() {
        v.status = VaultStatus::Closed;
    }
}

/// R-5: whether `r`, a node's answer for `claim_id` whose destination is `commitment`, is that
/// node's valid receipt: its own index, the claim's current commitment, a time not ahead of
/// `now` (beyond the clock skew), signed by the node's attestation key under the bond's domain.
fn valid_receipt(
    r: &ReceiptOut,
    claim_id: B256,
    commitment: B256,
    domain: &alloy::sol_types::Eip712Domain,
    signer: Address,
    now: u64,
) -> bool {
    r.claim_id == claim_id
        && r.commitment == commitment
        && commitment != B256::ZERO
        && r.received_at <= now.saturating_add(RECEIPT_CLOCK_SKEW_SECS)
        && select::signed_by(domain, &destination_received(claim_id, commitment, r.received_at), &r.signature, signer)
}

/// O-2: whether at least `need` of the nodes' reported Base blocks reach `block`.
pub fn caught_up(blocks: &[(u16, u64)], block: u64, need: usize) -> bool {
    blocks.iter().filter(|(_, b)| *b >= block).count() >= need
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
            landing: OnceCell::new(),
            dispute_warned: std::sync::Mutex::default(),
            node_blocks: std::sync::Mutex::default(),
            invites: None,
            receipts: std::sync::Mutex::default(),
            dispute_window: OnceLock::new(),
            grace_checked: AtomicBool::new(false),
            #[cfg(feature = "test-hooks")]
            withhold_flag: data_dir.join(TEST_WITHHOLD_FLAG),
            cfg,
        })
    }

    /// Mainnet beta (section 6): vault openings gated by the operator's invite file (`invites.rs`),
    /// uses counted in `data_dir`.
    pub fn with_invites(mut self, file: &Path, data_dir: &Path) -> Result<Self> {
        self.invites = Some(InviteGate::open(file, data_dir)?);
        Ok(self)
    }

    /// What `GET /config` says about openings: open and not invite-only without a gate; closed when
    /// the gate's file is unreadable (an opening would be refused too).
    pub fn openings(&self) -> OpeningsStatus {
        match &self.invites {
            None => OpeningsStatus {
                open: true,
                invite_only: false,
            },
            Some(gate) => gate.status().unwrap_or(OpeningsStatus {
                open: false,
                invite_only: true,
            }),
        }
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
        // Section 6: the operator's switch closes openings before anything else is looked at.
        ensure!(self.openings().open, PublicError::OpeningsClosed);
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
        // Section 6: an invite-only opening takes a use of its code (given back if it fails), only
        // for a stamped, signed request, so codes cannot be probed for free.
        let invite = self.invites.as_ref().map(|g| g.take(req.invite.as_deref())).transpose()?;
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
        if let Some(invite) = invite
            && let Err(e) = invite.keep()
        {
            error!("an invite's use could not be recorded");
            debug!(error = %scrubbed(&e), "invite use");
        }
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

    /// P5 under I30: the owner asks for a deposit back that no guest of the deployment can land on
    /// Base. The nodes check that again on their own view; any other deposit is refunded on Base,
    /// as a Refund claim, once a proof lands it.
    #[allow(clippy::too_many_arguments)]
    pub async fn request_refund(
        &self,
        vault_id: B256,
        note_id: B256,
        ua: &str,
        salt: [u8; 32],
        nonce: u64,
        deadline: u64,
        signature: Bytes,
    ) -> Result<()> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let v = self.vault(&vault_id).await?;
        let msg = RefundRequest {
            vaultId: vault_id,
            noteId: note_id,
            commitment: B256::from(r.commitment()),
            nonce,
            deadline,
        };
        // Before any node scans anything for it (M-8).
        self.check_owner(&v, &msg, &signature, deadline)?;
        match v.non_conforming.iter().find(|n| n.note_id == note_id) {
            Some(n) if n.offchain => {}
            Some(_) => return Err(PublicError::RefundOnBase.into()),
            None => {
                return Err(anyhow!("not a deposit of this vault that has not landed").context(PublicError::BadRequest));
            }
        }
        let body = RefundIn {
            note_id,
            recipient: Self::recipient_in(&r),
            nonce,
            deadline,
            signature,
        };
        let path = format!("/v1/vaults/{vault_id}/refund");
        let answers = self.nodes.post_many(&self.nodes.indices(), &path, &body).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "refund")?;
        self.update_vault(&vault_id, |v| {
            v.refunds.retain(|x| x.note_id != note_id);
            v.refunds.push(Refund {
                note_id,
                recipient: r,
                done: false,
            });
        })
        .await
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

    /// R-11.1: the owner asks for the vault's deposit back because its activation cannot land (the
    /// coverage cap has no room, and the Intake refuses the activation of a new vault rather than
    /// cancel it). The coordinator then proves the deposit in Refund mode on its next passes; the
    /// Intake lands it as a Refund claim of the vault's account, whose destination the account then
    /// gives. Refused, with a fixed code, unless this coordinator makes proofs, the owner signed it
    /// (`ProveRefund`, nonce unused), the deployment's guest has a Refund mode, the vault still waits
    /// for its deposit and Base does not know it, a confirmed deposit of its amount was seen, and the
    /// cap blocks the activation now.
    pub async fn request_refund_proof(
        &self,
        vault_id: B256,
        nonce: u64,
        deadline: u64,
        signature: &[u8],
    ) -> Result<()> {
        ensure!(self.proves_deposits(), PublicError::NotConfigured);
        let v = self.vault(&vault_id).await?;
        let msg = ProveRefund {
            vaultId: vault_id,
            nonce,
            deadline,
        };
        self.check_owner(&v, &msg, signature, deadline)?;
        ensure!(!v.used_request_nonces.contains(&nonce), PublicError::NonceUsed);
        ensure!(v.status == VaultStatus::AwaitingDeposit, PublicError::AlreadyLanded);
        ensure!(v.deposit_seen(), PublicError::NoDeposit);
        let landing = self.landing().await.context(PublicError::ChainUnavailable)?;
        ensure!(landing.proves_refunds(), PublicError::RefundUnavailable);
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let status = IZecuredRegistry::new(self.cfg.registry, &provider)
            .vault(vault_id)
            .call()
            .await
            .context(PublicError::ChainUnavailable)?
            .status;
        ensure!(status == STATUS_NONE, PublicError::AlreadyLanded);
        let intake = IZecuredIntake::new(self.v3().await.context(PublicError::ChainUnavailable)?.intake, &provider);
        let block = intake.mintBlock(v.amount).call().await.context(PublicError::ChainUnavailable)?;
        ensure!(block == events::MINT_BLOCK_CAP_EXCEEDED, PublicError::NotBlocked);
        self.update_vault(&vault_id, |cur| {
            cur.used_request_nonces.push(nonce);
            cur.refund_requested = true;
        })
        .await?;
        info!(vault = %short(&vault_id), "the owner asked for the deposit back: it is proven as a refund (R-11.1)");
        Ok(())
    }

    /// R-5: a liveness receipt for the claim, for its current destination in this coordinator's
    /// books: every node is asked at once, each answer checked against that node's attestation key
    /// under the bond's domain, and of the valid ones the earliest `receivedAt` is served (one
    /// honest signer is enough, and a signer that dates its receipt late cannot delay the claim's
    /// clock). A receipt served is kept for that destination, so asking again costs the nodes
    /// nothing; a changed destination is asked for anew.
    pub async fn claim_receipt(&self, claim_id: B256) -> Result<ClaimReceipt> {
        let commitment = {
            let store = self.store.lock().await;
            store
                .vaults
                .values()
                .flat_map(|v| v.claims.iter())
                .find(|c| c.id == claim_id.0)
                .map(|c| B256::from(c.commitment))
        }
        .ok_or(PublicError::NoReceipt)?;
        ensure!(commitment != B256::ZERO, PublicError::NoReceipt);
        {
            let cached = self.receipts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(r) = cached.get(&claim_id).filter(|r| r.commitment == commitment) {
                return Ok(r.clone());
            }
        }
        let bond = self.v3().await.context(PublicError::ChainUnavailable)?.bond;
        ensure!(bond != Address::ZERO, PublicError::NotConfigured);
        let domain = bond_domain(self.cfg.chain_id, bond);
        let answers: Vec<Answer<ReceiptOut>> =
            self.nodes.post_many(&self.nodes.indices(), "/v1/receipts", &ReceiptIn { claim_id }).await;
        let now = now();
        let receipt = answers
            .into_iter()
            .filter_map(|(index, answer)| {
                let r = answer.ok()?;
                let signer = self.cfg.node(index).ok()?.eth;
                valid_receipt(&r, claim_id, commitment, &domain, signer, now).then_some((index, r, signer))
            })
            .min_by_key(|(index, r, _)| (r.received_at, *index))
            .map(|(_, r, signer)| ClaimReceipt {
                claim_id,
                commitment,
                received_at: r.received_at,
                signer,
                signature: r.signature,
            })
            .ok_or(PublicError::NoReceipt)?;
        let mut cached = self.receipts.lock().unwrap_or_else(|p| p.into_inner());
        if cached.len() >= MAX_CACHED_RECEIPTS {
            cached.clear();
        }
        cached.insert(claim_id, receipt.clone());
        Ok(receipt)
    }

    /// The app's view of a vault, from the store alone: no Zcash lock, no scan (L-10). The
    /// deposits waiting for a refund proof are the ones persisted by the last scan (M-10).
    pub async fn view(&self, vault_id: B256) -> Result<VaultView> {
        let store = self.store.lock().await;
        let v = store.vaults.get(&vault_id).context(PublicError::UnknownVault)?;
        let window = self.dispute_window.get().copied();
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
                    proven: c.proven,
                    disputable_until: (c.state == ClaimState::Paid && !c.proven && c.confirmed_at != 0)
                        .then_some(())
                        .and(window)
                        .map(|w| c.confirmed_at.saturating_add(w)),
                    liveness_armed: v.armed(c),
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
        self.check_commit_grace().await;
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

    /// Applies the deployment's events up to Base's safe head: the Registry's, the Intake's pending
    /// refunds (R-12), the bond's liveness arming (R-5) and the Adapter's pre-liquidations. No lock
    /// is held while the RPC answers.
    async fn sync_base(&self) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let safe = events::safe_head(&provider).await?;
        let v3 = self.v3().await?;
        let contracts = Contracts {
            registry: self.cfg.registry,
            intake: v3.intake,
            bond: v3.bond,
            adapter: self.cfg.adapter,
        };
        let cursor = self.store.lock().await.base_cursor;
        let start = cursor.max(self.cfg.registry_from_block.saturating_sub(1)) + 1;
        // H-11: as many blocks per step as `eth_getLogs` is allowed (`ZECURED_LOG_RANGE`).
        let step = events::log_range_ceiling();
        let mut from = start;
        while from <= safe {
            let to = (from + step - 1).min(safe);
            let evs = events::fetch_deployment(&provider, &contracts, from, to).await?;
            let mut store = self.store.lock().await;
            for ev in evs {
                // The bond's arming names a claim, not a vault.
                let vault = match &ev.event {
                    RegistryEvent::LivenessArmed { claim_id, .. } => {
                        store.vaults.values().find(|v| v.claims.iter().any(|c| c.id == claim_id.0)).map(|v| v.vault_id)
                    }
                    _ => store.vaults.contains_key(&ev.vault_id).then_some(ev.vault_id),
                };
                if let Some(vault) = vault {
                    store.update(&vault, |v| apply_event(v, &ev))?;
                }
            }
            store.set_base_cursor(to)?;
            from = to + 1;
        }
        Ok(())
    }

    /// R-10: warns once if the deployment's `commitGrace` is shorter than a payout's lifetime: a
    /// payout committed just before a claim's liveness deadline could then land after its claimant
    /// took the claim's value from the bonds (the planner's rule 2 then keeps such claims back).
    async fn check_commit_grace(&self) {
        if self.grace_checked.load(Ordering::Relaxed) {
            return;
        }
        let read = async {
            let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
            let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
            let window = registry.disputeWindow().call().await.context("Registry.disputeWindow")?;
            let _ = self.dispute_window.set(window);
            let bond = self.v3().await?.bond;
            if bond == Address::ZERO {
                return anyhow::Ok(None);
            }
            Ok(Some(ISignerBond::new(bond, &provider).commitGrace().call().await.context("bond.commitGrace")?))
        };
        match read.await {
            Ok(grace) => {
                self.grace_checked.store(true, Ordering::Relaxed);
                let lifetime = self.cfg.payout_lifetime();
                if let Some(grace) = grace.filter(|g| *g < lifetime) {
                    warn!(
                        commit_grace = grace,
                        payout_lifetime = lifetime,
                        "the bond's commitGrace is shorter than a payout's lifetime: claims near their liveness deadline are not committed (R-10)"
                    );
                }
            }
            Err(e) => debug!(error = %scrubbed(&e), "commitGrace unknown"),
        }
    }

    async fn notes(&self, v: &CoordVault, decisions: &mut Vec<(NotePosition, Classification)>) -> Result<VaultNotes> {
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations: self.cfg.confirmations,
        };
        let fvk = fvk_of(v)?;
        let refunds = refund_tags(&v.claims);
        // I30: unknown (Base unreachable), nothing is proven as a refund nor refunded off-chain.
        let landing = self.landing().await.ok();
        let base = BaseView {
            ufvk: &v.ufvk,
            address: &v.address,
            activated_tag: v.activated_tag,
            refund_tags: &refunds,
            landing,
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
                offchain: notes.refundable.iter().any(|(r, _)| r == note_id),
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
        // Section 3 and T-05 (a): every landed payout's delivery is proven (a disputed one first),
        // whatever else the vault is doing.
        if let Err(e) = self.prove_deliveries(&v).await {
            log_failure(id, "proving a payout's delivery", &e);
        }
        // R-12: refund proofs pending on the Intake are finalized (or dropped) as they come due.
        if let Err(e) = self.keep_pending_refunds(&v).await {
            log_failure(id, "keeping a pending refund", &e);
        }
        if idle {
            self.mark_idle_scanned(id);
            return self.prove_refunds(&v, &notes).await;
        }

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
                        // R-11.1: its owner asked for it back (the cap blocks the activation).
                        let target = if v.refund_requested {
                            proofs::Target::refund(conforming)
                        } else {
                            proofs::Target::activation(conforming, v.amount)
                        };
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
                if self.pay_next(&v, &mut notes).await? {
                    return Ok(());
                }
            }
        }
        let v = self.vault(id).await?;
        if v.inflight.is_none() && !self.refund_next(&v, &notes).await? {
            self.prove_refunds(&v, &notes).await?;
        }
        Ok(())
    }

    /// P5: refunds the next deposit its owner asked for back that no guest of the deployment can
    /// land (I30), net of the fee. Returns whether a refund was started.
    async fn refund_next(&self, v: &CoordVault, notes: &VaultNotes) -> Result<bool> {
        for r in v.refunds.iter().filter(|r| !r.done) {
            let Some((_, note)) = notes.refundable.iter().find(|(id, _)| *id == r.note_id.0) else {
                continue;
            };
            let plan = ledger::refund_plan(r.note_id.0, note.value(), r.recipient.receiver)?;
            let kind = SignKind::Refund { note_id: r.note_id };
            self.execute(&v.vault_id, kind, &plan, std::slice::from_ref(note), notes.tip).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// I30: what the deployment's guests can land on Base, from the Intake's journal version, refund
    /// floor and largest work tier (R-15; immutable, read once). The nodes read it on each of their
    /// providers.
    pub(crate) async fn landing(&self) -> Result<Landing> {
        let landing = self
            .landing
            .get_or_try_init(|| async {
                let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
                let intake = IZecuredIntake::new(self.v3().await?.intake, &provider);
                let largest_tier = largest_tier(&intake.workTiers().call().await.context("Intake.workTiers")?)?;
                anyhow::Ok(if intake.journalV2().call().await.context("Intake.journalV2")? {
                    Landing::WithRefunds {
                        min_refund: intake.minRefundZat().call().await.context("Intake.minRefundZat")?,
                        largest_tier,
                    }
                } else {
                    Landing::ActivateOnly { largest_tier }
                })
            })
            .await?;
        Ok(*landing)
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
        let late = ledger::overdue(&v.books(), now(), period / 2).len();
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
    /// the note of a refunded deposit, planned exactly as the signers plan it (O-4, `planning`).
    async fn pay_next(&self, v: &CoordVault, notes: &mut VaultNotes) -> Result<bool> {
        let books = v.books();
        // Rule 3 (O-4) may count a confirmed claim unpaid again: the planning decides.
        if !books
            .iter()
            .any(|c| c.payable() || (c.state == ClaimState::Paid && !c.proven && c.kind.main_lineage()))
        {
            return Ok(false);
        }
        // K3 and T-04: nothing is paid from a pending or compromised vault (the Registry refuses
        // both); checking first saves proving a transaction.
        if !self.payable_on_base(&v.vault_id).await? {
            return Ok(false);
        }
        let planning = self.planning(v, notes, now()).await?;
        let claims = planning.claims;
        let bound = planning.bound;
        if !claims.iter().any(|c| c.payable()) {
            return Ok(false);
        }
        // O-2: the signers plan from their own view of Base; give them the time to apply the
        // vault's newest event before asking them to sign, then try anyway.
        if !self.nodes_caught_up(v).await {
            debug!(vault = %short(&v.vault_id), "waiting for the signers to apply the vault's newest Base event (O-2)");
            return Ok(false);
        }
        // The nodes never pay a vault's own addresses (security review H-2): plan the same way.
        let fvk = fvk_of(v)?;
        let (main, i10) = main_lineage(v.activated_tag.is_some(), v.outstanding, &claims, &notes.lineage, &bound);
        if let Err(e) = &i10
            && v.activated_tag.is_some()
            && claims.iter().any(|c| c.kind.main_lineage() && c.payable())
        {
            warn!(vault = %short(&v.vault_id), "the vault's main lineage does not add up (I10): nothing is paid from it");
            debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!("{e}")), "I10");
        }
        let next = {
            let store = self.store.lock().await;
            let recipient =
                |c: &_| store.recipient(c).filter(|r| !zecured_node::chain::owns_receiver(&fvk, &r.receiver));
            ledger::next_vault_payout(&claims, recipient, main, &notes.refund_values(), now())?
        };
        let NextPayout::Pay(plan) = next else { return Ok(false) };
        let inputs: Vec<ReceivedNote> = match plan.input {
            PayoutInput::Main => notes.lineage.clone(),
            PayoutInput::Refund(tag) => {
                notes.refunds.iter().filter(|(t, _)| *t == tag).map(|(_, n)| n.clone()).collect()
            }
            PayoutInput::Unlanded(_) => bail!("a payout of claims never spends a deposit that has not landed"),
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

    /// The vault's claims as the signers plan from them (`zecured_node::Node::planning`, the same
    /// rules on this coordinator's books): O-4 rule 3 counts unpaid again the confirmed claims whose
    /// input is still an unspent lineage note, the lineage keeps the planned change of that book
    /// value (H-2), the claims a commitment binds to the lineage note are the payout (RT-3), and
    /// rules 1 and 2 (R-10) set aside claims within one payout lifetime of their liveness due time.
    async fn planning(&self, v: &CoordVault, notes: &mut VaultNotes, now: u64) -> Result<Planning> {
        let tag_of = |n: &ReceivedNote| B256::from(deposit_tag(&v.ufvk, &note_cmx(n)));
        let candidates: Vec<B256> = notes.lineage.iter().map(tag_of).collect();
        let mut claims = v.books();
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let read_inputs = |ids: Vec<ClaimId>| {
            let registry = &registry;
            async move {
                let mut inputs = Vec::with_capacity(ids.len());
                for id in ids {
                    inputs.push((id, registry.claimInput(B256::from(id)).call().await.context("Registry.claimInput")?));
                }
                anyhow::Ok(inputs)
            }
        };
        let mut inputs = match candidates.is_empty() {
            true => vec![],
            false => read_inputs(zecured_node::unpaid_ids(&claims)).await?,
        };
        // Rule 3 only when the books do not add up as they are, as the nodes plan.
        let mut reverted = vec![];
        if !candidates.is_empty() && !notes.books_add_up(v.outstanding, &claims) {
            let confirmed = read_inputs(zecured_node::confirmed_ids(&claims)).await?;
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
                vault = %short(&v.vault_id),
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
            _ => vec![],
        };
        let bond = self.v3().await?.bond;
        let mut dues: Vec<(ClaimId, u64)> = vec![];
        if bond != Address::ZERO {
            let bond = ISignerBond::new(bond, &provider);
            for c in claims.iter().filter(|c| c.payable() && c.kind.liveness_covered()) {
                // Unreadable (a stale price, R-9): 0, as every node counts it.
                dues.push((c.id, bond.livenessDue(B256::from(c.id)).call().await.map(|d| d.dueAt).unwrap_or(0)));
            }
        }
        let due_of = |id: &ClaimId| dues.iter().find(|(c, _)| c == id).map_or(0, |(_, d)| *d);
        let deferred = ledger::defer_near_due(&mut claims, now, self.cfg.payout_lifetime(), due_of);
        if !deferred.is_empty() {
            info!(
                vault = %short(&v.vault_id),
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

    /// O-2: whether enough signers (a signing threshold) have applied the vault's newest Base event,
    /// by the last Base block each node's status reports; past [`NODE_CATCH_UP_SECS`] after the
    /// coordinator applied it, the signers are asked anyway (they refuse until they have it, and
    /// the next pass tries again).
    async fn nodes_caught_up(&self, v: &CoordVault) -> bool {
        if v.last_event_block == 0 || now() >= v.last_event_at.saturating_add(NODE_CATCH_UP_SECS) {
            return true;
        }
        let fresh = {
            let cached = self.node_blocks.lock().unwrap_or_else(|p| p.into_inner());
            cached.as_ref().filter(|(at, _)| at.elapsed() < NODE_STATUS_TTL).map(|(_, b)| b.clone())
        };
        let blocks = match fresh {
            Some(b) => b,
            None => {
                let mut blocks = vec![];
                for health in self.node_health().await {
                    let index = health.get("index").and_then(serde_json::Value::as_u64);
                    let block = health.get("base_block").and_then(serde_json::Value::as_u64);
                    if let (Some(index), Some(block)) = (index, block) {
                        blocks.push((u16::try_from(index).unwrap_or(0), block));
                    }
                }
                *self.node_blocks.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), blocks.clone()));
                blocks
            }
        };
        caught_up(&blocks, v.last_event_block, usize::from(MIN_SIGNERS))
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
        let (claim_ids, refund_note) = match &kind {
            SignKind::Payout { claim_ids } => (claim_ids.clone(), None),
            SignKind::Refund { note_id } => (vec![], Some(*note_id)),
        };
        // L23: a committed payout waits a random number of blocks before its broadcast (an
        // off-chain refund is not committed on Base, so it goes at once).
        let delay = match &kind {
            SignKind::Payout { .. } => self.payout_delay(),
            SignKind::Refund { .. } => 0,
        };
        // RT-13: counted from the tip now that the payout is signed, not from when it was planned.
        let signed_tip = if delay == 0 {
            tip
        } else {
            self.fetch_zcash_tip().await.unwrap_or(tip).max(tip)
        };
        let record = Inflight {
            claim_ids,
            refund_note,
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

    /// The mined payout's pending claims are confirmed on Base (R-1: never a reopened one, and not
    /// on a frozen vault, which refuses confirmations) and its record closed; its delivery proof
    /// then makes every claim final (section 3).
    async fn settle(&self, id: &B256, inflight: &Inflight) -> Result<()> {
        if !inflight.claim_ids.is_empty() {
            let pending = self.claims_to_confirm(id, &inflight.claim_ids).await?;
            if pending.is_empty() {
                // H-5: the confirmation went through but its receipt was lost (or another pass sent
                // it); or nothing is left to confirm: the proof settles the rest.
                info!(vault = %short(id), "nothing of the payout is left to confirm on Base");
            } else if self.vault_frozen(id).await {
                info!(vault = %short(id), "the vault is frozen: the payout is settled by its delivery proof, not confirmed (R-3)");
            } else {
                self.confirm_on_base(id, inflight, &pending).await?;
            }
        }
        if let Some(kept) = &inflight.spend_commit {
            // Section 3: the payout's commitment, whose delivery this coordinator proves (its event
            // records it too).
            let record = CommittedPayout {
                payout_ref: kept.record.payout_ref,
                input_tag: kept.record.input_tag,
                claims: kept.record.claim_ids.iter().map(|c| c.0).collect(),
            };
            self.update_vault(id, |v| {
                if !v.commitments.iter().any(|c| c.payout_ref == record.payout_ref) {
                    v.commitments.push(record);
                }
            })
            .await?;
        }
        self.update_vault(id, |v| {
            for c in v.claims.iter_mut().filter(|c| inflight.claim_ids.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
            }
            // Seen mined here (I-5): a later dispute of its confirmation does not reopen it in these
            // books.
            for id in &inflight.claim_ids {
                if !v.paid_here.contains(&id.0) {
                    v.paid_here.push(id.0);
                }
            }
            if let Some(note) = inflight.refund_note {
                for r in v.refunds.iter_mut().filter(|r| r.note_id == note) {
                    r.done = true;
                }
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

    /// R-1 and H-5: the claims of a mined payout that three signers confirm: those the Registry
    /// still has pending, never a reopened one; none if the books already show them all paid (the
    /// confirmation's event reached the safe head).
    async fn claims_to_confirm(&self, id: &B256, claim_ids: &[B256]) -> Result<Vec<B256>> {
        let (known_paid, reopened) = {
            let store = self.store.lock().await;
            let v = store.vaults.get(id);
            let paid = v.is_some_and(|v| {
                claim_ids.iter().all(|c| v.claims.iter().any(|x| x.id == c.0 && x.state == ClaimState::Paid))
            });
            (paid, v.map(|v| v.reopened.clone()).unwrap_or_default())
        };
        if known_paid {
            return Ok(vec![]);
        }
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IRegistryClaims::new(self.cfg.registry, &provider);
        let mut states = Vec::with_capacity(claim_ids.len());
        for c in claim_ids {
            let r = registry.claim(*c).call().await.context("Registry.claim")?;
            states.push((r.vaultId, r.status));
        }
        Ok(inflight::confirmable(id, claim_ids, &states, &reopened))
    }

    /// R-3: whether the slasher reports the vault frozen (an open F1/F2 case, or compromised), so
    /// that the Registry refuses its confirmations. Unreadable: not frozen (the confirmation is
    /// tried, and refused if it is).
    async fn vault_frozen(&self, id: &B256) -> bool {
        let read = async {
            let slasher = self.v3().await?.slasher;
            if slasher == Address::ZERO {
                return anyhow::Ok(false);
            }
            let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
            IZecuredSlasher::new(slasher, &provider)
                .vaultFrozen(*id)
                .call()
                .await
                .context("slasher.vaultFrozen")
        };
        read.await.unwrap_or(false)
    }

    /// R-1: `confirmPayout(vaultId, payoutRef, claimIds, sigs)` for the payout's `claims` still
    /// pending on Base, with three nodes' `PayoutConfirmation` over its payout reference.
    async fn confirm_on_base(&self, id: &B256, inflight: &Inflight, claims: &[B256]) -> Result<()> {
        let path = format!("/v1/vaults/{id}/confirm");
        let body = ConfirmIn {
            claim_ids: claims.to_vec(),
            txid: inflight.txid.clone(),
        };
        let reference = match &inflight.spend_commit {
            Some(kept) => kept.record.payout_ref,
            None => {
                let v = self.vault(id).await?;
                let txid = TxId::from_hex(&inflight.txid).context("stored txid")?;
                B256::from(payout_ref(&v.ufvk, txid.as_ref()))
            }
        };
        // R-1: each payout is confirmed once; a confirmation already on Base (its receipt lost, its
        // event not at the safe head yet) is not asked for again. Unreadable: asked for.
        let reader = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        if IZecuredRegistry::new(self.cfg.registry, &reader)
            .payoutConfirmed(reference)
            .call()
            .await
            .unwrap_or(false)
        {
            info!(vault = %short(id), "the payout's confirmation is on Base already");
            return Ok(());
        }
        let answers = self.nodes.post_many::<_, ConfirmResponse>(&self.nodes.indices(), &path, &body).await;
        let (answers, failed) = split(answers);
        let msg = payout_confirmation(*id, reference, claims);
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
        let call = registry.confirmPayout(*id, reference, claims.to_vec(), sigs);
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

    /// The words `GET /vaults/:id` reports a claim's state with: those app/lib/api.ts parses.
    #[test]
    fn claim_statuses_are_the_apps_words() {
        let states = [
            ClaimState::Unpaid,
            ClaimState::Paid,
            ClaimState::WrittenOff,
            ClaimState::Compensated,
            ClaimState::Disputed,
        ];
        assert_eq!(states.map(claim_status), ["pending", "paid", "written_off", "compensated", "disputed"]);
    }

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
            proven: false,
            confirmed_at: 0,
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

        // A claim this coordinator saw paid stays paid in its books, as with the nodes that
        // confirmed it: disputed, it is still proven (Base's state), and reopened it is not paid
        // again.
        v.paid_here = vec![[1; 32]];
        apply_event(&mut v, &change(ClaimChange::Disputed { due_at: 299 }));
        assert_eq!(v.claims[0].state, ClaimState::Disputed);
        assert_eq!(v.books()[0].state, ClaimState::Paid);
        assert!(!v.is_idle() && v.settled(), "busy proving, but nothing to pay");
        apply_event(&mut v, &change(ClaimChange::Reopened));
        assert_eq!((v.claims[0].state, v.books()[0].state), (ClaimState::Unpaid, ClaimState::Paid));
        assert!(!v.books()[0].payable());
        // D57: it proves that payout's inclusion still, which upholds the reopened claim.
        assert_eq!(v.to_uphold(), vec![[1; 32]]);
        assert!(!v.is_idle(), "busy proving");
        apply_event(
            &mut v,
            &change(ClaimChange::Upheld {
                payout_ref: B256::repeat_byte(2),
            }),
        );
        assert_eq!(v.claims[0].state, ClaimState::Paid);
        assert!(v.to_uphold().is_empty());
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
        apply_event(
            &mut v,
            &at(RegistryEvent::PayoutConfirmed {
                payout_ref: B256::repeat_byte(0xaa),
                claims: vec![B256::repeat_byte(1), B256::repeat_byte(2)],
            }),
        );
        assert_eq!(v.status, VaultStatus::Closed);
    }

    /// v4: the coordinator records the vault's commitments (to prove their delivery), its pending
    /// refunds until they open or drop (R-12), the claims armed on the bond (R-5), when each claim
    /// was confirmed and whether it is final, and the block of the vault's newest event (O-2).
    #[test]
    fn v4_events_move_the_coordinators_records() {
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        v.activated_tag = Some([0x7a; 32]);
        v.outstanding = 1_000_000;
        let ev = |block: u64, event: RegistryEvent| VaultEvent {
            block,
            timestamp: block * 2,
            ..at(event)
        };
        apply_event(&mut v, &ev(10, RegistryEvent::Claims(vec![claim(1, 400_000)])));
        assert!(v.last_event_block == 10 && v.last_event_at > 0);
        let committed = RegistryEvent::SpendCommitted {
            payout_ref: B256::repeat_byte(0xaa),
            input_tag: B256::repeat_byte(0x7a),
            claims: vec![B256::repeat_byte(1)],
        };
        apply_event(&mut v, &ev(11, committed.clone()));
        apply_event(&mut v, &ev(11, committed));
        assert_eq!(v.commitments.len(), 1, "once");
        assert_eq!(v.to_prove(), vec![[1; 32]], "committed, not final: to prove");
        let confirmed = RegistryEvent::PayoutConfirmed {
            payout_ref: B256::repeat_byte(0xaa),
            claims: vec![B256::repeat_byte(1)],
        };
        apply_event(&mut v, &ev(12, confirmed));
        assert_eq!((v.claims[0].state, v.claims[0].confirmed_at), (ClaimState::Paid, 24));
        // R-3: a confirmation nobody disputed in time is final as well: nothing left to prove.
        let mut expired = v.clone();
        let expiry = RegistryEvent::ClaimChanged {
            claim_id: B256::repeat_byte(1),
            change: ClaimChange::Expired,
        };
        apply_event(&mut expired, &ev(13, expiry));
        assert!(expired.claims[0].is_final() && expired.to_prove().is_empty());
        let proven = RegistryEvent::ClaimChanged {
            claim_id: B256::repeat_byte(1),
            change: ClaimChange::Proven {
                payout_ref: B256::repeat_byte(0xaa),
            },
        };
        apply_event(&mut v, &ev(13, proven));
        assert!(v.claims[0].is_final() && v.to_prove().is_empty());
        // R-12: a pending refund waits until its claim opens (or it is dropped).
        let pending = |tag: u8| RegistryEvent::RefundPending {
            deposit_tag: B256::repeat_byte(tag),
            reason: 0,
            ready_height: 100,
            ready_time: 200,
        };
        apply_event(&mut v, &ev(14, pending(0x51)));
        apply_event(&mut v, &ev(14, pending(0x52)));
        assert_eq!(v.pending_refunds.len(), 2);
        assert!(!{
            let mut idle = v.clone();
            idle.status = VaultStatus::Cancelled;
            idle.is_idle()
        });
        apply_event(
            &mut v,
            &ev(
                15,
                RegistryEvent::PendingRefundDropped {
                    deposit_tag: B256::repeat_byte(0x51),
                },
            ),
        );
        let opened = Claim {
            kind: ClaimKind::Refund,
            commitment: [0; 32],
            refund_tag: Some([0x52; 32]),
            ..claim(2, 250_000)
        };
        apply_event(&mut v, &ev(16, RegistryEvent::Claims(vec![opened])));
        assert!(v.pending_refunds.is_empty());
        // R-5: an arming counts for the claim's commitment at the time only.
        let mut c3 = claim(3, 100_000);
        apply_event(&mut v, &ev(17, RegistryEvent::Claims(vec![c3.clone()])));
        let armed = RegistryEvent::LivenessArmed {
            claim_id: B256::repeat_byte(3),
            commitment: B256::from(c3.commitment),
            payable_at: 34,
        };
        apply_event(&mut v, &ev(18, armed));
        assert!(v.armed(&v.claims[2].clone()));
        c3.commitment = [9; 32];
        assert!(!v.armed(&c3), "a changed destination voids the arming");
        assert_eq!(v.last_event_block, 18);
    }

    /// O-2: a signing threshold of nodes must report a Base block at least as recent as the event.
    #[test]
    fn the_signers_catch_up_before_they_are_asked() {
        let blocks = [(1, 100), (2, 99), (3, 100), (4, 50), (5, 101)];
        assert!(caught_up(&blocks, 100, 3));
        assert!(!caught_up(&blocks, 101, 3));
        assert!(caught_up(&blocks, 99, 4));
        assert!(!caught_up(&[], 1, 3));
    }

    /// R-5: a node's receipt counts only for the claim's current commitment, by that node's
    /// attestation key under the bond's domain, and not dated ahead of the clock.
    #[test]
    fn receipts_are_checked_against_the_node_and_the_claim() {
        let key = PrivateKeySigner::random();
        let domain = bond_domain(84_532, Address::repeat_byte(0xcc));
        let (claim_id, commitment) = (B256::repeat_byte(1), B256::repeat_byte(2));
        let receipt = |at: u64, signer: &PrivateKeySigner, c: B256| ReceiptOut {
            claim_id,
            commitment: c,
            received_at: at,
            signature: sign_typed(signer, &domain, &destination_received(claim_id, c, at)).unwrap(),
        };
        let now = 1_800_000_000;
        assert!(valid_receipt(
            &receipt(now - 10, &key, commitment),
            claim_id,
            commitment,
            &domain,
            key.address(),
            now
        ));
        // Another node's key, another commitment (a changed destination), the future, another domain.
        let other = PrivateKeySigner::random();
        assert!(!valid_receipt(&receipt(now, &other, commitment), claim_id, commitment, &domain, key.address(), now));
        let old = receipt(now, &key, B256::repeat_byte(3));
        assert!(!valid_receipt(&old, claim_id, commitment, &domain, key.address(), now));
        let ahead = receipt(now + RECEIPT_CLOCK_SKEW_SECS + 1, &key, commitment);
        assert!(!valid_receipt(&ahead, claim_id, commitment, &domain, key.address(), now));
        let elsewhere = bond_domain(84_532, Address::repeat_byte(0xcd));
        assert!(!valid_receipt(
            &receipt(now, &key, commitment),
            claim_id,
            commitment,
            &elsewhere,
            key.address(),
            now
        ));
        assert!(!valid_receipt(&receipt(now, &key, B256::ZERO), claim_id, B256::ZERO, &domain, key.address(), now));
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
            offchain: true,
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
                invite: None,
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

    /// Section 6: with an invite file, the operator's switch closes openings before anything else,
    /// an invite-only opening needs a listed code with a use left, and a failed opening gives its
    /// use back.
    #[tokio::test]
    async fn invite_codes_gate_vault_openings() {
        let (c, dir) = offline_with_stamps("invites");
        let file = dir.0.join("invites.json");
        let write =
            |config: &invites::InviteConfig| std::fs::write(&file, serde_json::to_vec(config).unwrap()).unwrap();
        let config = invites::InviteConfig {
            openings: true,
            invite_only: true,
            codes: vec![invites::InviteCode {
                hash: invites::code_hash("beta-7f3k"),
                uses: 1,
            }],
        };
        write(&config);
        let c = c.with_invites(&file, &dir.0).unwrap();
        assert_eq!(
            c.openings(),
            OpeningsStatus {
                open: true,
                invite_only: true
            }
        );
        let owner = PrivateKeySigner::random();
        let e = c.open_vault(open_request(&owner, &c, now() + 600)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::InviteRequired));
        let invited = |code: &str| OpenVaultRequest {
            invite: Some(code.into()),
            ..open_request(&owner, &c, now() + 600)
        };
        let e = c.open_vault(invited("beta-wrong")).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::InviteRequired));
        // Past the gate (then the unreachable light wallet server fails it, which gives the use
        // back): the same code passes the gate again.
        for _ in 0..2 {
            let e = c.open_vault(invited("beta-7f3k")).await.unwrap_err();
            assert_eq!(public_error(&e), Some(PublicError::ChainUnavailable));
        }
        // Closed: refused first, even an unsigned request without a stamp.
        write(&invites::InviteConfig {
            openings: false,
            ..config
        });
        let mut unsigned = invited("beta-7f3k");
        unsigned.signature = Bytes::new();
        unsigned.stamp = u64::MAX;
        let e = c.open_vault(unsigned).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::OpeningsClosed));
        assert!(!c.openings().open);
        // Without an invite file nothing is gated.
        let (free, _dir2) = offline("no-invites");
        assert_eq!(
            free.openings(),
            OpeningsStatus {
                open: true,
                invite_only: false
            }
        );
    }

    /// R-11.1: the owner's request to prove its deposit as a refund is refused, with a fixed code,
    /// before anything reaches Base unless it makes sense: proofs configured, the owner's signature,
    /// a fresh nonce, a vault still waiting for its deposit, a deposit seen.
    #[tokio::test]
    async fn a_refund_proof_request_is_checked_before_reaching_base() {
        let (c, dir) = offline("prove-refund");
        let owner = PrivateKeySigner::random();
        let mut v = vault(1);
        v.owner = owner.address();
        c.store.lock().await.save(v).unwrap();
        let id = B256::repeat_byte(1);
        let domain = c.domain();
        let sign = |key: &PrivateKeySigner, nonce: u64| {
            let msg = ProveRefund {
                vaultId: id,
                nonce,
                deadline: now() + 600,
            };
            sign_typed(key, &domain, &msg).unwrap()
        };
        let e = c.request_refund_proof(id, 1, now() + 600, &sign(&owner, 1)).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NotConfigured), "no proofs on this coordinator");
        let settings = proofs::DepositProofs {
            host: std::path::PathBuf::from("zecured-tier2-host"),
            mock: true,
            headers: std::path::PathBuf::new(),
            summaries: std::path::PathBuf::new(),
            concurrency: 1,
        };
        let c = c.with_deposit_proofs(settings, &dir.0).unwrap();
        let request = |key: &PrivateKeySigner, nonce: u64| {
            let sig = sign(key, nonce);
            let c = &c;
            async move { c.request_refund_proof(id, nonce, now() + 600, &sig).await }
        };
        let e = request(&PrivateKeySigner::random(), 1).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NotOwner));
        let e = request(&owner, 1).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NoDeposit), "no conforming deposit seen");
        let position = NotePosition {
            height: 4_486_100,
            tx_index: 1,
            action_index: 0,
        };
        c.store
            .lock()
            .await
            .update(&id, |v| v.decisions.push((position, Classification::Conforming)))
            .unwrap();
        // Past the local checks: Base is unreachable here.
        let e = request(&owner, 1).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::ChainUnavailable));
        c.store.lock().await.update(&id, |v| v.used_request_nonces.push(2)).unwrap();
        let e = request(&owner, 2).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NonceUsed));
        c.store.lock().await.update(&id, |v| v.status = VaultStatus::DepositSeen).unwrap();
        let e = request(&owner, 3).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::AlreadyLanded));
    }

    /// A Base RPC stub answering `eth_call` by function selector (`returns`), every other method
    /// with the chain id.
    async fn rpc_stub(returns: Vec<([u8; 4], Vec<u8>)>) -> String {
        use axum::{Json, Router, routing::post};
        let returns = Arc::new(returns);
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| {
                let returns = returns.clone();
                async move {
                    let result = match req["method"].as_str() {
                        Some("eth_call") => {
                            let call = &req["params"][0];
                            let data = call["input"].as_str().or(call["data"].as_str()).unwrap_or_default();
                            let selector =
                                hex::decode(data.trim_start_matches("0x").get(..8).unwrap_or_default()).unwrap();
                            let found = returns.iter().find(|(s, _)| s.as_slice() == selector.as_slice());
                            match found {
                                Some((_, ret)) => serde_json::json!(format!("0x{}", hex::encode(ret))),
                                None => {
                                    return Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"],
                                        "error": { "code": 3, "message": "execution reverted" } }));
                                }
                            }
                        }
                        _ => serde_json::json!("0x14a34"),
                    };
                    Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
                }
            }),
        );
        crate::nodes::tests::serve(app).await
    }

    fn word(a: Address) -> Vec<u8> {
        let mut w = vec![0u8; 12];
        w.extend_from_slice(a.as_slice());
        w
    }

    /// R-5: every node is asked; of the valid receipts (the node's own key, the claim's current
    /// commitment) the earliest is served, then kept for that commitment; none valid is
    /// `no_receipt`.
    #[tokio::test]
    async fn the_earliest_valid_receipt_is_served() {
        use alloy::sol_types::SolCall;
        use axum::{Json, Router, routing::post};
        let bond = Address::repeat_byte(0xcc);
        let rpc = rpc_stub(vec![
            (IZecuredRegistry::intakeCall::SELECTOR, word(Address::repeat_byte(0xa1))),
            (IZecuredRegistry::slasherCall::SELECTOR, word(Address::repeat_byte(0xa2))),
            (IZecuredRegistry::signerBondCall::SELECTOR, word(bond)),
        ])
        .await;
        let keys: Vec<PrivateKeySigner> = (0..5).map(|_| PrivateKeySigner::random()).collect();
        let (claim_id, commitment) = (B256::repeat_byte(1), B256::from([1u8; 32]));
        let domain = bond_domain(84_532, bond);
        // Node 1 signs with a stranger's key; nodes 2 and 3 answer validly (3 earlier); node 4
        // refuses; node 5 is down.
        let mut urls = vec![];
        for (received_at, key) in [
            (1_000u64, PrivateKeySigner::random()),
            (2_000, keys[1].clone()),
            (1_500, keys[2].clone()),
        ] {
            let answer = ReceiptOut {
                claim_id,
                commitment,
                received_at,
                signature: sign_typed(&key, &domain, &destination_received(claim_id, commitment, received_at)).unwrap(),
            };
            let app = Router::new().route("/v1/receipts", post(move || async move { Json(answer) }));
            urls.push(crate::nodes::tests::serve(app).await);
        }
        let refuse = Router::new().route(
            "/v1/receipts",
            post(|| async {
                (axum::http::StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": "no" })))
            }),
        );
        urls.push(crate::nodes::tests::serve(refuse).await);
        urls.push("http://127.0.0.1:9".into());
        let mut cfg = config(&urls);
        for (node, key) in cfg.nodes.iter_mut().zip(&keys) {
            node.eth = key.address();
        }
        cfg.base_rpc_url = rpc;
        let dir = TempDir::new("receipts");
        let c = Coordinator::new(cfg, &dir.0, &hex::encode(PrivateKeySigner::random().to_bytes())).unwrap();
        let e = c.claim_receipt(claim_id).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NoReceipt), "an unknown claim");
        let mut v = vault(1);
        v.claims = vec![claim(1, 300_000)];
        c.store.lock().await.save(v).unwrap();
        let r = c.claim_receipt(claim_id).await.unwrap();
        assert_eq!((r.received_at, r.signer, r.commitment), (1_500, keys[2].address(), commitment));
        // Kept for that commitment; a changed destination is asked for anew (and none answers).
        assert_eq!(c.claim_receipt(claim_id).await.unwrap(), r);
        c.store.lock().await.update(&B256::repeat_byte(1), |v| v.claims[0].commitment = [9; 32]).unwrap();
        let e = c.claim_receipt(claim_id).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NoReceipt));
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

    /// M-8: an unsigned destination or refund is refused without asking five nodes, and so is the
    /// off-chain refund of a deposit that can land on Base (I30).
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

        // Correctly signed: it does reach the (unreachable) nodes, whatever the vault's status: a
        // Refund claim of a cancelled vault, or of one whose activation is pending, needs its
        // destination too (T-02).
        let sig = sign_typed(&owner, &c.domain(), &msg).unwrap();
        for status in [
            VaultStatus::AwaitingDeposit,
            VaultStatus::DepositSeen,
            VaultStatus::Cancelled,
        ] {
            c.store.lock().await.update(&id, |v| v.status = status).unwrap();
            let e = c.add_destination(id, &ua, salt, 1, msg.deadline, sig.clone()).await.unwrap_err();
            assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable), "{status:?}");
            assert!(!format!("{e:#}").contains("127.0.0.1"), "no node URL in errors: {e:#}");
        }

        // P5: an off-chain refund needs the owner's signature, then a deposit no guest can land.
        let (landable, unlandable) = (B256::repeat_byte(8), B256::repeat_byte(9));
        c.store
            .lock()
            .await
            .update(&id, |v| {
                v.non_conforming = [(landable, false), (unlandable, true)]
                    .map(|(note_id, offchain)| NoteView {
                        note_id,
                        amount: 250_000,
                        offchain,
                    })
                    .to_vec();
            })
            .unwrap();
        let refund = |note_id: B256| RefundRequest {
            vaultId: id,
            noteId: note_id,
            commitment: B256::from(r.commitment()),
            nonce: 2,
            deadline: msg.deadline,
        };
        let request = |note_id: B256, key: &PrivateKeySigner| {
            let sig = sign_typed(key, &c.domain(), &refund(note_id)).unwrap();
            c.request_refund(id, note_id, &ua, salt, 2, msg.deadline, sig)
        };
        let e = request(unlandable, &stranger).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::NotOwner));
        let e = request(landable, &owner).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::RefundOnBase));
        let e = request(B256::repeat_byte(7), &owner).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::BadRequest));
        let e = request(unlandable, &owner).await.unwrap_err();
        assert_eq!(public_error(&e), Some(PublicError::SignersUnavailable));
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

    /// A Base RPC stub whose Registry reports every claim of `vault` with `status` (any other call
    /// reverts).
    async fn registry_stub(vault: B256, status: u8) -> String {
        use alloy::sol_types::SolCall;
        // claim(bytes32) returns (vaultId, kind, amount, commitment, status).
        let mut ret = vault.to_vec();
        ret.extend([0u8; 32]);
        ret.extend(alloy::primitives::U256::from(500_000u64).to_be_bytes::<32>());
        ret.extend([7u8; 32]);
        ret.extend(alloy::primitives::U256::from(status).to_be_bytes::<32>());
        rpc_stub(vec![(IRegistryClaims::claimCall::SELECTOR, ret)]).await
    }

    fn payout_in_flight(claims: &[u8]) -> Inflight {
        Inflight {
            claim_ids: claims.iter().map(|c| B256::repeat_byte(*c)).collect(),
            refund_note: None,
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
