//! Checking Registry events against the Registry's state (security review C-2).
//!
//! A Base RPC provider can fabricate logs, for example a `ReleaseRequested` that burns a vault's
//! whole outstanding for an attacker's commitment. Before a signer node applies an event it
//! reads the Registry's own storage with `eth_call` and accepts the event only if that state
//! proves it:
//! - `VaultActivated`: the vault is active (or closed) from the event's block on, for that
//!   account, and was pending (or new) before it; the event's deposit tag is used and names a
//!   lineage note of that vault and amount (K2-a: the note that backs the vault);
//! - new claims: every claim exists from the event's block on, for that vault, with that kind and
//!   amount, and (unless it is a refund, created without a destination) that commitment, and did
//!   not exist before it; a refund's note is a lineage note of the vault for its amount (T-02);
//! - a changed claim (a destination given, the bonds' payment, a write-off, a compensation, a
//!   dispute and its outcome): the Registry shows the change from the event's block on, and the
//!   claim was in the state the change starts from before it;
//! - `PayoutConfirmed`: every claim, of that vault, is paid (or already disputed) from the
//!   event's block on and was pending before it.
//!
//! What a node then applies is the claim as the Registry holds it at the block it read, not as
//! the log tells it: a claim's destination, state and whether the bonds paid it change after it
//! was created (T-02, T-05).
//!
//! The block itself must have the hash and timestamp the log came with. A provider that no
//! longer has the state of old blocks (not an archive node) is asked at a later block instead.
//! A claim's vault, kind and amount never change and a deposit's note stays a lineage note, so
//! that still proves everything a payout depends on, only not the exact block. A change the later
//! state no longer shows (a refund destination given again, an abandoned refund revived, a
//! dispute resolved) is *stale*: nothing is applied, since the later event that changed the claim
//! again carries its current state.
//!
//! An optional witness provider, operated independently, must return exactly the same answers:
//! a single dishonest provider then cannot make a node apply a fabricated event.

use std::future::Future;

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    primitives::{Address, B256},
    providers::Provider,
};
use anyhow::{Context, Result};
use zecured_policy::ledger::{self, ClaimKind};

use crate::events::{ClaimChange, IZecuredRegistry, RegistryEvent, VaultEvent};

/// `VaultStatus.None` / `ClaimStatus.None` (contracts/src/interfaces/v3/IZecuredRegistry.sol).
pub const STATUS_NONE: u8 = 0;
/// `ClaimStatus`: `Pending`, `Paid`, `WrittenOff`, `Compensated` (T-04), `Disputed` (T-05 (a)).
pub const CLAIM_PENDING: u8 = 1;
pub const CLAIM_PAID: u8 = 2;
pub const CLAIM_WRITTEN_OFF: u8 = 3;
pub const CLAIM_COMPENSATED: u8 = 4;
pub const CLAIM_DISPUTED: u8 = 5;
/// `VaultStatus`: `Active`, `Closed`, `Cancelled` (never activated; its deposits are refunded
/// through Refund claims, T-02) and `Pending` (a deposit proof reserved it, K3).
pub const VAULT_ACTIVE: u8 = 1;
pub const VAULT_CLOSED: u8 = 2;
pub const VAULT_CANCELLED: u8 = 3;
pub const VAULT_PENDING: u8 = 4;

/// The Registry's `ClaimKind` value for `kind`.
pub fn kind_code(kind: ClaimKind) -> u8 {
    match kind {
        ClaimKind::Release => 0,
        ClaimKind::Liquidation => 1,
        ClaimKind::CloseUser => 2,
        ClaimKind::CloseMarketMaker => 3,
        ClaimKind::Refund => 4,
        ClaimKind::Fee => 5,
        ClaimKind::Retired => 6,
    }
}

/// A node's view of a claim the Registry holds with status `status` (`None` for no claim).
pub fn claim_state(status: u8) -> Option<ledger::ClaimState> {
    Some(match status {
        CLAIM_PENDING => ledger::ClaimState::Unpaid,
        CLAIM_PAID => ledger::ClaimState::Paid,
        CLAIM_WRITTEN_OFF => ledger::ClaimState::WrittenOff,
        CLAIM_COMPENSATED => ledger::ClaimState::Compensated,
        CLAIM_DISPUTED => ledger::ClaimState::Disputed,
        _ => return None,
    })
}

/// `Registry.vault(vaultId)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultState {
    pub account: Address,
    pub outstanding: u64,
    pub status: u8,
}

/// `Registry.claimDetails(claimId)`, as far as the signers use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimRecord {
    pub vault_id: B256,
    pub kind: u8,
    pub amount: u64,
    pub commitment: B256,
    pub status: u8,
    /// When the claim became payable (T-05).
    pub created_at: u64,
    pub bond_paid: bool,
}

