//! A node's own view of both chains.
//!
//! Zcash: every vault is scanned from its birthday through this node's light wallet server
//! connection; deposit transactions are checked on the full transaction (P1, D5). The vault's
//! notes are sorted by what Base says about them: its main lineage, the notes of its refunded
//! deposits, and deposits that have not landed on Base. Of those, a deposit a guest of the
//! deployment can land leaves the vault only through a proof; only one no guest can land may be
//! refunded off-chain (P5 under I30).
//! Base: Registry events are applied up to the `safe` head only, each one checked against the
//! Registry's state first (C-2), and at most once per vault (M-9).

use std::{collections::HashMap, time::Duration};

use alloy::{
    primitives::{Address, B256},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, ensure};
use orchard::{
    keys::{FullViewingKey, Scope},
    note::ExtractedNoteCommitment,
};
use zcash_protocol::TxId;
use zecured_base::{
    events::{ClaimChange, RegistryEvent, VaultEvent},
    verify::{self, ProviderReader, Verdict},
};
use zecured_policy::{
    classify::{self, Classification, DepositNote, DepositRules, Landing, NotePosition},
    derive::{deposit_tag, note_id},
    ledger::{self, ClaimKind, ClaimState, VaultNote},
};
use zecured_zcash_ops::{
    keys::VaultSecret,
    lwd::{self, LwdClient},
    scan::{ReceivedNote, ScanState},
    tx, witness,
};

use crate::{
    config::ZcashNetwork,
    store::{Store, VaultRecord},
};

/// Time allowed to connect to the Base RPC (M-7).
pub const BASE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for one Base RPC request (M-7).
pub const BASE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What Base says about a vault's notes (K2-a, T-02).
#[derive(Clone, Copy, Debug)]
pub struct BaseView<'a> {
    /// The vault's viewing key string: note tags are keyed by it (D24).
    pub ufvk: &'a str,
    /// The vault's published unified address: a deposit must pay exactly its receiver (K2-c).
    pub address: &'a str,
    /// The deposit tag of `VaultActivated`, once seen: the note that backs the vault.
    pub activated_tag: Option<[u8; 32]>,
    /// The tags of the vault's refunded deposits (`RefundOpened`): each note pays its own refund.
    pub refund_tags: &'a [[u8; 32]],
    /// I30: what the deployment's guests can land on Base (the Intake's journal version and
    /// refund floor). `None` while unknown: then nothing is refunded off-chain.
    pub landing: Option<Landing>,
}

/// One scanned note, as far as sorting the vault's notes is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteFacts {
    pub position: NotePosition,
    pub value: u64,
    /// Received at the vault's published address (a deposit), not its internal one.
    pub external: bool,
    /// D24 tag of the note.
    pub tag: [u8; 32],
    /// P1 decision, once made.
    pub decision: Option<Classification>,
    /// Its transaction is a D5 deposit transaction (deposits only).
    pub ironwood_only: bool,
    /// It pays exactly the vault's published receiver (deposits only).
    pub to_vault_receiver: bool,
}

impl NoteFacts {
    /// The note as P1 and the deposit statement see it.
    pub fn deposit(&self) -> DepositNote {
        DepositNote {
            position: self.position,
            value: self.value,
            ironwood_only: self.ironwood_only,
            to_vault_receiver: self.to_vault_receiver,
        }
    }
}

/// The note that roots a vault's main lineage (K2-a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    /// This scanned note.
    Note(usize),
    /// None: Base has not activated the vault (nothing was minted).
    None,
    /// Base activated the vault with a note this node cannot see: its Zcash data is wrong or
    /// incomplete, and nothing is paid from the main lineage until someone looks into it.
    Missing,
}

/// K2-a: the root of the vault's main lineage: the note Base activated the vault with (named by
/// its deposit tag), whatever P1 decided. A deposit proof needs no signer, and P1 rests on the
/// light wallet server's answers; before the activation nothing is paid from the vault's deposit.
pub fn lineage_root(notes: &[NoteFacts], base: &BaseView<'_>) -> Root {
    let Some(tag) = base.activated_tag else {
        return Root::None;
    };
    notes.iter().position(|n| n.external && n.tag == tag).map_or(Root::Missing, Root::Note)
}

/// P5 under I30: whether an unspent deposit that has not landed on Base may be refunded off-chain.
/// P1 classified it non-conforming, no guest of the deployment can land it (`landing`; unknown:
/// never), it pays its own refund's fee, and it is deep enough: a note of the vault's amount waits
/// twice the deposit depth (K2), so that no reorg brings back a chain where a guest could land it
/// (a late payment mined again by the expiry height). Every other deposit leaves the vault only as
/// a Refund claim, once a proof lands it.
pub fn refundable_offchain(n: &NoteFacts, landing: Option<Landing>, rules: &DepositRules, tip: u32) -> bool {
    let Some(landing) = landing else {
        return false;
    };
    n.external
        && matches!(n.decision, Some(Classification::NonConforming(_)))
        && !landing.can_land(&n.deposit(), rules)
        && n.value > zecured_zcash_ops::spend::zip317_fee(1, 1)
        && (n.value != rules.amount
            || classify::confirmations(n.position.height, tip) >= rules.confirmations.saturating_mul(2))
}

/// The vault's notes, sorted by what Base says about them (see `zecured_policy::ledger`).
pub struct VaultNotes {
    pub tip: u32,
    /// Unspent main-lineage notes: the deposit Base activated the vault with and its own change.
    pub lineage: Vec<ReceivedNote>,
    /// Unspent notes of the vault's refunded deposits, by tag: each pays its own Refund claim
    /// (T-02, I30).
    pub refunds: Vec<([u8; 32], ReceivedNote)>,
    /// Unspent deposits (notes received at the vault's published address) that are no lineage
    /// note of the vault, by note id: deposits that have not landed on Base, as collateral or as a
    /// refund. A committed payout spends one once a proof lands it.
    pub unlanded: Vec<([u8; 32], ReceivedNote)>,
    /// The note ids of the deposits of [`Self::unlanded`] that a guest of the deployment can land on
    /// Base (I30): a proof lands each as collateral or as a Refund claim.
    pub landable: Vec<[u8; 32]>,
    /// The deposits of [`Self::unlanded`] that may be refunded off-chain (P5): no guest of the
    /// deployment can land them ([`refundable_offchain`], I30).
    pub refundable: Vec<([u8; 32], ReceivedNote)>,
    /// P1's conforming deposit, if decided (spent or not): the deposit a proof activates the vault
    /// with.
    pub conforming: Option<ReceivedNote>,
    /// Unspent notes in no lineage, ignored for I10 and fees (H-2): zero-value notes, notes sent
    /// to the internal address by anyone but the vault, and outputs of a vault transaction that
    /// are not its planned change (see [`Self::retain_planned_change`]).
    pub ignored: Vec<ReceivedNote>,
    /// Nullifiers of every unspent note of the vault, whatever its lineage.
    pub unspent: Vec<[u8; 32]>,
}

