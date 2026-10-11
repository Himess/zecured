//! Checking Registry events against the Registry's state (security review C-2).
//!
//! A Base RPC provider can fabricate logs, for example a `ReleaseRequested` that burns a vault's
//! whole outstanding for an attacker's commitment. Before a signer node applies an event it
//! reads the Registry's own storage with `eth_call` and accepts the event only if that state
//! proves it:
//! - `VaultActivated`: the vault exists from the event's block on, for that account, and did
//!   not exist before it;
//! - claim events: every claim exists from the event's block on, for that vault, with that
//!   kind, amount and commitment, and did not exist before it;
//! - `PayoutConfirmed`: every claim, of that vault, is paid from the event's block on and was
//!   pending before it;
//! - `VaultCancelled`: the vault is cancelled from the event's block on and was new before it.
//!
//! The block itself must have the hash and timestamp the log came with. A provider that no
//! longer has the state of old blocks (not an archive node) is asked at a later block instead:
//! claims never change once created and statuses only move forward, so that still proves
//! everything a payout depends on, only not the exact block.
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
use zecured_policy::ledger::ClaimKind;

use crate::events::{IZecuredRegistry, RegistryEvent, VaultEvent};

/// `VaultStatus.None` / `ClaimStatus.None` (contracts/src/interfaces/IZecuredRegistry.sol).
pub const STATUS_NONE: u8 = 0;
/// `ClaimStatus.Pending`.
pub const CLAIM_PENDING: u8 = 1;
/// `ClaimStatus.Paid`.
pub const CLAIM_PAID: u8 = 2;
/// `VaultStatus.Active` / `VaultStatus.Closed` / `VaultStatus.Cancelled`.
pub const VAULT_ACTIVE: u8 = 1;
pub const VAULT_CLOSED: u8 = 2;
pub const VAULT_CANCELLED: u8 = 3;

/// The Registry's `ClaimKind` value for `kind`.
pub fn kind_code(kind: ClaimKind) -> u8 {
    match kind {
        ClaimKind::Release => 0,
        ClaimKind::Liquidation => 1,
        ClaimKind::CloseUser => 2,
        ClaimKind::CloseMarketMaker => 3,
    }
}

/// `Registry.vault(vaultId)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultState {
    pub account: Address,
    pub outstanding: u64,
    pub status: u8,
}