/// `Registry.lineageOf(tag)`: the vault a lineage note belongs to (zero for none) and its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lineage {
    pub vault_id: B256,
    pub value: u64,
}

/// A block's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub hash: B256,
    pub timestamp: u64,
}

/// Read access to the Registry's state at a given block.
pub trait RegistryReader {
    fn vault_at(&self, vault_id: B256, block: u64) -> impl Future<Output = Result<VaultState>> + Send;
    fn claim_at(&self, claim_id: B256, block: u64) -> impl Future<Output = Result<ClaimRecord>> + Send;
    fn tag_used_at(&self, deposit_tag: B256, block: u64) -> impl Future<Output = Result<bool>> + Send;
    fn lineage_at(&self, tag: B256, block: u64) -> impl Future<Output = Result<Lineage>> + Send;
    fn block(&self, number: u64) -> impl Future<Output = Result<BlockInfo>> + Send;
}

/// [`RegistryReader`] over an RPC provider (`eth_call` at a block number).
pub struct ProviderReader<'a, P> {
    pub provider: &'a P,
    pub registry: Address,
}

impl<P: Provider> RegistryReader for ProviderReader<'_, P> {
    async fn vault_at(&self, vault_id: B256, block: u64) -> Result<VaultState> {
        let v = IZecuredRegistry::new(self.registry, self.provider)
            .vault(vault_id)
            .call()
            .block(BlockId::number(block))
            .await
            .with_context(|| format!("Registry.vault at block {block}"))?;
        Ok(VaultState {
            account: v.account,
            outstanding: v.outstanding,
            status: v.status,
        })
    }

    async fn claim_at(&self, claim_id: B256, block: u64) -> Result<ClaimRecord> {
        let c = IZecuredRegistry::new(self.registry, self.provider)
            .claimDetails(claim_id)
            .call()
            .block(BlockId::number(block))
            .await
            .with_context(|| format!("Registry.claimDetails at block {block}"))?;
        Ok(ClaimRecord {
            vault_id: c.vaultId,
            kind: c.kind,
            amount: c.amount,
            commitment: c.commitment,
            status: c.status,
            created_at: c.createdAt.to::<u64>(),
            bond_paid: c.bondPaid,
        })
    }

    async fn tag_used_at(&self, deposit_tag: B256, block: u64) -> Result<bool> {
        IZecuredRegistry::new(self.registry, self.provider)
            .depositTagUsed(deposit_tag)
            .call()
            .block(BlockId::number(block))
            .await
            .with_context(|| format!("Registry.depositTagUsed at block {block}"))
    }

    async fn lineage_at(&self, tag: B256, block: u64) -> Result<Lineage> {
        let l = IZecuredRegistry::new(self.registry, self.provider)
            .lineageOf(tag)
            .call()
            .block(BlockId::number(block))
            .await
            .with_context(|| format!("Registry.lineageOf at block {block}"))?;
        Ok(Lineage {
            vault_id: l.vaultId,
            value: l.value,
        })
    }

    async fn block(&self, number: u64) -> Result<BlockInfo> {
        let b = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Number(number))
            .await
            .with_context(|| format!("get block {number}"))?
            .with_context(|| format!("block {number} not found"))?;
        Ok(BlockInfo {
            hash: b.header.hash,
            timestamp: b.header.timestamp,
        })
    }
}

/// Which state an event is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// The event's block and the block before it (pins the event to its block).
    EventBlock,
    /// A later block, for providers without the state of old blocks.
    Later(u64),
}

/// Everything read to check one event; a witness must read exactly the same.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    pub block: Option<BlockInfo>,
    pub vaults: Vec<VaultState>,
    pub claims: Vec<ClaimRecord>,
    pub tags: Vec<bool>,
    pub lineages: Vec<Lineage>,
}

/// The outcome of checking an event on one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The state proves the event; the event to apply carries the state's values.
    Proven(Evidence, VaultEvent),
    /// At a later block the state no longer shows the event's change: a later event changed it
    /// again (and carries the current state). Nothing to apply.
    Stale(Evidence, String),
    /// The state contradicts the event.
    Contradicted(String),
}

/// A lineage note of `vault_id` worth `value`.
fn lineage_is(l: &Lineage, vault_id: B256, value: u64) -> bool {
    l.vault_id == vault_id && l.value == value
}

