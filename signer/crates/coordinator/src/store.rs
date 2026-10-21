//! Coordinator state: public vault data only (no key material).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zecured_node::store::write_atomic;
use zecured_policy::{
    classify::{Classification, NotePosition},
    ledger::{Claim, ClaimState, Commitment, Recipient},
};

use crate::error::PublicError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultStatus {
    /// Waiting for the Ironwood deposit.
    AwaitingDeposit,
    /// A deposit proof is on Base and its activation pending (K3): the Intake waits for the relay
    /// to move on before it mints.
    DepositSeen,
    /// zZEC minted into the account's Morpho position.
    Active,
    /// `outstanding` is zero and nothing is left to pay.
    Closed,
    /// No conforming deposit before the expiry height.
    Expired,
    /// Never activated and never will be (Registry v3): a deposit landed as a refund while the
    /// vault was new, or a pending activation was converted into one (T-02).
    Cancelled,
}

/// A payout broadcast to Zcash and not yet confirmed on Base.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inflight {
    /// Claims settled (empty for a P5 refund).
    pub claim_ids: Vec<B256>,
    /// P5: the deposit refunded off-chain, if any (I30: one no guest of the deployment can land).
    #[serde(default)]
    pub refund_note: Option<B256>,
    pub txid: String,
    pub expiry_height: u32,
    /// The signed transaction (hex), recorded before the first broadcast and rebroadcast until
    /// it is mined or expires: a lost broadcast response must never lead to a second payout.
    #[serde(default)]
    pub raw_tx: Option<String>,
    /// Nullifiers of the vault notes this payout spends: a spent input means the payout (or
    /// something else) was mined, so it is never rebuilt blindly (I-5). Empty in records
    /// written before this field existed.
    #[serde(default)]
    pub inputs: Vec<B256>,
    /// L23: the Zcash height from which the payout is broadcast (0: at once). Its commitment is
    /// on Base before it is signed, so broadcasting right away would point at the next block.
    #[serde(default)]
    pub broadcast_from: u32,
    /// Tier 3: the payout's commitment and the signatures it was recorded with. Every pass checks
    /// that Base still has it and submits it again if a reorg dropped it (RT-5): a mined payout
    /// whose commitment is gone would be an uncommitted spend.
    #[serde(default)]
    pub spend_commit: Option<KeptCommitment>,
}

/// A payout commitment as it was recorded on Base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptCommitment {
    pub record: zecured_node::signing::SpendCommitRecord,
    pub sigs: Vec<Bytes>,
}

/// P5: a refund the owner asked for and the signers accepted, of a deposit no guest of the
/// deployment can land on Base (I30).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refund {
    pub note_id: B256,
    pub recipient: Recipient,
    pub done: bool,
}

/// One signer node's signature, by node index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSignature {
    pub index: u16,
    pub signature: Bytes,
}

/// A payout committed on Base (`SpendCommitted`): the coordinator proves its delivery once the
/// transaction that spends its input is deep enough (section 3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedPayout {
    pub payout_ref: B256,
    pub input_tag: B256,
    pub claims: Vec<[u8; 32]>,
    /// W-1: the reference committed for each claim, in `claims`' order (empty in a record made before
    /// v4.1: then nothing of the payout is proven, see `delivery::landed_payouts`).
    #[serde(default)]
    pub claim_refs: Vec<B256>,
}

/// R-12: a refund proof waiting on the Intake until the relay has moved on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRefund {
    pub deposit_tag: [u8; 32],
    pub ready_height: u64,
    pub ready_time: u64,
}

/// R-5: a claim's liveness clock armed on the bond (`LivenessArmed`), for its commitment then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Armed {
    pub claim_id: [u8; 32],
    pub commitment: [u8; 32],
    pub payable_at: u64,
}

