//! Coordinator state: public vault data only (no key material).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zecured_node::store::write_atomic;
use zecured_policy::{
    classify::{Classification, NotePosition},
    ledger::{Claim, Commitment, Recipient},
};

use crate::error::PublicError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultStatus {
    /// Waiting for the Ironwood deposit.
    AwaitingDeposit,
    /// Conforming deposit found; attestation submitted to Base, or a deposit proof accepted
    /// and waiting for `finalizeActivation` (K3).
    DepositSeen,
    /// zZEC minted into the account's Morpho position.
    Active,
    /// `outstanding` is zero and every claim is paid.
    Closed,
    /// No conforming deposit before the expiry height.
    Expired,
    /// Cancelled on Base before it was ever activated (D23): its conforming deposit is
    /// refundable.
    Cancelled,
}

/// A payout broadcast to Zcash and not yet confirmed on Base.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inflight {
    /// Claims settled (empty for a P5 refund).
    pub claim_ids: Vec<B256>,
    /// Non-conforming note refunded (P5), if any.
    pub refund_note: Option<B256>,
    pub txid: String,
    pub expiry_height: u32,
    /// The signed transaction (hex), recorded before the first broadcast and rebroadcast until
    /// it is mined or expires: a lost broadcast response must never lead to a second payout.
    #[serde(default)]
    pub raw_tx: Option<String>,
    /// Nullifiers of the vault notes this payout spends: a spent input means the payout (or
    /// something else) was mined, so it is never rebuilt blindly (I-5). Empty in records
    /// written before this field existed.
    #[serde(default)]
    pub inputs: Vec<B256>,
    /// L23: the Zcash height from which the payout is broadcast (0: at once). Its commitment is
    /// on Base before it is signed, so broadcasting right away would point at the next block.
    #[serde(default)]
    pub broadcast_from: u32,
    /// Tier 3: the payout's commitment and the signatures it was recorded with. Every pass checks
    /// that Base still has it and submits it again if a reorg dropped it (RT-5): a mined payout
    /// whose commitment is gone would be an uncommitted spend.
    #[serde(default)]
    pub spend_commit: Option<KeptCommitment>,
}

/// A payout commitment as it was recorded on Base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptCommitment {
    pub record: zecured_node::signing::SpendCommitRecord,
    pub sigs: Vec<Bytes>,
}

/// A refund the owner asked for and the signers accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refund {
    pub note_id: B256,
    pub recipient: Recipient,
    pub done: bool,
}

/// One signer node's signature, by node index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSignature {
    pub index: u16,
    pub signature: Bytes,
}

/// A refundable (non-conforming, unspent) note of a vault, as the app shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteView {
    pub note_id: B256,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoordVault {
    pub vault_id: B256,
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub birthday: u32,
    pub expiry_height: u32,
    pub ufvk: String,
    pub address: String,
    pub public_key_package: serde_json::Value,
    pub status: VaultStatus,
    #[serde(default)]
    pub decisions: Vec<(NotePosition, Classification)>,
    #[serde(default)]
    pub attest_tx: Option<B256>,
    #[serde(default)]
    pub outstanding: u64,
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub inflight: Option<Inflight>,
    #[serde(default)]
    pub refunds: Vec<Refund>,
    /// Extra confirmations before attesting (D15), drawn once.
    #[serde(default)]
    pub attest_delay: Option<u32>,
    /// `ViewKeyRequest` nonces already used.
    #[serde(default)]
    pub used_view_nonces: Vec<u64>,
    /// Unspent non-conforming notes as of the last scan. Persisted so that refundable notes
    /// stay visible after a restart and while an expired or closed vault is scanned only now
    /// and then (M-10).
    #[serde(default)]
    pub non_conforming: Vec<NoteView>,
    /// Every signer's `VaultOpened` signature for `address` and the tag key of `ufvk`, ordered by
    /// node index: what the owner checked before paying the vault (C-4), and what a deposit proof
    /// is submitted with. Empty for vaults opened before.
    #[serde(default)]
    pub opened_signatures: Vec<NodeSignature>,
    /// The conforming deposit of a vault that was never activated, once its deposit window is
    /// over: the owner may cancel the vault to get it back (D23, L-8).
    #[serde(default)]
    pub cancellable: Option<NoteView>,
    /// K2-a: the deposit tag of the `VaultActivated` event (the note that backs the vault):
    /// payouts are planned from the same lineage root as the signers'.
    #[serde(default)]
    pub activated_tag: Option<[u8; 32]>,
    /// When the vault was opened (Unix seconds; 0 for vaults opened before H-01).
    #[serde(default)]
    pub opened_at: u64,
}

impl CoordVault {
    /// Nothing is left to do for the vault but to watch for late deposits.
    pub fn is_idle(&self) -> bool {
        matches!(self.status, VaultStatus::Expired | VaultStatus::Closed | VaultStatus::Cancelled)
            && self.inflight.is_none()
            && self.refunds.iter().all(|r| r.done)
    }