/// Whether the state of a claim shows `change` (for a later block: still shows it).
fn shows(change: &ClaimChange, now: &ClaimRecord, at: At) -> bool {
    match change {
        ClaimChange::RefundDestination(c) => {
            now.kind == kind_code(ClaimKind::Refund)
                && now.commitment == *c
                && (at != At::EventBlock || now.status == CLAIM_PENDING)
        }
        ClaimChange::ReimburseDestination(c) => now.bond_paid && now.commitment == *c,
        ClaimChange::BondPaid => now.bond_paid,
        ClaimChange::WrittenOff => now.status == CLAIM_WRITTEN_OFF,
        ClaimChange::Compensated => now.status == CLAIM_COMPENSATED,
        ClaimChange::Disputed { .. } => now.status == CLAIM_DISPUTED,
        ClaimChange::Upheld { .. } => now.status == CLAIM_PAID,
        ClaimChange::Reopened => now.status == CLAIM_PENDING,
    }
}

/// Whether a claim in state `prev` (the block before the change) is where `change` starts from.
fn starts_from(change: &ClaimChange, prev: &ClaimRecord) -> bool {
    match change {
        // A refund may have been created (or abandoned) in the same block.
        ClaimChange::RefundDestination(_) => true,
        ClaimChange::ReimburseDestination(_) => prev.bond_paid && prev.commitment == B256::ZERO,
        ClaimChange::BondPaid => !prev.bond_paid && prev.status == CLAIM_PENDING,
        ClaimChange::WrittenOff | ClaimChange::Compensated => prev.status == CLAIM_PENDING,
        ClaimChange::Disputed { .. } => prev.status == CLAIM_PAID,
        ClaimChange::Upheld { .. } | ClaimChange::Reopened => prev.status == CLAIM_DISPUTED,
    }
}

/// Reads from `reader` the state that proves or refutes `ev`, and judges it. An error means
/// the provider could not answer (try again later), never that the event is false.
pub async fn check_event<R: RegistryReader>(reader: &R, ev: &VaultEvent, at: At) -> Result<Check> {
    let block = reader.block(ev.block).await?;
    let mut evidence = Evidence {
        block: Some(block),
        ..Evidence::default()
    };
    if ev.block_hash.is_some_and(|h| h != block.hash) || block.timestamp != ev.timestamp {
        return Ok(Check::Contradicted("block hash or timestamp differs from the log's".into()));
    }
    let (before, after) = match at {
        At::EventBlock => (ev.block.checked_sub(1), ev.block),
        At::Later(n) => (None, n.max(ev.block)),
    };

    let mut problems: Vec<String> = vec![];
    let mut stale: Option<String> = None;
    let mut applied = ev.clone();
    match &ev.event {
        RegistryEvent::Reserved { .. } | RegistryEvent::ReservationReleased => {
            problems.push("signer nodes do not apply reservation events".into());
        }
        RegistryEvent::VaultActivated {
            account,
            amount,
            deposit_tag,
        } => {
            let now = reader.vault_at(ev.vault_id, after).await?;
            evidence.vaults.push(now);
            if !(now.status == VAULT_ACTIVE || now.status == VAULT_CLOSED)
                || now.account != *account
                || now.outstanding > *amount
            {
                problems.push("no such vault activation in the Registry".into());
            }
            // Tags are never released once a vault is activated (only a dropped pending
            // activation frees one, and that vault was never activated), and a deposit's note
            // stays a lineage note.
            let used = reader.tag_used_at(*deposit_tag, after).await?;
            evidence.tags.push(used);
            let lineage = reader.lineage_at(*deposit_tag, after).await?;
            evidence.lineages.push(lineage);
            if !used || !lineage_is(&lineage, ev.vault_id, *amount) {
                problems.push("the activation's deposit tag is not this vault's deposit in the Registry".into());
            }
            if let Some(b) = before {
                let prev = reader.vault_at(ev.vault_id, b).await?;
                evidence.vaults.push(prev);
                if !(prev.status == STATUS_NONE || prev.status == VAULT_PENDING) {
                    problems.push("the vault was active, closed or cancelled before the event's block".into());
                }
            }
        }
        RegistryEvent::Claims(claims) => {
            let mut proven = Vec::with_capacity(claims.len());
            for c in claims {
                let id = B256::from(c.id);
                let now = reader.claim_at(id, after).await?;
                evidence.claims.push(now);
                let matches = now.status != STATUS_NONE
                    && now.vault_id == ev.vault_id
                    && now.kind == kind_code(c.kind)
                    && now.amount == c.amount;
                if !matches {
                    problems.push("no such claim in the Registry".into());
                }
                // The destination a claim was created with is the log's, except a refund's (none
                // yet) and a bond-paid claim's (cleared since, T-05).
                if c.kind != ClaimKind::Refund && !now.bond_paid && now.commitment.0 != c.commitment {
                    problems.push("the claim's commitment differs in the Registry".into());
                }
                if let Some(tag) = c.refund_tag {
                    let lineage = reader.lineage_at(B256::from(tag), after).await?;
                    evidence.lineages.push(lineage);
                    if !lineage_is(&lineage, ev.vault_id, c.amount) {
                        problems.push("the refund's note is not a lineage note of the vault in the Registry".into());
                    }
                }
                if let Some(b) = before {
                    let prev = reader.claim_at(id, b).await?;
                    evidence.claims.push(prev);
                    if prev.status != STATUS_NONE {
                        problems.push("claim existed before the event's block".into());
                    }
                }
                proven.push(ledger::Claim {
                    commitment: now.commitment.0,
                    state: claim_state(now.status).unwrap_or(c.state),
                    created_at: now.created_at,
                    bond_paid: now.bond_paid,
                    ..c.clone()
                });
            }
            applied.event = RegistryEvent::Claims(proven);
        }
        RegistryEvent::ClaimChanged { claim_id, change } => {
            let now = reader.claim_at(*claim_id, after).await?;
            evidence.claims.push(now);
            if now.status == STATUS_NONE || now.vault_id != ev.vault_id {
                problems.push("no such claim of the vault in the Registry".into());
            } else if !shows(change, &now, at) {
                match at {
                    At::EventBlock => problems.push("the Registry does not show the claim's change".into()),
                    At::Later(_) => stale = Some("the claim changed again since".into()),
                }
            }
            if let Some(b) = before {
                let prev = reader.claim_at(*claim_id, b).await?;
                evidence.claims.push(prev);
                if !starts_from(change, &prev) {
                    problems.push("the claim was not in the state the change starts from".into());
                }
            }
        }
        RegistryEvent::PayoutConfirmed(ids) => {
            let mut confirmed = vec![];
            for &id in ids {
                let now = reader.claim_at(id, after).await?;
                evidence.claims.push(now);
                if now.vault_id != ev.vault_id {
                    problems.push("claim of another vault".into());
                }
                match (now.status, at) {
                    // Disputed since its confirmation (T-05 (a)): the dispute's event follows.
                    (CLAIM_PAID | CLAIM_DISPUTED, _) => confirmed.push(id),
                    // Later: reopened after a dispute (or never confirmed): not paid.
                    (_, At::Later(_)) => {}
                    _ => problems.push("claim is not paid in the Registry".into()),
                }
                if let Some(b) = before {
                    let prev = reader.claim_at(id, b).await?;
                    evidence.claims.push(prev);
                    if prev.status != CLAIM_PENDING {
                        problems.push("claim was not pending before the event's block".into());
                    }
                }
            }
            if confirmed.is_empty() && problems.is_empty() {
                stale = Some("none of the claims is still paid".into());
            }
            applied.event = RegistryEvent::PayoutConfirmed(confirmed);
        }
    }
    Ok(if !problems.is_empty() {
        Check::Contradicted(problems.join("; "))
    } else if let Some(why) = stale {
        Check::Stale(evidence, why)
    } else {
        Check::Proven(evidence, applied)
    })
}

