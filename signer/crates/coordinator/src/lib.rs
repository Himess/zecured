//! Zecured coordinator: drives the signer network.
//!
//! It holds **no** key material for vaults: it relays DKG messages (sealed node-to-node),
//! scans vaults with their viewing keys, builds and proves PCZTs, runs the FROST rounds with
//! whichever signers accept, aggregates, broadcasts, and relays attestations and payout
//! confirmations to Base with its own gas key. Every decision that moves value is taken
//! independently by at least three signer nodes.

pub mod nodes;
pub mod store;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, Bytes},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use orchard::{circuit::ProvingKey, keys::FullViewingKey};
use reddsa::frost::redpallas::{keys::PublicKeyPackage, round1::SigningCommitments, round2::SignatureShare};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_protocol::{
    TxId,
    consensus::{BlockHeight, Network},
};
use zecured_base::{
    DepositAttestation, PayoutConfirmation, ViewKeyRequest,
    events::{self, IZecuredRegistry, RegistryEvent},
    order_signatures, recover_typed, registry_domain,
};
use zecured_node::{
    AttestResponse, ConfirmIn, ConfirmResponse, DestinationIn, RecipientIn, RefundIn,
    chain::{VaultNotes, ZcashView},
    config::NetworkConfig,
    dkg::{Round1Out, Round2In, Round2Out, Round3In, Round3Out, Sealed, VaultOpen},
    now,
    signing::{CommitRequest, CommitResponse, ShareRequest, ShareResponse, SignKind},
};
use zecured_policy::{
    classify::{self, DepositRules},
    ledger::{self, ClaimState, NextPayout, PayoutPlan, Recipient},
};
use zecured_threshold::{self as threshold, MIN_SIGNERS, signer_id};
use zecured_zcash_ops::{
    keys,
    scan::ReceivedNote,
    spend::{self, Payment},
    tx,
};

use crate::{
    nodes::{Answer, Nodes},
    store::{CoordVault, Inflight, Refund, Store, VaultStatus},
};

const NETWORK: Network = Network::TestNetwork;
const ZATS_PER_ZEC: u64 = 100_000_000;

/// The app's request to open a vault (`OpenVault`, signed by the owner key).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenVaultRequest {
    pub owner: Address,
    pub account: Address,
    pub amount: u64,
    pub request_id: B256,
    pub deadline: u64,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenVaultResponse {
    pub vault_id: B256,
    pub address: String,
    pub zip321: String,
    pub amount: u64,
    pub expiry_height: u32,
}

/// Public view of a vault for the app.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultView {
    pub vault_id: B256,
    pub status: VaultStatus,
    pub amount: u64,
    pub address: String,
    pub expiry_height: u32,
    pub outstanding: u64,
    pub claims: Vec<ClaimView>,
    pub non_conforming: Vec<NoteView>,
    pub inflight: bool,
    pub birthday_height: u32,
    /// Zcash tip at the last coordinator pass (0 before the first pass).
    pub tip_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimView {
    pub claim_id: B256,
    pub kind: ledger::ClaimKind,
    pub amount: u64,
    pub paid: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteView {
    pub note_id: B256,
    pub amount: u64,
}

/// One signer's reaction to the demo attack.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainVote {
    pub index: u16,
    pub signed: bool,
    pub reason: Option<String>,
}

/// Result of the threshold demo: an attempt to sweep a vault to an arbitrary address.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainReport {
    pub vault_id: B256,
    pub amount: u64,
    pub votes: Vec<DrainVote>,
    /// True if at least three signers would have signed (the demo never broadcasts).
    pub threshold_reached: bool,
}

pub struct Coordinator {
    pub cfg: NetworkConfig,
    nodes: Nodes,
    store: Mutex<Store>,
    zcash: Mutex<ZcashView>,
    gas: PrivateKeySigner,
    pk: OnceLock<Arc<ProvingKey>>,
    /// Zcash tip seen by the last coordinator pass (for the app's countdowns).
    last_tip: std::sync::atomic::AtomicU32,
    data_dir: PathBuf,
}

/// TEST ONLY: while this file exists in the coordinator's data directory, the next payout is
/// signed and recorded but neither broadcast nor rebroadcast, so it expires unmined and the
/// rebuild path runs end to end (docs/TESTING.md). The file is removed when used.
const TEST_WITHHOLD_FLAG: &str = "test-withhold-next-broadcast";

