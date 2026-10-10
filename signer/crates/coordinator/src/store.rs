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
}

/// A refund the owner asked for and the signers accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refund {
    pub note_id: B256,
    pub recipient: Recipient,
    pub done: bool,
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
        fs::create_dir_all(dir.join("vaults"))?;
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
}