/// What a node may do with an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The Registry's state proves the event (and the witness agrees, if one is set): apply this
    /// event, which carries the Registry's values.
    Proven(VaultEvent),
    /// The Registry's state changed again since the event (both providers agree): skip it.
    Stale(String),
    /// The primary provider's own state contradicts the event: it was fabricated.
    Forged(String),
    /// The witness reports another state: one of the providers is lying or broken.
    Disputed(String),
}

/// Checks `ev` against the primary provider's Registry state and, if set, the witness's, which
/// must read exactly the same evidence. `later` is the block to read at if the primary cannot
/// serve the state at the event's block: a recent block whose state both providers still have
/// (non-archive nodes keep only the last ~128 states).
/// An error means a provider could not answer: try again later.
pub async fn verify_event<P: RegistryReader, W: RegistryReader>(
    primary: &P,
    witness: Option<&W>,
    ev: &VaultEvent,
    later: u64,
) -> Result<Verdict> {
    let (at, check) = match check_event(primary, ev, At::EventBlock).await {
        Ok(c) => (At::EventBlock, c),
        // Not an archive node: the state of old blocks is gone; ask at a later block.
        Err(_) => (At::Later(later), check_event(primary, ev, At::Later(later)).await?),
    };
    let (evidence, outcome) = match check {
        Check::Proven(e, applied) => (e, Verdict::Proven(applied)),
        Check::Stale(e, why) => (e, Verdict::Stale(why)),
        Check::Contradicted(why) => return Ok(Verdict::Forged(why)),
    };
    let Some(witness) = witness else {
        return Ok(outcome);
    };
    Ok(match check_event(witness, ev, at).await.context("witness RPC")? {
        Check::Proven(e, _) | Check::Stale(e, _) if e == evidence => outcome,
        Check::Proven(..) | Check::Stale(..) => {
            Verdict::Disputed("the witness RPC reports different Registry state".into())
        }
        Check::Contradicted(why) => Verdict::Disputed(format!("witness RPC: {why}")),
    })
}

