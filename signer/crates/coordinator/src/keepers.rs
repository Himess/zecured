//! Keyless keepers of v4: calls anyone may make, which the contracts check in full. The coordinator
//! makes them so that nothing waits for a stranger. Each is simulated first and sent only when Base
//! would take it and it changes something; a refusal is tried again after [`KEEPER_RETRY`], and no
//! failure here ever stands in the way of a payout.
//!
//! - R-3 (`expireConfirmation`): a confirmation by signatures nobody disputed within the dispute
//!   window is final. One made within the window before its vault froze is not (the Registry
//!   refuses it: only a proof of its payout, or the compensator, settles it). Expired, no
//!   confirmation of a compromised vault holds the compensator's tally (`unprovenConfirmations`),
//!   and none waits for a proof.
//! - R-2 (`proveInclusionByCase`): an executed F2 or F4 case proved its committed payout is in a
//!   block; its journal makes that payout's claims final, without a proof of the coordinator's.
//! - R-19, W-12 (`burnCollateral`): a compromised vault's collateral is burned down to the LLTV
//!   inside `executeSlash`, within a fixed gas budget and at that moment's price. While the vault is
//!   active the keeper simulates the burn again every [`KEEPER_RETRY`] and runs it whenever it would
//!   retire something (a failed burn, or a price that rose and freed collateral above the LLTV).
//! - W-4 (`armLiveness`): every claim the bonds may pay is armed with its earliest receipt as soon
//!   as a node gives one, so that no claim is armed late (a late arming protects in full the
//!   commitments made before it, so its claimant gains nothing by waiting).
//! - W-2 (`followNote`): an F1 case about a note whose tag a deposit took over from this vault's
//!   (false) change record moves to the deposit's vault at once.
//!
//! One more keeper is off-chain: W-6, a bond-paid claim's treasury recipient, derived from the
//! signers' treasury keys, is registered with the nodes once three signers gave Base its commitment
//! (the payout is then planned and verified like any other). A destination that is not the derived
//! one is never registered: the claim waits, and the operators are warned.

use std::time::{Duration, Instant};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, anyhow, ensure};
use tracing::{debug, info, warn};
use zcash_protocol::TxId;
use zecured_base::{
    events::{ISignerBond, IZecuredCompensator, IZecuredRegistry, IZecuredSlasher, RegistryEvent, VaultEvent},
    gas_with_headroom,
};
use zecured_policy::{
    derive::payout_ref,
    ledger::{ClaimState, claim_ref},
};

use crate::{
    Coordinator, RECEIPT_TIMEOUT,
    error::scrubbed,
    log_failure, now, short,
    store::{CoordVault, InclusionCase, VaultStatus},
};

/// Seconds past the end of a confirmation's dispute window before it is expired: the coordinator's
/// clock against Base's.
pub(crate) const EXPIRY_MARGIN_SECS: u64 = 120;
/// How long a keeper waits before it tries one vault again after Base refused its call (or the call
/// would have changed nothing).
pub(crate) const KEEPER_RETRY: Duration = Duration::from_secs(600);

/// The keepers, for their retry clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Keeper {
    Expiry,
    Cases,
    Burn,
    Arm,
    Follow,
    Treasury,
}

/// R-3: the vault's confirmations by signatures (paid, not final) whose dispute window of `window`
/// seconds ended, by the margin, before `now`.
pub(crate) fn expirable(v: &CoordVault, window: u64, now: u64) -> Vec<B256> {
    v.claims
        .iter()
        .filter(|c| {
            c.state == ClaimState::Paid
                && !c.proven
                && c.confirmed_at != 0
                && now > c.confirmed_at.saturating_add(window).saturating_add(EXPIRY_MARGIN_SECS)
        })
        .map(|c| B256::from(c.id))
        .collect()
}

