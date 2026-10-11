//! Node side of vault creation: FROST DKG plus agreement on the ZIP 2005 `sk`.
//!
//! Round 1: check the owner's `OpenVault` request (P0), run DKG part 1, commit to a random
//! contribution `r_i` to the vault secret.
//! Round 2: run DKG part 2; seal each recipient's round-2 package together with `r_i`.
//! Round 3: open the packages sealed to us, check every `r_j` against its commitment,
//! finish the DKG, derive `sk = H(vault_id, r_1..r_5)` and the vault's viewing keys.
//!
//! The coordinator relays every message but only ever sees public packages, commitments
//! and ciphertexts.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rand_core_06::{OsRng, RngCore};
use reddsa::frost::redpallas::{
    Identifier,
    keys::{
        EvenY,
        dkg::{self, round1, round2},
    },
};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;
use zecured_base::{OpenVault, recover_typed, registry_domain};
use zecured_policy::derive::{sk_commitment, vault_sk};
use zecured_threshold::{MAX_SIGNERS, MIN_SIGNERS, ak_bytes, signer_id};
use zecured_zcash_ops::keys::{VaultSecret, encode_ironwood_ua, encode_ufvk, vault_address};

pub use crate::envelope::Sealed;

use crate::{
    config::NetworkConfig,
    envelope,
    store::{Store, VaultRecord},
};