#[cfg(test)]
mod tests {

    use anyhow::anyhow;
    use zecured_policy::ledger::{Claim, ClaimState as Paid};

    use super::*;

    const VAULT: B256 = B256::repeat_byte(1);
    const ACCOUNT: Address = Address::repeat_byte(2);
    const CLAIM: B256 = B256::repeat_byte(3);
    const COMMITMENT: [u8; 32] = [4; 32];
    const TAG: B256 = B256::repeat_byte(0x7a);

    /// A Registry whose history is a list of state changes by block.
    #[derive(Clone, Default)]
    struct Chain {
        /// (from block, state)
        vault: Vec<(u64, VaultState)>,
        claim: Vec<(u64, ClaimRecord)>,
        /// (from block, used) of the vault's deposit tag; every other tag is unused.
        tag: Vec<(u64, bool)>,
        /// (from block, lineage note) of `TAG`; every other tag is no lineage note.
        lineage: Vec<(u64, Lineage)>,
        /// Blocks below this have no state any more (not an archive node).
        pruned_below: u64,
        /// Hash reported for every block.
        hash: B256,
        /// The provider is unreachable.
        down: bool,
    }

    impl Chain {
        fn ok(&self, block: u64) -> Result<()> {
            if self.down {
                return Err(anyhow!("connection refused"));
            }
            if block < self.pruned_below {
                return Err(anyhow!("missing trie node"));
            }
            Ok(())
        }
    }

    fn latest<T: Copy>(history: &[(u64, T)], block: u64, none: T) -> T {
        history.iter().rev().find(|(from, _)| *from <= block).map_or(none, |(_, s)| *s)
    }

    const NO_CLAIM: ClaimRecord = ClaimRecord {
        vault_id: B256::ZERO,
        kind: 0,
        amount: 0,
        commitment: B256::ZERO,
        status: STATUS_NONE,
        created_at: 0,
        bond_paid: false,
    };

    impl RegistryReader for Chain {
        async fn vault_at(&self, _: B256, block: u64) -> Result<VaultState> {
            self.ok(block)?;
            let none = VaultState {
                account: Address::ZERO,
                outstanding: 0,
                status: STATUS_NONE,
            };
            Ok(latest(&self.vault, block, none))
        }

        async fn claim_at(&self, _: B256, block: u64) -> Result<ClaimRecord> {
            self.ok(block)?;
            Ok(latest(&self.claim, block, NO_CLAIM))
        }

        async fn tag_used_at(&self, deposit_tag: B256, block: u64) -> Result<bool> {
            self.ok(block)?;
            Ok(deposit_tag == TAG && latest(&self.tag, block, false))
        }

        async fn lineage_at(&self, tag: B256, block: u64) -> Result<Lineage> {
            self.ok(block)?;
            let none = Lineage {
                vault_id: B256::ZERO,
                value: 0,
            };
            Ok(if tag == TAG {
                latest(&self.lineage, block, none)
            } else {
                none
            })
        }

        async fn block(&self, number: u64) -> Result<BlockInfo> {
            if self.down {
                return Err(anyhow!("connection refused"));
            }
            Ok(BlockInfo {
                hash: self.hash,
                timestamp: number * 2,
            })
        }
    }

    fn claim_state(status: u8) -> ClaimRecord {
        ClaimRecord {
            vault_id: VAULT,
            kind: kind_code(ClaimKind::Release),
            amount: 300_000,
            commitment: B256::from(COMMITMENT),
            status,
            created_at: 200,
            bond_paid: false,
        }
    }

    const DEPOSIT: Lineage = Lineage {
        vault_id: VAULT,
        value: 1_000_000,
    };

    /// Reserved at block 40, activated at 50; a release claim created at 100 and paid at 200.
    fn honest() -> Chain {
        let vault = |outstanding, status| VaultState {
            account: ACCOUNT,
            outstanding,
            status,
        };
        Chain {
            vault: vec![
                (40, vault(0, VAULT_PENDING)),
                (50, vault(1_000_000, VAULT_ACTIVE)),
                (100, vault(700_000, VAULT_ACTIVE)),
            ],
            claim: vec![(100, claim_state(CLAIM_PENDING)), (200, claim_state(CLAIM_PAID))],
            tag: vec![(40, true)],
            lineage: vec![(40, DEPOSIT)],
            hash: B256::repeat_byte(9),
            ..Default::default()
        }
    }

