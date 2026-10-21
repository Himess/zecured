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
//! - R-19 (`burnCollateral`): a compromised vault's collateral is burned down to the LLTV inside
//!   `executeSlash`, within a fixed gas budget; when that burn did not happen (no
//!   `CollateralBurned`), it is run again with the gas it needs.

use std::time::{Duration, Instant};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, anyhow, ensure};
use tracing::{debug, info, warn};
use zecured_base::{
    events::{IZecuredCompensator, IZecuredRegistry, IZecuredSlasher, RegistryEvent, VaultEvent},
    gas_with_headroom,
};
use zecured_policy::ledger::ClaimState;

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
/// final yet (from the payout's commitment as this coordinator recorded it), or, for a payout it
/// has no record of, every claim of the vault not final yet (the slasher skips the others).
pub(crate) fn case_claims(v: &CoordVault, case: &InclusionCase) -> Vec<B256> {
    let open = |id: &[u8; 32]| {
        v.claims.iter().any(|c| {
            c.id == *id && !c.proven && matches!(c.state, ClaimState::Unpaid | ClaimState::Paid | ClaimState::Disputed)
        })
    };
    match v.commitments.iter().find(|c| c.payout_ref == case.payout_ref) {
        Some(c) => c.claims.iter().filter(|id| open(id)).map(|id| B256::from(*id)).collect(),
        None => v.claims.iter().filter(|c| open(&c.id)).map(|c| B256::from(c.id)).collect(),
    }
}

/// R-19: whether the compromised vault's burn should be run again: compromised, still active, and
/// no `CollateralBurned` seen.
pub(crate) fn burn_due(v: &CoordVault) -> bool {
    v.compromised && !v.collateral_burned && v.status == VaultStatus::Active
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
        let slasher = self.v3().await?.slasher;
        ensure!(slasher != Address::ZERO, "an executed case without a slasher");
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.gas.clone()))
            .connect_http(self.cfg.base_rpc_url.parse()?);
        let s = IZecuredSlasher::new(slasher, &provider);
        let mut used = vec![];
        for case in &v.inclusion_cases {
            let claims = case_claims(v, case);
            if claims.is_empty() {
                used.push(case.case_id);
                continue;
            }
            let call = s.proveInclusionByCase(case.case_id, claims.clone());
            let upheld = match call.call().await {
                Ok(upheld) => upheld,
                Err(e) => {
                    debug!(vault = %short(&v.vault_id), error = %scrubbed(&anyhow!(e)), "an executed case cannot prove its payout now");
                    continue;
                }
            };
            if upheld.is_zero() {
                used.push(case.case_id); // its claims are final already, or none is of its payout
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
            used.push(case.case_id);
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
        info!(vault = %short(&v.vault_id), "the compromised vault's collateral was burned down to the LLTV (R-19)");
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
        v.commitments = vec![CommittedPayout {
            payout_ref: B256::repeat_byte(0xaa),
            input_tag: B256::repeat_byte(0x7a),
            claims: vec![[1; 32], [2; 32], [3; 32], [4; 32]],
        }];
        let case = |payout: u8| InclusionCase {
            case_id: B256::repeat_byte(0xca),
            payout_ref: B256::repeat_byte(payout),
        };
        let ids = |ids: &[u8]| ids.iter().map(|i| B256::repeat_byte(*i)).collect::<Vec<_>>();
        assert_eq!(case_claims(&v, &case(0xaa)), ids(&[1, 2, 4]));
        assert_eq!(case_claims(&v, &case(0xbb)), ids(&[1, 2, 4, 5]), "a payout without a record");
    }

    /// R-19: a compromised, active vault without a burn seen.
    #[test]
    fn a_missing_burn_is_due() {
        let mut v = vault(1);
        v.status = VaultStatus::Active;
        assert!(!burn_due(&v));
        v.compromised = true;
        assert!(burn_due(&v));
        v.collateral_burned = true;
        assert!(!burn_due(&v));
        v.collateral_burned = false;
        v.status = VaultStatus::Closed;
        assert!(!burn_due(&v), "retired already");
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