impl VaultNotes {
    pub fn lineage_value(&self) -> u64 {
        self.lineage.iter().map(|n| n.value()).sum()
    }

    /// H-2: keeps in the main lineage only the deposit and change worth exactly the vault's book
    /// value (`ledger::book_value(outstanding, claims)`): the planned change of the payout that
    /// created it, once that payout's claims are marked paid. Anything else moves to
    /// [`Self::ignored`].
    pub fn retain_planned_change(&mut self, book_value: u64) {
        let dropped = ledger::retain_planned_change(
            &mut self.lineage,
            book_value,
            |n| n.scope == Scope::Internal,
            ReceivedNote::value,
        );
        self.ignored.extend(dropped);
    }

    /// The unspent refund notes by tag and value, as the planner takes them.
    pub fn refund_values(&self) -> Vec<([u8; 32], u64)> {
        self.refunds.iter().map(|(tag, n)| (*tag, n.value())).collect()
    }

    /// Whether the main lineage adds up for `claims` (I10) once restricted to the planned change of
    /// their book value ([`Self::retain_planned_change`], without changing `self`). When it does
    /// not, a confirmed claim may still be unpaid (O-4 rule 3: its input is still unspent).
    pub fn books_add_up(&self, outstanding: u64, claims: &[ledger::Claim]) -> bool {
        let book = ledger::book_value(outstanding, claims);
        let value: u64 = self
            .lineage
            .iter()
            .filter(|n| n.scope != Scope::Internal || n.value() == book)
            .map(ReceivedNote::value)
            .sum();
        ledger::check_i10(value, outstanding, claims).is_ok()
    }
}

/// The note commitment (`cmx`) of a scanned note: what its tag is keyed over (D24).
pub fn note_cmx(n: &ReceivedNote) -> [u8; 32] {
    ExtractedNoteCommitment::from(n.note.commitment()).to_bytes()
}

/// The tags of the vault's refunded deposits, from its Refund claims.
pub fn refund_tags(claims: &[ledger::Claim]) -> Vec<[u8; 32]> {
    claims.iter().filter(|c| c.kind == ClaimKind::Refund).filter_map(|c| c.refund_tag).collect()
}

/// Zcash access for one node.
pub struct ZcashView {
    url: String,
    /// The deployment's Zcash network: the light wallet server must follow it.
    network: ZcashNetwork,
    /// Independent light wallet servers that must agree on a deposit (C-1).
    witnesses: Vec<String>,
    client: Option<LwdClient>,
    scans: HashMap<B256, ScanState>,
    /// Ironwood-only flag of deposit transactions, by txid.
    shapes: HashMap<[u8; 32], bool>,
}

/// The deposit statement's parameters for the deployment's Zcash network (its guests are built per
/// network; nodes read deposits as the deployment's guest proves them).
pub fn statement_network(network: ZcashNetwork) -> &'static zecured_deposit_statement::Network {
    match network {
        ZcashNetwork::Testnet => &zecured_deposit_statement::TESTNET,
        ZcashNetwork::Mainnet => &zecured_deposit_statement::MAINNET,
    }
}

pub fn vault_fvk(v: &VaultRecord) -> Result<FullViewingKey> {
    VaultSecret::from_bytes(v.sk_bytes()?).full_viewing_key(&v.ak_bytes()?)
}

/// Whether `receiver` is one of the vault's own addresses (any diversifier, either scope). A
/// payout to it would land back in the vault, outside its books (H-2).
pub fn owns_receiver(fvk: &FullViewingKey, receiver: &[u8; 43]) -> bool {
    Option::<orchard::Address>::from(orchard::Address::from_raw_address_bytes(receiver))
        .is_some_and(|a| fvk.scope_for_address(&a).is_some())
}

/// Adds the P1 decisions of a scan to `current` (decisions are final: a changed one is an
/// error). Returns whether anything was added.
pub fn merge_decisions(
    current: &mut Vec<(NotePosition, Classification)>,
    scanned: &[(NotePosition, Classification)],
) -> Result<bool> {
    let mut added = false;
    for (pos, class) in scanned {
        match current.iter().find(|(p, _)| p == pos) {
            Some((_, c)) => ensure!(c == class, "P1 decision changed for {pos:?}"),
            None => {
                current.push((*pos, *class));
                added = true;
            }
        }
    }
    Ok(added)
}

impl ZcashView {
    pub fn new(url: &str, network: ZcashNetwork) -> Self {
        Self::with_witnesses(url, network, vec![])
    }

    /// A view whose deposit checks also need every `witnesses` server to agree (C-1).
    pub fn with_witnesses(url: &str, network: ZcashNetwork, witnesses: Vec<String>) -> Self {
        Self {
            url: url.to_owned(),
            network,
            witnesses,
            client: None,
            scans: HashMap::new(),
            shapes: HashMap::new(),
        }
    }

    /// The deployment's Zcash network.
    pub fn network(&self) -> ZcashNetwork {
        self.network
    }

    /// The light wallet server, connected once it has said it follows the deployment's network.
    async fn client(&mut self) -> Result<&mut LwdClient> {
        if self.client.is_none() {
            let mut client = lwd::connect(&self.url).await?;
            lwd::check_network(&mut client, self.network).await?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("just set"))
    }

    pub async fn tip(&mut self) -> Result<u32> {
        let res = lwd::latest_block(self.client().await?).await;
        if res.is_err() {
            self.client = None; // reconnect next time
        }
        Ok(u32::try_from(res?.height)?)
    }

