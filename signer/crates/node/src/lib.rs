//! A Zecured signer node: one of five FROST participants per vault.
//!
//! The node never trusts the coordinator. It checks owner requests itself (P0), classifies
//! deposits on its own Zcash view (P1), reads Base events at the safe head, recomputes every
//! payout plan and verifies the PCZT against it before contributing a signature share.

pub mod api;
pub mod auth;
pub mod chain;
pub mod config;
pub mod dkg;
pub mod envelope;
pub mod signing;
pub mod store;

use std::{
    collections::HashMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::primitives::{B256, Bytes};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zecured_base::{
    DepositAttestation, Destination, PayoutConfirmation, RefundRequest, claim_ids_hash, recover_typed,
    registry_domain, sign_typed,
};
use zecured_policy::{
    classify::Classification,
    derive::deposit_tag,
    ledger::{ClaimState, Recipient, commitment},
};
use zcash_protocol::TxId;

use crate::{
    chain::ZcashView,
    config::{Identity, NetworkConfig},
    signing::{CommitRequest, CommitResponse, Session, ShareRequest, ShareResponse},
    store::{RefundRecord, Store},
};

/// Demo switch for the threshold demonstration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Enforce the policy.
    Normal,
    /// DEMO: sign any PCZT without checks (a compromised signer).
    Compromised,
    /// DEMO: refuse to answer (a crashed or partitioned signer).
    Offline,
}

/// A payout this node signed and will confirm once mined.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inflight {
    pub claim_ids: Vec<[u8; 32]>,
    pub inputs: Vec<[u8; 32]>,
}

