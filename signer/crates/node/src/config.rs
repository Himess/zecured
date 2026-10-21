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
        let bytes: [u8; 32] =
            hex::decode(&self.x25519)?.try_into().map_err(|_| anyhow!("x25519 key must be 32 bytes"))?;
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
    /// Address whose signature every coordinator request must carry (D16). `None` disables
    /// the check, which a node accepts only when started with `--insecure-no-auth` (local
    /// development only, L-1).
    #[serde(default)]
    pub coordinator: Option<Address>,
    /// Upper bound of the random extra confirmations before a deposit is attested (D15).
    #[serde(default)]
    pub attest_delay_max_blocks: u32,
    /// L23: upper bound of the random number of Zcash blocks a payout waits between its
    /// commitment on Base and its broadcast (docs/TIER3.md § 11; 0 broadcasts at once).
    #[serde(default)]
    pub payout_delay_max_blocks: u32,
    /// T3-4: Base blocks a payout's commitment must at least be under before any node releases a
    /// share. Since review H-11 a minimum only: the commitment must also be under Base's `safe`
    /// head, or `commit_depth_secs` deep.
    #[serde(default = "default_commit_depth")]
    pub commit_depth_blocks: u64,
    /// Review H-11, T3-4: a node releases a payout's share once its commitment is at or below Base's
    /// `safe` head, or, while that lags, once the commitment has been on Base this many seconds (by
    /// block timestamps), whichever comes first. Seconds, unlike blocks, keep their meaning when
    /// Base's blocks go from 2 s to 200 ms (Denim). At most [`MAX_COMMIT_DEPTH_SECS`].
    #[serde(default = "default_commit_depth_secs")]
    pub commit_depth_secs: u64,
    /// A local fork (anvil): the only network on which the privacy delays may be 0 (review H-05).
    #[serde(default)]
    pub local_fork: bool,
    /// Review H-01: the cost of opening a vault and the vaults that may wait for a deposit.
    #[serde(default)]
    pub open_vault: OpenVaultLimits,
}

/// Review H-01: what opening a vault costs its requester, and how many vaults may wait for their
/// deposit at once. The coordinator applies them; the stamp's bits are public (`GET /config`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenVaultLimits {
    /// Zero bits the open-vault stamp must start with (`zecured_base::stamp`); 0: no stamp.
    pub stamp_bits: u32,
    /// Seconds a vault waiting for its deposit holds one of the `max_pending` slots while no deposit
    /// is seen. Only the slot is released: the vault stays open, and a deposit is accepted, until
    /// its expiry height as before.
    pub pending_slot_secs: u64,
    /// Most vaults holding a slot at once.
    pub max_pending: usize,
}

impl Default for OpenVaultLimits {
    fn default() -> Self {
        Self {
            stamp_bits: zecured_base::stamp::DEFAULT_BITS,
            pending_slot_secs: 30 * 60,
            max_pending: 512,
        }
    }
}

fn default_commit_depth() -> u64 {
    crate::DEFAULT_COMMIT_DEPTH
}

fn default_commit_depth_secs() -> u64 {
    crate::DEFAULT_COMMIT_DEPTH_SECS
}

/// The most `commit_depth_secs` may be: the wait for the depth, the commitment's own transaction
/// and round 2 must fit in a signing session (`SESSION_TTL`, 300 s), or no payout could be signed
/// while Base's safe head lags.
pub const MAX_COMMIT_DEPTH_SECS: u64 = 180;

/// Blocks of a payout's expiry window kept free of the L23 delay: signing, a rebroadcast or two
/// and the inclusion itself (RT-15).
pub const PAYOUT_DELAY_HEADROOM: u32 = 40;

/// L5 (D15): the least `attest_delay_max_blocks` outside a local fork (review H-05).
pub const MIN_ATTEST_DELAY_MAX_BLOCKS: u32 = 2;