    /// Scans the vault up to the current tip and sorts its notes (node variant). New P1
    /// decisions are added to `v.decisions`; the caller persists them.
    pub async fn vault_notes(
        &mut self,
        v: &mut VaultRecord,
        confirmations: u32,
        landing: Option<Landing>,
    ) -> Result<VaultNotes> {
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations,
        };
        let fvk = vault_fvk(v)?;
        let (ufvk, address) = (v.ufvk.clone(), v.address.clone());
        let refunds = refund_tags(&v.claims);
        let base = BaseView {
            ufvk: &ufvk,
            address: &address,
            activated_tag: v.activated_tag,
            refund_tags: &refunds,
            landing,
        };
        self.notes(&v.vault_id, &fvk, v.birthday, &rules, &mut v.decisions, base).await
    }

    /// Scans a vault up to the current tip, extends its persisted P1 `decisions` and sorts its
    /// notes by what Base says about them.
    ///
    /// The main lineage returned here is the deposit Base activated the vault with and every note
    /// the vault's own transactions returned to its internal address (H-2,
    /// [`ledger::lineage_members`]); [`VaultNotes::retain_planned_change`] then keeps only the
    /// planned change. A refunded deposit's note is its own refund's input (T-02). Every other
    /// deposit has not landed on Base; those no guest can land are refundable off-chain (P5).
    pub async fn notes(
        &mut self,
        vault_id: &B256,
        fvk: &FullViewingKey,
        birthday: u32,
        rules: &DepositRules,
        decisions: &mut Vec<(classify::NotePosition, Classification)>,
        base: BaseView<'_>,
    ) -> Result<VaultNotes> {
        let tip = self.tip().await?;
        // tonic clients are cheap handles over one connection; clone to scan while `self`
        // stays borrowable.
        let mut client = self.client().await?.clone();
        if !self.scans.contains_key(vault_id) {
            let state = ScanState::from_birthday(&mut client, fvk.clone(), birthday).await?;
            self.scans.insert(*vault_id, state);
        }
        let scanned = self.scans.get_mut(vault_id).expect("inserted").scan_to(&mut client, tip).await;
        if let Err(e) = scanned {
            // Reorg (or a broken stream): start over from the birthday; vault scans are short.
            // No details: scan errors carry block heights, which could link a vault to its notes.
            let _ = e;
            tracing::warn!("vault scan failed; rescanning it from its birthday");
            let mut state = ScanState::from_birthday(&mut client, fvk.clone(), birthday).await?;
            state.scan_to(&mut client, tip).await?;
            self.scans.insert(*vault_id, state);
        }

        // P1: classify external notes in chain order; persist new decisions.
        let externals: Vec<ReceivedNote> =
            self.scans[vault_id].notes.iter().filter(|n| n.scope == Scope::External).cloned().collect();
        // K2-c: the deposit statement's own reading of the vault's address (on the deployment's
        // network). A vault whose address it rejects can receive no conforming deposit.
        let receiver = zecured_deposit_statement::vault_receiver_on(statement_network(self.network), base.address).ok();
        let mut deposits = Vec::with_capacity(externals.len());
        for n in &externals {
            let ironwood_only = self.ironwood_only(&n.txid).await?;
            deposits.push(DepositNote {
                position: to_policy_position(&n.position),
                value: n.value(),
                ironwood_only,
                to_vault_receiver: receiver == Some(n.recipient.to_raw_address_bytes()),
            });
        }
        let decided = classify::classify(rules, tip, &deposits);
        for (note, class) in &decided {
            if *class != Classification::Pending && !decisions.iter().any(|(p, _)| *p == note.position) {
                decisions.push((note.position, *class));
            }
        }
        // A persisted decision is final, even if a later reorg would reorder things.
        for (pos, class) in decisions.iter() {
            if let Some((_, now)) = decided.iter().find(|(n, _)| n.position == *pos) {
                ensure!(now == class || *now == Classification::Pending, "P1 decision changed for {pos:?}");
            }
        }

        let scan = &self.scans[vault_id];
        let decision =
            |n: &ReceivedNote| decisions.iter().find(|(p, _)| *p == to_policy_position(&n.position)).map(|(_, c)| *c);
        let conforming = scan
            .notes
            .iter()
            .find(|n| n.scope == Scope::External && decision(n) == Some(Classification::Conforming))
            .cloned();

        // H-2: the main lineage follows the vault's own spends from its root (K2-a).
        let mut ordered: Vec<&ReceivedNote> = scan.notes.iter().collect();
        ordered.sort_by_key(|n| n.position);
        let facts: Vec<NoteFacts> = ordered
            .iter()
            .map(|n| {
                let position = to_policy_position(&n.position);
                let shape = deposits.iter().find(|d| d.position == position);
                NoteFacts {
                    position,
                    value: n.value(),
                    external: n.scope == Scope::External,
                    tag: deposit_tag(base.ufvk, &ExtractedNoteCommitment::from(n.note.commitment()).to_bytes()),
                    decision: decision(n),
                    ironwood_only: shape.is_some_and(|d| d.ironwood_only),
                    to_vault_receiver: shape.is_some_and(|d| d.to_vault_receiver),
                }
            })
            .collect();
        let root = lineage_root(&facts, &base);
        if root == Root::Missing {
            // No heights or note data: only the vault id, which is public on Base.
            tracing::error!(
                vault = %vault_id,
                "Base activated this vault with a note this node cannot see: no payout of its main lineage (K2-a)"
            );
        }
        if base.refund_tags.iter().any(|t| !facts.iter().any(|f| f.external && f.tag == *t)) {
            tracing::error!(vault = %vault_id, "Base refunds a deposit of this vault that this node cannot see (T-02)");
        }
        let refs: Vec<VaultNote> = ordered
            .iter()
            .map(|n| VaultNote {
                nullifier: n.nullifier.to_bytes(),
                value: n.value(),
                internal: n.scope == Scope::Internal,
                created_in: *n.txid.as_ref(),
                spent_in: n.spent_in.map(|t| *t.as_ref()),
            })
            .collect();
        let root_nullifier = match root {
            Root::Note(i) => Some(refs[i].nullifier),
            Root::None | Root::Missing => None,
        };
        let members = ledger::lineage_members(root_nullifier.as_ref(), &refs);

        let mut notes = VaultNotes {
            tip,
            lineage: vec![],
            refunds: vec![],
            unlanded: vec![],
            landable: vec![],
            refundable: vec![],
            conforming,
            ignored: vec![],
            unspent: vec![],
        };
        for ((n, f), in_lineage) in ordered.into_iter().zip(&facts).zip(members) {
            if n.spent_at.is_some() {
                continue;
            }
            notes.unspent.push(n.nullifier.to_bytes());
            if in_lineage {
                notes.lineage.push(n.clone());
            } else if n.scope == Scope::External && base.refund_tags.contains(&f.tag) {
                notes.refunds.push((f.tag, n.clone()));
            } else if n.scope == Scope::External {
                let id = note_id(&vault_id.0, &to_policy_position(&n.position));
                if base.landing.is_some_and(|l| l.can_land(&f.deposit(), rules)) {
                    notes.landable.push(id);
                } else if refundable_offchain(f, base.landing, rules, tip) {
                    notes.refundable.push((id, n.clone()));
                }
                notes.unlanded.push((id, n.clone()));
            } else {
                notes.ignored.push(n.clone());
            }
        }
        Ok(notes)
    }

    /// The scan state of a vault already scanned through [`Self::notes`].
    pub fn scan(&self, vault_id: &B256) -> Option<&ScanState> {
        self.scans.get(vault_id)
    }

    async fn ironwood_only(&mut self, txid: &TxId) -> Result<bool> {
        let key = *txid.as_ref();
        if let Some(b) = self.shapes.get(&key) {
            return Ok(*b);
        }
        let params = self.network.params();
        let (t, _) = tx::fetch(self.client().await?, &params, txid).await?;
        // K2-c: exactly the deposit statement's D5 on the deployment's network (v6, an epoch its
        // guest proves, Ironwood only, outputs enabled).
        let flag = zecured_deposit_statement::check_deposit_transaction_on(statement_network(self.network), &t).is_ok();
        self.shapes.insert(key, flag);
        Ok(flag)
    }

    /// C-1: every witness server must agree with ours on the block at the deposit's height, list
    /// the deposit transaction in it, serve the same transaction bytes and have a tip within a
    /// few blocks of ours; those bytes must create the note we scanned. Without witnesses this is
    /// a no-op (one public NU7 server today).
    pub async fn check_deposit_witnesses(&mut self, note: &ReceivedNote) -> Result<()> {
        if self.witnesses.is_empty() {
            return Ok(());
        }
        let height = note.position.height;
        let primary = match witness::deposit_view(self.client().await?, &note.txid, height).await {
            Ok(v) => v,
            Err(e) => {
                self.client = None;
                return Err(e.context("our light wallet server"));
            }
        };
        let cmx = ExtractedNoteCommitment::from(note.note.commitment()).to_bytes();
        witness::check_note_in_tx(
            &self.network.params(),
            &note.txid,
            &primary.tx,
            height,
            note.position.action_index,
            &cmx,
        )?;
        for (i, url) in self.witnesses.iter().enumerate() {
            let name = format!("witness light wallet server #{}", i + 1);
            let network = self.network;
            let view = async {
                let mut client = lwd::connect(url).await?;
                lwd::check_network(&mut client, network).await?;
                witness::deposit_view(&mut client, &note.txid, height).await
            }
            .await
            .with_context(|| format!("{name} did not answer"))?;
            witness::check_agreement(&primary, &view, height).with_context(|| format!("{name} disagrees"))?;
        }
        Ok(())
    }

    /// Confirmations of a mined transaction and whether it spends every given nullifier.
    pub async fn spends(&mut self, txid: &TxId, nullifiers: &[[u8; 32]]) -> Result<(u32, bool)> {
        let tip = self.tip().await?;
        let params = self.network.params();
        let (t, height) = tx::fetch(self.client().await?, &params, txid).await?;
        let spent: Vec<[u8; 32]> = t
            .ironwood_bundle()
            .map(|b| b.actions().iter().map(|a| a.nullifier().to_bytes()).collect())
            .unwrap_or_default();
        let all = nullifiers.iter().all(|n| spent.contains(n));
        Ok((classify::confirmations(u32::from(height), tip), all))
    }
}