/// `Registry.claim(claimId)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimState {
    pub vault_id: B256,
    pub kind: u8,
    pub amount: u64,
    pub commitment: B256,
    pub status: u8,
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
    fn claim_at(&self, claim_id: B256, block: u64) -> impl Future<Output = Result<ClaimState>> + Send;
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

    async fn claim_at(&self, claim_id: B256, block: u64) -> Result<ClaimState> {
        let c = IZecuredRegistry::new(self.registry, self.provider)
            .claim(claim_id)
            .call()
            .block(BlockId::number(block))
            .await
            .with_context(|| format!("Registry.claim at block {block}"))?;
        Ok(ClaimState {
            vault_id: c.vaultId,
            kind: c.kind,
            amount: c.amount,
            commitment: c.commitment,
            status: c.status,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub block: BlockInfo,
    pub vaults: Vec<VaultState>,
    pub claims: Vec<ClaimState>,
}

/// The outcome of checking an event on one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The state proves the event.
    Proven(Evidence),
    /// The state contradicts the event.
    Contradicted(String),
}

/// Reads from `reader` the state that proves or refutes `ev`, and judges it. An error means
/// the provider could not answer (try again later), never that the event is false.
pub async fn check_event<R: RegistryReader>(reader: &R, ev: &VaultEvent, at: At) -> Result<Check> {
    let block = reader.block(ev.block).await?;
    let mut evidence = Evidence {
        block,
        vaults: vec![],
        claims: vec![],
    };
    if ev.block_hash.is_some_and(|h| h != block.hash) || block.timestamp != ev.timestamp {
        return Ok(Check::Contradicted("block hash or timestamp differs from the log's".into()));
    }
    let (before, after) = match at {
        At::EventBlock => (ev.block.checked_sub(1), ev.block),
        At::Later(n) => (None, n.max(ev.block)),
    };

    let mut problems: Vec<String> = vec![];
    match &ev.event {
        RegistryEvent::VaultActivated { account, amount } => {
            let now = reader.vault_at(ev.vault_id, after).await?;
            evidence.vaults.push(now);
            // Active or Closed (a cancelled vault was never activated).
            if !(now.status == VAULT_ACTIVE || now.status == VAULT_CLOSED)
                || now.account != *account
                || now.outstanding > *amount
            {
                problems.push("no such vault activation in the Registry".into());
            }
            if let Some(b) = before {
                let prev = reader.vault_at(ev.vault_id, b).await?;
                evidence.vaults.push(prev);
                if prev.status != STATUS_NONE {
                    problems.push("vault existed before the event's block".into());
                }
            }
        }
        RegistryEvent::Claims(claims) => {
            for c in claims {
                let id = B256::from(c.id);
                let now = reader.claim_at(id, after).await?;
                evidence.claims.push(now);
                let matches = now.status != STATUS_NONE
                    && now.vault_id == ev.vault_id
                    && now.kind == kind_code(c.kind)
                    && now.amount == c.amount
                    && now.commitment.0 == c.commitment;
                if !matches {
                    problems.push("no such claim in the Registry".into());
                }
                if let Some(b) = before {
                    let prev = reader.claim_at(id, b).await?;
                    evidence.claims.push(prev);
                    if prev.status != STATUS_NONE {
                        problems.push("claim existed before the event's block".into());
                    }
                }
            }
        }
        RegistryEvent::VaultCancelled => {
            let now = reader.vault_at(ev.vault_id, after).await?;
            evidence.vaults.push(now);
            if now.status != VAULT_CANCELLED {
                problems.push("no such cancellation in the Registry".into());
            }
            if let Some(b) = before {
                let prev = reader.vault_at(ev.vault_id, b).await?;
                evidence.vaults.push(prev);
                if prev.status != STATUS_NONE {
                    problems.push("vault was not new before the event's block".into());
                }
            }
        }
        RegistryEvent::PayoutConfirmed(ids) => {
            for &id in ids {
                let now = reader.claim_at(id, after).await?;
                evidence.claims.push(now);
                if now.status != CLAIM_PAID || now.vault_id != ev.vault_id {
                    problems.push("claim is not paid in the Registry".into());
                }
                if let Some(b) = before {
                    let prev = reader.claim_at(id, b).await?;
                    evidence.claims.push(prev);
                    if prev.status != CLAIM_PENDING {
                        problems.push("claim was not pending before the event's block".into());
                    }
                }
            }
        }
    }
    Ok(if problems.is_empty() {
        Check::Proven(evidence)
    } else {
        Check::Contradicted(problems.join("; "))
    })
}

/// What a node may do with an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The Registry's state proves the event (and the witness agrees, if one is set).
    Proven,
    /// The primary provider's own state contradicts the event: it was fabricated.
    Forged(String),
    /// The witness reports another state: one of the providers is lying or broken.
    Disputed(String),
}

