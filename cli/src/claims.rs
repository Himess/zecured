//! A loan's claims as Base holds them (Registry v3): its Refund claims (T-02), which wait for the
//! account's destination, and the claims a claimant may dispute (T-05 (a)).

use alloy::{
    primitives::{Address, B256},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, ensure};
use zecured_base::{
    claim_id_of,
    events::IZecuredRegistry,
    verify::{CLAIM_COMPENSATED, CLAIM_DISPUTED, CLAIM_PAID, CLAIM_PENDING, CLAIM_WRITTEN_OFF, kind_code},
};
use zecured_policy::ledger::ClaimKind;

/// One claim of a vault on Base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimOnBase {
    pub id: B256,
    pub kind: u8,
    pub status: u8,
    pub amount: u64,
    /// Zero while the claim has no destination.
    pub commitment: B256,
    /// When it became payable (Unix seconds).
    pub created_at: u64,
    pub bond_paid: bool,
    pub claimant: Address,
    /// A commitment on Base binds it to this input (zero: none yet).
    pub input: B256,
}

/// Every claim of `vault`, in sequence.
pub async fn vault_claims(registry: Address, vault: B256, rpc: &str) -> Result<Vec<ClaimOnBase>> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    claims_on(&provider, registry, vault).await
}

async fn claims_on<P: Provider>(provider: &P, registry: Address, vault: B256) -> Result<Vec<ClaimOnBase>> {
    let r = IZecuredRegistry::new(registry, provider);
    let count = r.vaultDetails(vault).call().await.context("Registry.vaultDetails")?.claimCount;
    let mut out = Vec::with_capacity(usize::try_from(count)?);
    for sequence in 0..count {
        let id = claim_id_of(vault, sequence);
        let c = r.claimDetails(id).call().await.context("Registry.claimDetails")?;
        let input = r.claimInput(id).call().await.context("Registry.claimInput")?;
        out.push(ClaimOnBase {
            id,
            kind: c.kind,
            status: c.status,
            amount: c.amount,
            commitment: c.commitment,
            created_at: c.createdAt.to::<u64>(),
            bond_paid: c.bondPaid,
            claimant: c.claimant,
            input,
        });
    }
    Ok(out)
}

/// The words a claim's status is shown with.
pub fn status_name(status: u8) -> &'static str {
    match status {
        CLAIM_PENDING => "pending",
        CLAIM_PAID => "paid",
        CLAIM_WRITTEN_OFF => "written off",
        CLAIM_COMPENSATED => "compensated",
        CLAIM_DISPUTED => "disputed",
        _ => "unknown",
    }
}

/// T-02: whether the vault's account may give (or change) this refund's destination: a Refund
/// claim, pending or abandoned (written off: a destination revives it), not yet bound to a payout
/// and not paid by the bonds.
pub fn refund_destination_allowed(c: &ClaimOnBase) -> Result<()> {
    ensure!(c.kind == kind_code(ClaimKind::Refund), "not a Refund claim");
    ensure!(
        c.status == CLAIM_PENDING || c.status == CLAIM_WRITTEN_OFF,
        "the refund is {} on Base",
        status_name(c.status)
    );
    ensure!(c.input == B256::ZERO, "a payout of the refund is already committed: its destination is final");
    ensure!(!c.bond_paid, "the bonds paid this refund: its ZEC reimburses them");
    Ok(())
}

/// The vault's Refund claims as a table: claim id, amount, status, destination.
pub fn refund_table(claims: &[ClaimOnBase]) -> Vec<String> {
    claims
        .iter()
        .filter(|c| c.kind == kind_code(ClaimKind::Refund))
        .map(|c| {
            let destination = if c.commitment == B256::ZERO {
                "no destination yet (loan refund-destination)"
            } else {
                "destination given"
            };
            format!("{}  {:>12} zat  {:<12} {destination}", c.id, c.amount, status_name(c.status))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(kind: ClaimKind, status: u8) -> ClaimOnBase {
        ClaimOnBase {
            id: B256::repeat_byte(1),
            kind: kind_code(kind),
            status,
            amount: 250_000,
            commitment: B256::ZERO,
            created_at: 100,
            bond_paid: false,
            claimant: Address::repeat_byte(2),
            input: B256::ZERO,
        }
    }

    /// T-02: a destination is given to a pending or abandoned refund that no payout is bound to.
    #[test]
    fn a_refund_destination_is_given_while_the_refund_is_open() {
        refund_destination_allowed(&claim(ClaimKind::Refund, CLAIM_PENDING)).unwrap();
        refund_destination_allowed(&claim(ClaimKind::Refund, CLAIM_WRITTEN_OFF)).unwrap();
        assert!(refund_destination_allowed(&claim(ClaimKind::Release, CLAIM_PENDING)).is_err());
        assert!(refund_destination_allowed(&claim(ClaimKind::Refund, CLAIM_PAID)).is_err());
        let bound = ClaimOnBase {
            input: B256::repeat_byte(3),
            ..claim(ClaimKind::Refund, CLAIM_PENDING)
        };
        assert!(refund_destination_allowed(&bound).is_err());
        let bond_paid = ClaimOnBase {
            bond_paid: true,
            ..claim(ClaimKind::Refund, CLAIM_PENDING)
        };
        assert!(refund_destination_allowed(&bond_paid).is_err());
    }

    #[test]
    fn only_refunds_are_listed() {
        let rows = refund_table(&[
            claim(ClaimKind::Refund, CLAIM_PENDING),
            claim(ClaimKind::Fee, CLAIM_PENDING),
        ]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("pending") && rows[0].contains("no destination yet"));
    }
}