pub fn to_policy_position(p: &zecured_zcash_ops::scan::NotePosition) -> classify::NotePosition {
    classify::NotePosition {
        height: p.height,
        tx_index: p.tx_index,
        action_index: p.action_index,
    }
}

/// A Base RPC provider whose every request is bounded in time (M-7).
pub fn base_provider(url: &str) -> Result<impl Provider + Clone + use<>> {
    let client = alloy::transports::http::reqwest::Client::builder()
        .connect_timeout(BASE_CONNECT_TIMEOUT)
        .timeout(BASE_REQUEST_TIMEOUT)
        .build()
        .context("Base RPC HTTP client")?;
    Ok(ProviderBuilder::new().connect_reqwest(client, url.parse().context("Base RPC URL")?))
}

/// C-2: checks a Registry event against the Registry's state on `primary` and, if set, on the
/// witness RPC (see `zecured_base::verify`). `later` is a recent block whose state both have,
/// used when the state at the event's own block is gone.
pub async fn verify_event<P: Provider>(
    primary: &P,
    witness: Option<&P>,
    registry: Address,
    ev: &VaultEvent,
    later: u64,
) -> Result<Verdict> {
    let reader = |provider| ProviderReader { provider, registry };
    let witness = witness.map(reader);
    verify::verify_event(&reader(primary), witness.as_ref(), ev, later).await
}

/// Whether this node confirmed a payout that paid `claim` (it saw it mined itself, H-5).
fn confirmed_here(v: &VaultRecord, claim: &[u8; 32]) -> bool {
    v.confirmed.iter().any(|p| p.claim_ids.contains(claim))
}