    /// A conforming deposit was classified (P1): it is mined and has its confirmations.
    pub fn deposit_seen(&self) -> bool {
        self.decisions.iter().any(|(_, c)| *c == Classification::Conforming)
    }

    /// H-01: whether the vault holds one of the slots of vaults waiting for their deposit: for
    /// `slot_secs` after it opened, or longer once its deposit is seen. Only the count changes: the
    /// vault itself waits for its deposit, is scanned and takes a late one exactly as before.
    pub fn holds_slot(&self, now: u64, slot_secs: u64) -> bool {
        self.status == VaultStatus::AwaitingDeposit
            && (now < self.opened_at.saturating_add(slot_secs) || self.deposit_seen())
    }
}

pub struct Store {
    dir: PathBuf,
    pub vaults: BTreeMap<B256, CoordVault>,
    pub recipients: BTreeMap<String, Recipient>,
    pub base_cursor: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Meta {
    base_cursor: u64,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        // Vault records hold public data only, but salts sit in recipients.json: private directories.
        zecured_node::store::private_dir(dir)?;
        zecured_node::store::private_dir(&dir.join("vaults"))?;
        let mut vaults = BTreeMap::new();
        for entry in fs::read_dir(dir.join("vaults"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let v: CoordVault =
                    serde_json::from_slice(&fs::read(&path)?).with_context(|| format!("parse {}", path.display()))?;
                vaults.insert(v.vault_id, v);
            }
        }
        let recipients = fs::read(dir.join("recipients.json"))
            .ok()
            .map(|r| serde_json::from_slice(&r))
            .transpose()?
            .unwrap_or_default();
        let meta: Meta = fs::read(dir.join("meta.json"))
            .ok()
            .map(|r| serde_json::from_slice(&r))
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            dir: dir.to_owned(),
            vaults,
            recipients,
            base_cursor: meta.base_cursor,
        })
    }

    pub fn save(&mut self, v: CoordVault) -> Result<()> {
        let path = self.dir.join("vaults").join(format!("{}.json", hex::encode(v.vault_id)));
        write_atomic(&path, &serde_json::to_vec_pretty(&v)?)?;
        self.vaults.insert(v.vault_id, v);
        Ok(())
    }

    /// Read-modify-write of one vault record (L-5): `f` sees the current record, so changes
    /// made by others since the caller's last read (refunds, view-key nonces, Base events) are
    /// kept. The record changes in memory only once it is on disk.
    pub fn update<R>(&mut self, id: &B256, f: impl FnOnce(&mut CoordVault) -> R) -> Result<R> {
        let mut v = self.vaults.get(id).cloned().context(PublicError::UnknownVault)?;
        let r = f(&mut v);
        self.save(v)?;
        Ok(r)
    }

    pub fn add_recipient(&mut self, r: Recipient) -> Result<Commitment> {
        let c = r.commitment();
        self.recipients.insert(hex::encode(c), r);
        write_atomic(&self.dir.join("recipients.json"), &serde_json::to_vec(&self.recipients)?)?;
        Ok(c)
    }

    pub fn recipient(&self, c: &Commitment) -> Option<Recipient> {
        self.recipients.get(&hex::encode(c)).copied()
    }

    pub fn set_base_cursor(&mut self, block: u64) -> Result<()> {
        self.base_cursor = block;
        write_atomic(&self.dir.join("meta.json"), &serde_json::to_vec(&Meta { base_cursor: block })?)
    }

    pub fn is_bound(&self, account: Address, owner: Address) -> bool {
        self.vaults.values().any(|v| v.account == account || v.owner == owner)
    }

    /// H-01: vaults holding a slot among those waiting for their deposit ([`CoordVault::holds_slot`]).
    pub fn holding_slots(&self, now: u64, slot_secs: u64) -> usize {
        self.vaults.values().filter(|v| v.holds_slot(now, slot_secs)).count()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A temporary directory removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let mut r = [0u8; 8];
            rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut r);
            let p = std::env::temp_dir().join(format!("zecured-coord-{tag}-{}", hex::encode(r)));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn vault(id: u8) -> CoordVault {
        CoordVault {
            vault_id: B256::repeat_byte(id),
            owner: Address::repeat_byte(id),
            account: Address::repeat_byte(id.wrapping_add(100)),
            amount: 1_000_000,
            birthday: 100,
            expiry_height: 388,
            ufvk: String::new(),
            address: String::new(),
            public_key_package: serde_json::Value::Null,
            status: VaultStatus::AwaitingDeposit,
            decisions: vec![],
            attest_tx: None,
            outstanding: 0,
            claims: vec![],
            inflight: None,
            refunds: vec![],
            attest_delay: None,
            used_view_nonces: vec![],
            non_conforming: vec![],
            opened_signatures: vec![],
            cancellable: None,
            activated_tag: None,
            opened_at: 0,
        }
    }

    fn refund(id: u8) -> Refund {
        Refund {
            note_id: B256::repeat_byte(id),
            recipient: Recipient {
                receiver: [id; 43],
                salt: [id; 32],
            },
            done: false,
        }
    }

    /// L-5: a pass that read the vault before a long scan must not erase a refund (or a
    /// view-key nonce) added meanwhile.
    #[test]
    fn updates_keep_concurrent_changes() {
        let dir = TempDir::new("update");
        let mut store = Store::open(&dir.0).unwrap();
        store.save(vault(1)).unwrap();
        let id = B256::repeat_byte(1);

        let stale = store.vaults[&id].clone(); // a pass starts its scan
        store.update(&id, |v| v.refunds.push(refund(9))).unwrap(); // a refund request lands
        store.update(&id, |v| v.used_view_nonces.push(7)).unwrap(); // a view-key request lands
        let notes = vec![NoteView {
            note_id: B256::repeat_byte(9),
            amount: 20_000,
        }];
        // The pass applies only what its scan produced.
        store.update(&id, |v| v.non_conforming = notes.clone()).unwrap();
        assert!(stale.refunds.is_empty());

        let reopened = Store::open(&dir.0).unwrap();
        let v = &reopened.vaults[&id];
        assert_eq!(v.refunds.len(), 1);
        assert_eq!(v.used_view_nonces, vec![7]);
        assert_eq!(v.non_conforming, notes, "refundable notes survive a restart (M-10)");
        assert!(store.update(&B256::repeat_byte(2), |_| ()).is_err());
    }

    #[test]
    fn old_records_load_with_defaults() {
        let dir = TempDir::new("compat");
        let mut v = serde_json::to_value(vault(3)).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("non_conforming");
        obj.remove("opened_signatures");
        obj.insert(
            "inflight".into(),
            serde_json::json!({ "claim_ids": [], "refund_note": null, "txid": "00", "expiry_height": 5 }),
        );
        fs::create_dir_all(dir.0.join("vaults")).unwrap();
        fs::write(dir.0.join("vaults").join("x.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        let store = Store::open(&dir.0).unwrap();
        let v = &store.vaults[&B256::repeat_byte(3)];
        assert!(v.non_conforming.is_empty());
        assert!(v.opened_signatures.is_empty(), "vaults opened before C-4 have no signatures");
        assert!(v.inflight.as_ref().unwrap().inputs.is_empty());
    }

    #[test]
    fn idle_vaults_and_pending_count() {
        let dir = TempDir::new("idle");
        let mut store = Store::open(&dir.0).unwrap();
        let mut v = vault(1);
        assert!(!v.is_idle());
        v.status = VaultStatus::Expired;
        assert!(v.is_idle());
        v.refunds.push(refund(2));
        assert!(!v.is_idle(), "a pending refund keeps it busy");
        store.save(v).unwrap();
        let mut waiting = vault(2);
        waiting.opened_at = 1_000;
        store.save(waiting).unwrap();
        assert_eq!(store.holding_slots(1_000, 1_800), 1);
    }

    /// H-01: a vault without a deposit releases its slot `slot_secs` after it opened, one with a
    /// conforming deposit keeps it, and any other status holds none. The released vault itself is
    /// unchanged: still waiting for its deposit and scanned on every pass (not idle), so a deposit
    /// that comes later is taken exactly as before.
    #[test]
    fn a_vault_without_a_deposit_releases_its_slot() {
        let position = NotePosition {
            height: 4_486_100,
            tx_index: 1,
            action_index: 0,
        };
        let opened = 1_700_000_000;
        let mut v = vault(1);
        v.opened_at = opened;
        assert!(v.holds_slot(opened, 1_800));
        assert!(v.holds_slot(opened + 1_799, 1_800));
        assert!(!v.holds_slot(opened + 1_800, 1_800), "released after 30 minutes");
        assert_eq!(v.status, VaultStatus::AwaitingDeposit);
        assert!(!v.is_idle(), "still scanned on every pass");
        // A non-conforming payment does not hold the slot; a conforming deposit does.
        let wrong_amount = zecured_policy::classify::NonConformingReason::SecondPayment;
        v.decisions.push((position, Classification::NonConforming(wrong_amount)));
        assert!(!v.holds_slot(opened + 3_600, 1_800));
        let later = NotePosition {
            height: position.height + 1,
            ..position
        };
        v.decisions.push((later, Classification::Conforming));
        assert!(v.deposit_seen() && v.holds_slot(opened + 3_600, 1_800));
        for status in [
            VaultStatus::DepositSeen,
            VaultStatus::Active,
            VaultStatus::Expired,
            VaultStatus::Cancelled,
        ] {
            v.status = status;
            assert!(!v.holds_slot(opened, 1_800), "{status:?}");
        }
        // Vaults opened before H-01 (no opening time) hold no slot unless their deposit is seen.
        assert!(!vault(2).holds_slot(opened, 1_800));
    }
}
