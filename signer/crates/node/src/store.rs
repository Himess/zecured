//! Durable node state: one JSON file per vault plus a global recipient book.
//!
//! Files are replaced atomically (write to a temporary file, then rename). Key material is
//! stored unencrypted on the node's disk (hackathon limitation, see THREAT_MODEL.md); it is
//! never logged.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use alloy::primitives::{Address, B256};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zecured_policy::{
    classify::{Classification, NotePosition},
    ledger::{Claim, Commitment, Recipient},
};

/// Writes `bytes` to `path` atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// A deposit attestation this node signed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttestationRecord {
    pub position: NotePosition,
    pub nullifier: [u8; 32],
    pub deposit_tag: [u8; 32],
}

/// An accepted P5 refund request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundRecord {
    pub note_id: [u8; 32],
    pub recipient: Recipient,
}

/// Everything a node knows about one vault.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultRecord {
    pub vault_id: B256,
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub birthday: u32,
    pub expiry_height: u32,
    /// FROST key package (SECRET).
    pub key_package: serde_json::Value,
    pub public_key_package: serde_json::Value,
    /// ZIP 2005 `sk`, hex (SECRET).
    pub sk: String,
    pub ak: String,
    pub ufvk: String,
    pub address: String,
    /// P1 decisions, once made (never changed).
    #[serde(default)]
    pub decisions: Vec<(NotePosition, Classification)>,
    #[serde(default)]
    pub attestation: Option<AttestationRecord>,
    /// `VaultActivated` seen on Base.
    #[serde(default)]
    pub activated: bool,
    /// Mirror of the Registry's `outstanding`.
    #[serde(default)]
    pub outstanding: u64,
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub refunds: Vec<RefundRecord>,
    /// Owner request nonces already used (P0).
    #[serde(default)]
    pub used_nonces: Vec<u64>,
    /// The payout this node signed and has not confirmed yet (survives restarts).
    #[serde(default)]
    pub inflight: Option<crate::Inflight>,
}

impl VaultRecord {
    pub fn sk_bytes(&self) -> Result<[u8; 32]> {
        hex::decode(&self.sk)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("stored sk is not 32 bytes"))
    }

    pub fn ak_bytes(&self) -> Result<[u8; 32]> {
        hex::decode(&self.ak)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("stored ak is not 32 bytes"))
    }

    /// Marks an owner nonce used; false if it was already used.
    pub fn use_nonce(&mut self, nonce: u64) -> bool {
        if self.used_nonces.contains(&nonce) {
            false
        } else {
            self.used_nonces.push(nonce);
            true
        }
    }
}

/// On-disk store of one node.
pub struct Store {
    dir: PathBuf,
    pub vaults: BTreeMap<B256, VaultRecord>,
    pub recipients: BTreeMap<String, Recipient>,
    /// Last Base block whose Registry events were applied.
    pub base_cursor: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Meta {
    base_cursor: u64,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir.join("vaults"))?;
        let mut vaults = BTreeMap::new();
        for entry in fs::read_dir(dir.join("vaults"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let v: VaultRecord = serde_json::from_slice(&fs::read(&path)?)
                    .with_context(|| format!("parse {}", path.display()))?;
                vaults.insert(v.vault_id, v);
            }
        }
        let recipients = match fs::read(dir.join("recipients.json")) {
            Ok(raw) => serde_json::from_slice(&raw)?,
            Err(_) => BTreeMap::new(),
        };
        let meta: Meta = match fs::read(dir.join("meta.json")) {
            Ok(raw) => serde_json::from_slice(&raw)?,
            Err(_) => Meta::default(),
        };
        Ok(Self {
            dir: dir.to_owned(),
            vaults,
            recipients,
            base_cursor: meta.base_cursor,
        })
    }

    pub fn save_vault(&mut self, v: VaultRecord) -> Result<()> {
        let path = self.dir.join("vaults").join(format!("{}.json", hex::encode(v.vault_id)));
        write_atomic(&path, &serde_json::to_vec_pretty(&v)?)?;
        self.vaults.insert(v.vault_id, v);
        Ok(())
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
        write_atomic(
            &self.dir.join("meta.json"),
            &serde_json::to_vec(&Meta { base_cursor: block })?,
        )
    }

    /// True if `account` or `owner` is already bound to a vault (I4, checked off-chain).
    pub fn is_bound(&self, account: Address, owner: Address) -> bool {
        self.vaults
            .values()
            .any(|v| v.account == account || v.owner == owner)
    }
}