/// R-2: the claims an executed case's journal may make final: those of its payout that are not
/// final yet, from the payout's commitment as this coordinator recorded it, once the vault's scan
/// shows the payout's transaction (`spent`). W-1: only the claims committed under the reference
/// their claimant computes (`keccak256(salt || txid)`, `salt_of` by claim id): a hidden claim is the
/// compensator's. A payout without a record here proves nothing yet (never every claim of the vault).
pub(crate) fn case_claims(
    v: &CoordVault,
    case: &InclusionCase,
    spent: &[([u8; 32], TxId, u32)],
    salt_of: impl Fn(&[u8; 32]) -> Option<[u8; 32]>,
) -> Vec<B256> {
    let open = |id: &[u8; 32]| {
        v.claims.iter().any(|c| {
            c.id == *id && !c.proven && matches!(c.state, ClaimState::Unpaid | ClaimState::Paid | ClaimState::Disputed)
        })
    };
    let Some(c) = v.commitments.iter().find(|c| c.payout_ref == case.payout_ref) else {
        return vec![];
    };
    let Some(&(_, txid, _)) = spent.iter().find(|(tag, ..)| B256::from(*tag) == c.input_tag) else {
        return vec![];
    };
    if B256::from(payout_ref(&v.ufvk, txid.as_ref())) != c.payout_ref {
        return vec![];
    }
    c.claims
        .iter()
        .enumerate()
        .filter(|(_, id)| open(id))
        .filter(|(i, id)| match (c.claim_refs.get(*i), salt_of(id)) {
            (Some(committed), Some(salt)) => *committed == B256::from(claim_ref(&salt, txid.as_ref())),
            _ => false,
        })
        .map(|(_, id)| B256::from(*id))
        .collect()
}

/// R-19, W-12: whether the compromised vault's burn should be simulated again: compromised and still
/// active (the simulation tells whether it would retire anything now).
pub(crate) fn burn_due(v: &CoordVault) -> bool {
    v.compromised && v.status == VaultStatus::Active
}

/// W-4: the vault's claims the bonds may pay that no receipt armed for their current destination.
pub(crate) fn unarmed(v: &CoordVault) -> Vec<B256> {
    v.claims
        .iter()
        .filter(|c| c.payable() && c.kind.liveness_covered() && !c.bond_paid)
        .filter(|c| !v.armed.iter().any(|a| a.claim_id == c.id && a.commitment == c.commitment))
        .map(|c| B256::from(c.id))
        .collect()
}

/// W-6: the vault's bond-paid claims with a destination on Base whose recipient is not `known` here
/// (claim id, commitment).
pub(crate) fn unregistered_reimbursements(
    v: &CoordVault,
    known: impl Fn(&[u8; 32]) -> bool,
) -> Vec<([u8; 32], [u8; 32])> {
    v.claims
        .iter()
        .filter(|c| c.bond_paid && c.payable() && !known(&c.commitment))
        .map(|c| (c.id, c.commitment))
        .collect()
}

/// R-2: an executed case's event names neither its vault nor its kind: they are read from the
/// slasher (`caseOf`), with the payout it names (its `ref`). Done before the events are applied,
/// without the store's lock. A case that cannot be read stays unresolved (no vault: not applied)
/// rather than stop the coordinator's view of Base: the payout's delivery is still proven the
/// usual way (section 3); this only spares that proof.
pub(crate) async fn resolve_cases<P: Provider>(provider: &P, slasher: Address, evs: &mut [VaultEvent]) {
    for ev in evs.iter_mut() {
        let RegistryEvent::CaseExecuted { case_id, .. } = ev.event else {
            continue;
        };
        if slasher == Address::ZERO {
            continue;
        }
        match IZecuredSlasher::new(slasher, provider).caseOf(case_id).call().await {
            Ok(case) => {
                ev.vault_id = case.vaultId;
                ev.event = RegistryEvent::CaseExecuted {
                    case_id,
                    kind: case.kind,
                    payout_ref: case.reference,
                };
            }
            Err(e) => {
                warn!("an executed case could not be read: its payout is proven the usual way");
                debug!(error = %scrubbed(&anyhow!(e)), "slasher.caseOf");
            }
        }
    }
}

