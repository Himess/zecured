//! Network configuration (public, shared by all nodes and the coordinator) and the node's
//! own secret identity.

use std::{fs, path::Path};

use alloy::{primitives::Address, signers::local::PrivateKeySigner};
use anyhow::{Context, Result, anyhow};
use rand_core_06::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

/// Public description of one signer node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    /// FROST participant index, 1..=5.
    pub index: u16,
    /// Base URL of the node's HTTP API (used by the coordinator).
    pub url: String,
    /// Static X25519 public key (hex) for node-to-node encryption.
    pub x25519: String,
    /// Address of the node's attestation key (one of the Registry's signer set).
    pub eth: Address,
}

impl NodeInfo {
    pub fn x25519_public(&self) -> Result<PublicKey> {
        let bytes: [u8; 32] = hex::decode(&self.x25519)?
            .try_into()
            .map_err(|_| anyhow!("x25519 key must be 32 bytes"))?;
        Ok(PublicKey::from(bytes))
    }
}

/// Configuration shared by the whole signer network.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub lightwalletd_url: String,
    pub base_rpc_url: String,
    pub chain_id: u64,
    pub registry: Address,
    pub adapter: Address,
    /// First Base block to ingest Registry events from (the Registry's deployment block).
    pub registry_from_block: u64,
    /// Zcash confirmations required before P1 classifies a note.
    pub confirmations: u32,
    /// Blocks a new vault accepts its deposit for (expiry height = birthday + this).
    pub vault_expiry_blocks: u32,
    /// Accepted collateral denominations, in zatoshis.
    pub denominations: Vec<u64>,
    pub nodes: Vec<NodeInfo>,
}

impl NetworkConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&raw).context("parse network config")?;
        anyhow::ensure!(cfg.nodes.len() == 5, "the network has exactly 5 signer nodes");
        Ok(cfg)
    }

    pub fn node(&self, index: u16) -> Result<&NodeInfo> {
        self.nodes
            .iter()
            .find(|n| n.index == index)
            .with_context(|| format!("node {index} not in network config"))
    }
}

/// A node's secret identity: its FROST index, Base attestation key and X25519 key.
#[derive(Serialize, Deserialize)]
struct IdentityFile {
    index: u16,
    eth_private_key: String,
    x25519_secret: String,
}

pub struct Identity {
    pub index: u16,
    pub eth: PrivateKeySigner,
    pub x25519: StaticSecret,
}

impl Identity {
    /// Creates a fresh identity file (fails if one exists). The Base key may be supplied
    /// (e.g. from `.env`) so the Registry's signer set can be fixed before nodes start.
    pub fn create(path: &Path, index: u16, eth_private_key: Option<&str>) -> Result<Identity> {
        anyhow::ensure!(!path.exists(), "{} already exists", path.display());
        let eth = match eth_private_key {
            Some(k) => k.parse::<PrivateKeySigner>().context("invalid Base private key")?,
            None => PrivateKeySigner::random(),
        };
        let x25519 = StaticSecret::random_from_rng(OsRng);
        let file = IdentityFile {
            index,
            eth_private_key: hex::encode(eth.to_bytes()),
            x25519_secret: hex::encode(x25519.to_bytes()),
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        crate::store::write_atomic(path, serde_json::to_vec_pretty(&file)?.as_slice())?;
        Ok(Identity { index, eth, x25519 })
    }

    pub fn load(path: &Path) -> Result<Identity> {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let file: IdentityFile = serde_json::from_str(&raw)?;
        let eth = file.eth_private_key.parse::<PrivateKeySigner>()?;
        let x: [u8; 32] = hex::decode(&file.x25519_secret)?
            .try_into()
            .map_err(|_| anyhow!("x25519 secret must be 32 bytes"))?;
        Ok(Identity {
            index: file.index,
            eth,
            x25519: StaticSecret::from(x),
        })
    }

    /// The public part, to put in the network configuration.
    pub fn public_info(&self, url: &str) -> NodeInfo {
        NodeInfo {
            index: self.index,
            url: url.to_owned(),
            x25519: hex::encode(PublicKey::from(&self.x25519).as_bytes()),
            eth: self.eth.address(),
        }
    }
}