/// `zcash:` payment URI for `amount` zatoshis (ZIP 321).
pub fn zip321_uri(address: &str, amount: u64) -> String {
    let whole = amount / ZATS_PER_ZEC;
    let frac = amount % ZATS_PER_ZEC;
    let amount = if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", format!("{frac:08}").trim_end_matches('0'))
    };
    format!("zcash:{address}?amount={amount}")
}

fn fvk_of(v: &CoordVault) -> Result<FullViewingKey> {
    UnifiedFullViewingKey::decode(&NETWORK, &v.ufvk)
        .map_err(|e| anyhow!("stored UFVK: {e}"))?
        .orchard()
        .cloned()
        .context("UFVK has no Orchard-protocol key")
}

fn short(id: &B256) -> String {
    hex::encode(&id[..4])
}

/// Successful answers, or an error if fewer than `need` nodes agreed.
fn at_least<T>(answers: Vec<Answer<T>>, need: usize, what: &str) -> Result<Vec<(u16, T)>> {
    let mut ok = vec![];
    let mut errors = vec![];
    for (i, a) in answers {
        match a {
            Ok(v) => ok.push((i, v)),
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    if ok.len() < need {
        bail!("{what}: only {} of {need} signers agreed ({})", ok.len(), errors.join("; "));
    }
    Ok(ok)
}

impl Coordinator {
    pub fn new(cfg: NetworkConfig, data_dir: &Path, gas_key: &str) -> Result<Self> {
        let gas: PrivateKeySigner = gas_key.parse().context("coordinator gas key")?;
        Ok(Self {
            nodes: Nodes::new(&cfg, Some(gas.clone()))?,
            zcash: Mutex::new(ZcashView::new(&cfg.lightwalletd_url)),
            store: Mutex::new(Store::open(data_dir)?),
            gas,
            pk: OnceLock::new(),
            last_tip: std::sync::atomic::AtomicU32::new(0),
            data_dir: data_dir.to_owned(),
            cfg,
        })
    }

    /// The Halo 2 proving key (built once, a few seconds).
    async fn proving_key(&self) -> Result<Arc<ProvingKey>> {
        if let Some(pk) = self.pk.get() {
            return Ok(pk.clone());
        }
        let pk = Arc::new(tokio::task::spawn_blocking(spend::proving_key).await?);
        Ok(self.pk.get_or_init(|| pk).clone())
    }

    // ------------------------------------------------------------------ vault opening

    pub async fn open_vault(&self, req: OpenVaultRequest) -> Result<OpenVaultResponse> {
        ensure!(self.cfg.denominations.contains(&req.amount), "not a standard denomination");
        ensure!(!self.store.lock().await.is_bound(req.account, req.owner), "account or owner already has a vault");
        let tip = self.zcash.lock().await.tip().await?;
        let mut id = [0u8; 32];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut id);
        let open = VaultOpen {
            vault_id: B256::from(id),
            owner: req.owner,
            account: req.account,
            amount: req.amount,
            request_id: req.request_id,
            deadline: req.deadline,
            signature: req.signature,
            birthday: tip,
            expiry_height: tip + self.cfg.vault_expiry_blocks,
        };
        let all = self.nodes.indices();
        let n = all.len();

        let r1: Vec<Round1Out> = at_least(self.nodes.post_many(&all, "/v1/dkg/round1", &open).await, n, "DKG round 1")?
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        let r2in = Round2In {
            vault_id: open.vault_id,
            round1: r1.clone(),
        };
        let r2: Vec<(u16, Round2Out)> =
            at_least(self.nodes.post_many(&all, "/v1/dkg/round2", &r2in).await, n, "DKG round 2")?;
        let mut to: BTreeMap<u16, BTreeMap<u16, Sealed>> = BTreeMap::new();
        for (_, out) in r2 {
            for (recipient, sealed) in out.sealed {
                to.entry(recipient).or_default().insert(out.index, sealed);
            }
        }
        let bodies: Vec<(u16, Round3In)> = all
            .iter()
            .map(|&i| {
                (
                    i,
                    Round3In {
                        vault_id: open.vault_id,
                        round1: r1.clone(),
                        sealed: to.remove(&i).unwrap_or_default(),
                    },
                )
            })
            .collect();
        let r3: Vec<(u16, Round3Out)> =
            at_least(self.nodes.post_each(&bodies, "/v1/dkg/round3").await, n, "DKG round 3")?;
        let first = &r3[0].1;
        for (_, o) in &r3[1..] {
            ensure!(
                o.ak == first.ak
                    && o.ufvk == first.ufvk
                    && o.address == first.address
                    && o.public_key_package == first.public_key_package,
                "signers disagree on the vault keys"
            );
        }
        let vault = CoordVault {
            vault_id: open.vault_id,
            owner: open.owner,
            account: open.account,
            amount: open.amount,
            birthday: open.birthday,
            expiry_height: open.expiry_height,
            ufvk: first.ufvk.clone(),
            address: first.address.clone(),
            public_key_package: first.public_key_package.clone(),
            status: VaultStatus::AwaitingDeposit,
            decisions: vec![],
            attest_tx: None,
            outstanding: 0,
            claims: vec![],
            inflight: None,
            refunds: vec![],
            attest_delay: None,
            used_view_nonces: vec![],
        };
        let resp = OpenVaultResponse {
            vault_id: vault.vault_id,
            zip321: zip321_uri(&vault.address, vault.amount),
            address: vault.address.clone(),
            amount: vault.amount,
            expiry_height: vault.expiry_height,
        };
        self.store.lock().await.save(vault)?;
        info!(vault = %short(&resp.vault_id), "vault opened");
        Ok(resp)
    }

    // ------------------------------------------------------------------ recipients

    fn recipient_from_ua(ua: &str, salt: [u8; 32]) -> Result<Recipient> {
        let addr = keys::ironwood_receiver(&NETWORK, ua)?;
        Ok(Recipient {
            receiver: addr.to_raw_address_bytes(),
            salt,
        })
    }

    fn recipient_in(r: &Recipient) -> RecipientIn {
        RecipientIn {
            receiver: hex::encode(r.receiver),
            salt: hex::encode(r.salt),
        }
    }

    /// Third-party recipient (liquidator / market maker): reveal to the signers.
    pub async fn add_recipient(&self, ua: &str, salt: [u8; 32]) -> Result<B256> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let answers = self.nodes.post_many(&self.nodes.indices(), "/v1/recipients", &Self::recipient_in(&r)).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "recipient")?;
        Ok(B256::from(self.store.lock().await.add_recipient(r)?))
    }

    /// The owner's payout destination, validated before any Base transaction burns zZEC.
    pub async fn add_destination(
        &self,
        vault_id: B256,
        ua: &str,
        salt: [u8; 32],
        nonce: u64,
        deadline: u64,
        signature: Bytes,
    ) -> Result<B256> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let body = DestinationIn {
            recipient: Self::recipient_in(&r),
            nonce,
            deadline,
            signature,
        };
        let path = format!("/v1/vaults/{vault_id}/destination");
        let answers = self.nodes.post_many(&self.nodes.indices(), &path, &body).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "destination")?;
        Ok(B256::from(self.store.lock().await.add_recipient(r)?))
    }

    /// P5: the owner asks for a non-conforming deposit back.
    #[allow(clippy::too_many_arguments)]
    pub async fn request_refund(
        &self,
        vault_id: B256,
        note_id: B256,
        ua: &str,
        salt: [u8; 32],
        nonce: u64,
        deadline: u64,
        signature: Bytes,
    ) -> Result<()> {
        let r = Self::recipient_from_ua(ua, salt)?;
        let body = RefundIn {
            note_id,
            recipient: Self::recipient_in(&r),
            nonce,
            deadline,
            signature,
        };
        let path = format!("/v1/vaults/{vault_id}/refund");
        let answers = self.nodes.post_many(&self.nodes.indices(), &path, &body).await;
        at_least::<serde_json::Value>(answers, usize::from(MIN_SIGNERS), "refund")?;
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        v.refunds.retain(|x| x.note_id != note_id);
        v.refunds.push(Refund {
            note_id,
            recipient: r,
            done: false,
        });
        store.save(v)
    }

    /// The vault's viewing key, for the owner only.
    pub async fn viewing_key(&self, vault_id: B256, nonce: u64, deadline: u64, signature: &[u8]) -> Result<String> {
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        let msg = ViewKeyRequest {
            vaultId: vault_id,
            nonce,
            deadline,
        };
        let signer = recover_typed(&registry_domain(self.cfg.chain_id, self.cfg.registry), &msg, signature)?;
        ensure!(signer == v.owner, "not signed by the vault owner");
        ensure!(deadline >= now(), "request expired");
        ensure!(!v.used_view_nonces.contains(&nonce), "nonce already used");
        v.used_view_nonces.push(nonce);
        let ufvk = v.ufvk.clone();
        store.save(v)?;
        Ok(ufvk)
    }

    pub async fn view(&self, vault_id: B256) -> Result<VaultView> {
        let store = self.store.lock().await;
        let v = store.vaults.get(&vault_id).context("unknown vault")?;
        let non_conforming = {
            let zcash = self.zcash.lock().await;
            zcash
                .scan(&vault_id)
                .map(|s| {
                    s.unspent()
                        .into_iter()
                        .filter(|n| {
                            v.decisions.iter().any(|(p, c)| {
                                matches!(c, classify::Classification::NonConforming(_))
                                    && p.height == n.position.height
                                    && p.tx_index == n.position.tx_index
                                    && p.action_index == n.position.action_index
                            })
                        })
                        .map(|n| NoteView {
                            note_id: B256::from(zecured_policy::derive::note_id(
                                &vault_id.0,
                                &classify::NotePosition {
                                    height: n.position.height,
                                    tx_index: n.position.tx_index,
                                    action_index: n.position.action_index,
                                },
                            )),
                            amount: n.value(),
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(VaultView {
            vault_id,
            status: v.status,
            amount: v.amount,
            address: v.address.clone(),
            expiry_height: v.expiry_height,
            outstanding: v.outstanding,
            claims: v
                .claims
                .iter()
                .map(|c| ClaimView {
                    claim_id: B256::from(c.id),
                    kind: c.kind,
                    amount: c.amount,
                    paid: c.state == ClaimState::Paid,
                })
                .collect(),
            non_conforming,
            inflight: v.inflight.is_some(),
            birthday_height: v.birthday,
            tip_height: self.last_tip.load(std::sync::atomic::Ordering::Relaxed),
        })
    }

    // ------------------------------------------------------------------ threshold demo

    /// DEMO: builds a transaction sweeping every unspent note of the vault to `to` and asks
    /// all signers for round-1 commitments. Honest signers refuse (the PCZT matches no
    /// plan); signers switched to "compromised" mode accept. Never broadcasts anything.
    pub async fn demo_drain(&self, vault_id: B256, to: &str) -> Result<DrainReport> {
        let mut v = self.store.lock().await.vaults.get(&vault_id).cloned().context("unknown vault")?;
        let notes = self.notes(&mut v).await?;
        let mut inputs: Vec<ReceivedNote> = notes.lineage.clone();
        inputs.extend(notes.non_conforming.iter().map(|(_, n)| n.clone()));
        ensure!(!inputs.is_empty(), "the vault holds nothing to sweep");
        let total: u64 = inputs.iter().map(|n| n.value()).sum();
        let fee = spend::zip317_fee(inputs.len(), 1);
        let attacker = keys::ironwood_receiver(&NETWORK, to)?;
        let payments = [Payment {
            recipient: attacker,
            value: total.checked_sub(fee).context("vault too small to sweep")?,
            privacy: spend::OutputPrivacy::NoOvk,
        }];
        let fvk = fvk_of(&v)?;
        let anchor = self.zcash.lock().await.scan(&vault_id).context("vault not scanned")?.anchor();
        let refs: Vec<&ReceivedNote> = inputs.iter().collect();
        let unsigned = spend::build(&NETWORK, BlockHeight::from_u32(notes.tip + 1), &fvk, anchor, &refs, &payments)?;
        let pk = self.proving_key().await?;
        let pczt = tokio::task::spawn_blocking(move || spend::finalize_and_prove(unsigned.pczt, &pk)).await??;
        let mut sid = [0u8; 16];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut sid);
        let req = CommitRequest {
            session_id: format!("demo-{}", hex::encode(sid)),
            vault_id,
            kind: SignKind::Payout { claim_ids: vec![] },
            pczt: pczt.serialize().map_err(|e| anyhow!("serialize PCZT: {e:?}"))?,
        };
        let answers: Vec<Answer<CommitResponse>> =
            self.nodes.post_many(&self.nodes.indices(), "/v1/sign/commit", &req).await;
        let votes: Vec<DrainVote> = answers
            .into_iter()
            .map(|(index, a)| match a {
                Ok(_) => DrainVote {
                    index,
                    signed: true,
                    reason: None,
                },
                Err(e) => DrainVote {
                    index,
                    signed: false,
                    reason: Some(format!("{e:#}")),
                },
            })
            .collect();
        let signed = votes.iter().filter(|v| v.signed).count();
        info!(vault = %short(&vault_id), signed, "threshold demo: sweep attempt");
        Ok(DrainReport {
            vault_id,
            amount: total,
            threshold_reached: signed >= usize::from(MIN_SIGNERS),
            votes,
        })
    }

    /// Forwards a demo mode switch to one node.
    pub async fn set_node_mode(&self, index: u16, mode: &str, token: &str) -> Result<serde_json::Value> {
        self.nodes.post(index, "/v1/admin/mode", &serde_json::json!({ "mode": mode, "token": token })).await
    }

    /// Health of every node (unreachable nodes are reported as such).
    pub async fn node_health(&self) -> Vec<serde_json::Value> {
        let mut out = vec![];
        for i in self.nodes.indices() {
            out.push(match self.nodes.get_json(i, "/v1/health").await {
                Ok(v) => v,
                Err(_) => serde_json::json!({ "index": i, "mode": "unreachable" }),
            });
        }
        out
    }

    // ------------------------------------------------------------------ main loop

    /// One pass over Base events and every vault. Errors of one vault do not stop others.
    pub async fn tick(&self) -> Result<()> {
        if let Err(e) = self.sync_base().await {
            warn!("base sync failed: {e:#}");
        }
        let ids: Vec<B256> = self.store.lock().await.vaults.keys().copied().collect();
        for id in ids {
            if let Err(e) = self.process(&id).await {
                warn!(vault = %short(&id), "{e:#}");
            }
        }
        Ok(())
    }

    async fn sync_base(&self) -> Result<()> {
        let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
        let safe = events::safe_head(&provider).await?;
        let mut store = self.store.lock().await;
        let start = store.base_cursor.max(self.cfg.registry_from_block.saturating_sub(1)) + 1;
        if start > safe {
            return Ok(());
        }
        const MAX_RANGE: u64 = 2_000;
        let mut from = start;
        while from <= safe {
            let to = (from + MAX_RANGE - 1).min(safe);
            for ev in events::fetch(&provider, self.cfg.registry, from, to).await? {
                let Some(mut v) = store.vaults.get(&ev.vault_id).cloned() else {
                    continue;
                };
                match ev.event {
                    RegistryEvent::VaultActivated { amount, .. } => {
                        v.status = VaultStatus::Active;
                        v.outstanding = amount;
                    }
                    RegistryEvent::Claims(claims) => {
                        for c in claims {
                            if v.claims.iter().any(|x| x.id == c.id) {
                                continue;
                            }
                            // Every claim burns exactly its amount (D14: releases may be partial).
                            v.outstanding = v.outstanding.saturating_sub(c.amount);
                            v.claims.push(c);
                        }
                    }
                    RegistryEvent::PayoutConfirmed(ids) => {
                        for c in v.claims.iter_mut().filter(|c| ids.iter().any(|i| i.0 == c.id)) {
                            c.state = ClaimState::Paid;
                        }
                    }
                }
                if v.status == VaultStatus::Active
                    && v.outstanding == 0
                    && v.claims.iter().all(|c| c.state == ClaimState::Paid)
                    && !v.claims.is_empty()
                {
                    v.status = VaultStatus::Closed;
                }
                store.save(v)?;
            }
            store.set_base_cursor(to)?;
            from = to + 1;
        }
        Ok(())
    }

    async fn notes(&self, v: &mut CoordVault) -> Result<VaultNotes> {
        let rules = DepositRules {
            amount: v.amount,
            expiry_height: v.expiry_height,
            confirmations: self.cfg.confirmations,
        };
        let fvk = fvk_of(v)?;
        self.zcash.lock().await.notes(&v.vault_id, &fvk, v.birthday, &rules, &mut v.decisions).await
    }

    async fn process(&self, id: &B256) -> Result<()> {
        let mut v = self.store.lock().await.vaults.get(id).context("vault vanished")?.clone();
        let refunds_pending = v.refunds.iter().any(|r| !r.done);
        if matches!(v.status, VaultStatus::Expired | VaultStatus::Closed) && v.inflight.is_none() && !refunds_pending {
            return Ok(());
        }
        let notes = self.notes(&mut v).await?;
        self.last_tip.fetch_max(notes.tip, std::sync::atomic::Ordering::Relaxed);
        self.store.lock().await.save(v.clone())?;

        if let Some(inflight) = v.inflight.clone() {
            return self.check_inflight(v, inflight, notes.tip).await;
        }
        match v.status {
            VaultStatus::AwaitingDeposit => {
                if let Some(conforming) = &notes.conforming {
                    // D15: wait a random number of extra confirmations, drawn once per vault.
                    let delay = match v.attest_delay {
                        Some(d) => d,
                        None => {
                            let max = self.cfg.attest_delay_max_blocks;
                            let d = if max == 0 {
                                0
                            } else {
                                rand_core_06::RngCore::next_u32(&mut rand_core_06::OsRng) % (max + 1)
                            };
                            v.attest_delay = Some(d);
                            self.store.lock().await.save(v.clone())?;
                            d
                        }
                    };
                    let confs = classify::confirmations(conforming.position.height, notes.tip);
                    if confs < self.cfg.confirmations + delay {
                        return Ok(());
                    }
                    self.attest(v.clone()).await?;
                } else if notes.tip > v.expiry_height + self.cfg.confirmations {
                    v.status = VaultStatus::Expired;
                    self.store.lock().await.save(v.clone())?;
                }
            }
            VaultStatus::DepositSeen | VaultStatus::Expired => {}
            VaultStatus::Active | VaultStatus::Closed => {
                if self.pay_next(&v, &notes).await? {
                    return Ok(());
                }
            }
        }
        let v = self.store.lock().await.vaults.get(id).cloned().context("vault vanished")?;
        if v.inflight.is_none() {
            self.refund_next(&v, &notes).await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ P1 → Base

    async fn attest(&self, mut v: CoordVault) -> Result<()> {
        let path = format!("/v1/vaults/{}/attest", v.vault_id);
        let answers: Vec<Answer<AttestResponse>> =
            self.nodes.post_many(&self.nodes.indices(), &path, &serde_json::json!({})).await;
        let ok = at_least(answers, usize::from(MIN_SIGNERS), "deposit attestation")?;
        let tag = ok[0].1.deposit_tag;
        let att = DepositAttestation {
            vaultId: v.vault_id,
            account: v.account,
            amount: v.amount,
            depositTag: tag,
        };
        let sigs: Vec<Bytes> = ok
            .iter()
            .filter(|(_, a)| a.deposit_tag == tag && a.account == v.account && a.amount == v.amount)
            .map(|(_, a)| a.signature.clone())
            .collect();
        ensure!(sigs.len() >= usize::from(MIN_SIGNERS), "signers disagree on the attestation");
        let sigs = order_signatures(&registry_domain(self.cfg.chain_id, self.cfg.registry), &att, &sigs)?;

        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let call = registry.attestDeposit(
            IZecuredRegistry::DepositAttestation {
                vaultId: att.vaultId,
                account: att.account,
                amount: att.amount,
                depositTag: att.depositTag,
            },
            sigs,
        );
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate attestDeposit")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send attestDeposit")?
            .get_receipt()
            .await
            .context("attestDeposit receipt")?;
        ensure!(receipt.status(), "attestDeposit reverted");
        v.status = VaultStatus::DepositSeen;
        v.attest_tx = Some(receipt.transaction_hash);
        self.store.lock().await.save(v.clone())?;
        info!(vault = %short(&v.vault_id), "deposit attested on Base");
        Ok(())
    }

    // ------------------------------------------------------------------ payouts

    /// Returns true if a payout was started.
    async fn pay_next(&self, v: &CoordVault, notes: &VaultNotes) -> Result<bool> {
        if !v.claims.iter().any(|c| c.state == ClaimState::Unpaid) {
            return Ok(false);
        }
        ledger::check_i10(notes.lineage_value(), v.outstanding, &v.claims).map_err(|e| anyhow!("{e}"))?;
        let next = {
            let store = self.store.lock().await;
            ledger::next_payout(&v.claims, |c| store.recipient(c), notes.lineage_value(), notes.lineage.len(), now())?
        };
        let NextPayout::Pay(plan) = next else { return Ok(false) };
        let kind = SignKind::Payout {
            claim_ids: plan.claims.iter().map(|c| B256::from(*c)).collect(),
        };
        self.execute(v, kind, &plan, &notes.lineage, notes.tip).await?;
        Ok(true)
    }

    async fn refund_next(&self, v: &CoordVault, notes: &VaultNotes) -> Result<()> {
        for r in v.refunds.iter().filter(|r| !r.done) {
            let Some((_, note)) = notes.non_conforming.iter().find(|(id, _)| *id == r.note_id.0) else {
                continue;
            };
            let plan = ledger::refund_plan(note.value(), r.recipient.receiver)?;
            let kind = SignKind::Refund { note_id: r.note_id };
            return self.execute(v, kind, &plan, std::slice::from_ref(note), notes.tip).await;
        }
        Ok(())
    }

    /// Builds, proves, gets signed (3 of 5) and broadcasts the transaction for `plan`.
    async fn execute(
        &self,
        v: &CoordVault,
        kind: SignKind,
        plan: &PayoutPlan,
        inputs: &[ReceivedNote],
        tip: u32,
    ) -> Result<()> {
        let fvk = fvk_of(v)?;
        let anchor = self.zcash.lock().await.scan(&v.vault_id).context("vault not scanned")?.anchor();
        let payments: Vec<Payment> = plan
            .payments
            .iter()
            .map(|p| {
                Ok(Payment {
                    recipient: Option::from(orchard::Address::from_raw_address_bytes(&p.receiver))
                        .context("invalid receiver")?,
                    value: p.value,
                    privacy: p.privacy,
                })
            })
            .collect::<Result<_>>()?;
        let refs: Vec<&ReceivedNote> = inputs.iter().collect();
        let unsigned = spend::build(&NETWORK, BlockHeight::from_u32(tip + 1), &fvk, anchor, &refs, &payments)?;
        ensure!(
            unsigned.fee == plan.fee && unsigned.change == plan.change,
            "built transaction does not match the plan"
        );
        let pk = self.proving_key().await?;
        let pczt = tokio::task::spawn_blocking(move || spend::finalize_and_prove(unsigned.pczt, &pk)).await??;
        let mut indices = unsigned.spend_action_indices.clone();
        indices.sort_unstable();
        let (sighash, requests) = spend::signing_requests(&pczt, &indices)?;
        let expiry = *pczt.global().expiry_height();
        let pczt_bytes = pczt.clone().serialize().map_err(|e| anyhow!("serialize PCZT: {e:?}"))?;

        // Round 1 on every signer; each checks the PCZT against its own plan.
        let mut sid = [0u8; 16];
        rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut sid);
        let session_id = hex::encode(sid);
        let commit_req = CommitRequest {
            session_id: session_id.clone(),
            vault_id: v.vault_id,
            kind: kind.clone(),
            pczt: pczt_bytes,
        };
        let commits: Vec<(u16, CommitResponse)> = at_least(
            self.nodes.post_many(&self.nodes.indices(), "/v1/sign/commit", &commit_req).await,
            usize::from(MIN_SIGNERS),
            "signing round 1",
        )?;
        let chosen: Vec<(u16, CommitResponse)> = commits.into_iter().take(usize::from(MIN_SIGNERS)).collect();
        for (i, c) in &chosen {
            ensure!(c.commitments.len() == requests.len(), "signer {i} committed to a different number of spends");
        }
        let per_spend: Vec<BTreeMap<u16, serde_json::Value>> = (0..requests.len())
            .map(|k| chosen.iter().map(|(i, c)| (*i, c.commitments[k].clone())).collect())
            .collect();

        // Round 2 on the chosen signers only.
        let share_req = ShareRequest {
            session_id,
            commitments: per_spend.clone(),
        };
        let chosen_ids: Vec<u16> = chosen.iter().map(|(i, _)| *i).collect();
        let shares: Vec<(u16, ShareResponse)> = at_least(
            self.nodes.post_many(&chosen_ids, "/v1/sign/share", &share_req).await,
            chosen_ids.len(),
            "signing round 2",
        )?;

        let pkp: PublicKeyPackage = serde_json::from_value(v.public_key_package.clone())?;
        let mut signatures = Vec::with_capacity(requests.len());
        for (k, req) in requests.iter().enumerate() {
            let mut commitments = BTreeMap::new();
            for (i, c) in &per_spend[k] {
                commitments.insert(signer_id(*i)?, serde_json::from_value::<SigningCommitments>(c.clone())?);
            }
            let package = threshold::signing_package(commitments, &sighash)?;
            let mut share_map = BTreeMap::new();
            for (i, s) in &shares {
                share_map.insert(signer_id(*i)?, serde_json::from_value::<SignatureShare>(s.shares[k].clone())?);
            }
            signatures.push((req.action_index, threshold::aggregate(&package, &share_map, &pkp, &req.alpha)?));
        }
        let pczt = spend::apply_signatures(pczt, &signatures)?;
        let tx = spend::extract(pczt)?;
        let raw = spend::serialize(&tx)?;

        // Record the payout before broadcasting it: if the broadcast fails or its response is
        // lost, the same transaction is rebroadcast until it is mined or expires, and nothing
        // else is built for these claims in the meantime.
        let withhold = std::fs::remove_file(self.data_dir.join(TEST_WITHHOLD_FLAG)).is_ok();
        let mut v = v.clone();
        let (claim_ids, refund_note) = match &kind {
            SignKind::Payout { claim_ids } => (claim_ids.clone(), None),
            SignKind::Refund { note_id } => (vec![], Some(*note_id)),
        };
        v.inflight = Some(Inflight {
            claim_ids,
            refund_note,
            txid: tx.txid().to_string(),
            expiry_height: expiry,
            raw_tx: (!withhold).then(|| hex::encode(&raw)),
        });
        self.store.lock().await.save(v.clone())?;
        if withhold {
            warn!(vault = %short(&v.vault_id), expiry, "TEST: payout withheld; it will expire unmined");
            return Ok(());
        }
        match self.broadcast(raw).await {
            Ok(()) => info!(vault = %short(&v.vault_id), signers = ?chosen_ids, "payout broadcast"),
            Err(e) => warn!(vault = %short(&v.vault_id), "payout broadcast failed, will retry: {e:#}"),
        }
        Ok(())
    }

    async fn broadcast(&self, raw: Vec<u8>) -> Result<()> {
        let mut client = zecured_zcash_ops::lwd::connect(&self.cfg.lightwalletd_url).await?;
        spend::broadcast_raw(&mut client, raw).await
    }

    /// Waits for a broadcast payout; confirms it on Base once it has enough confirmations,
    /// or drops it (to be rebuilt) once it can no longer be mined.
    async fn check_inflight(&self, mut v: CoordVault, inflight: Inflight, tip: u32) -> Result<()> {
        let txid = TxId::from_hex(&inflight.txid).context("stored txid")?;
        let mined = {
            let mut client = zecured_zcash_ops::lwd::connect(&self.cfg.lightwalletd_url).await?;
            tx::fetch(&mut client, &NETWORK, &txid).await
        };
        match mined {
            Ok((_, height)) => {
                if classify::confirmations(u32::from(height), tip) < self.cfg.confirmations {
                    return Ok(());
                }
                if !inflight.claim_ids.is_empty() {
                    self.confirm_on_base(&v, &inflight).await?;
                    for c in v.claims.iter_mut().filter(|c| inflight.claim_ids.iter().any(|i| i.0 == c.id)) {
                        c.state = ClaimState::Paid;
                    }
                }
                if let Some(note) = inflight.refund_note {
                    for r in v.refunds.iter_mut().filter(|r| r.note_id == note) {
                        r.done = true;
                    }
                }
                if v.status == VaultStatus::Active
                    && v.outstanding == 0
                    && v.claims.iter().all(|c| c.state == ClaimState::Paid)
                {
                    v.status = VaultStatus::Closed;
                }
                v.inflight = None;
                self.store.lock().await.save(v)?;
                Ok(())
            }
            Err(_) if tip > inflight.expiry_height => {
                warn!(vault = %short(&v.vault_id), "payout expired unmined; will rebuild");
                v.inflight = None;
                self.store.lock().await.save(v)?;
                Ok(())
            }
            Err(_) => {
                // Not mined yet: make sure it is (still) in the mempool. Rejections such as
                // "already in the mempool" are expected and harmless.
                if let Some(raw) = inflight.raw_tx.as_deref().and_then(|h| hex::decode(h).ok()) {
                    let _ = self.broadcast(raw).await;
                }
                Ok(())
            }
        }
    }

    async fn confirm_on_base(&self, v: &CoordVault, inflight: &Inflight) -> Result<()> {
        let path = format!("/v1/vaults/{}/confirm", v.vault_id);
        let body = ConfirmIn {
            claim_ids: inflight.claim_ids.clone(),
            txid: inflight.txid.clone(),
        };
        let ok: Vec<(u16, ConfirmResponse)> = at_least(
            self.nodes.post_many(&self.nodes.indices(), &path, &body).await,
            usize::from(MIN_SIGNERS),
            "payout confirmation",
        )?;
        let msg = PayoutConfirmation {
            vaultId: v.vault_id,
            claimIdsHash: zecured_base::claim_ids_hash(&inflight.claim_ids),
        };
        let sigs: Vec<Bytes> = ok.into_iter().map(|(_, r)| r.signature).collect();
        let sigs = order_signatures(&registry_domain(self.cfg.chain_id, self.cfg.registry), &msg, &sigs)?;
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let call = registry.confirmPayout(v.vault_id, inflight.claim_ids.clone(), sigs);
        let gas = zecured_base::gas_with_headroom(call.estimate_gas().await.context("estimate confirmPayout")?);
        let receipt = call.gas(gas).send().await.context("send confirmPayout")?.get_receipt().await?;
        ensure!(receipt.status(), "confirmPayout reverted");
        info!(vault = %short(&v.vault_id), "payout confirmed on Base");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip321_amounts() {
        assert_eq!(zip321_uri("utest1x", 1_000_000), "zcash:utest1x?amount=0.01");
        assert_eq!(zip321_uri("utest1x", 5_000_000), "zcash:utest1x?amount=0.05");
        assert_eq!(zip321_uri("utest1x", 100_000_000), "zcash:utest1x?amount=1");
        assert_eq!(zip321_uri("utest1x", 123_456_789), "zcash:utest1x?amount=1.23456789");
    }
}