impl Coordinator {
    /// The v4 keepers of one vault; their failures are logged, never returned.
    pub(crate) async fn keep(&self, v: &CoordVault) {
        if let Err(e) = self.expire_confirmations(v).await {
            log_failure(&v.vault_id, "expiring a confirmation", &e);
        }
        if let Err(e) = self.prove_by_cases(v).await {
            log_failure(&v.vault_id, "proving a payout by an executed case", &e);
        }
        if let Err(e) = self.burn_again(v).await {
            log_failure(&v.vault_id, "burning a compromised vault's collateral", &e);
        }
        if let Err(e) = self.arm_claims(v).await {
            log_failure(&v.vault_id, "arming a claim's liveness", &e);
        }
        if let Err(e) = self.follow_notes(v).await {
            log_failure(&v.vault_id, "moving an F1 case with its note", &e);
        }
        if let Err(e) = self.register_reimbursements(v).await {
            log_failure(&v.vault_id, "registering a reimbursement recipient", &e);
        }
    }

    /// Whether `keeper` may act for `vault` now, and if so marks it as acting: once per
    /// [`KEEPER_RETRY`] at most.
    pub(crate) fn keeper_due(&self, vault: &B256, keeper: Keeper) -> bool {
        let mut tries = self.keeper_tries.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if tries.get(&(*vault, keeper)).is_some_and(|at| now.duration_since(*at) < KEEPER_RETRY) {
            return false;
        }
        tries.insert((*vault, keeper), now);
        true
    }

    /// The compensator the Registry names (immutable, read once); zero without one.
    pub(crate) async fn compensator(&self) -> Result<Address> {
        let compensator = self
            .compensator
            .get_or_try_init(|| async {
                let provider = ProviderBuilder::new().connect_http(self.cfg.base_rpc_url.parse()?);
                IZecuredRegistry::new(self.cfg.registry, &provider)
                    .compensator()
                    .call()
                    .await
                    .context("Registry.compensator")
            })
            .await?;
        Ok(*compensator)
    }

    /// R-3: expires the vault's confirmations whose dispute window has ended.
    pub(crate) async fn expire_confirmations(&self, v: &CoordVault) -> Result<()> {
        let Some(window) = self.dispute_window.get().copied() else {
            return Ok(()); // read at the start of each pass (`check_commit_grace`)
        };
        let due = expirable(v, window, now());
        if due.is_empty() || !self.keeper_due(&v.vault_id, Keeper::Expiry) {
            return Ok(());
        }
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let registry = IZecuredRegistry::new(self.cfg.registry, &provider);
        let open = registry.unprovenConfirmations(v.vault_id).call().await.context("Registry.unprovenConfirmations")?;
        if open.is_zero() {
            return Ok(()); // settled on Base already; its events have not reached the safe head
        }
        for claim in due {
            let call = registry.expireConfirmation(claim);
            if let Err(e) = call.call().await {
                // Proven or disputed meanwhile, its window still open by Base's clock, or made
                // shortly before a theft case (R-3): that one only a proof or the compensator settles.
                debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "a confirmation does not expire now");
                continue;
            }
            let gas = gas_with_headroom(call.estimate_gas().await.context("estimate expireConfirmation")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send expireConfirmation")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("expireConfirmation receipt")?;
            ensure!(receipt.status(), "expireConfirmation reverted");
            info!(vault = %short(&v.vault_id), "a confirmation nobody disputed in time is final now (R-3)");
        }
        Ok(())
    }

