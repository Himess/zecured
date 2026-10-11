//! Durable node state: one JSON file per vault plus a global recipient book.
//!
//! Files are replaced atomically (write to a temporary file, then rename). Key material is
//! stored unencrypted on the node's disk (hackathon limitation, see THREAT_MODEL.md); it is
//! never logged. Every file is readable by the node's user only (0600, directories 0700);
//! files written before that rule are fixed when the store is opened (L-3).

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

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).with_context(|| format!("restrict {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}

/// Creates `dir` (and its parents) and makes it accessible to the node's user only (0700).
pub fn private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    set_mode(dir, 0o700)
}

/// Makes an existing file readable by the node's user only (0600).
pub fn private_file(path: &Path) -> Result<()> {
    set_mode(path, 0o600)
}

/// Writes `bytes` to `path` atomically; the file is readable by the node's user only.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        // `mode` only applies to a new file; a leftover temporary file keeps its permissions.
        // Restrict it before any secret is written.
        private_file(&tmp)?;
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

/// A payout this node confirmed (signed `PayoutConfirmation` for), kept so it can sign the same
/// confirmation again if submitting it to Base failed (H-5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedPayout {
    pub claim_ids: Vec<[u8; 32]>,
    /// Id of the mined transaction (internal byte order).
    pub txid: [u8; 32],
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
    /// Payouts this node confirmed (H-5).
    #[serde(default)]
    pub confirmed: Vec<ConfirmedPayout>,
    /// Base position `(block, log index)` of the last Registry event applied to this vault: an
    /// event is applied at most once, whatever happens to the global cursor (M-9).
    #[serde(default)]
    pub last_event: Option<(u64, u64)>,
}

impl VaultRecord {
    pub fn sk_bytes(&self) -> Result<[u8; 32]> {
        hex::decode(&self.sk)?.try_into().map_err(|_| anyhow::anyhow!("stored sk is not 32 bytes"))
    }

    pub fn ak_bytes(&self) -> Result<[u8; 32]> {
        hex::decode(&self.ak)?.try_into().map_err(|_| anyhow::anyhow!("stored ak is not 32 bytes"))
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
        private_dir(dir)?;
        private_dir(&dir.join("vaults"))?;
        let mut vaults = BTreeMap::new();
        for entry in fs::read_dir(dir.join("vaults"))? {
            let path = entry?.path();
            if path.is_file() {
                private_file(&path)?;
            }
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let v: VaultRecord =
                    serde_json::from_slice(&fs::read(&path)?).with_context(|| format!("parse {}", path.display()))?;
                vaults.insert(v.vault_id, v);
            }
        }
        for name in ["recipients.json", "meta.json"] {
            let path = dir.join(name);
            if path.is_file() {
                private_file(&path)?;
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
        write_atomic(&self.dir.join("meta.json"), &serde_json::to_vec(&Meta { base_cursor: block })?)
    }

    /// True if `account` or `owner` is already bound to a vault (I4, checked off-chain).
    pub fn is_bound(&self, account: Address, owner: Address) -> bool {
        self.vaults.values().any(|v| v.account == account || v.owner == owner)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn store_files_are_private_and_old_ones_are_fixed() {
        let dir = std::env::temp_dir().join(format!("zecured-store-perm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A store written before L-3: world-readable files and directories.
        fs::create_dir_all(dir.join("vaults")).unwrap();
        for (path, perm) in [
            (dir.clone(), 0o755),
            (dir.join("vaults"), 0o755),
            (dir.join("recipients.json"), 0o644),
            (dir.join("vaults/old.tmp"), 0o644),
        ] {
            if path.extension().is_some() {
                fs::write(&path, b"{}").unwrap();
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(perm)).unwrap();
        }

        let mut store = Store::open(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("vaults")), 0o700);
        assert_eq!(mode(&dir.join("recipients.json")), 0o600);
        assert_eq!(mode(&dir.join("vaults/old.tmp")), 0o600);

        // New files are private, even when a world-readable temporary file is left over.
        fs::write(dir.join("meta.tmp"), b"").unwrap();
        fs::set_permissions(dir.join("meta.tmp"), fs::Permissions::from_mode(0o644)).unwrap();
        store.set_base_cursor(7).unwrap();
        store
            .add_recipient(Recipient {
                receiver: [1; 43],
                salt: [2; 32],
            })
            .unwrap();
        assert_eq!(mode(&dir.join("meta.json")), 0o600);
        assert_eq!(mode(&dir.join("recipients.json")), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
