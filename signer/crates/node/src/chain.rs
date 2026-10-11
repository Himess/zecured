//! A node's own view of both chains.
//!
//! Zcash: every vault is scanned from its birthday through this node's light wallet server
//! connection; deposit transactions are checked on the full transaction (P1, D5), and before a
//! deposit is attested any configured witness servers must agree on it (C-1).
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
use zcash_protocol::{TxId, consensus::Network};
use zecured_base::{
    events::{RegistryEvent, VaultEvent},
    verify::{self, ProviderReader, Verdict},
};
use zecured_policy::{
    classify::{self, Classification, DepositNote, DepositRules, NotePosition},
    derive::{deposit_tag, note_id},
    ledger::{self, ClaimState, VaultNote},
};
use zecured_zcash_ops::{
    keys::VaultSecret,
    lwd::{self, LwdClient},
    scan::{ReceivedNote, ScanState},
    tx::{self, TxShape},
    witness,
};

use crate::store::{Store, VaultRecord};

/// Time allowed to connect to the Base RPC (M-7).
pub const BASE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time allowed for one Base RPC request (M-7).
pub const BASE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What Base says about a vault, for sorting its notes (D23, K2-a).
#[derive(Clone, Copy, Debug)]
pub struct BaseView<'a> {
    /// The vault's viewing key string: deposit tags are keyed by it (D24).
    pub ufvk: &'a str,
    /// `VaultCancelled` seen: never activated, never will be.
    pub cancelled: bool,
    /// The deposit tag of `VaultActivated`, once seen: the note that backs the vault.
    pub activated_tag: Option<[u8; 32]>,
}

/// One scanned note, as far as sorting the vault's notes is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteFacts {
    pub height: u32,
    pub value: u64,
    /// Received at the vault's published address (a deposit), not its internal one.
    pub external: bool,
    /// D24 deposit tag of the note.
    pub tag: [u8; 32],
    /// P1 decision, once made.
    pub decision: Option<Classification>,
}

/// The note that roots a vault's lineage (K2-a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    /// This scanned note.
    Note(usize),
    /// None: no conforming deposit decided yet, or the vault was cancelled (nothing minted).
    None,
    /// Base activated the vault with a note this node cannot see: its Zcash data is wrong or
    /// incomplete, and no refund is safe until someone looks into it.
    Missing,
}

/// K2-a: the root of the vault's lineage. Once Base activated the vault, the note it activated
/// it with (named by its deposit tag), whatever P1 decided: a deposit proof needs no signer, and
/// P1 rests on the light wallet server's answers. Before that, P1's conforming deposit.
pub fn lineage_root(notes: &[NoteFacts], base: &BaseView<'_>) -> Root {
    if base.cancelled {
        return Root::None;
    }
    let found = match base.activated_tag {
        Some(tag) => match notes.iter().position(|n| n.external && n.tag == tag) {
            Some(i) => Some(i),
            None => return Root::Missing,
        },
        None => notes.iter().position(|n| n.external && n.decision == Some(Classification::Conforming)),
    };
    found.map_or(Root::None, Root::Note)
}

/// P5 / D23 / K2: whether an unspent deposit outside the lineage may be refunded.
pub fn refundable(n: &NoteFacts, base: &BaseView<'_>, root: Root, rules: &DepositRules, tip: u32) -> bool {
    if root == Root::Missing || !n.external {
        return false;
    }
    let classified = match n.decision {
        Some(Classification::NonConforming(_)) => true,
        // P1's conforming deposit, once the vault is cancelled (D23) or activated with
        // another note (K2-a). Before either, a proof may still activate the vault with it.
        Some(Classification::Conforming) => base.cancelled || base.activated_tag.is_some(),
        // Not classified yet (too few confirmations).
        _ => false,
    };
    // K2: a note of the vault's amount could back a mint. Refund it only once it is buried
    // twice as deep as a deposit must be, so that no reorg brings back a chain where it was
    // the vault's first deposit.
    classified
        && (n.value != rules.amount || classify::confirmations(n.height, tip) >= rules.confirmations.saturating_mul(2))
}