impl NetworkConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&raw).context("parse network config")?;
        cfg.check()?;
        Ok(cfg)
    }

    /// Values a network config must respect.
    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(self.nodes.len() == 5, "the network has exactly 5 signer nodes");
        // RT-15: a delay near the expiry would let every payout expire before its broadcast.
        let max_delay = zecured_zcash_ops::spend::PAYOUT_EXPIRY_DELTA.saturating_sub(PAYOUT_DELAY_HEADROOM);
        anyhow::ensure!(
            self.payout_delay_max_blocks <= max_delay,
            "payout_delay_max_blocks must leave {PAYOUT_DELAY_HEADROOM} of the payout's {} expiry blocks (at most {max_delay})",
            zecured_zcash_ops::spend::PAYOUT_EXPIRY_DELTA
        );
        // H-11: the commitment depth's time fallback fits in a signing session.
        anyhow::ensure!(
            self.commit_depth_secs <= MAX_COMMIT_DEPTH_SECS,
            "commit_depth_secs must be at most {MAX_COMMIT_DEPTH_SECS} (a signing session lasts {} s)",
            crate::SESSION_TTL.as_secs()
        );
        // H-01: a stamp a browser can still compute, and room for at least one vault.
        anyhow::ensure!(
            self.open_vault.stamp_bits <= zecured_base::stamp::MAX_BITS,
            "open_vault.stamp_bits must be at most {}",
            zecured_base::stamp::MAX_BITS
        );
        anyhow::ensure!(
            self.open_vault.max_pending >= 1 && self.open_vault.pending_slot_secs >= 60,
            "open_vault needs max_pending >= 1 and pending_slot_secs >= 60"
        );
        // H-05: the delays that hide a payout's and a mint's timing (L23, L5) may only be off on a
        // local fork.
        if !self.local_fork {
            anyhow::ensure!(
                self.payout_delay_max_blocks >= 1,
                "payout_delay_max_blocks must be at least 1 outside a local fork (L23; 8 on the judge deployment)"
            );
            anyhow::ensure!(
                self.attest_delay_max_blocks >= MIN_ATTEST_DELAY_MAX_BLOCKS,
                "attest_delay_max_blocks must be at least {MIN_ATTEST_DELAY_MAX_BLOCKS} outside a local fork (L5)"
            );
        }
        Ok(())
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
            crate::store::private_dir(dir)?;
        }
        crate::store::write_atomic(path, serde_json::to_vec_pretty(&file)?.as_slice())?;
        Ok(Identity { index, eth, x25519 })
    }

    pub fn load(path: &Path) -> Result<Identity> {
        // L-3: an identity written before permissions were restricted is fixed on load.
        if let Some(dir) = path.parent() {
            crate::store::private_dir(dir)?;
        }
        crate::store::private_file(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(index: u16) -> NodeInfo {
        NodeInfo {
            index,
            url: String::new(),
            x25519: String::new(),
            eth: Address::repeat_byte(u8::try_from(index).unwrap()),
        }
    }

    /// The judge deployment's values.
    fn judge_config() -> NetworkConfig {
        NetworkConfig {
            lightwalletd_url: String::new(),
            base_rpc_url: String::new(),
            chain_id: 84_532,
            registry: Address::ZERO,
            adapter: Address::ZERO,
            registry_from_block: 0,
            confirmations: 10,
            vault_expiry_blocks: 288,
            denominations: vec![1_000_000],
            nodes: (1..=5).map(node).collect(),
            coordinator: None,
            attest_delay_max_blocks: MIN_ATTEST_DELAY_MAX_BLOCKS,
            payout_delay_max_blocks: 8,
            commit_depth_blocks: 3,
            commit_depth_secs: 120,
            local_fork: false,
            open_vault: OpenVaultLimits::default(),
        }
    }

    /// RT-15: the L23 delay must leave room in the payout's expiry window.
    #[test]
    fn the_payout_delay_leaves_room_before_expiry() {
        let mut cfg = judge_config();
        cfg.check().unwrap();
        cfg.payout_delay_max_blocks = zecured_zcash_ops::spend::PAYOUT_EXPIRY_DELTA - PAYOUT_DELAY_HEADROOM;
        cfg.check().unwrap();
        cfg.payout_delay_max_blocks += 1;
        assert!(cfg.check().is_err());
        cfg.payout_delay_max_blocks = 8;
        cfg.nodes.pop();
        assert!(cfg.check().is_err(), "five nodes");
    }

    /// H-05: the privacy delays (L23, L5) can only be switched off on a local fork.
    #[test]
    fn the_privacy_delays_are_on_outside_a_local_fork() {
        let mut cfg = judge_config();
        cfg.payout_delay_max_blocks = 0;
        assert!(cfg.check().is_err(), "no payout delay");
        cfg.payout_delay_max_blocks = 1;
        cfg.check().unwrap();
        cfg.attest_delay_max_blocks = MIN_ATTEST_DELAY_MAX_BLOCKS - 1;
        assert!(cfg.check().is_err(), "attestation delay below the minimum");
        cfg.attest_delay_max_blocks = 0;
        cfg.payout_delay_max_blocks = 0;
        cfg.local_fork = true;
        cfg.check().unwrap();
    }

    /// H-11: a network config written before the time depth gets 120 s; the depth fits in a
    /// signing session.
    #[test]
    fn the_commitment_depth_defaults_and_fits_a_session() {
        let mut old = serde_json::to_value(judge_config()).unwrap();
        old.as_object_mut().unwrap().remove("commit_depth_secs");
        old.as_object_mut().unwrap().remove("commit_depth_blocks");
        let cfg: NetworkConfig = serde_json::from_value(old).unwrap();
        assert_eq!((cfg.commit_depth_blocks, cfg.commit_depth_secs), (3, 120));
        cfg.check().unwrap();
        let mut cfg = judge_config();
        cfg.commit_depth_secs = MAX_COMMIT_DEPTH_SECS;
        cfg.check().unwrap();
        assert!(MAX_COMMIT_DEPTH_SECS + 60 < crate::SESSION_TTL.as_secs());
        cfg.commit_depth_secs = MAX_COMMIT_DEPTH_SECS + 1;
        assert!(cfg.check().is_err());
    }

    /// H-01: a network config written before the open-vault limits gets the defaults (a 20-bit
    /// stamp, a 30-minute slot, 512 slots); the limits stay within reason.
    #[test]
    fn open_vault_limits_default_and_are_checked() {
        let mut old = serde_json::to_value(judge_config()).unwrap();
        old.as_object_mut().unwrap().remove("open_vault");
        let cfg: NetworkConfig = serde_json::from_value(old).unwrap();
        assert_eq!(cfg.open_vault, OpenVaultLimits::default());
        assert_eq!(
            (cfg.open_vault.stamp_bits, cfg.open_vault.pending_slot_secs, cfg.open_vault.max_pending),
            (20, 1_800, 512)
        );
        cfg.check().unwrap();
        let partial: OpenVaultLimits = serde_json::from_value(serde_json::json!({ "stamp_bits": 0 })).unwrap();
        assert_eq!((partial.stamp_bits, partial.max_pending), (0, 512));

        let mut cfg = judge_config();
        cfg.open_vault.stamp_bits = zecured_base::stamp::MAX_BITS + 1;
        assert!(cfg.check().is_err());
        cfg.open_vault = OpenVaultLimits {
            max_pending: 0,
            ..OpenVaultLimits::default()
        };
        assert!(cfg.check().is_err());
        cfg.open_vault = OpenVaultLimits {
            pending_slot_secs: 59,
            ..OpenVaultLimits::default()
        };
        assert!(cfg.check().is_err());
    }
}