    /// R-2: makes final, by each executed F2 or F4 case of the vault, its payout's claims.
    pub(crate) async fn prove_by_cases(&self, v: &CoordVault) -> Result<()> {
        if v.inclusion_cases.is_empty() || !self.keeper_due(&v.vault_id, Keeper::Cases) {
            return Ok(());
        }
        // W-1: the payouts' transactions from the vault's scan (none yet: no claim is proven by a
        // case), and the claims' salts (never logged).
        let spent = {
            let zcash = self.zcash.lock().await;
            zcash
                .scan(&v.vault_id)
                .map(|scan| crate::delivery::spent_notes(&v.ufvk, &scan.notes))
                .unwrap_or_default()
        };
        let salts: Vec<([u8; 32], [u8; 32])> = {
            let store = self.store.lock().await;
            v.claims.iter().filter_map(|c| Some((c.id, store.recipient(&c.commitment)?.salt))).collect()
        };
        let salt_of = |id: &[u8; 32]| salts.iter().find(|(c, _)| c == id).map(|(_, s)| *s);
        let todo = v.inclusion_cases.iter().map(|case| (case.case_id, case_claims(v, case, &spent, salt_of))).collect();
        self.prove_cases(v, todo).await
    }

    /// R-2: `proveInclusionByCase` for each executed case and the claims it may make final; a case
    /// with none, or whose claims are all final already, is used up; one Base refuses now is kept.
    pub(crate) async fn prove_cases(&self, v: &CoordVault, todo: Vec<(B256, Vec<B256>)>) -> Result<()> {
        let slasher = self.v3().await?.slasher;
        ensure!(slasher != Address::ZERO, "an executed case without a slasher");
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let s = IZecuredSlasher::new(slasher, &provider);
        let mut used = vec![];
        for (case_id, claims) in todo {
            if claims.is_empty() {
                used.push(case_id);
                continue;
            }
            let call = s.proveInclusionByCase(case_id, claims.clone());
            let upheld = match call.call().await {
                Ok(upheld) => upheld,
                Err(e) => {
                    debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "an executed case cannot prove its payout now");
                    continue;
                }
            };
            if upheld.is_zero() {
                used.push(case_id); // its claims are final already, or none is of its payout
                continue;
            }
            let gas = gas_with_headroom(call.estimate_gas().await.context("estimate proveInclusionByCase")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send proveInclusionByCase")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("proveInclusionByCase receipt")?;
            ensure!(receipt.status(), "proveInclusionByCase reverted");
            info!(vault = %short(&v.vault_id), claims = claims.len(), "an executed case proved a payout's delivery: its claims are final (R-2)");
            used.push(case_id);
        }
        if !used.is_empty() {
            self.update_vault(&v.vault_id, |cur| cur.inclusion_cases.retain(|c| !used.contains(&c.case_id)))
                .await?;
        }
        Ok(())
    }