    fn event(block: u64, event: RegistryEvent) -> VaultEvent {
        VaultEvent {
            vault_id: VAULT,
            block,
            log_index: 0,
            block_hash: Some(B256::repeat_byte(9)),
            timestamp: block * 2,
            event,
        }
    }

    fn release_claim(block: u64, amount: u64, commitment: [u8; 32]) -> Claim {
        Claim {
            id: CLAIM.0,
            kind: ClaimKind::Release,
            amount,
            commitment,
            order: (block, 0),
            created_at: block * 2,
            partner: None,
            state: Paid::Unpaid,
            refund_tag: None,
            bond_paid: false,
        }
    }

    fn release(block: u64, amount: u64, commitment: [u8; 32]) -> VaultEvent {
        event(block, RegistryEvent::Claims(vec![release_claim(block, amount, commitment)]))
    }

    fn activation(block: u64, account: Address, deposit_tag: B256) -> VaultEvent {
        event(
            block,
            RegistryEvent::VaultActivated {
                account,
                amount: 1_000_000,
                deposit_tag,
            },
        )
    }

    async fn verdict(primary: &Chain, witness: Option<&Chain>, ev: &VaultEvent) -> Result<Verdict> {
        verify_event(primary, witness, ev, 1_000).await
    }

    fn forged(v: Verdict) -> bool {
        matches!(v, Verdict::Forged(_))
    }