/// An unspent deposit of a vault that has not landed on Base: the vault's own deposit until its
/// proof lands it (`candidate`), or a payment that cannot back the loan, refunded once a proof
/// lands it as a Refund claim (T-02) or, if no guest of the deployment can land it, off-chain at
/// its owner's request (P5, I30).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteView {
    pub note_id: B256,
    pub amount: u64,
    /// P5: no guest of the deployment can land it, so its owner may ask for it back off-chain.
    #[serde(default)]
    pub offchain: bool,
    /// The vault's own deposit waiting to land, not a payment that cannot back the loan: P1 has
    /// not decided it yet (it waits for its confirmations) or decided it conforming (it waits for
    /// its proof). The app lists only the others as non-conforming.
    #[serde(default)]
    pub candidate: bool,
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
    /// P5: refunds of deposits no guest can land, as the signers accepted them (I30).
    #[serde(default)]
    pub refunds: Vec<Refund>,
    /// The claims this coordinator saw paid on Zcash: their payout mined and settled here.
    #[serde(default)]
    pub paid_here: Vec<[u8; 32]>,
    /// T-05 (a), D57: claims reopened on Base after a dispute. A later proof of their payout still
    /// upholds them; the coordinator proves the payouts it saw mined.
    #[serde(default)]
    pub reopened: Vec<[u8; 32]>,
    /// Extra confirmations before proving the deposit (D15), drawn once.
    #[serde(default)]
    pub attest_delay: Option<u32>,
    /// `ViewKeyRequest` nonces already used.
    #[serde(default)]
    pub used_view_nonces: Vec<u64>,
    /// Unspent deposits that have not landed on Base, as of the last scan. Persisted so that they
    /// stay visible after a restart and while an expired or closed vault is scanned only now and
    /// then (M-10).
    #[serde(default)]
    pub non_conforming: Vec<NoteView>,
    /// Every signer's `VaultOpened` signature for `address` and the tag key of `ufvk`, ordered by
    /// node index: what the owner checked before paying the vault (C-4), and what a deposit proof
    /// is submitted with. Empty for vaults opened before.
    #[serde(default)]
    pub opened_signatures: Vec<NodeSignature>,
    /// K2-a: the deposit tag of the `VaultActivated` event (the note that backs the vault):
    /// payouts are planned from the same lineage root as the signers'.
    #[serde(default)]
    pub activated_tag: Option<[u8; 32]>,
    /// K3: the deposit tag of a pending activation (`VaultReserved`), until it is activated,
    /// released or converted into a refund.
    #[serde(default)]
    pub pending_tag: Option<[u8; 32]>,
    /// When the vault was opened (Unix seconds; 0 for vaults opened before H-01).
    #[serde(default)]
    pub opened_at: u64,
    /// Section 3: the vault's payouts committed on Base, whose delivery this coordinator proves.
    #[serde(default)]
    pub commitments: Vec<CommittedPayout>,
    /// R-12: refund proofs of the vault's deposits pending on the Intake.
    #[serde(default)]
    pub pending_refunds: Vec<PendingRefund>,
    /// R-5: the claims whose liveness clock their claimant armed.
    #[serde(default)]
    pub armed: Vec<Armed>,
    /// O-2: the Base block of the last event applied to this vault, and when it was applied (Unix
    /// seconds): the signers act on it once their own view reaches that block.
    #[serde(default)]
    pub last_event_block: u64,
    #[serde(default)]
    pub last_event_at: u64,
    /// R-11.1: the owner asked to prove the vault's deposit in Refund mode (its activation cannot
    /// land: the coverage cap is full).
    #[serde(default)]
    pub refund_requested: bool,
    /// Nonces of the owner requests only the coordinator checks (`ProveRefund`).
    #[serde(default)]
    pub used_request_nonces: Vec<u64>,
    /// R-2: executed F2 or F4 cases about the vault's payouts, whose journals prove those payouts'
    /// inclusion (`proveInclusionByCase`), until the coordinator has used them.
    #[serde(default)]
    pub inclusion_cases: Vec<InclusionCase>,
    /// T-04: an executed F1 or F2 case compromised the vault (`VaultCompromised`).
    #[serde(default)]
    pub compromised: bool,
    /// R-19: the compensator burned the compromised vault's collateral (`CollateralBurned`).
    #[serde(default)]
    pub collateral_burned: bool,
    /// R-20: the compensator settled the compromised vault while it stays active
    /// (`VaultCompensated`).
    #[serde(default)]
    pub compensated: bool,
    /// W-2: the F1 cases opened on this vault (`ViolationProven` of kind 1), which follow their note
    /// when a deposit takes its tag over.
    #[serde(default)]
    pub f1_cases: Vec<B256>,
    /// W-2: tags of this vault's change records that a deposit took over (`ChangeTagOverridden`), whose F1
    /// cases the keeper moves to the deposit's vault (`followNote`).
    #[serde(default)]
    pub overridden_tags: Vec<B256>,
}

/// R-2: an executed case whose journal proved a committed payout of the vault in a block: an F2
/// (a withheld change) or an F4 (a wrong claim reference) names the payout `payout_ref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionCase {
    pub case_id: B256,
    pub payout_ref: B256,
}

