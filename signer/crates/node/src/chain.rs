//! A node's own view of both chains.
//!
//! Zcash: every vault is scanned from its birthday through this node's light wallet server
//! connection; deposit transactions are checked on the full transaction (P1, D5).
//! Base: Registry events are applied up to the `safe` head only.

use std::collections::HashMap;

use alloy::{primitives::B256, providers::ProviderBuilder};
use anyhow::{Context, Result, ensure};
use orchard::keys::{FullViewingKey, Scope};
use zcash_protocol::{TxId, consensus::Network};
use zecured_base::events::{self, RegistryEvent, VaultEvent};
use zecured_policy::{
    classify::{self, Classification, DepositNote, DepositRules},
    derive::note_id,
    ledger::ClaimState,
};
use zecured_zcash_ops::{
    keys::VaultSecret,
    lwd::{self, LwdClient},
    scan::{ReceivedNote, ScanState},
    tx::{self, TxShape},
};

use crate::store::{Store, VaultRecord};

/// The vault's notes, sorted into the two lineages (see `zecured_policy::ledger`).
pub struct VaultNotes {
    pub tip: u32,
    /// Unspent conforming-lineage notes: the conforming deposit and change notes.
    pub lineage: Vec<ReceivedNote>,
    /// Unspent non-conforming deposits, with their note id.
    pub non_conforming: Vec<([u8; 32], ReceivedNote)>,
    /// The conforming deposit, if decided (spent or not).
    pub conforming: Option<ReceivedNote>,
}

impl VaultNotes {
    pub fn lineage_value(&self) -> u64 {
        self.lineage.iter().map(|n| n.value()).sum()
    }
}

/// Zcash access for one node.
pub struct ZcashView {
    url: String,
    client: Option<LwdClient>,
    scans: HashMap<B256, ScanState>,
    /// Ironwood-only flag of deposit transactions, by txid.
    shapes: HashMap<[u8; 32], bool>,
}

pub fn vault_fvk(v: &VaultRecord) -> Result<FullViewingKey> {
    VaultSecret::from_bytes(v.sk_bytes()?).full_viewing_key(&v.ak_bytes()?)
}

impl ZcashView {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
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

    /// Scans the vault up to the current tip and sorts its notes (node store variant).
    pub async fn vault_notes(&mut self, store: &mut Store, vault_id: &B256, confirmations: u32) -> Result<VaultNotes> {
        let mut v = store.vaults.get(vault_id).context("unknown vault")?.clone();
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations,
        };
        let fvk = vault_fvk(&v)?;
        let notes = self.notes(vault_id, &fvk, v.birthday, &rules, &mut v.decisions).await?;
        store.save_vault(v)?;
        Ok(notes)
    }

    /// Scans a vault up to the current tip, extends its persisted P1 `decisions` and sorts
    /// its notes into the two lineages.
    pub async fn notes(
        &mut self,
        vault_id: &B256,
        fvk: &FullViewingKey,
        birthday: u32,
        rules: &DepositRules,
        decisions: &mut Vec<(classify::NotePosition, Classification)>,
    ) -> Result<VaultNotes> {
        let tip = self.tip().await?;
        // tonic clients are cheap handles over one connection; clone to scan while `self`
        // stays borrowable.
        let mut client = self.client().await?.clone();
        if !self.scans.contains_key(vault_id) {
            let state = ScanState::from_birthday(&mut client, fvk.clone(), birthday).await?;
            self.scans.insert(*vault_id, state);
        }
        let scanned = self
            .scans
            .get_mut(vault_id)
            .expect("inserted")
            .scan_to(&mut client, tip)
            .await;
        if let Err(e) = scanned {
            // Reorg (or a broken stream): start over from the birthday; vault scans are short.
            tracing::warn!("rescanning vault after: {e:#}");
            let mut state = ScanState::from_birthday(&mut client, fvk.clone(), birthday).await?;
            state.scan_to(&mut client, tip).await?;
            self.scans.insert(*vault_id, state);
        }

        // P1: classify external notes in chain order; persist new decisions.
        let externals: Vec<ReceivedNote> = self.scans[vault_id]
            .notes
            .iter()
            .filter(|n| n.scope == Scope::External)
            .cloned()
            .collect();
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
        let decision = |n: &ReceivedNote| {
            decisions
                .iter()
                .find(|(p, _)| *p == to_policy_position(&n.position))
                .map(|(_, c)| *c)
        };
        let conforming = scan
            .notes
            .iter()
            .find(|n| n.scope == Scope::External && decision(n) == Some(Classification::Conforming))
            .cloned();
        let lineage = scan
            .unspent()
            .into_iter()
            .filter(|n| {
                n.scope == Scope::Internal || (n.scope == Scope::External && decision(n) == Some(Classification::Conforming))
            })
            .cloned()
            .collect();
        let non_conforming = scan
            .unspent()
            .into_iter()
            .filter(|n| n.scope == Scope::External && matches!(decision(n), Some(Classification::NonConforming(_))))
            .map(|n| (note_id(&vault_id.0, &to_policy_position(&n.position)), n.clone()))
            .collect();
        Ok(VaultNotes {
            tip,
            lineage,
            non_conforming,
            conforming,
        })
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

fn to_policy_position(p: &zecured_zcash_ops::scan::NotePosition) -> classify::NotePosition {
    classify::NotePosition {
        height: p.height,
        tx_index: p.tx_index,
        action_index: p.action_index,
    }
}

/// Applies Registry events up to Base's safe head. Returns the number of events applied.
pub async fn ingest_base(store: &mut Store, rpc_url: &str, registry: alloy::primitives::Address, from_block: u64) -> Result<usize> {
    let provider = ProviderBuilder::new().connect_http(rpc_url.parse().context("Base RPC URL")?);
    let safe = events::safe_head(&provider).await?;
    let start = store.base_cursor.max(from_block.saturating_sub(1)) + 1;
    if start > safe {
        return Ok(0);
    }
    // Keep each getLogs range small enough for public RPCs.
    const MAX_RANGE: u64 = 2_000;
    let mut applied = 0;
    let mut from = start;
    while from <= safe {
        let to = (from + MAX_RANGE - 1).min(safe);
        for ev in events::fetch(&provider, registry, from, to).await? {
            if apply(store, ev)? {
                applied += 1;
            }
        }
        store.set_base_cursor(to)?;
        from = to + 1;
    }
    Ok(applied)
}

/// Applies one event to the vault it concerns (events of other vaults are ignored).
fn apply(store: &mut Store, ev: VaultEvent) -> Result<bool> {
    let Some(mut v) = store.vaults.get(&ev.vault_id).cloned() else {
        return Ok(false);
    };
    match ev.event {
        RegistryEvent::VaultActivated { account, amount } => {
            ensure!(account == v.account && amount == v.amount, "activation does not match the vault");
            v.activated = true;
            v.outstanding = amount;
        }
        RegistryEvent::Claims(claims) => {
            for c in claims {
                if v.claims.iter().any(|x| x.id == c.id) {
                    continue;
                }
                // Every claim burns exactly its amount (a full release burns all of it).
                v.outstanding = v.outstanding.checked_sub(c.amount).context("claim exceeds outstanding")?;
                v.claims.push(c);
            }
        }
        RegistryEvent::PayoutConfirmed(ids) => {
            for c in v.claims.iter_mut().filter(|c| ids.iter().any(|i| i.0 == c.id)) {
                c.state = ClaimState::Paid;
            }
        }
    }
    store.save_vault(v)?;
    Ok(true)
}