/// Checks `ev` against the primary provider's Registry state and, if set, the witness's, which
/// must prove it with exactly the same evidence. `later` is the block to read at if the primary
/// cannot serve the state at the event's block: a recent block whose state both providers
/// still have (non-archive nodes keep only the last ~128 states).
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
    let evidence = match check {
        Check::Proven(e) => e,
        Check::Contradicted(why) => return Ok(Verdict::Forged(why)),
    };
    let Some(witness) = witness else {
        return Ok(Verdict::Proven);
    };
    Ok(match check_event(witness, ev, at).await.context("witness RPC")? {
        Check::Proven(e) if e == evidence => Verdict::Proven,
        Check::Proven(_) => Verdict::Disputed("the witness RPC reports different Registry state".into()),
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

    /// A Registry whose history is a list of state changes by block.
    #[derive(Clone, Default)]
    struct Chain {
        /// (from block, state)
        vault: Vec<(u64, VaultState)>,
        claim: Vec<(u64, ClaimState)>,
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

        async fn claim_at(&self, _: B256, block: u64) -> Result<ClaimState> {
            self.ok(block)?;
            let none = ClaimState {
                vault_id: B256::ZERO,
                kind: 0,
                amount: 0,
                commitment: B256::ZERO,
                status: STATUS_NONE,
            };
            Ok(latest(&self.claim, block, none))
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

    fn claim_state(status: u8) -> ClaimState {
        ClaimState {
            vault_id: VAULT,
            kind: kind_code(ClaimKind::Release),
            amount: 300_000,
            commitment: B256::from(COMMITMENT),
            status,
        }
    }

    /// Activated at block 50; a release claim created at 100 and paid at 200.
    fn honest() -> Chain {
        Chain {
            vault: vec![
                (
                    50,
                    VaultState {
                        account: ACCOUNT,
                        outstanding: 1_000_000,
                        status: 1,
                    },
                ),
                (
                    100,
                    VaultState {
                        account: ACCOUNT,
                        outstanding: 700_000,
                        status: 1,
                    },
                ),
            ],
            claim: vec![(100, claim_state(CLAIM_PENDING)), (200, claim_state(CLAIM_PAID))],
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

    fn release(block: u64, amount: u64, commitment: [u8; 32]) -> VaultEvent {
        event(
            block,
            RegistryEvent::Claims(vec![Claim {
                id: CLAIM.0,
                kind: ClaimKind::Release,
                amount,
                commitment,
                order: (block, 0),
                created_at: block * 2,
                partner: None,
                state: Paid::Unpaid,
            }]),
        )
    }

    async fn verdict(primary: &Chain, witness: Option<&Chain>, ev: &VaultEvent) -> Result<Verdict> {
        verify_event(primary, witness, ev, 1_000).await
    }

    #[tokio::test]
    async fn genuine_events_are_proven() {
        let chain = honest();
        let activation = event(
            50,
            RegistryEvent::VaultActivated {
                account: ACCOUNT,
                amount: 1_000_000,
            },
        );
        for ev in [
            activation,
            release(100, 300_000, COMMITMENT),
            event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM])),
        ] {
            assert_eq!(verdict(&chain, None, &ev).await.unwrap(), Verdict::Proven, "{ev:?}");
            assert_eq!(verdict(&chain, Some(&chain.clone()), &ev).await.unwrap(), Verdict::Proven, "{ev:?}");
        }
    }

    #[tokio::test]
    async fn fabricated_events_are_refused() {
        let chain = honest();
        let forged = [
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
            event(
                50,
                RegistryEvent::VaultActivated {
                    account: Address::repeat_byte(8),
                    amount: 1_000_000,
                },
            ),
            event(
                120,
                RegistryEvent::VaultActivated {
                    account: ACCOUNT,
                    amount: 1_000_000,
                },
            ),
            // The log's block is not the provider's block.
            release(100, 300_000, COMMITMENT).tap(|e| e.block_hash = Some(B256::repeat_byte(5))),
        ];
        for ev in forged {
            assert!(matches!(verdict(&chain, None, &ev).await.unwrap(), Verdict::Forged(_)), "{ev:?}");
        }
    }

    #[tokio::test]
    async fn cancellations_are_checked_against_the_vault_status() {
        // A vault that was never activated, cancelled at block 60.
        let cancelled = Chain {
            vault: vec![(
                60,
                VaultState {
                    account: Address::ZERO,
                    outstanding: 0,
                    status: VAULT_CANCELLED,
                },
            )],
            hash: B256::repeat_byte(9),
            ..Default::default()
        };
        let ev = event(60, RegistryEvent::VaultCancelled);
        assert_eq!(verdict(&cancelled, None, &ev).await.unwrap(), Verdict::Proven);
        // Re-dated after the fact, or claimed for an activated vault.
        let late = event(70, RegistryEvent::VaultCancelled);
        assert!(matches!(verdict(&cancelled, None, &late).await.unwrap(), Verdict::Forged(_)));
        assert!(matches!(verdict(&honest(), None, &ev).await.unwrap(), Verdict::Forged(_)));
    }

    #[tokio::test]
    async fn a_witness_must_agree() {
        let primary = honest();
        let ev = release(100, 300_000, COMMITMENT);
        // A lying primary (its state backs its fabricated log) is caught by an honest witness.
        let mut lying = honest();
        lying.claim = vec![(
            100,
            ClaimState {
                commitment: B256::repeat_byte(6),
                ..claim_state(CLAIM_PENDING)
            },
        )];
        let forged = release(100, 300_000, [6; 32]);
        assert_eq!(verdict(&lying, None, &forged).await.unwrap(), Verdict::Proven);
        assert!(matches!(verdict(&lying, Some(&primary), &forged).await.unwrap(), Verdict::Disputed(_)));
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
        assert_eq!(verdict(&pruned, Some(&pruned.clone()), &ev).await.unwrap(), Verdict::Proven);
        assert!(matches!(verdict(&pruned, None, &release(100, 300_000, [6; 32])).await.unwrap(), Verdict::Forged(_)));
        // A confirmation is still checked as paid.
        let confirmed = event(200, RegistryEvent::PayoutConfirmed(vec![CLAIM]));
        assert_eq!(verdict(&pruned, None, &confirmed).await.unwrap(), Verdict::Proven);
    }

    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
    impl Tap for VaultEvent {}
}