/// The owner's request to open a vault, plus the coordinator's chain parameters.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultOpen {
    pub vault_id: B256,
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub request_id: B256,
    pub deadline: u64,
    /// `OpenVault` EIP-712 signature by `owner`.
    pub signature: Bytes,
    pub birthday: u32,
    pub expiry_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round1Out {
    pub index: u16,
    pub package: serde_json::Value,
    pub sk_commitment: B256,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round2In {
    pub vault_id: B256,
    pub round1: Vec<Round1Out>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round2Out {
    pub index: u16,
    /// Sealed message per recipient index.
    pub sealed: BTreeMap<u16, Sealed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round3In {
    pub vault_id: B256,
    pub round1: Vec<Round1Out>,
    /// Sealed messages addressed to this node, by sender index.
    pub sealed: BTreeMap<u16, Sealed>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round3Out {
    pub index: u16,
    pub ak: String,
    pub ufvk: String,
    pub address: String,
    /// Public: group key + verifying shares (needed by the coordinator to aggregate).
    pub public_key_package: serde_json::Value,
}

/// What travels inside a sealed round-2 message.
#[derive(Serialize, Deserialize)]
struct Round2Payload {
    package: serde_json::Value,
    contribution: B256,
}

/// DKG state kept in memory between rounds.
pub struct Pending {
    open: VaultOpen,
    contribution: [u8; 32],
    round1_secret: Option<round1::SecretPackage>,
    round2_secret: Option<round2::SecretPackage>,
}

fn context(vault_id: &B256) -> Vec<u8> {
    let mut c = b"zecured/dkg2/".to_vec();
    c.extend_from_slice(vault_id.as_slice());
    c
}

/// P0 checks on an `OpenVault` request.
pub fn check_open(cfg: &NetworkConfig, store: &Store, open: &VaultOpen, now: u64, zcash_tip: u32) -> Result<()> {
    let msg = OpenVault {
        owner: open.owner,
        account: open.account,
        amount: open.amount,
        requestId: open.request_id,
        deadline: open.deadline,
    };
    let signer = recover_typed(&registry_domain(cfg.chain_id, cfg.registry), &msg, &open.signature)?;
    ensure!(signer == open.owner, "OpenVault not signed by the owner");
    ensure!(open.deadline >= now, "request expired");
    ensure!(cfg.denominations.contains(&open.amount), "not a standard denomination");
    ensure!(!store.vaults.contains_key(&open.vault_id), "vault id already used");
    ensure!(!store.is_bound(open.account, open.owner), "account or owner already has a vault");
    ensure!(
        open.birthday.abs_diff(zcash_tip) <= MAX_TIP_SKEW,
        "birthday {} too far from our tip {zcash_tip}",
        open.birthday
    );
    ensure!(open.expiry_height == open.birthday + cfg.vault_expiry_blocks, "unexpected expiry height");
    Ok(())
}

/// Blocks the coordinator's view of the Zcash tip may differ from ours.
const MAX_TIP_SKEW: u32 = 20;

pub fn round1(index: u16, open: VaultOpen) -> Result<(Pending, Round1Out)> {
    let id = signer_id(index)?;
    let (secret, package) = dkg::part1(id, MAX_SIGNERS, MIN_SIGNERS, OsRng).map_err(|e| anyhow!("dkg part1: {e}"))?;
    let mut contribution = [0u8; 32];
    OsRng.fill_bytes(&mut contribution);
    let out = Round1Out {
        index,
        package: serde_json::to_value(&package)?,
        sk_commitment: B256::from(sk_commitment(&open.vault_id.0, &contribution)),
    };
    Ok((
        Pending {
            open,
            contribution,
            round1_secret: Some(secret),
            round2_secret: None,
        },
        out,
    ))
}

fn others_round1(index: u16, round1: &[Round1Out]) -> Result<BTreeMap<Identifier, round1::Package>> {
    let mut map = BTreeMap::new();
    for r in round1.iter().filter(|r| r.index != index) {
        map.insert(signer_id(r.index)?, serde_json::from_value(r.package.clone())?);
    }
    ensure!(map.len() == usize::from(MAX_SIGNERS) - 1, "need round-1 packages from all other signers");
    Ok(map)
}

pub fn round2(
    cfg: &NetworkConfig,
    me: &x25519_dalek::StaticSecret,
    index: u16,
    pending: &mut Pending,
    input: &Round2In,
) -> Result<Round2Out> {
    let secret = pending.round1_secret.take().context("round 1 not run or round 2 repeated")?;
    let (secret2, packages) =
        dkg::part2(secret, &others_round1(index, &input.round1)?).map_err(|e| anyhow!("dkg part2: {e}"))?;
    pending.round2_secret = Some(secret2);
    let mut sealed = BTreeMap::new();
    for node in cfg.nodes.iter().filter(|n| n.index != index) {
        let package = packages
            .get(&signer_id(node.index)?)
            .with_context(|| format!("no round-2 package for {}", node.index))?;
        let payload = serde_json::to_vec(&Round2Payload {
            package: serde_json::to_value(package)?,
            contribution: B256::from(pending.contribution),
        })?;
        sealed.insert(node.index, envelope::seal(me, &node.x25519_public()?, &context(&input.vault_id), &payload)?);
    }
    Ok(Round2Out { index, sealed })
}

pub fn round3(
    cfg: &NetworkConfig,
    me: &x25519_dalek::StaticSecret,
    index: u16,
    pending: Pending,
    input: &Round3In,
    store: &mut Store,
) -> Result<Round3Out> {
    let secret2 = pending.round2_secret.as_ref().context("round 2 not run")?;
    let vault_id = pending.open.vault_id;
    ensure!(input.vault_id == vault_id, "vault id mismatch");

    let mut contributions: BTreeMap<u16, [u8; 32]> = BTreeMap::new();
    contributions.insert(index, pending.contribution);
    let mut received = BTreeMap::new();
    for (&sender, sealed) in &input.sealed {
        let node = cfg.node(sender)?;
        let plain = envelope::open(me, &node.x25519_public()?, &context(&vault_id), sealed)?;
        let payload: Round2Payload = serde_json::from_slice(&plain)?;
        let commitment =
            input.round1.iter().find(|r| r.index == sender).context("missing round-1 output")?.sk_commitment;
        if sk_commitment(&vault_id.0, &payload.contribution.0) != commitment.0 {
            bail!("signer {sender} revealed a contribution that does not match its commitment");
        }
        contributions.insert(sender, payload.contribution.0);
        received.insert(signer_id(sender)?, serde_json::from_value::<round2::Package>(payload.package)?);
    }
    ensure!(contributions.len() == usize::from(MAX_SIGNERS), "missing contributions");

    let (key_package, public_key_package) =
        dkg::part3(secret2, &others_round1(index, &input.round1)?, &received).map_err(|e| anyhow!("dkg part3: {e}"))?;
    ensure!(public_key_package.has_even_y(), "group key must have ỹ = 0");
    let ak = ak_bytes(&public_key_package)?;
    let ordered: Vec<[u8; 32]> = contributions.values().copied().collect();
    let sk = vault_sk(&vault_id.0, &ordered);
    let fvk = VaultSecret::from_bytes(sk).full_viewing_key(&ak)?;
    let network = Network::TestNetwork;
    let out = Round3Out {
        index,
        ak: hex::encode(ak),
        ufvk: encode_ufvk(&network, &fvk)?,
        address: encode_ironwood_ua(&network, vault_address(&fvk))?,
        public_key_package: serde_json::to_value(&public_key_package)?,
    };
    let open = pending.open;
    store.save_vault(VaultRecord {
        vault_id,
        owner: open.owner,
        account: open.account,
        amount: open.amount,
        birthday: open.birthday,
        expiry_height: open.expiry_height,
        key_package: serde_json::to_value(&key_package)?,
        public_key_package: serde_json::to_value(&public_key_package)?,
        sk: hex::encode(sk),
        ak: out.ak.clone(),
        ufvk: out.ufvk.clone(),
        address: out.address.clone(),
        decisions: vec![],
        attestation: None,
        activated: false,
        outstanding: 0,
        claims: vec![],
        refunds: vec![],
        used_nonces: vec![],
        inflight: None,
    })?;
    Ok(out)
}