    /// R-19: runs a compromised vault's burn again when the one inside `executeSlash` did not
    /// happen, if it retires something now.
    pub(crate) async fn burn_again(&self, v: &CoordVault) -> Result<()> {
        if !burn_due(v) || !self.keeper_due(&v.vault_id, Keeper::Burn) {
            return Ok(());
        }
        let compensator = self.compensator().await?;
        if compensator == Address::ZERO {
            return Ok(());
        }
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let contract = IZecuredCompensator::new(compensator, &provider);
        let call = contract.burnCollateral(v.vault_id);
        let retired = match call.call().await {
            Ok(retired) => retired,
            Err(e) => {
                // Not burnable now (an account that revoked the Registry's Morpho authorization, a
                // vault no longer active): tried again later; the settlement covers it (R-20).
                debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "the collateral cannot be burned now");
                return Ok(());
            }
        };
        if retired == 0 {
            return Ok(()); // nothing above the LLTV to burn now
        }
        let gas = gas_with_headroom(call.estimate_gas().await.context("estimate burnCollateral")?);
        let receipt = call
            .gas(gas)
            .send()
            .await
            .context("send burnCollateral")?
            .with_timeout(Some(RECEIPT_TIMEOUT))
            .get_receipt()
            .await
            .context("burnCollateral receipt")?;
        ensure!(receipt.status(), "burnCollateral reverted");
        info!(vault = %short(&v.vault_id), "the compromised vault's collateral was burned down to the LLTV (R-19, W-12)");
        Ok(())
    }

    /// W-4: arms the vault's unarmed claims with their earliest receipt; a claim no node has the
    /// destination of yet waits for the next pass.
    pub(crate) async fn arm_claims(&self, v: &CoordVault) -> Result<()> {
        let claims = unarmed(v);
        if claims.is_empty() || !self.keeper_due(&v.vault_id, Keeper::Arm) {
            return Ok(());
        }
        let bond = self.v3().await?.bond;
        if bond == Address::ZERO {
            return Ok(());
        }
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let contract = ISignerBond::new(bond, &provider);
        for id in claims {
            let Ok(r) = self.claim_receipt(id).await else {
                continue; // no node holds the claim's destination yet
            };
            let call = contract.armLiveness(id, r.received_at, r.signature.clone());
            if let Err(e) = call.call().await {
                debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "a claim cannot be armed now");
                continue;
            }
            let gas = gas_with_headroom(call.estimate_gas().await.context("estimate armLiveness")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send armLiveness")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("armLiveness receipt")?;
            ensure!(receipt.status(), "armLiveness reverted");
            info!(vault = %short(&v.vault_id), "a claim's liveness is armed with its earliest receipt (W-4)");
        }
        Ok(())
    }

    /// W-6: registers with the nodes the treasury recipient of each bond-paid claim whose Base
    /// destination is the claim's derived treasury destination; any other destination is never
    /// registered (no node can pay it then), and the operators are warned.
    pub(crate) async fn register_reimbursements(&self, v: &CoordVault) -> Result<()> {
        let Some(keys) = &self.treasury else {
            return Ok(());
        };
        let todo = {
            let store = self.store.lock().await;
            unregistered_reimbursements(v, |c| store.recipient(c).is_some())
        };
        if todo.is_empty() || !self.keeper_due(&v.vault_id, Keeper::Treasury) {
            return Ok(());
        }
        for (id, commitment) in todo {
            let r = keys.reimbursement(&id);
            if r.commitment() != commitment {
                warn!(vault = %short(&v.vault_id), "a bond-paid claim's destination is not its treasury destination: not registered (W-6)");
                continue;
            }
            self.register_recipient(r).await?;
            info!(vault = %short(&v.vault_id), "a bond-paid claim's treasury recipient is registered with the signers (W-6)");
        }
        Ok(())
    }

    /// W-2: moves the vault's F1 cases about a note whose tag a deposit took over to the deposit's
    /// vault (`followNote`): an open one at once, rather than at the end of its window; an executed one
    /// too (the victim's vault gets its own case). A case that cannot move (moved already, dismissed,
    /// its anchor gone) is forgotten.
    pub(crate) async fn follow_notes(&self, v: &CoordVault) -> Result<()> {
        if v.overridden_tags.is_empty() || v.f1_cases.is_empty() || !self.keeper_due(&v.vault_id, Keeper::Follow) {
            return Ok(());
        }
        let slasher = self.v3().await?.slasher;
        if slasher == Address::ZERO {
            return Ok(());
        }
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let s = IZecuredSlasher::new(slasher, &provider);
        let mut done = vec![];
        for case_id in &v.f1_cases {
            let case = s.caseOf(*case_id).call().await.context("slasher.caseOf")?;
            if !v.overridden_tags.contains(&case.noteTag) {
                continue;
            }
            let call = s.followNote(*case_id);
            let moved = match call.call().await {
                Ok(moved) => moved,
                Err(e) => {
                    debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "an F1 case does not follow its note");
                    done.push(*case_id);
                    continue;
                }
            };
            if moved == B256::ZERO && case.executed {
                done.push(*case_id); // followed already
                continue;
            }
            let gas = gas_with_headroom(call.estimate_gas().await.context("estimate followNote")?);
            let receipt = call
                .gas(gas)
                .send()
                .await
                .context("send followNote")?
                .with_timeout(Some(RECEIPT_TIMEOUT))
                .get_receipt()
                .await
                .context("followNote receipt")?;
            ensure!(receipt.status(), "followNote reverted");
            warn!(vault = %short(&v.vault_id), "an F1 case followed its note to the vault that owns its tag now (W-2)");
            done.push(*case_id);
        }
        if !done.is_empty() {
            self.update_vault(&v.vault_id, |cur| cur.f1_cases.retain(|c| !done.contains(c))).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloy::sol_types::SolCall;
    use axum::{Json, Router, routing::post};
    use zecured_base::events::{VIOLATION_WITHHELD_CHANGE, VaultEvent};
    use zecured_policy::ledger::{Claim, ClaimKind};

    use super::*;
    use crate::store::{CommittedPayout, tests::vault};

    /// W-6: a bond-paid claim's treasury recipient is registered with every signer when its Base
    /// destination is the derived one, and only once; a destination that is not derived never is, and
    /// a coordinator without the treasury keys registers none.
    #[tokio::test]
    async fn w6_derived_treasury_destinations_are_registered() {
        use std::sync::{Arc, Mutex};

        use orchard::keys::{FullViewingKey, Scope, SpendingKey};

        use crate::TreasuryKeys;

        let sk = Option::<SpendingKey>::from(SpendingKey::from_bytes([7; 32])).unwrap();
        let ivk = FullViewingKey::from(&sk).to_ivk(Scope::External).to_bytes();
        let keys = TreasuryKeys::from_bytes(&ivk, [9; 32]).unwrap();
        let posted: Arc<Mutex<usize>> = Arc::default();
        let log = posted.clone();
        let app = Router::new().route(
            "/v1/recipients",
            post(move |Json(_): Json<serde_json::Value>| {
                let log = log.clone();
                async move {
                    *log.lock().unwrap() += 1;
                    Json(serde_json::json!({ "commitment": "0x00" }))
                }
            }),
        );
        let url = crate::nodes::tests::serve(app).await;
        let urls = vec![url; 5];
        let gas = hex::encode(alloy::signers::local::PrivateKeySigner::random().to_bytes());
        let dir = crate::store::tests::TempDir::new("w6-treasury");
        let derived = keys.reimbursement(&[1; 32]);
        let mut v = vault(1);
        v.claims = vec![
            Claim {
                bond_paid: true,
                commitment: derived.commitment(),
                ..claim(1, ClaimState::Unpaid, false, 0)
            },
            Claim {
                bond_paid: true,
                commitment: [0xbb; 32], // not the claim's treasury destination
                ..claim(2, ClaimState::Unpaid, false, 0)
            },
            claim(3, ClaimState::Unpaid, false, 0),
        ];
        let without = Coordinator::new(crate::nodes::tests::config(&urls), &dir.0, &gas).unwrap();
        without.register_reimbursements(&v).await.unwrap();
        assert_eq!(*posted.lock().unwrap(), 0, "without the treasury keys: none");
        drop(without);

        let c = Coordinator::new(crate::nodes::tests::config(&urls), &dir.0, &gas).unwrap().with_treasury(keys);
        c.register_reimbursements(&v).await.unwrap();
        assert_eq!(*posted.lock().unwrap(), 5, "claim 1's, to each signer");
        assert!(c.store.lock().await.recipient(&derived.commitment()).is_some());
        assert!(c.store.lock().await.recipient(&[0xbb; 32]).is_none(), "never a destination that is not derived");
        let left = unregistered_reimbursements(&v, |x| c.store.try_lock().unwrap().recipient(x).is_some());
        assert_eq!(left, vec![([2; 32], [0xbb; 32])]);
        c.keeper_tries.lock().unwrap().clear();
        c.register_reimbursements(&v).await.unwrap();
        assert_eq!(*posted.lock().unwrap(), 5, "registered once");
    }

    fn claim(id: u8, state: ClaimState, proven: bool, confirmed_at: u64) -> Claim {
        Claim {
            id: [id; 32],
            kind: ClaimKind::Liquidation,
            amount: 100_000,
            commitment: [id; 32],
            order: (10, u64::from(id)),
            created_at: 0,
            partner: None,
            state,
            refund_tag: None,
            bond_paid: false,
            proven,
            confirmed_at,
        }
    }

    /// R-3: only confirmations by signatures (paid, not final) past their window by the margin.
    #[test]
    fn confirmations_expire_after_their_window() {
        let mut v = vault(1);
        v.claims = vec![
            claim(1, ClaimState::Paid, false, 1_000),
            claim(2, ClaimState::Paid, false, 5_000),
            claim(3, ClaimState::Paid, true, 1_000),
            claim(4, ClaimState::Paid, false, 0),
            claim(5, ClaimState::Unpaid, false, 1_000),
            claim(6, ClaimState::Disputed, false, 1_000),
        ];
        let window = 3_600;
        assert!(expirable(&v, window, 1_000 + window).is_empty());
        assert_eq!(expirable(&v, window, 1_000 + window + EXPIRY_MARGIN_SECS + 1), vec![B256::repeat_byte(1)]);
        assert_eq!(expirable(&v, window, 10_000), vec![B256::repeat_byte(1), B256::repeat_byte(2)]);
    }

    /// R-2: the claims of the case's payout not final yet; without a record of the payout, every
    /// claim of the vault not final yet.
    #[test]
    fn a_case_proves_its_payouts_open_claims() {
        let mut v = vault(1);
        v.claims = vec![
            claim(1, ClaimState::Unpaid, false, 0),
            claim(2, ClaimState::Paid, false, 50),
            claim(3, ClaimState::Paid, true, 50),
            claim(4, ClaimState::Disputed, false, 0),
            claim(5, ClaimState::Unpaid, false, 0),
            claim(6, ClaimState::WrittenOff, false, 0),
        ];
        v.ufvk = "uviewtest1vault".into();
        let tx = [9u8; 32];
        let x = B256::from(payout_ref(&v.ufvk, &tx));
        let honest = |id: u8| B256::from(claim_ref(&[id; 32], &tx));
        v.commitments = vec![CommittedPayout {
            payout_ref: x,
            input_tag: B256::repeat_byte(0x7a),
            claims: vec![[1; 32], [2; 32], [3; 32], [4; 32]],
            // W-1: claim 4 is named under a reference its claimant never computes.
            claim_refs: vec![honest(1), honest(2), honest(3), B256::repeat_byte(0xee)],
        }];
        let case = |payout: B256| InclusionCase {
            case_id: B256::repeat_byte(0xca),
            payout_ref: payout,
        };
        let spent = vec![([0x7a; 32], TxId::from_bytes(tx), 50)];
        let salt = |id: &[u8; 32]| Some([id[0]; 32]);
        let ids = |ids: &[u8]| ids.iter().map(|i| B256::repeat_byte(*i)).collect::<Vec<_>>();
        assert_eq!(case_claims(&v, &case(x), &spent, salt), ids(&[1, 2]), "open and honest: not 3 (final), not 4");
        assert!(case_claims(&v, &case(B256::repeat_byte(0xbb)), &spent, salt).is_empty(), "no record: nothing");
        assert!(case_claims(&v, &case(x), &[], salt).is_empty(), "its transaction not seen yet: nothing");
    }

    /// R-19, W-12: a compromised vault is burned again while it is active, also after a first burn
    /// (a higher price frees collateral above the LLTV); the simulation decides whether to send.
    #[test]
    fn a_compromised_active_vault_is_burned_again() {
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        assert!(!burn_due(&v));
        v.compromised = true;
        assert!(burn_due(&v));
        v.collateral_burned = true;
        assert!(burn_due(&v), "W-12: after a first burn too");
        v.status = VaultStatus::Closed;
        assert!(!burn_due(&v), "retired already");
    }

    /// W-4: the claims to arm are the payable ones the bonds cover that no receipt armed for their
    /// current destination.
    #[test]
    fn w4_unarmed_claims_are_armed() {
        let mut v = vault(1);
        v.claims = vec![
            claim(1, ClaimState::Unpaid, false, 0),
            claim(2, ClaimState::Unpaid, false, 0),
            claim(3, ClaimState::Paid, false, 50),
            Claim {
                bond_paid: true,
                ..claim(4, ClaimState::Unpaid, false, 0)
            },
            claim(5, ClaimState::Unpaid, false, 0),
        ];
        v.armed = vec![
            crate::store::Armed {
                claim_id: [2; 32],
                commitment: [2; 32],
                payable_at: 10,
            },
            crate::store::Armed {
                claim_id: [5; 32],
                commitment: [0x55; 32], // an earlier destination
                payable_at: 10,
            },
        ];
        assert_eq!(unarmed(&v), vec![B256::repeat_byte(1), B256::repeat_byte(5)]);
    }

    /// R-2: an executed case's vault, kind and payout come from the slasher's record of the case;
    /// other events are left as they are.
    #[tokio::test]
    async fn executed_cases_are_read_from_the_slasher() {
        let case = IZecuredSlasher::ViolationCase {
            kind: VIOLATION_WITHHELD_CHANGE,
            anchor: B256::repeat_byte(0xa0),
            beneficiary: Address::repeat_byte(0xbe),
            reference: B256::repeat_byte(0xaa),
            noteTag: B256::repeat_byte(0x7a),
            vaultId: B256::repeat_byte(6),
            signerBitmap: 7,
            executableAt: 1,
            executed: true,
            value: 0,
        };
        let ret = IZecuredSlasher::caseOfCall::abi_encode_returns(&case);
        let app = Router::new().route(
            "/",
            post(move |Json(req): Json<serde_json::Value>| {
                let ret = ret.clone();
                async move {
                    let result = match req["method"].as_str() {
                        Some("eth_call") => serde_json::json!(format!("0x{}", hex::encode(ret))),
                        _ => serde_json::json!("0x14a34"),
                    };
                    Json(serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }))
                }
            }),
        );
        let rpc = crate::nodes::tests::serve(app).await;
        let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap());
        let at = |vault_id: B256, event: RegistryEvent| VaultEvent {
            vault_id,
            block: 10,
            log_index: 0,
            block_hash: None,
            timestamp: 20,
            event,
        };
        let other = at(
            B256::repeat_byte(9),
            RegistryEvent::VaultCompromised {
                case_id: B256::repeat_byte(0xca),
            },
        );
        let mut evs = vec![
            at(
                B256::ZERO,
                RegistryEvent::CaseExecuted {
                    case_id: B256::repeat_byte(0xca),
                    kind: 0,
                    payout_ref: B256::ZERO,
                },
            ),
            other.clone(),
        ];
        resolve_cases(&provider, Address::repeat_byte(0xa4), &mut evs).await;
        assert_eq!(evs[0].vault_id, B256::repeat_byte(6));
        assert_eq!(
            evs[0].event,
            RegistryEvent::CaseExecuted {
                case_id: B256::repeat_byte(0xca),
                kind: VIOLATION_WITHHELD_CHANGE,
                payout_ref: B256::repeat_byte(0xaa)
            }
        );
        assert_eq!(evs[1], other);
        // Without a slasher, or with one that cannot be read, the case stays unresolved (no vault:
        // not applied), and the coordinator's view of Base goes on.
        let unread = at(
            B256::ZERO,
            RegistryEvent::CaseExecuted {
                case_id: B256::repeat_byte(0xca),
                kind: 0,
                payout_ref: B256::ZERO,
            },
        );
        let mut unresolved = vec![unread.clone()];
        resolve_cases(&provider, Address::ZERO, &mut unresolved).await;
        assert_eq!(unresolved, [unread.clone()]);
        let down = ProviderBuilder::new().connect_http("http://127.0.0.1:9".parse().unwrap());
        resolve_cases(&down, Address::repeat_byte(0xa4), &mut unresolved).await;
        assert_eq!(unresolved, [unread]);
    }
}