/// The vault's notes, sorted into the two lineages (see `zecured_policy::ledger`).
pub struct VaultNotes {
    pub tip: u32,
    /// Unspent conforming-lineage notes: the conforming deposit and the vault's own change.
    pub lineage: Vec<ReceivedNote>,
    /// Unspent non-conforming deposits, with their note id.
    pub non_conforming: Vec<([u8; 32], ReceivedNote)>,
    /// The conforming deposit, if decided (spent or not).
    pub conforming: Option<ReceivedNote>,
    /// Unspent notes in neither lineage, ignored for I10 and fees (H-2): zero-value notes,
    /// notes sent to the internal address by anyone but the vault, and outputs of a vault
    /// transaction that are not its planned change (see [`Self::retain_planned_change`]).
    pub ignored: Vec<ReceivedNote>,
    /// Nullifiers of every unspent note of the vault, whatever its lineage.
    pub unspent: Vec<[u8; 32]>,
}

impl VaultNotes {
    pub fn lineage_value(&self) -> u64 {
        self.lineage.iter().map(|n| n.value()).sum()
    }

    /// H-2: keeps in the lineage only the conforming deposit and change worth exactly the
    /// vault's book value (`ledger::book_value(outstanding, claims)`): the planned change of
    /// the payout that created it, once that payout's claims are marked paid. Anything else
    /// moves to [`Self::ignored`].
    pub fn retain_planned_change(&mut self, book_value: u64) {
        let dropped = ledger::retain_planned_change(
            &mut self.lineage,
            book_value,
            |n| n.scope == Scope::Internal,
            ReceivedNote::value,
        );
        self.ignored.extend(dropped);
    }
}

/// Zcash access for one node.
pub struct ZcashView {
    url: String,
    /// Independent light wallet servers that must agree before a deposit is attested (C-1).
    witnesses: Vec<String>,
    client: Option<LwdClient>,
    scans: HashMap<B256, ScanState>,
    /// Ironwood-only flag of deposit transactions, by txid.
    shapes: HashMap<[u8; 32], bool>,
}

pub fn vault_fvk(v: &VaultRecord) -> Result<FullViewingKey> {
    VaultSecret::from_bytes(v.sk_bytes()?).full_viewing_key(&v.ak_bytes()?)
}