    #[tokio::test]
    async fn genuine_events_are_proven() {
        let chain = honest();
        for ev in [
            activation(50, ACCOUNT, TAG),
            release(100, 300_000, COMMITMENT),
            event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM])),
        ] {
            assert!(matches!(verdict(&chain, None, &ev).await.unwrap(), Verdict::Proven(_)), "{ev:?}");
            assert!(matches!(verdict(&chain, Some(&chain.clone()), &ev).await.unwrap(), Verdict::Proven(_)), "{ev:?}");
        }
        // What is applied carries the Registry's values (here the same as the log's).
        let Verdict::Proven(applied) = verdict(&chain, None, &release(100, 300_000, COMMITMENT)).await.unwrap() else {
            panic!()
        };
        assert_eq!(applied, release(100, 300_000, COMMITMENT));
    }

    #[tokio::test]
    async fn fabricated_events_are_refused() {
        let chain = honest();
        let forged_events = [
            // A release for the attacker's commitment, or for more than was burned.
            release(100, 300_000, [6; 32]),
            release(100, 1_000_000, COMMITMENT),
            // A claim that does not exist at all.
            release(100, 300_000, COMMITMENT).tap(|e| e.vault_id = B256::repeat_byte(7)),
            // A real claim re-dated to another block (reordering the payout queue).
            release(150, 300_000, COMMITMENT),
            // A confirmation of a claim that is still pending, or re-dated.
            event(150, RegistryEvent::PayoutConfirmed(vec![CLAIM])),
            event(250, RegistryEvent::PayoutConfirmed(vec![CLAIM])),
            // An activation for another account, or before it happened.
            activation(50, Address::repeat_byte(8), TAG),
            activation(120, ACCOUNT, TAG),
            // K2-a: a real activation reported with another note's tag (which would make the
            // signers root the vault's lineage at that note).
            activation(50, ACCOUNT, B256::repeat_byte(0x7b)),
            // The log's block is not the provider's block.
            release(100, 300_000, COMMITMENT).tap(|e| e.block_hash = Some(B256::repeat_byte(5))),
        ];
        for ev in forged_events {
            assert!(forged(verdict(&chain, None, &ev).await.unwrap()), "{ev:?}");
        }
    }

    /// T-02: a refund lands with its deposit's note as a lineage note of the vault for its amount;
    /// the node applies the destination the Registry holds (none at first).
    #[tokio::test]
    async fn refunds_are_checked_against_their_lineage_note() {
        let refund_state = |commitment: B256, status| ClaimRecord {
            vault_id: VAULT,
            kind: kind_code(ClaimKind::Refund),
            amount: 250_000,
            commitment,
            status,
            created_at: 120,
            bond_paid: false,
        };
        let chain = Chain {
            vault: vec![(
                60,
                VaultState {
                    account: ACCOUNT,
                    outstanding: 0,
                    status: VAULT_CANCELLED,
                },
            )],
            claim: vec![
                (60, refund_state(B256::ZERO, CLAIM_PENDING)),
                (70, refund_state(B256::from(COMMITMENT), CLAIM_PENDING)),
            ],
            tag: vec![(60, true)],
            lineage: vec![(
                60,
                Lineage {
                    vault_id: VAULT,
                    value: 250_000,
                },
            )],
            hash: B256::repeat_byte(9),
            ..Default::default()
        };
        let refund = |tag: B256, amount: u64| {
            event(
                60,
                RegistryEvent::Claims(vec![Claim {
                    kind: ClaimKind::Refund,
                    commitment: [0; 32],
                    refund_tag: Some(tag.0),
                    ..release_claim(60, amount, [0; 32])
                }]),
            )
        };
        let Verdict::Proven(applied) = verdict(&chain, None, &refund(TAG, 250_000)).await.unwrap() else {
            panic!()
        };
        let RegistryEvent::Claims(claims) = applied.event else {
            panic!()
        };
        assert_eq!(claims[0].commitment, [0; 32], "no destination at its block");
        // Another note's tag, or another amount: refused.
        assert!(forged(verdict(&chain, None, &refund(B256::repeat_byte(0x7c), 250_000)).await.unwrap()));
        assert!(forged(verdict(&chain, None, &refund(TAG, 260_000)).await.unwrap()));
        // The destination given at block 70.
        let destination = |c: [u8; 32]| {
            event(
                70,
                RegistryEvent::ClaimChanged {
                    claim_id: CLAIM,
                    change: ClaimChange::RefundDestination(B256::from(c)),
                },
            )
        };
        assert!(matches!(verdict(&chain, None, &destination(COMMITMENT)).await.unwrap(), Verdict::Proven(_)));
        assert!(forged(verdict(&chain, None, &destination([6; 32])).await.unwrap()));

        // A provider without old state: the creation is applied with the destination given since.
        let pruned = Chain {
            pruned_below: 500,
            ..chain.clone()
        };
        let Verdict::Proven(applied) = verdict(&pruned, None, &refund(TAG, 250_000)).await.unwrap() else {
            panic!()
        };
        let RegistryEvent::Claims(claims) = applied.event else {
            panic!()
        };
        assert_eq!(claims[0].commitment, COMMITMENT);
        assert_eq!(claims[0].created_at, 120, "its liveness clock starts with its destination");
    }

    /// T-05: once the bonds paid a claim, its original destination is gone from the Registry: a
    /// later check cannot tell it, and the node applies the destination the Registry holds now.
    #[tokio::test]
    async fn a_bond_paid_claim_is_applied_with_its_current_destination() {
        let mut chain = honest();
        let bond_paid = ClaimRecord {
            bond_paid: true,
            commitment: B256::ZERO,
            ..claim_state(CLAIM_PENDING)
        };
        chain.claim = vec![(100, claim_state(CLAIM_PENDING)), (300, bond_paid)];
        chain.pruned_below = 500;
        // The log's commitment cannot be checked any more, but it is not what is applied.
        for commitment in [COMMITMENT, [6; 32]] {
            let Verdict::Proven(applied) = verdict(&chain, None, &release(100, 300_000, commitment)).await.unwrap()
            else {
                panic!()
            };
            let RegistryEvent::Claims(claims) = applied.event else {
                panic!()
            };
            assert_eq!((claims[0].commitment, claims[0].bond_paid), ([0; 32], true));
        }
        // At the event's block (an archive provider), the log's commitment must be the claim's.
        chain.pruned_below = 0;
        assert!(forged(verdict(&chain, None, &release(100, 300_000, [6; 32])).await.unwrap()));
    }

    /// A change of a claim is checked at its block; at a later block a change the state no longer
    /// shows is stale (skipped), and a forged one is never applied.
    #[tokio::test]
    async fn claim_changes_are_checked_and_stale_ones_skipped() {
        let mut chain = honest();
        let disputed = ClaimRecord {
            status: CLAIM_DISPUTED,
            ..claim_state(CLAIM_PAID)
        };
        // Paid at 200, disputed at 300, reopened at 400 (pending again).
        chain.claim = vec![
            (100, claim_state(CLAIM_PENDING)),
            (200, claim_state(CLAIM_PAID)),
            (300, disputed),
            (400, claim_state(CLAIM_PENDING)),
        ];
        let change = |block: u64, change: ClaimChange| {
            event(
                block,
                RegistryEvent::ClaimChanged {
                    claim_id: CLAIM,
                    change,
                },
            )
        };
        let dispute = change(300, ClaimChange::Disputed { due_at: 9_000 });
        let reopen = change(400, ClaimChange::Reopened);
        assert!(matches!(verdict(&chain, None, &dispute).await.unwrap(), Verdict::Proven(_)));
        assert!(matches!(verdict(&chain, None, &reopen).await.unwrap(), Verdict::Proven(_)));
        // Re-dated, or a change that did not happen.
        assert!(forged(verdict(&chain, None, &change(350, ClaimChange::Disputed { due_at: 9_000 })).await.unwrap()));
        assert!(forged(verdict(&chain, None, &change(300, ClaimChange::Compensated)).await.unwrap()));
        assert!(forged(verdict(&chain, None, &change(200, ClaimChange::Upheld { payout_ref: TAG })).await.unwrap()));
        // At a later block (state pruned): the dispute was resolved since: stale; the reopening
        // still shows.
        chain.pruned_below = 500;
        assert!(matches!(verdict(&chain, None, &dispute).await.unwrap(), Verdict::Stale(_)));
        assert!(matches!(verdict(&chain, None, &reopen).await.unwrap(), Verdict::Proven(_)));
        // The confirmation at 200: the claim is pending again now (reopened): nothing to apply.
        let confirmed = event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM]));
        assert!(matches!(verdict(&chain, None, &confirmed).await.unwrap(), Verdict::Stale(_)));
        // A witness that reads another state disputes even a stale verdict.
        let mut other = chain.clone();
        other.claim.push((450, claim_state(CLAIM_PAID)));
        assert!(matches!(verdict(&chain, Some(&other), &dispute).await.unwrap(), Verdict::Disputed(_)));
    }

    /// T-05 (a): a claim confirmed and disputed in the same block is still a genuine confirmation.
    #[tokio::test]
    async fn a_confirmation_disputed_since_is_applied() {
        let mut chain = honest();
        let disputed = ClaimRecord {
            status: CLAIM_DISPUTED,
            ..claim_state(CLAIM_PAID)
        };
        chain.claim = vec![(100, claim_state(CLAIM_PENDING)), (200, disputed)];
        let confirmed = event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM]));
        let Verdict::Proven(applied) = verdict(&chain, None, &confirmed).await.unwrap() else {
            panic!()
        };
        assert_eq!(applied.event, RegistryEvent::PayoutConfirmed(vec![CLAIM]));
    }

    #[tokio::test]
    async fn a_witness_must_agree() {
        let primary = honest();
        let ev = release(100, 300_000, COMMITMENT);
        // A lying primary (its state backs its fabricated log) is caught by an honest witness.
        let mut lying = honest();
        lying.claim = vec![(
            100,
            ClaimRecord {
                commitment: B256::repeat_byte(6),
                ..claim_state(CLAIM_PENDING)
            },
        )];
        let fabricated = release(100, 300_000, [6; 32]);
        assert!(matches!(verdict(&lying, None, &fabricated).await.unwrap(), Verdict::Proven(_)));
        assert!(matches!(verdict(&lying, Some(&primary), &fabricated).await.unwrap(), Verdict::Disputed(_)));
        // Another block hash on the witness (another chain or a reorg).
        let mut fork = honest();
        fork.hash = B256::repeat_byte(5);
        assert!(matches!(verdict(&primary, Some(&fork), &ev).await.unwrap(), Verdict::Disputed(_)));
        // Same claim, other state (e.g. the witness already saw it paid at that block).
        let mut ahead = honest();
        ahead.claim = vec![(100, claim_state(CLAIM_PAID))];
        assert!(matches!(verdict(&primary, Some(&ahead), &ev).await.unwrap(), Verdict::Disputed(_)));
        // An unreachable witness is not agreement: try again later.
        let down = Chain { down: true, ..honest() };
        assert!(verdict(&primary, Some(&down), &ev).await.is_err());
        assert!(verdict(&down, None, &ev).await.is_err());
    }

    #[tokio::test]
    async fn providers_without_old_state_are_asked_at_a_later_block() {
        let pruned = Chain {
            pruned_below: 500,
            ..honest()
        };
        let ev = release(100, 300_000, COMMITMENT);
        assert!(matches!(verdict(&pruned, Some(&pruned.clone()), &ev).await.unwrap(), Verdict::Proven(_)));
        assert!(forged(verdict(&pruned, None, &release(100, 300_000, [6; 32])).await.unwrap()));
        // A confirmation is still checked as paid; an activation still names the vault's deposit.
        let confirmed = event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM]));
        assert!(matches!(verdict(&pruned, None, &confirmed).await.unwrap(), Verdict::Proven(_)));
        assert!(matches!(verdict(&pruned, None, &activation(50, ACCOUNT, TAG)).await.unwrap(), Verdict::Proven(_)));
        assert!(forged(verdict(&pruned, None, &activation(50, ACCOUNT, B256::repeat_byte(0x7b))).await.unwrap()));
    }

    /// Signer nodes never apply a reservation; checking one is refused outright.
    #[tokio::test]
    async fn reservations_are_not_checked() {
        let ev = event(40, RegistryEvent::ReservationReleased);
        assert!(forged(verdict(&honest(), None, &ev).await.unwrap()));
    }

    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
    impl Tap for VaultEvent {}
}
