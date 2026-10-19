//! The Zcash network a deployment's vaults live on. Every part of the off-chain stack takes it from
//! the deployment's configuration (the signers' network config, the deployment file): one set of
//! binaries runs the judge deployment on Zcash Testnet and the mainnet beta on Zcash Mainnet.
//!
//! Each Base chain goes with one Zcash network: Base Sepolia (84532), and a plain anvil (31337), with
//! Testnet; Base (8453) with Mainnet. Any other pairing is refused, except on an explicit local fork
//! (a fork of Base mainnet rehearsing with Testnet ZEC, say), where the network must be named.

use std::fmt;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;

/// Base Sepolia's chain id.
pub const BASE_SEPOLIA: u64 = 84_532;
/// Base mainnet's chain id.
pub const BASE_MAINNET: u64 = 8_453;
/// A plain anvil's default chain id.
pub const ANVIL: u64 = 31_337;

/// A Zcash network, as configurations name it (`"testnet"`, `"mainnet"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZcashNetwork {
    Testnet,
    Mainnet,
}

impl ZcashNetwork {
    /// The consensus parameters (address and key encodings, branch ids by height).
    pub fn params(self) -> Network {
        match self {
            ZcashNetwork::Testnet => Network::TestNetwork,
            ZcashNetwork::Mainnet => Network::MainNetwork,
        }
    }

    /// The Zcash network the Base chain `chain_id` goes with, if it is one of Zecured's.
    pub fn for_base_chain(chain_id: u64) -> Option<Self> {
        match chain_id {
            BASE_SEPOLIA | ANVIL => Some(ZcashNetwork::Testnet),
            BASE_MAINNET => Some(ZcashNetwork::Mainnet),
            _ => None,
        }
    }

    /// The network of a deployment on Base chain `chain_id`: `configured` when given, else the one
    /// that chain goes with. A pairing other than the chain's own, or a chain without one, is
    /// refused unless `local_fork` (where the network must then be named).
    pub fn resolve(configured: Option<Self>, chain_id: u64, local_fork: bool) -> Result<Self> {
        let paired = Self::for_base_chain(chain_id);
        match (configured, paired) {
            (Some(c), Some(p)) if c == p => Ok(c),
            (Some(c), _) => {
                ensure!(
                    local_fork,
                    "Zcash {c} does not go with Base chain {chain_id} ({}): refused outside an explicit local fork",
                    paired.map_or("no Zcash network".to_owned(), |p| format!("Zcash {p}"))
                );
                Ok(c)
            }
            (None, Some(p)) => Ok(p),
            (None, None) => bail!("Base chain {chain_id} goes with no Zcash network: name it (zcash_network)"),
        }
    }

    /// The deployment file's name for it (`DeployZecured`'s `zcashNetwork`), or this crate's own.
    pub fn from_name(name: &str) -> Result<Self> {
        Ok(match name {
            "testnet" | "zcash-testnet" => ZcashNetwork::Testnet,
            "mainnet" | "zcash-mainnet" => ZcashNetwork::Mainnet,
            other => bail!("unknown Zcash network {other:?} (testnet or mainnet)"),
        })
    }

    /// What a light wallet server reports for it (`GetLightdInfo.chain_name`).
    pub fn lightd_chain_name(self) -> &'static str {
        match self {
            ZcashNetwork::Testnet => "test",
            ZcashNetwork::Mainnet => "main",
        }
    }

    /// The target block spacing a payout's lifetime is counted in, in seconds: 25 s on Testnet (NU7
    /// is active there, ZIP 218), 75 s on Mainnet until its NU7 activation (a network config may
    /// set `payout_lifetime_secs` instead once Mainnet runs NU7).
    pub fn block_secs(self) -> u64 {
        match self {
            ZcashNetwork::Testnet => 25,
            ZcashNetwork::Mainnet => 75,
        }
    }
}

impl fmt::Display for ZcashNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ZcashNetwork::Testnet => "testnet",
            ZcashNetwork::Mainnet => "mainnet",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each Base chain goes with its own Zcash network; another pairing only on a local fork.
    #[test]
    fn the_zcash_network_goes_with_the_base_chain() {
        use ZcashNetwork::{Mainnet, Testnet};
        assert_eq!(ZcashNetwork::resolve(None, BASE_SEPOLIA, false).unwrap(), Testnet);
        assert_eq!(ZcashNetwork::resolve(None, ANVIL, false).unwrap(), Testnet);
        assert_eq!(ZcashNetwork::resolve(None, BASE_MAINNET, false).unwrap(), Mainnet);
        assert_eq!(ZcashNetwork::resolve(Some(Mainnet), BASE_MAINNET, false).unwrap(), Mainnet);
        assert_eq!(ZcashNetwork::resolve(Some(Testnet), BASE_SEPOLIA, false).unwrap(), Testnet);
        // A mismatch is refused, except on an explicit local fork.
        let e = ZcashNetwork::resolve(Some(Testnet), BASE_MAINNET, false).unwrap_err();
        assert!(e.to_string().contains("local fork"), "{e}");
        assert!(ZcashNetwork::resolve(Some(Mainnet), BASE_SEPOLIA, false).is_err());
        assert_eq!(ZcashNetwork::resolve(Some(Testnet), BASE_MAINNET, true).unwrap(), Testnet);
        // A chain without a Zcash network: named on a local fork only.
        assert!(ZcashNetwork::resolve(None, 1, true).is_err());
        assert!(ZcashNetwork::resolve(Some(Testnet), 1, false).is_err());
        assert_eq!(ZcashNetwork::resolve(Some(Testnet), 1, true).unwrap(), Testnet);
    }

    #[test]
    fn names_and_parameters() {
        assert_eq!(serde_json::to_value(ZcashNetwork::Mainnet).unwrap(), "mainnet");
        assert_eq!(serde_json::from_str::<ZcashNetwork>("\"testnet\"").unwrap(), ZcashNetwork::Testnet);
        assert_eq!(ZcashNetwork::from_name("zcash-mainnet").unwrap(), ZcashNetwork::Mainnet);
        assert_eq!(ZcashNetwork::from_name("testnet").unwrap(), ZcashNetwork::Testnet);
        assert!(ZcashNetwork::from_name("regtest").is_err());
        assert_eq!(ZcashNetwork::Mainnet.params(), Network::MainNetwork);
        assert_eq!(ZcashNetwork::Testnet.lightd_chain_name(), "test");
        assert_eq!((ZcashNetwork::Testnet.block_secs(), ZcashNetwork::Mainnet.block_secs()), (25, 75));
    }
}