/// Whether `receiver` is one of the vault's own addresses (any diversifier, either scope). A
/// payout or refund to it would land back in the vault, outside its books (H-2).
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
    pub fn new(url: &str) -> Self {
        Self::with_witnesses(url, vec![])
    }

    /// A view whose deposit attestations also need every `witnesses` server to agree (C-1).
    pub fn with_witnesses(url: &str, witnesses: Vec<String>) -> Self {
        Self {
            url: url.to_owned(),
            witnesses,
            client: None,
            scans: HashMap::new(),
            shapes: HashMap::new(),
        }
    }

    async fn client(&mut self) -> Result<&mut LwdClient> {
        if self.client.is_none() {
            self.client = Some(lwd::connect(&self.url).await?);
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
    pub async fn vault_notes(&mut self, v: &mut VaultRecord, confirmations: u32) -> Result<VaultNotes> {
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations,
        };
        let fvk = vault_fvk(v)?;
        let ufvk = v.ufvk.clone();
        let base = BaseView {
            ufvk: &ufvk,
            cancelled: v.cancelled,
            activated_tag: v.activated_tag,
        };
        self.notes(&v.vault_id, &fvk, v.birthday, &rules, &mut v.decisions, base).await
    }

    /// Scans a vault up to the current tip, extends its persisted P1 `decisions` and sorts
    /// its notes into the two lineages.
    ///
    /// The lineage returned here is the conforming deposit and every note the vault's own
    /// transactions returned to its internal address (H-2, [`ledger::lineage_members`]);
    /// [`VaultNotes::retain_planned_change`] then keeps only the planned change.
    ///
    /// A cancelled vault (D23) has no lineage: it was never activated, so nothing was minted
    /// against its conforming deposit, which is refundable like a non-conforming one. An
    /// activated vault's lineage starts at the note Base activated it with ([`lineage_root`]).
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
        let mut deposits = Vec::with_capacity(externals.len());
        for n in &externals {
            let ironwood_only = self.ironwood_only(&n.txid).await?;
            deposits.push(DepositNote {
                position: to_policy_position(&n.position),
                value: n.value(),
                ironwood_only,
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

        // H-2: the lineage follows the vault's own spends from its root (K2-a).
        let mut ordered: Vec<&ReceivedNote> = scan.notes.iter().collect();
        ordered.sort_by_key(|n| n.position);
        let facts: Vec<NoteFacts> = ordered
            .iter()
            .map(|n| NoteFacts {
                height: n.position.height,
                value: n.value(),
                external: n.scope == Scope::External,
                tag: deposit_tag(base.ufvk, &ExtractedNoteCommitment::from(n.note.commitment()).to_bytes()),
                decision: decision(n),
            })
            .collect();
        let root = lineage_root(&facts, &base);
        if root == Root::Missing {
            // No heights or note data: only the vault id, which is public on Base.
            tracing::error!(
                vault = %vault_id,
                "Base activated this vault with a note this node cannot see: refunds halted (K2-a)"
            );
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
            non_conforming: vec![],
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
            } else if n.scope == Scope::External {
                if refundable(f, &base, root, rules, tip) {
                    notes.non_conforming.push((note_id(&vault_id.0, &to_policy_position(&n.position)), n.clone()));
                }
                // Otherwise not refundable yet: not classified, not deep enough, or halted.
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
        let (t, _) = tx::fetch(self.client().await?, &Network::TestNetwork, txid).await?;
        let flag = TxShape::of(&t).is_ironwood_only();
        self.shapes.insert(key, flag);
        Ok(flag)
    }

    /// C-1: before a deposit is attested, every witness server must agree with ours on the
    /// block at the deposit's height, list the deposit transaction in it, serve the same
    /// transaction bytes and have a tip within a few blocks of ours; those bytes must create
    /// the note we scanned. Without witnesses this is a no-op (one public NU7 server today).
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
            &Network::TestNetwork,
            &note.txid,
            &primary.tx,
            height,
            note.position.action_index,
            &cmx,
        )?;
        for (i, url) in self.witnesses.iter().enumerate() {
            let name = format!("witness light wallet server #{}", i + 1);
            let view = async {
                let mut client = lwd::connect(url).await?;
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
        let (t, height) = tx::fetch(self.client().await?, &Network::TestNetwork, txid).await?;
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
pub fn base_provider(url: &str) -> Result<impl Provider + Clone> {
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
        RegistryEvent::VaultActivated {
            account,
            amount,
            deposit_tag,
        } => {
            ensure!(*account == v.account && *amount == v.amount, "activation does not match the vault");
            ensure!(!v.cancelled, "activation of a cancelled vault");
            // M-9: activation sets `outstanding` once; later claims only lower it.
            if !v.activated {
                v.activated = true;
                v.outstanding = *amount;
                // K2-a: the note Base minted against heads the lineage and is never refunded.
                v.activated_tag = Some(deposit_tag.0);
            }
        }
        RegistryEvent::Claims(claims) => {
            for c in claims {
                if v.claims.iter().any(|x| x.id == c.id) {
                    continue;
                }
                // Every claim burns exactly its amount (a full release burns all of it).
                v.outstanding = v.outstanding.checked_sub(c.amount).context("claim exceeds outstanding")?;
                v.claims.push(c.clone());
            }
        }
        RegistryEvent::PayoutConfirmed(ids) => {
            for c in v.claims.iter_mut().filter(|c| ids.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
            }
        }
        RegistryEvent::VaultCancelled => {
            ensure!(!v.activated, "cancellation of an activated vault");
            v.cancelled = true;
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
    use zecured_policy::ledger::{Claim, ClaimKind};

    use super::*;

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
            attestation: None,
            activated: false,
            outstanding: 0,
            claims: vec![],
            refunds: vec![],
            used_nonces: vec![],
            inflight: None,
            confirmed: vec![],
            last_event: None,
            cancelled: false,
            activated_tag: None,
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

    fn liquidation(block: u64, id: u8, amount: u64) -> VaultEvent {
        event(
            block,
            0,
            RegistryEvent::Claims(vec![Claim {
                id: [id; 32],
                kind: ClaimKind::Liquidation,
                amount,
                commitment: [id; 32],
                order: (block, 0),
                created_at: block,
                partner: None,
                state: ClaimState::Unpaid,
            }]),
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

    // ----- K2-a: the lineage root and what is refundable -----

    const RULES: DepositRules = DepositRules {
        amount: 1_000_000,
        expiry_height: 388,
        confirmations: 10,
    };

    fn deposit(height: u32, value: u64, tag: u8, decision: Option<Classification>) -> NoteFacts {
        NoteFacts {
            height,
            value,
            external: true,
            tag: [tag; 32],
            decision,
        }
    }

    const SECOND: Option<Classification> =
        Some(Classification::NonConforming(classify::NonConformingReason::SecondPayment));
    const CONFORMING: Option<Classification> = Some(Classification::Conforming);

    fn base(cancelled: bool, activated_tag: Option<u8>) -> BaseView<'static> {
        BaseView {
            ufvk: "",
            cancelled,
            activated_tag: activated_tag.map(|t| [t; 32]),
        }
    }

    #[test]
    fn the_lineage_starts_at_the_note_base_activated_the_vault_with() {
        // P1 decided the first note conforms; the Registry minted against the second (a deposit
        // proof needs no signer, and P1 may have been fed bad data).
        let notes = [deposit(200, 1_000_000, 1, CONFORMING), deposit(210, 1_000_000, 2, SECOND)];
        assert_eq!(lineage_root(&notes, &base(false, None)), Root::Note(0), "before activation: P1");
        assert_eq!(lineage_root(&notes, &base(false, Some(2))), Root::Note(1), "after: the activated note");
        assert_eq!(lineage_root(&notes, &base(false, Some(1))), Root::Note(0));
        // Base activated the vault with a note this node cannot see.
        assert_eq!(lineage_root(&notes, &base(false, Some(9))), Root::Missing);
        // Cancelled: nothing was minted.
        assert_eq!(lineage_root(&notes, &base(true, None)), Root::None);
        // Only deposits (external notes) can root it.
        let internal = NoteFacts {
            external: false,
            ..deposit(200, 1_000_000, 2, None)
        };
        assert_eq!(lineage_root(&[internal], &base(false, Some(2))), Root::Missing);
        assert_eq!(lineage_root(&[], &base(false, None)), Root::None);
    }

    #[test]
    fn the_note_that_backs_the_vault_is_never_refundable() {
        let tip = 300;
        let notes = [deposit(200, 1_000_000, 1, CONFORMING), deposit(210, 1_000_000, 2, SECOND)];
        // Activated with the second note: it roots the lineage (never offered for refund), and
        // P1's "conforming" first note is now an extra payment, refundable once deep enough.
        let b = base(false, Some(2));
        let root = lineage_root(&notes, &b);
        assert_eq!(root, Root::Note(1));
        assert!(refundable(&notes[0], &b, root, &RULES, tip));
        // Before activation, P1's conforming note is not refundable (a proof may still use it),
        // and neither is a second payment of the vault's amount that is not yet 2N deep.
        let none = base(false, None);
        let root = lineage_root(&notes, &none);
        assert!(!refundable(&notes[0], &none, root, &RULES, tip));
        assert!(refundable(&notes[1], &none, root, &RULES, tip), "91 confirmations >= 2N");
        assert!(!refundable(&notes[1], &none, root, &RULES, 210 + 18), "19 < 2N = 20");
        assert!(refundable(&notes[1], &none, root, &RULES, 210 + 19), "20 = 2N");
    }

    #[test]
    fn refunds_halt_while_the_activated_note_is_missing() {
        let notes = [deposit(200, 1_000_000, 1, CONFORMING), deposit(210, 300_000, 2, SECOND)];
        let b = base(false, Some(9));
        let root = lineage_root(&notes, &b);
        assert_eq!(root, Root::Missing);
        for n in &notes {
            assert!(!refundable(n, &b, root, &RULES, 1_000));
        }
    }

    #[test]
    fn other_amounts_refund_at_the_deposit_depth_and_unclassified_notes_never() {
        let none = base(false, None);
        let wrong_amount = deposit(290, 300_000, 3, SECOND);
        let root = lineage_root(&[wrong_amount], &none);
        assert!(refundable(&wrong_amount, &none, root, &RULES, 300), "cannot back a mint: no 2N wait");
        let pending = deposit(295, 300_000, 3, Some(Classification::Pending));
        assert!(!refundable(&pending, &none, root, &RULES, 1_000));
        let unknown = deposit(295, 300_000, 3, None);
        assert!(!refundable(&unknown, &none, root, &RULES, 1_000));
        // D23: the conforming deposit of a cancelled vault.
        let conforming = deposit(200, 1_000_000, 1, CONFORMING);
        let cancelled = base(true, None);
        assert!(refundable(&conforming, &cancelled, lineage_root(&[conforming], &cancelled), &RULES, 300));
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

    /// D23: a cancellation marks a never-activated vault; one for an activated vault is refused
    /// (the Registry cannot produce it, so it would be a forged or misread event).
    #[test]
    fn cancellations_apply_only_to_vaults_never_activated() {
        let (mut fresh, dir) = store();
        assert_eq!(apply_all(&mut fresh, &[event(10, 0, RegistryEvent::VaultCancelled)]), 1);
        assert!(fresh.vaults[&VAULT].cancelled);
        std::fs::remove_dir_all(&dir).unwrap();

        let (mut active, dir) = store();
        assert_eq!(apply_all(&mut active, &[activation(10), event(20, 0, RegistryEvent::VaultCancelled)]), 1);
        let v = &active.vaults[&VAULT];
        assert!(v.activated && !v.cancelled);
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
