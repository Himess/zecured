//! Coordinator state: public vault data only (no key material).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use alloy::primitives::{Address, B256};
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
    /// Conforming deposit found; attestation submitted to Base.
    DepositSeen,
    /// zZEC minted into the account's Morpho position.
    Active,
    /// `outstanding` is zero and every claim is paid.
    Closed,
    /// No conforming deposit before the expiry height.
    Expired,
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
}

/// A refund the owner asked for and the signers accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refund {
    pub note_id: B256,
    pub recipient: Recipient,
    pub done: bool,
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
}

impl CoordVault {
    /// Nothing is left to do for the vault but to watch for late deposits.
    pub fn is_idle(&self) -> bool {
        matches!(self.status, VaultStatus::Expired | VaultStatus::Closed)
            && self.inflight.is_none()
            && self.refunds.iter().all(|r| r.done)
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

    /// Vaults still waiting for their deposit.
    pub fn pending(&self) -> usize {
        self.vaults.values().filter(|v| v.status == VaultStatus::AwaitingDeposit).count()
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
        obj.insert(
            "inflight".into(),
            serde_json::json!({ "claim_ids": [], "refund_note": null, "txid": "00", "expiry_height": 5 }),
        );
        fs::create_dir_all(dir.0.join("vaults")).unwrap();
        fs::write(dir.0.join("vaults").join("x.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        let store = Store::open(&dir.0).unwrap();
        let v = &store.vaults[&B256::repeat_byte(3)];
        assert!(v.non_conforming.is_empty());
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
        store.save(vault(2)).unwrap();
        assert_eq!(store.pending(), 1);
    }
}