impl CoordVault {
    /// Nothing is left to do for the vault but to watch for late deposits: no payout in flight,
    /// no claim payable nor disputed, no accepted off-chain refund waiting. A Refund claim waiting
    /// for its account's destination does not keep it busy: the destination's event does.
    pub fn is_idle(&self) -> bool {
        matches!(self.status, VaultStatus::Expired | VaultStatus::Closed | VaultStatus::Cancelled)
            && self.inflight.is_none()
            && self.to_uphold().is_empty()
            && !self.books().iter().any(|c| c.payable())
            && self.refunds.iter().all(|r| r.done)
            && self.pending_refunds.is_empty()
    }

    /// Nothing is left to pay: no claim pending or disputed (written-off dust stays in the
    /// vault, unpaid for good).
    pub fn settled(&self) -> bool {
        !self.books().iter().any(|c| matches!(c.state, ClaimState::Unpaid | ClaimState::Disputed))
    }

    /// T-05 (a): the claims whose payout this coordinator proves with a deadline: those disputed
    /// on Base, and those reopened there whose payout it saw mined (D57: a later proof still
    /// upholds them).
    pub fn to_uphold(&self) -> Vec<[u8; 32]> {
        self.claims
            .iter()
            .filter(|c| {
                c.state == ClaimState::Disputed
                    || (c.state == ClaimState::Unpaid
                        && self.reopened.contains(&c.id)
                        && self.paid_here.contains(&c.id))
            })
            .map(|c| c.id)
            .collect()
    }

    /// Section 3: claims this coordinator's inclusion proof made final on Base (the slasher upheld or
    /// confirmed each). The books mark them at once, as they will on the slasher's event: a pass
    /// before that event is ingested must not prove the same payout again.
    pub fn proven_on_base(&mut self, claims: &[[u8; 32]]) {
        for c in self.claims.iter_mut().filter(|c| claims.contains(&c.id)) {
            c.state = ClaimState::Paid;
            c.proven = true;
        }
    }

    /// Section 3: the committed claims not final on Base yet (pending, confirmed or disputed), whose
    /// payout's delivery this coordinator proves once it is mined deep enough.
    pub fn to_prove(&self) -> Vec<[u8; 32]> {
        self.claims
            .iter()
            .filter(|c| {
                !c.proven
                    && matches!(c.state, ClaimState::Unpaid | ClaimState::Paid | ClaimState::Disputed)
                    && self.commitments.iter().any(|p| p.claims.contains(&c.id))
            })
            .map(|c| c.id)
            .collect()
    }

    /// R-5: whether the claim's liveness clock is armed for its current commitment.
    pub fn armed(&self, claim: &Claim) -> bool {
        claim.commitment != [0; 32]
            && self.armed.iter().any(|a| a.claim_id == claim.id && a.commitment == claim.commitment)
    }

    /// The claims as this coordinator's books count them, and plans from: a claim it saw paid on
    /// Zcash stays paid when its confirmation is disputed or reopened on Base (T-05 (a)), exactly
    /// as it does for every node that confirmed it, so that both plan the same payouts. Its value
    /// left the vault with that payout.
    pub fn books(&self) -> Vec<Claim> {
        self.claims
            .iter()
            .cloned()
            .map(|mut c| {
                if self.paid_here.contains(&c.id) {
                    c.state = ClaimState::Paid;
                }
                c
            })
            .collect()
    }

    /// A conforming deposit was classified (P1): it is mined and has its confirmations.
    pub fn deposit_seen(&self) -> bool {
        self.decisions.iter().any(|(_, c)| *c == Classification::Conforming)
    }

    /// H-01: whether the vault holds one of the slots of vaults waiting for their deposit: for
    /// `slot_secs` after it opened, or longer once its deposit is seen. Only the count changes: the
    /// vault itself waits for its deposit, is scanned and takes a late one exactly as before.
    pub fn holds_slot(&self, now: u64, slot_secs: u64) -> bool {
        self.status == VaultStatus::AwaitingDeposit
            && (now < self.opened_at.saturating_add(slot_secs) || self.deposit_seen())
    }
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
        // Vault records hold public data only, but salts sit in recipients.json: private directories.
        zecured_node::store::private_dir(dir)?;
        zecured_node::store::private_dir(&dir.join("vaults"))?;
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