pub struct Node {
    pub cfg: NetworkConfig,
    pub id: Identity,
    pub store: Mutex<Store>,
    pub zcash: Mutex<ZcashView>,
    pub pending_dkg: Mutex<HashMap<B256, dkg::Pending>>,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub mode: Mutex<Mode>,
    pub admin_token: Option<String>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Base64 (standard alphabet) serde helpers for binary fields in the JSON API.
pub mod b64 {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttestResponse {
    pub index: u16,
    pub account: alloy::primitives::Address,
    pub amount: u64,
    pub deposit_tag: B256,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecipientIn {
    /// Raw 43-byte Orchard-protocol receiver, hex.
    pub receiver: String,
    /// 32-byte salt, hex.
    pub salt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationIn {
    #[serde(flatten)]
    pub recipient: RecipientIn,
    pub nonce: u64,
    pub deadline: u64,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefundIn {
    pub note_id: B256,
    #[serde(flatten)]
    pub recipient: RecipientIn,
    pub nonce: u64,
    pub deadline: u64,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmIn {
    pub claim_ids: Vec<B256>,
    pub txid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmResponse {
    pub index: u16,
    pub signature: Bytes,
}

fn parse_recipient(r: &RecipientIn) -> Result<Recipient> {
    let receiver: [u8; 43] = hex::decode(r.receiver.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("receiver must be 43 bytes"))?;
    ensure!(
        bool::from(orchard::Address::from_raw_address_bytes(&receiver).is_some()),
        "not a valid Orchard-protocol (Ironwood) receiver"
    );
    let salt: [u8; 32] = hex::decode(r.salt.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow!("salt must be 32 bytes"))?;
    Ok(Recipient { receiver, salt })
}

impl Node {
    pub fn open(cfg: NetworkConfig, id: Identity, data_dir: &Path, admin_token: Option<String>) -> Result<Self> {
        let store = Store::open(&data_dir.join("store"))?;
        let zcash = ZcashView::new(&cfg.lightwalletd_url);
        Ok(Self {
            cfg,
            id,
            store: Mutex::new(store),
            zcash: Mutex::new(zcash),
            pending_dkg: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            mode: Mutex::new(Mode::Normal),
            admin_token,
        })
    }

    pub async fn mode(&self) -> Mode {
        *self.mode.lock().await
    }

    fn domain(&self) -> alloy::sol_types::Eip712Domain {
        registry_domain(self.cfg.chain_id, self.cfg.registry)
    }

    // ----- vault creation -----

    pub async fn dkg_round1(&self, open: dkg::VaultOpen) -> Result<dkg::Round1Out> {
        let tip = self.zcash.lock().await.tip().await?;
        {
            let store = self.store.lock().await;
            dkg::check_open(&self.cfg, &store, &open, now(), tip)?;
        }
        let vault_id = open.vault_id;
        let (pending, out) = dkg::round1(self.id.index, open)?;
        self.pending_dkg.lock().await.insert(vault_id, pending);
        Ok(out)
    }

    pub async fn dkg_round2(&self, input: dkg::Round2In) -> Result<dkg::Round2Out> {
        let mut pending = self.pending_dkg.lock().await;
        let p = pending.get_mut(&input.vault_id).context("no DKG in progress for this vault")?;
        dkg::round2(&self.cfg, &self.id.x25519, self.id.index, p, &input)
    }

    pub async fn dkg_round3(&self, input: dkg::Round3In) -> Result<dkg::Round3Out> {
        let p = self
            .pending_dkg
            .lock()
            .await
            .remove(&input.vault_id)
            .context("no DKG in progress for this vault")?;
        let mut store = self.store.lock().await;
        dkg::round3(&self.cfg, &self.id.x25519, self.id.index, p, &input, &mut store)
    }

    // ----- P1 -----

    pub async fn attest(&self, vault_id: B256) -> Result<AttestResponse> {
        let mut store = self.store.lock().await;
        let notes = self
            .zcash
            .lock()
            .await
            .vault_notes(&mut store, &vault_id, self.cfg.confirmations)
            .await?;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        let note = notes.conforming.context("no conforming deposit (yet)")?;
        let nullifier = note.nullifier.to_bytes();
        let tag = deposit_tag(&v.sk_bytes()?, &nullifier);
        let msg = DepositAttestation {
            vaultId: vault_id,
            account: v.account,
            amount: v.amount,
            depositTag: B256::from(tag),
        };
        let signature = sign_typed(&self.id.eth, &self.domain(), &msg)?;
        if v.attestation.is_none() {
            v.attestation = Some(store::AttestationRecord {
                position: v
                    .decisions
                    .iter()
                    .find(|(_, c)| *c == Classification::Conforming)
                    .map(|(p, _)| *p)
                    .context("conforming decision missing")?,
                nullifier,
                deposit_tag: tag,
            });
            store.save_vault(v.clone())?;
        }
        Ok(AttestResponse {
            index: self.id.index,
            account: v.account,
            amount: v.amount,
            deposit_tag: B256::from(tag),
            signature,
        })
    }

    // ----- recipients and owner requests (P0) -----

    pub async fn add_recipient(&self, r: RecipientIn) -> Result<B256> {
        let recipient = parse_recipient(&r)?;
        Ok(B256::from(self.store.lock().await.add_recipient(recipient)?))
    }

    pub async fn destination(&self, vault_id: B256, d: DestinationIn) -> Result<B256> {
        let recipient = parse_recipient(&d.recipient)?;
        let c = recipient.commitment();
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        let msg = Destination {
            vaultId: vault_id,
            commitment: B256::from(c),
            nonce: d.nonce,
            deadline: d.deadline,
        };
        ensure!(recover_typed(&self.domain(), &msg, &d.signature)? == v.owner, "not signed by the vault owner");
        ensure!(d.deadline >= now(), "request expired");
        ensure!(v.use_nonce(d.nonce), "nonce already used");
        store.save_vault(v)?;
        store.add_recipient(recipient)?;
        Ok(B256::from(c))
    }

    pub async fn refund_request(&self, vault_id: B256, r: RefundIn) -> Result<()> {
        let recipient = parse_recipient(&r.recipient)?;
        let mut store = self.store.lock().await;
        let notes = self
            .zcash
            .lock()
            .await
            .vault_notes(&mut store, &vault_id, self.cfg.confirmations)
            .await?;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        let msg = RefundRequest {
            vaultId: vault_id,
            noteId: r.note_id,
            commitment: B256::from(commitment(&recipient.salt, &recipient.receiver)),
            nonce: r.nonce,
            deadline: r.deadline,
        };
        ensure!(recover_typed(&self.domain(), &msg, &r.signature)? == v.owner, "not signed by the vault owner");
        ensure!(r.deadline >= now(), "request expired");
        ensure!(
            notes.non_conforming.iter().any(|(id, _)| *id == r.note_id.0),
            "note is not an unspent non-conforming deposit"
        );
        ensure!(v.use_nonce(r.nonce), "nonce already used");
        v.refunds.retain(|x| x.note_id != r.note_id.0);
        v.refunds.push(RefundRecord {
            note_id: r.note_id.0,
            recipient,
        });
        store.save_vault(v)?;
        Ok(())
    }

    // ----- signing -----

    pub async fn sign_commit(&self, req: CommitRequest) -> Result<CommitResponse> {
        let mode = self.mode().await;
        let mut store = self.store.lock().await;
        let (session, commitments) = if mode == Mode::Compromised {
            let v = store.vaults.get(&req.vault_id).context("unknown vault")?;
            signing::commit_unchecked(v, &req)?
        } else {
            let notes = self
                .zcash
                .lock()
                .await
                .vault_notes(&mut store, &req.vault_id, self.cfg.confirmations)
                .await?;
            let v = store.vaults.get(&req.vault_id).context("unknown vault")?;
            let (plan, inputs) = signing::expected_plan(&store, v, &notes, &req.kind, now())?;
            signing::commit(v, &plan, &inputs, notes.tip, &req)?
        };
        let commitments = commitments
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        self.sessions.lock().await.insert(req.session_id.clone(), session);
        Ok(CommitResponse {
            index: self.id.index,
            commitments,
        })
    }

    pub async fn sign_share(&self, req: ShareRequest) -> Result<ShareResponse> {
        let session = self
            .sessions
            .lock()
            .await
            .remove(&req.session_id)
            .context("unknown or finished signing session")?;
        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&session.vault_id).context("unknown vault")?.clone();
        if let signing::SignKind::Payout { claim_ids } = &session.kind {
            v.inflight = Some(Inflight {
                claim_ids: claim_ids.iter().map(|c| c.0).collect(),
                inputs: session.inputs.clone(),
            });
            store.save_vault(v.clone())?;
        }
        let shares = signing::share(self.id.index, &v, session, &req)?;
        Ok(ShareResponse {
            index: self.id.index,
            shares,
        })
    }

    /// Signs `PayoutConfirmation` once the payout this node signed is mined and confirmed.
    pub async fn confirm(&self, vault_id: B256, c: ConfirmIn) -> Result<ConfirmResponse> {
        let inflight = {
            let store = self.store.lock().await;
            store
                .vaults
                .get(&vault_id)
                .context("unknown vault")?
                .inflight
                .clone()
                .context("no payout in flight for this vault")?
        };
        let wanted: Vec<[u8; 32]> = c.claim_ids.iter().map(|x| x.0).collect();
        ensure!(wanted == inflight.claim_ids, "claims differ from the payout we signed");
        let txid = TxId::from_hex(&c.txid).context("invalid txid")?;
        let (confs, all_spent) = self.zcash.lock().await.spends(&txid, &inflight.inputs).await?;
        ensure!(all_spent, "transaction does not spend the vault notes of this payout");
        ensure!(confs >= self.cfg.confirmations, "payout has {confs} confirmations");

        let mut store = self.store.lock().await;
        let mut v = store.vaults.get(&vault_id).context("unknown vault")?.clone();
        for claim in v.claims.iter_mut().filter(|x| wanted.contains(&x.id)) {
            claim.state = ClaimState::Paid;
        }
        v.inflight = None;
        store.save_vault(v)?;
        let msg = PayoutConfirmation {
            vaultId: vault_id,
            claimIdsHash: claim_ids_hash(&c.claim_ids),
        };
        Ok(ConfirmResponse {
            index: self.id.index,
            signature: sign_typed(&self.id.eth, &self.domain(), &msg)?,
        })
    }

    // ----- background -----

    /// Applies new Registry events (up to Base's safe head).
    pub async fn sync_base(&self) -> Result<usize> {
        let mut store = self.store.lock().await;
        chain::ingest_base(&mut store, &self.cfg.base_rpc_url, self.cfg.registry, self.cfg.registry_from_block).await
    }

    pub async fn set_mode(&self, mode: Mode, token: &str) -> Result<Mode> {
        match &self.admin_token {
            Some(t) if t == token => {}
            _ => bail!("unauthorized"),
        }
        *self.mode.lock().await = mode;
        Ok(mode)
    }
}