/// Applies one checked event to the vault it concerns (events of other vaults, and events
/// already applied to this vault, are ignored). Returns whether the vault changed.
pub fn apply(store: &mut Store, ev: &VaultEvent) -> Result<bool> {
    let Some(mut v) = store.vaults.get(&ev.vault_id).cloned() else {
        return Ok(false);
    };
    // M-9: each event is applied at most once per vault, even if the cursor is behind.
    let position = (ev.block, ev.log_index);
    if v.last_event.is_some_and(|last| position <= last) {
        return Ok(false);
    }
    match &ev.event {
        // K3: nothing is paid from a pending vault, and its reservation can still be released. The
        // other contracts' events and the Tier 3 commitments are the coordinator's.
        RegistryEvent::Reserved { .. }
        | RegistryEvent::ReservationReleased
        | RegistryEvent::SpendCommitted { .. }
        | RegistryEvent::RefundPending { .. }
        | RegistryEvent::PendingRefundDropped { .. }
        | RegistryEvent::LivenessArmed { .. }
        | RegistryEvent::PreLiquidated { .. }
        | RegistryEvent::CaseExecuted { .. }
        | RegistryEvent::CaseProven { .. }
        | RegistryEvent::VaultCompromised { .. }
        | RegistryEvent::CollateralBurned { .. }
        | RegistryEvent::VaultCompensated { .. } => return Ok(false),
        // R-16: a deposit took a tag that a commitment of this vault had registered as its change.
        // An honest commitment's change can never carry a deposit's tag (another note, or another
        // vault's key): the commitment's signers registered a false record. Only the vault's id
        // prefix is logged.
        RegistryEvent::ChangeTagOverridden { .. } => {
            tracing::error!(
                vault = %hex::encode(&ev.vault_id[..4]),
                "a deposit took a change tag of this vault's commitment (R-16): that commitment's signers registered a false change; its payout cannot be continued from that tag"
            );
        }
        RegistryEvent::VaultActivated {
            account,
            amount,
            deposit_tag,
        } => {
            ensure!(*account == v.account && *amount == v.amount, "activation does not match the vault");
            // M-9: activation sets `outstanding` once; later claims only lower it.
            if !v.activated {
                v.activated = true;
                v.outstanding = *amount;
                // K2-a: the note Base minted against heads the main lineage.
                v.activated_tag = Some(deposit_tag.0);
            }
        }
        RegistryEvent::Claims(claims) => {
            for c in claims {
                if v.claims.iter().any(|x| x.id == c.id) {
                    continue;
                }
                // Every claim but a refund burns exactly its amount (a full release burns all of
                // it, the fee is minted less, a retirement burns what it retires); a refund's own
                // note backs it.
                if c.kind != ClaimKind::Refund {
                    v.outstanding = v.outstanding.checked_sub(c.amount).context("claim exceeds outstanding")?;
                }
                v.claims.push(c.clone());
            }
        }
        RegistryEvent::ClaimChanged { claim_id, change } => {
            let paid_here = confirmed_here(&v, &claim_id.0);
            if *change == ClaimChange::Reopened && !v.reopened.contains(&claim_id.0) {
                v.reopened.push(claim_id.0);
            }
            let claim = v.claims.iter_mut().find(|c| c.id == claim_id.0).context("change of an unknown claim")?;
            match change {
                ClaimChange::RefundDestination(c) => {
                    claim.commitment = c.0;
                    // Base restarts the claim's liveness clock: it is payable from now on (T-05).
                    claim.created_at = ev.timestamp;
                    if claim.state == ClaimState::WrittenOff {
                        claim.state = ClaimState::Unpaid; // an abandoned refund, revived
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
                // T-05 (a): a node that saw the payout mined itself knows the claim was paid; any
                // other node counts it unpaid again (a confirmation that never reached Zcash leaves
                // its value in the input) until the dispute is resolved.
                ClaimChange::Disputed { .. } => {
                    if !paid_here {
                        claim.state = ClaimState::Disputed;
                    }
                }
                // Section 3: a proof makes the claim paid and final, whatever this node saw; so does
                // a confirmation nobody disputed in time (R-3): the Registry never takes the claim
                // into a commitment again, so O-4's rule 3 must never plan it again.
                ClaimChange::Upheld { .. } | ClaimChange::Proven { .. } | ClaimChange::Expired => {
                    claim.state = ClaimState::Paid;
                    claim.proven = true;
                }
                ClaimChange::Reopened => {
                    if claim.state == ClaimState::Disputed {
                        claim.state = ClaimState::Unpaid;
                    }
                }
                // R-11: out of the coverage cap until a destination is given; nothing to pay yet.
                ClaimChange::Dormant => {}
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
    v.last_event = Some(position);
    store.save_vault(v)?;
    Ok(true)
}

/// Applies checked events in order. One that cannot be applied is skipped with an alert
/// instead of halting every vault (M-9). Returns the number of vaults changed.
pub fn apply_all(store: &mut Store, events: &[VaultEvent]) -> usize {
    let mut applied = 0;
    for ev in events {
        match apply(store, ev) {
            Ok(true) => applied += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(
                block = ev.block,
                log_index = ev.log_index,
                "skipping a Registry event that cannot be applied: {e:#}"
            ),
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;
    use zecured_policy::ledger::Claim;

    use super::*;
    use crate::store::ConfirmedPayout;

    const VAULT: B256 = B256::repeat_byte(1);
    const ACCOUNT: Address = Address::repeat_byte(2);

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("zecured-chain-{}-{}", std::process::id(), rand_suffix()));
        let mut s = Store::open(&dir).unwrap();
        s.save_vault(VaultRecord {
            vault_id: VAULT,
            owner: Address::repeat_byte(3),
            account: ACCOUNT,
            amount: 1_000_000,
            birthday: 100,
            expiry_height: 388,
            key_package: serde_json::Value::Null,
            public_key_package: serde_json::Value::Null,
            sk: String::new(),
            ak: String::new(),
            ufvk: String::new(),
            address: String::new(),
            decisions: vec![],
            activated: false,
            outstanding: 0,
            claims: vec![],
            refunds: vec![],
            used_nonces: vec![],
            inflight: None,
            confirmed: vec![],
            last_event: None,
            activated_tag: None,
            reopened: vec![],
        })
        .unwrap();
        (s, dir)
    }

    fn rand_suffix() -> u64 {
        use rand_core_06::RngCore;
        rand_core_06::OsRng.next_u64()
    }

    fn event(block: u64, log_index: u64, event: RegistryEvent) -> VaultEvent {
        VaultEvent {
            vault_id: VAULT,
            block,
            log_index,
            block_hash: None,
            timestamp: block,
            event,
        }
    }

    fn new_claim(id: u8, kind: ClaimKind, amount: u64, block: u64) -> Claim {
        Claim {
            id: [id; 32],
            kind,
            amount,
            commitment: [id; 32],
            order: (block, 0),
            created_at: block,
            partner: None,
            state: ClaimState::Unpaid,
            refund_tag: None,
            bond_paid: false,
            proven: false,
            confirmed_at: 0,
        }
    }

    fn liquidation(block: u64, id: u8, amount: u64) -> VaultEvent {
        event(block, 0, RegistryEvent::Claims(vec![new_claim(id, ClaimKind::Liquidation, amount, block)]))
    }

    fn change(block: u64, id: u8, change: ClaimChange) -> VaultEvent {
        event(
            block,
            0,
            RegistryEvent::ClaimChanged {
                claim_id: B256::repeat_byte(id),
                change,
            },
        )
    }

    const ACTIVATED_TAG: [u8; 32] = [0x7a; 32];

    fn activation(block: u64) -> VaultEvent {
        event(
            block,
            0,
            RegistryEvent::VaultActivated {
                account: ACCOUNT,
                amount: 1_000_000,
                deposit_tag: B256::from(ACTIVATED_TAG),
            },
        )
    }

    #[test]
    fn activation_records_the_note_that_backs_the_vault() {
        let (mut store, dir) = store();
        assert_eq!(apply_all(&mut store, &[activation(10)]), 1);
        assert_eq!(store.vaults[&VAULT].activated_tag, Some(ACTIVATED_TAG));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ----- K2-a: the main lineage's root -----

    fn deposit(height: u32, value: u64, tag: u8, decision: Option<Classification>) -> NoteFacts {
        NoteFacts {
            position: NotePosition {
                height,
                tx_index: 0,
                action_index: 0,
            },
            value,
            external: true,
            tag: [tag; 32],
            decision,
            ironwood_only: true,
            to_vault_receiver: true,
        }
    }

    const SECOND: Option<Classification> =
        Some(Classification::NonConforming(classify::NonConformingReason::SecondPayment));
    const CONFORMING: Option<Classification> = Some(Classification::Conforming);

    fn base(activated_tag: Option<u8>) -> BaseView<'static> {
        BaseView {
            ufvk: "",
            address: "",
            activated_tag: activated_tag.map(|t| [t; 32]),
            refund_tags: &[],
            landing: None,
        }
    }

    #[test]
    fn the_main_lineage_starts_at_the_note_base_activated_the_vault_with() {
        // P1 decided the first note conforms; the Registry minted against the second (a deposit
        // proof needs no signer, and P1 may have been fed bad data).
        let notes = [
            deposit(200, 1_000_000, 1, CONFORMING),
            deposit(210, 1_000_000, 2, SECOND),
        ];
        assert_eq!(lineage_root(&notes, &base(None)), Root::None, "nothing was minted yet");
        assert_eq!(lineage_root(&notes, &base(Some(2))), Root::Note(1), "the activated note");
        assert_eq!(lineage_root(&notes, &base(Some(1))), Root::Note(0));
        // Base activated the vault with a note this node cannot see.
        assert_eq!(lineage_root(&notes, &base(Some(9))), Root::Missing);
        // Only deposits (external notes) can root it.
        let internal = NoteFacts {
            external: false,
            ..deposit(200, 1_000_000, 2, None)
        };
        assert_eq!(lineage_root(&[internal], &base(Some(2))), Root::Missing);
        assert_eq!(lineage_root(&[], &base(None)), Root::None);
    }

    // ----- P5 under I30: what may be refunded off-chain -----

    const RULES: DepositRules = DepositRules {
        amount: 1_000_000,
        expiry_height: 289,
        confirmations: 10,
    };
    const WRONG_AMOUNT: Option<Classification> =
        Some(Classification::NonConforming(classify::NonConformingReason::WrongAmount));
    const LATE: Option<Classification> = Some(Classification::NonConforming(classify::NonConformingReason::Late));

    /// Under v1 only a deposit Activate mode cannot prove is refunded off-chain: another amount, a
    /// late payment (twice the deposit depth first), a transaction with other pools, another
    /// address of the vault. A second payment of the vault's amount lands as a Refund claim.
    #[test]
    fn under_v1_only_what_activate_mode_cannot_prove_is_refunded_off_chain() {
        let v1 = Some(Landing::ActivateOnly { largest_tier: u64::MAX });
        assert!(refundable_offchain(&deposit(200, 1_234_567, 1, WRONG_AMOUNT), v1, &RULES, 209));
        assert!(!refundable_offchain(&deposit(200, 1_000_000, 1, SECOND), v1, &RULES, 1_000), "provable");
        let late = deposit(300, 1_000_000, 1, LATE);
        assert!(!refundable_offchain(&late, v1, &RULES, 318), "19 < 2N = 20");
        assert!(refundable_offchain(&late, v1, &RULES, 319), "20 = 2N");
        let crossing = NoteFacts {
            ironwood_only: false,
            ..deposit(200, 1_000_000, 1, SECOND)
        };
        assert!(refundable_offchain(&crossing, v1, &RULES, 1_000));
        let elsewhere = NoteFacts {
            to_vault_receiver: false,
            ..deposit(200, 1_000_000, 1, SECOND)
        };
        assert!(refundable_offchain(&elsewhere, v1, &RULES, 1_000));
    }

    /// Under v2 a guest lands every note to the published receiver from the Intake's floor on:
    /// only another address of the vault, or a note below the floor, is refunded off-chain.
    /// Unknown deployments, undecided notes, P1's deposit and change refund nothing off-chain.
    #[test]
    fn under_v2_almost_everything_lands_as_a_refund_claim() {
        let v2 = Some(Landing::WithRefunds {
            min_refund: 20_000,
            largest_tier: u64::MAX,
        });
        for n in [
            deposit(200, 1_234_567, 1, WRONG_AMOUNT),
            deposit(300, 1_000_000, 1, LATE),
        ] {
            assert!(!refundable_offchain(&n, v2, &RULES, 1_000));
        }
        assert!(refundable_offchain(&deposit(200, 19_999, 1, WRONG_AMOUNT), v2, &RULES, 1_000));
        // A note that cannot pay its own refund's fee (10,000 zat) stays in the vault.
        assert!(!refundable_offchain(&deposit(200, 10_000, 1, WRONG_AMOUNT), v2, &RULES, 1_000));
        let elsewhere = NoteFacts {
            to_vault_receiver: false,
            ..deposit(200, 1_234_567, 1, WRONG_AMOUNT)
        };
        assert!(refundable_offchain(&elsewhere, v2, &RULES, 1_000));
        let other = deposit(200, 1_234_567, 1, WRONG_AMOUNT);
        assert!(!refundable_offchain(&other, None, &RULES, 1_000), "the deployment is unknown");
        // R-15: above the Intake's largest work tier no proof lands: refunded off-chain.
        let capped = Some(Landing::WithRefunds {
            min_refund: 20_000,
            largest_tier: 1_000_000,
        });
        assert!(refundable_offchain(&other, capped, &RULES, 1_000), "above the largest tier");
        assert!(!refundable_offchain(&deposit(200, 999_999, 1, WRONG_AMOUNT), capped, &RULES, 1_000));
        let v1 = Some(Landing::ActivateOnly { largest_tier: u64::MAX });
        assert!(!refundable_offchain(&deposit(200, 1_234_567, 1, None), v1, &RULES, 1_000), "undecided");
        assert!(!refundable_offchain(&deposit(200, 1_000_000, 1, CONFORMING), v1, &RULES, 1_000));
        let change = NoteFacts {
            external: false,
            ..deposit(200, 1_234_567, 1, None)
        };
        assert!(!refundable_offchain(&change, v1, &RULES, 1_000));
    }

    /// M-9: a crash between saving a vault and saving the cursor re-applies the chunk; the
    /// activation must not reset `outstanding` while the claim is deduplicated (I10 would fail
    /// for good).
    #[test]
    fn reapplied_events_change_nothing() {
        let (mut store, dir) = store();
        let events = [activation(10), liquidation(20, 7, 700_000)];
        assert_eq!(apply_all(&mut store, &events), 2);
        assert_eq!(store.vaults[&VAULT].outstanding, 300_000);
        // The chunk is fetched and applied again (the cursor was not saved), also after a
        // restart: the per-vault position is on disk.
        assert_eq!(apply_all(&mut store, &events), 0);
        let mut store = Store::open(&dir).unwrap();
        assert_eq!(store.vaults[&VAULT].last_event, Some((20, 0)));
        assert_eq!(apply_all(&mut store, &events), 0);
        assert_eq!(store.vaults[&VAULT].outstanding, 300_000);
        // Even an activation applied again despite the position (e.g. a record written before
        // positions were kept) does not reset `outstanding`.
        store.vaults.get_mut(&VAULT).unwrap().last_event = None;
        apply(&mut store, &activation(10)).unwrap();
        assert_eq!(store.vaults[&VAULT].outstanding, 300_000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// M-9: an event that cannot be applied is skipped; later events still are.
    #[test]
    fn a_failing_event_does_not_halt_ingestion() {
        let (mut store, dir) = store();
        let events = [
            activation(10),
            // Burns more than the vault holds: cannot be applied.
            liquidation(20, 7, 2_000_000),
            liquidation(30, 8, 400_000),
        ];
        assert_eq!(apply_all(&mut store, &events), 2);
        let v = &store.vaults[&VAULT];
        assert_eq!(v.outstanding, 600_000);
        assert_eq!(v.claims.len(), 1);
        assert_eq!(v.last_event, Some((30, 0)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// T-06 and T-02: the fee is minted less (it lowers `outstanding`), a refund does not touch
    /// it (its own note backs it), and the reservation events are not the signers'.
    #[test]
    fn fees_lower_the_outstanding_and_refunds_do_not() {
        let (mut store, dir) = store();
        let refund = Claim {
            refund_tag: Some([0xee; 32]),
            commitment: [0; 32],
            ..new_claim(9, ClaimKind::Refund, 250_000, 5)
        };
        let events = [
            event(5, 0, RegistryEvent::Claims(vec![refund])),
            event(
                8,
                0,
                RegistryEvent::Reserved {
                    account: ACCOUNT,
                    amount: 1_000_000,
                    deposit_tag: B256::from(ACTIVATED_TAG),
                },
            ),
            activation(10),
            event(10, 1, RegistryEvent::Claims(vec![new_claim(4, ClaimKind::Fee, 25_000, 10)])),
        ];
        assert_eq!(apply_all(&mut store, &events), 3, "the reservation is not applied");
        let v = &store.vaults[&VAULT];
        assert_eq!(v.outstanding, 975_000);
        assert_eq!(refund_tags(&v.claims), vec![[0xee; 32]]);
        // Before its destination, the refund is not payable; then it is, and its liveness clock
        // starts (T-05).
        assert!(!v.claims[0].payable());
        assert_eq!(apply_all(&mut store, &[change(40, 9, ClaimChange::RefundDestination(B256::repeat_byte(3)))]), 1);
        let r = &store.vaults[&VAULT].claims[0];
        assert!(r.payable());
        assert_eq!((r.commitment, r.created_at), ([3; 32], 40));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// T-02 and T-05: an abandoned refund is written off and revived by its next destination; a
    /// bond-paid claim loses its destination until three signers give its Reimburse destination.
    #[test]
    fn claim_changes_follow_the_registry() {
        let (mut store, dir) = store();
        let refund = Claim {
            refund_tag: Some([0xee; 32]),
            commitment: [0; 32],
            ..new_claim(9, ClaimKind::Refund, 250_000, 5)
        };
        apply_all(
            &mut store,
            &[
                event(5, 0, RegistryEvent::Claims(vec![refund])),
                activation(10),
                liquidation(20, 7, 300_000),
            ],
        );
        let state =
            |store: &Store, id: u8| store.vaults[&VAULT].claims.iter().find(|c| c.id == [id; 32]).unwrap().clone();
        apply_all(&mut store, &[change(30, 9, ClaimChange::WrittenOff)]);
        assert_eq!(state(&store, 9).state, ClaimState::WrittenOff);
        apply_all(&mut store, &[change(40, 9, ClaimChange::RefundDestination(B256::repeat_byte(3)))]);
        assert_eq!(state(&store, 9).state, ClaimState::Unpaid);
        apply_all(&mut store, &[change(50, 7, ClaimChange::BondPaid)]);
        let bond_paid = state(&store, 7);
        assert!(bond_paid.bond_paid && bond_paid.commitment == [0; 32] && !bond_paid.payable());
        apply_all(&mut store, &[change(60, 7, ClaimChange::ReimburseDestination(B256::repeat_byte(5)))]);
        let reimbursed = state(&store, 7);
        assert!(reimbursed.bond_paid && reimbursed.payable());
        assert_eq!(reimbursed.commitment, [5; 32]);
        // A change of a claim this node does not know is skipped.
        assert_eq!(apply_all(&mut store, &[change(70, 1, ClaimChange::Compensated)]), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// T-05 (a): a disputed confirmation is unpaid again for a node that did not see the payout
    /// mined, and pending once reopened; a node that confirmed the payout itself keeps it paid
    /// (paying it again would pay it twice). An upheld dispute is paid.
    #[test]
    fn disputes_follow_what_this_node_saw() {
        let (mut store, dir) = store();
        apply_all(&mut store, &[activation(10), liquidation(20, 7, 300_000), liquidation(21, 8, 200_000)]);
        let confirmed = event(
            30,
            0,
            RegistryEvent::PayoutConfirmed {
                payout_ref: B256::repeat_byte(0xaa),
                claims: vec![B256::repeat_byte(7), B256::repeat_byte(8)],
            },
        );
        apply_all(&mut store, &[confirmed]);
        // This node confirmed the payout of claim 8 itself.
        let mut v = store.vaults[&VAULT].clone();
        v.confirmed.push(ConfirmedPayout {
            claim_ids: vec![[8; 32]],
            txid: [1; 32],
        });
        store.save_vault(v).unwrap();
        let state =
            |store: &Store, id: u8| store.vaults[&VAULT].claims.iter().find(|c| c.id == [id; 32]).unwrap().state;
        apply_all(
            &mut store,
            &[
                change(40, 7, ClaimChange::Disputed { due_at: 9_000 }),
                change(41, 8, ClaimChange::Disputed { due_at: 9_000 }),
            ],
        );
        assert_eq!((state(&store, 7), state(&store, 8)), (ClaimState::Disputed, ClaimState::Paid));
        apply_all(
            &mut store,
            &[
                change(50, 7, ClaimChange::Reopened),
                change(51, 8, ClaimChange::Reopened),
            ],
        );
        assert_eq!((state(&store, 7), state(&store, 8)), (ClaimState::Unpaid, ClaimState::Paid));
        apply_all(&mut store, &[change(60, 7, ClaimChange::Disputed { due_at: 9_500 })]);
        apply_all(
            &mut store,
            &[change(
                61,
                7,
                ClaimChange::Upheld {
                    payout_ref: B256::repeat_byte(2),
                },
            )],
        );
        assert_eq!(state(&store, 7), ClaimState::Paid);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// v4: a confirmation records when the dispute window opens; a proof (routine or after a
    /// dispute) makes the claim final; a reopening is remembered (R-1: no signed confirmation for
    /// it any more); dormant refunds and overridden change tags change no claim.
    #[test]
    fn proofs_finalize_claims_and_reopenings_are_remembered() {
        let (mut store, dir) = store();
        apply_all(&mut store, &[activation(10), liquidation(20, 7, 300_000), liquidation(21, 8, 200_000)]);
        let confirmed = event(
            30,
            0,
            RegistryEvent::PayoutConfirmed {
                payout_ref: B256::repeat_byte(0xaa),
                claims: vec![B256::repeat_byte(7)],
            },
        );
        apply_all(&mut store, &[confirmed]);
        let claim =
            |store: &Store, id: u8| store.vaults[&VAULT].claims.iter().find(|c| c.id == [id; 32]).unwrap().clone();
        let c7 = claim(&store, 7);
        assert_eq!((c7.state, c7.confirmed_at, c7.proven), (ClaimState::Paid, 30, false));
        // Proven: final. Claim 8 is proven straight from pending (its confirmation never came).
        let proven = |block: u64, id: u8| {
            change(
                block,
                id,
                ClaimChange::Proven {
                    payout_ref: B256::repeat_byte(0xaa),
                },
            )
        };
        apply_all(&mut store, &[proven(40, 7), proven(41, 8)]);
        assert!(claim(&store, 7).is_final() && claim(&store, 8).is_final());
        // R-3: a confirmation nobody disputed in time is final too: O-4's rule 3 never takes it back.
        apply_all(&mut store, &[liquidation(42, 10, 50_000)]);
        let confirmed = event(
            43,
            0,
            RegistryEvent::PayoutConfirmed {
                payout_ref: B256::repeat_byte(0xab),
                claims: vec![B256::repeat_byte(10)],
            },
        );
        apply_all(&mut store, &[confirmed, change(44, 10, ClaimChange::Expired)]);
        assert!(claim(&store, 10).is_final());
        let mut claims = store.vaults[&VAULT].claims.clone();
        assert!(ledger::revert_unlanded(&mut claims, &[[10; 32]]).is_empty());
        // A reopened claim is remembered; a dormant refund or an overridden tag changes nothing.
        apply_all(&mut store, &[liquidation(50, 9, 100_000)]);
        apply_all(
            &mut store,
            &[
                change(60, 9, ClaimChange::Disputed { due_at: 1 }),
                change(61, 9, ClaimChange::Reopened),
            ],
        );
        assert_eq!(store.vaults[&VAULT].reopened, vec![[9; 32]]);
        let before = store.vaults[&VAULT].claims.clone();
        let overridden = event(
            70,
            0,
            RegistryEvent::ChangeTagOverridden {
                tag: B256::repeat_byte(0x7a),
            },
        );
        assert_eq!(apply_all(&mut store, &[overridden]), 1);
        apply_all(&mut store, &[change(71, 9, ClaimChange::Dormant)]);
        assert_eq!(store.vaults[&VAULT].claims, before);
        // The coordinator's events are not the signers'.
        let pending = event(
            80,
            0,
            RegistryEvent::RefundPending {
                deposit_tag: B256::repeat_byte(0x7b),
                reason: 0,
                ready_height: 1,
                ready_time: 1,
            },
        );
        assert_eq!(apply_all(&mut store, &[pending]), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decisions_merge_but_never_change() {
        let p = |h| NotePosition {
            height: h,
            tx_index: 0,
            action_index: 0,
        };
        let mut current = vec![(p(1), Classification::Conforming)];
        assert!(!merge_decisions(&mut current, &[(p(1), Classification::Conforming)]).unwrap());
        assert!(
            merge_decisions(
                &mut current,
                &[(p(2), Classification::NonConforming(classify::NonConformingReason::SecondPayment))]
            )
            .unwrap()
        );
        assert_eq!(current.len(), 2);
        assert!(merge_decisions(&mut current, &[(p(1), Classification::Pending)]).is_err());
    }
}
