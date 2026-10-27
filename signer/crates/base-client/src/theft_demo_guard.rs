//! The theft demo's guard (review T3-6, docs/TIER3.md § 8). The theft demo runs on its own
//! deployment, where three signer nodes can be switched to "compromised" so that a theft really
//! happens and is then proven and slashed. Its binaries (the node's and the backend's
//! `demo-tier3` feature) must never run against the judge deployment. They refuse to start if the
//! configured Registry is a judge Registry, or if any signer they would use (the network
//! config's, the Registry's own on-chain signer set, or the node's key) is a judge signer. The
//! judge deployment is compiled in, so a misconfiguration ends in an error message, never in a
//! theft on the judge network.

use alloy::{
    primitives::{Address, address},
    providers::ProviderBuilder,
};
use anyhow::{Context, Result, ensure};

use crate::events::IZecuredRegistry;

/// The judge deployment's five signers (`deployments/base-sepolia.json`, `signers`).
pub const JUDGE_SIGNERS: [Address; 5] = [
    address!("0x293e0446E84420FC477CE34F79608693a9131086"),
    address!("0x3a9B1b2AF1E37fF5b90B36667DF1058Ad841A31d"),
    address!("0x8552AD9D7AAFBe315dbF0Ca950F3A6AfBc6a91e6"),
    address!("0xD62BdAE4d63FFF774Bd4a49a8e5ce8FC8353e442"),
    address!("0xeE66Db11851FB7363a88c776D10c7f5284667EcE"),
];

/// The judge deployment's Registry (`deployments/base-sepolia.json`, `contracts.ZecuredRegistry`).
pub const JUDGE_REGISTRIES: &[Address] = &[address!("0xDC9C7E6abeF85A7bE0C65e4208bC87D098b86479")];

/// Refuses a theft-demo configuration that names the judge Registry or a judge signer.
pub fn check(registry: Address, signers: &[Address]) -> Result<()> {
    ensure!(
        !JUDGE_REGISTRIES.contains(&registry),
        "theft demo: {registry} is the judge deployment's Registry; this build never runs against it"
    );
    for s in signers {
        ensure!(
            !JUDGE_SIGNERS.contains(s),
            "theft demo: {s} is a signer of the judge deployment; the theft demo uses its own five keys"
        );
    }
    Ok(())
}

/// [`check`] against the Registry's on-chain signer set as well, read through `rpc`.
pub async fn check_on_chain(rpc: &str, registry: Address) -> Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    let signers = IZecuredRegistry::new(registry, &provider)
        .signers()
        .call()
        .await
        .context("theft demo: reading the Registry's signers")?;
    check(registry, &signers)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_judge_deployment_is_refused() {
        let other = address!("0x1111111111111111111111111111111111111111");
        assert!(check(other, &[other]).is_ok());
        for s in JUDGE_SIGNERS {
            assert!(check(other, &[other, s]).is_err());
        }
        for r in JUDGE_REGISTRIES {
            assert!(check(*r, &[other]).is_err());
        }
    }

    /// Once the judge deployment exists, the compiled-in lists must name it.
    #[test]
    fn the_lists_match_the_judge_deployment() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/deployments/base-sepolia.json");
        let Ok(raw) = std::fs::read(&path) else {
            return; // not deployed yet
        };
        let d: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let mut signers: Vec<Address> =
            serde_json::from_value(d["signers"].clone()).expect("the deployment lists its signers");
        let mut judge = JUDGE_SIGNERS.to_vec();
        signers.sort();
        judge.sort();
        assert_eq!(signers, judge, "JUDGE_SIGNERS must be the deployment's signers");
        let registry: Address = serde_json::from_value(d["contracts"]["ZecuredRegistry"].clone()).unwrap();
        assert!(JUDGE_REGISTRIES.contains(&registry), "JUDGE_REGISTRIES must name {registry}");
    }
}