    /// Read-modify-write of one vault record (L-5): `f` sees the current record, so changes
    /// made by others since the caller's last read (view-key nonces, Base events) are
    /// kept. The record changes in memory only once it is on disk.
    pub fn update<R>(&mut self, id: &B256, f: impl FnOnce(&mut CoordVault) -> R) -> Result<R> {
        let mut v = self.vaults.get(id).cloned().context(PublicError::UnknownVault)?;
        let r = f(&mut v);
        self.save(v)?;
        Ok(r)
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

    /// H-01: vaults holding a slot among those waiting for their deposit ([`CoordVault::holds_slot`]).
    pub fn holding_slots(&self, now: u64, slot_secs: u64) -> usize {
        self.vaults.values().filter(|v| v.holds_slot(now, slot_secs)).count()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A temporary directory removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let mut r = [0u8; 8];
            rand_core_06::RngCore::fill_bytes(&mut rand_core_06::OsRng, &mut r);
            let p = std::env::temp_dir().join(format!("zecured-coord-{tag}-{}", hex::encode(r)));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn vault(id: u8) -> CoordVault {
        CoordVault {
            vault_id: B256::repeat_byte(id),
            owner: Address::repeat_byte(id),
            account: Address::repeat_byte(id.wrapping_add(100)),
            amount: 1_000_000,
            birthday: 100,
            expiry_height: 388,
            ufvk: String::new(),
            address: String::new(),
            public_key_package: serde_json::Value::Null,
            status: VaultStatus::AwaitingDeposit,
            decisions: vec![],
            attest_tx: None,
            outstanding: 0,
            claims: vec![],
            inflight: None,
            refunds: vec![],
            paid_here: vec![],
            reopened: vec![],
            attest_delay: None,
            used_view_nonces: vec![],
            non_conforming: vec![],
            opened_signatures: vec![],
            activated_tag: None,
            pending_tag: None,
            opened_at: 0,
            commitments: vec![],
            pending_refunds: vec![],
            armed: vec![],
            last_event_block: 0,
            last_event_at: 0,
            refund_requested: false,
            used_request_nonces: vec![],
            inclusion_cases: vec![],
            compromised: false,
            collateral_burned: false,
            compensated: false,
            f1_cases: vec![],
            overridden_tags: vec![],
        }
    }

    /// A pending claim of `amount`, with a destination.
    fn claim(id: u8, amount: u64) -> Claim {
        Claim {
            id: [id; 32],
            kind: zecured_policy::ledger::ClaimKind::Liquidation,
            amount,
            commitment: [id; 32],
            order: (1, u64::from(id)),
            created_at: 0,
            partner: None,
            state: ClaimState::Unpaid,
            refund_tag: None,
            bond_paid: false,
            proven: false,
            confirmed_at: 0,
        }
    }

    /// L-5: a pass that read the vault before a long scan must not erase a claim (or a view-key
    /// nonce) added meanwhile.
    #[test]
    fn updates_keep_concurrent_changes() {
        let dir = TempDir::new("update");
        let mut store = Store::open(&dir.0).unwrap();
        store.save(vault(1)).unwrap();
        let id = B256::repeat_byte(1);

        let stale = store.vaults[&id].clone(); // a pass starts its scan
        store.update(&id, |v| v.claims.push(claim(9, 20_000))).unwrap(); // a Base event lands
        store.update(&id, |v| v.used_view_nonces.push(7)).unwrap(); // a view-key request lands
        let notes = vec![NoteView {
            note_id: B256::repeat_byte(9),
            amount: 20_000,
            offchain: false,
            candidate: false,
        }];
        // The pass applies only what its scan produced.
        store.update(&id, |v| v.non_conforming = notes.clone()).unwrap();
        assert!(stale.claims.is_empty());

        let reopened = Store::open(&dir.0).unwrap();
        let v = &reopened.vaults[&id];
        assert_eq!(v.claims.len(), 1);
        assert_eq!(v.used_view_nonces, vec![7]);
        assert_eq!(v.non_conforming, notes, "unlanded deposits survive a restart (M-10)");
        assert!(store.update(&B256::repeat_byte(2), |_| ()).is_err());
    }

    /// Section 3: once Base took this coordinator's inclusion proof, its claims leave both lists at
    /// once (in the second e2e round a pass before the slasher's event was ingested proved a
    /// disputed payout again); the other claims stay.
    #[test]
    fn a_proof_taken_on_base_is_not_made_again() {
        let mut v = vault(4);
        let mut disputed = claim(1, 500_000);
        disputed.state = ClaimState::Disputed;
        let mut confirmed = claim(2, 300_000);
        confirmed.state = ClaimState::Paid;
        let other = claim(3, 200_000);
        v.claims = vec![disputed, confirmed, other];
        v.commitments = vec![CommittedPayout {
            payout_ref: B256::repeat_byte(7),
            input_tag: B256::repeat_byte(8),
            claims: vec![[1; 32], [2; 32], [3; 32]],
            claim_refs: vec![],
        }];
        assert_eq!(v.to_uphold(), vec![[1; 32]]);
        assert_eq!(v.to_prove(), vec![[1; 32], [2; 32], [3; 32]]);
        v.proven_on_base(&[[1; 32], [2; 32]]);
        assert!(v.to_uphold().is_empty());
        assert_eq!(v.to_prove(), vec![[3; 32]]);
        assert!(v.claims[..2].iter().all(|c| c.proven && c.state == ClaimState::Paid));
    }

    #[test]
    fn old_records_load_with_defaults() {
        let dir = TempDir::new("compat");
        let mut v = serde_json::to_value(vault(3)).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("non_conforming");
        obj.remove("opened_signatures");
        obj.insert(
            "inflight".into(),
            serde_json::json!({ "claim_ids": [], "refund_note": null, "txid": "00", "expiry_height": 5 }),
        );
        fs::create_dir_all(dir.0.join("vaults")).unwrap();
        fs::write(dir.0.join("vaults").join("x.json"), serde_json::to_vec(&v).unwrap()).unwrap();
        let store = Store::open(&dir.0).unwrap();
        let v = &store.vaults[&B256::repeat_byte(3)];
        assert!(v.non_conforming.is_empty());
        assert!(v.opened_signatures.is_empty(), "vaults opened before C-4 have no signatures");
        assert!(v.inflight.as_ref().unwrap().inputs.is_empty());
    }

    #[test]
    fn idle_vaults_and_pending_count() {
        let dir = TempDir::new("idle");
        let mut store = Store::open(&dir.0).unwrap();
        let mut v = vault(1);
        assert!(!v.is_idle());
        v.status = VaultStatus::Cancelled;
        assert!(v.is_idle());
        // A refund without its destination does not keep it busy; with one, it does.
        let mut refund = Claim {
            kind: zecured_policy::ledger::ClaimKind::Refund,
            commitment: [0; 32],
            ..claim(2, 50_000)
        };
        v.claims.push(refund.clone());
        assert!(v.is_idle() && !v.settled());
        refund.commitment = [2; 32];
        v.claims[0] = refund;
        assert!(!v.is_idle(), "a payable refund keeps it busy");
        store.save(v).unwrap();
        let mut waiting = vault(2);
        waiting.opened_at = 1_000;
        store.save(waiting).unwrap();
        assert_eq!(store.holding_slots(1_000, 1_800), 1);
    }

    /// H-01: a vault without a deposit releases its slot `slot_secs` after it opened, one with a
    /// conforming deposit keeps it, and any other status holds none. The released vault itself is
    /// unchanged: still waiting for its deposit and scanned on every pass (not idle), so a deposit
    /// that comes later is taken exactly as before.
    #[test]
    fn a_vault_without_a_deposit_releases_its_slot() {
        let position = NotePosition {
            height: 4_486_100,
            tx_index: 1,
            action_index: 0,
        };
        let opened = 1_700_000_000;
        let mut v = vault(1);
        v.opened_at = opened;
        assert!(v.holds_slot(opened, 1_800));
        assert!(v.holds_slot(opened + 1_799, 1_800));
        assert!(!v.holds_slot(opened + 1_800, 1_800), "released after 30 minutes");
        assert_eq!(v.status, VaultStatus::AwaitingDeposit);
        assert!(!v.is_idle(), "still scanned on every pass");
        // A non-conforming payment does not hold the slot; a conforming deposit does.
        let wrong_amount = zecured_policy::classify::NonConformingReason::SecondPayment;
        v.decisions.push((position, Classification::NonConforming(wrong_amount)));
        assert!(!v.holds_slot(opened + 3_600, 1_800));
        let later = NotePosition {
            height: position.height + 1,
            ..position
        };
        v.decisions.push((later, Classification::Conforming));
        assert!(v.deposit_seen() && v.holds_slot(opened + 3_600, 1_800));
        for status in [
            VaultStatus::DepositSeen,
            VaultStatus::Active,
            VaultStatus::Expired,
            VaultStatus::Cancelled,
        ] {
            v.status = status;
            assert!(!v.holds_slot(opened, 1_800), "{status:?}");
        }
        // Vaults opened before H-01 (no opening time) hold no slot unless their deposit is seen.
        assert!(!vault(2).holds_slot(opened, 1_800));
    }
}
