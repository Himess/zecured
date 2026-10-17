//! A loan's claims as Base holds them (Registry v3): its Refund claims (T-02), which wait for the
//! account's destination, the claims a claimant may dispute (T-05 (a)), and the liveness receipts
//! that arm a claim's liveness clock (R-5, v4).

use alloy::{
    primitives::{Address, B256, Bytes},
    providers::{Provider, ProviderBuilder},
};
use anyhow::{Context, Result, ensure};
use zecured_base::{
    claim_id_of,
    events::IZecuredRegistry,
    operator::{bond_domain, destination_received},
    recover_typed,
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

/// The Registry's bond contract (`signerBond()`, immutable), where liveness clocks are armed.
pub async fn signer_bond(registry: Address, rpc: &str) -> Result<Address> {
    let provider = ProviderBuilder::new().connect_http(rpc.parse().context("Base RPC URL")?);
    let bond = IZecuredRegistry::new(registry, &provider)
        .signerBond()
        .call()
        .await
        .context("Registry.signerBond")?;
    ensure!(bond != Address::ZERO, "this deployment has no signer bond");
    Ok(bond)
}

/// R-5: a liveness receipt as `POST /claims/:claimId/receipt` serves it: a signer's statement
/// that it holds and has validated the claim's destination since `received_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub claim_id: B256,
    pub commitment: B256,
    pub received_at: u64,
    pub signer: Address,
    pub signature: Bytes,
}

pub fn parse_receipt(r: &serde_json::Value) -> Result<Receipt> {
    let text = |name: &str| {
        r.get(name).and_then(|v| v.as_str()).with_context(|| format!("the backend's receipt has no {name}"))
    };
    Ok(Receipt {
        claim_id: text("claimId")?.parse()?,
        commitment: text("commitment")?.parse()?,
        received_at: r.get("receivedAt").and_then(|v| v.as_u64()).context("the backend's receipt has no receivedAt")?,
        signer: text("signer")?.parse()?,
        signature: text("signature")?.parse()?,
    })
}

/// R-5: whether a liveness clock can be armed for this claim: pending, with a destination.
pub fn arming_allowed(c: &ClaimOnBase) -> Result<()> {
    ensure!(
        c.status == CLAIM_PENDING,
        "the claim is {} on Base: no liveness clock to arm",
        status_name(c.status)
    );
    ensure!(c.commitment != B256::ZERO, "the claim has no destination yet");
    Ok(())
}

/// R-5: the receipt counts only if it is for this claim and its current destination on Base, and
/// was signed, under the bond's EIP-712 domain, by a signer of the local network config (never
/// trusted from the backend).
pub fn check_receipt(chain_id: u64, bond: Address, signers: &[Address], c: &ClaimOnBase, r: &Receipt) -> Result<()> {
    ensure!(r.claim_id == c.id, "the backend served a receipt for another claim");
    ensure!(
        r.commitment == c.commitment,
        "the receipt is for another destination than the claim's on Base (changed since? ask again)"
    );
    ensure!(signers.contains(&r.signer), "the receipt's signer is not a signer of the network config");
    let message = destination_received(r.claim_id, r.commitment, r.received_at);
    let recovered = recover_typed(&bond_domain(chain_id, bond), &message, &r.signature)?;
    ensure!(recovered == r.signer, "the receipt's signature is not its signer's");
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

    /// R-5: a receipt arms the clock only for the claim's current destination, signed under the
    /// bond's domain by a signer of the network config; a pending claim with a destination only.
    #[test]
    fn liveness_receipts_are_checked_against_base_and_the_signer_set() {
        use alloy::signers::local::PrivateKeySigner;
        use zecured_base::sign_typed;

        let (chain_id, bond) = (84_532, Address::repeat_byte(0xb0));
        let node = PrivateKeySigner::random();
        let stranger = PrivateKeySigner::random();
        let signers = [Address::repeat_byte(1), node.address()];
        let c = ClaimOnBase {
            commitment: B256::repeat_byte(0xcc),
            ..claim(ClaimKind::Release, CLAIM_PENDING)
        };
        arming_allowed(&c).unwrap();
        assert!(arming_allowed(&claim(ClaimKind::Release, CLAIM_PENDING)).is_err(), "no destination yet");
        assert!(
            arming_allowed(&ClaimOnBase {
                status: CLAIM_PAID,
                ..c.clone()
            })
            .is_err()
        );
        let receipt = |by: &PrivateKeySigner, commitment: B256| Receipt {
            claim_id: c.id,
            commitment,
            received_at: 1_760_000_000,
            signer: by.address(),
            signature: sign_typed(
                by,
                &bond_domain(chain_id, bond),
                &destination_received(c.id, commitment, 1_760_000_000),
            )
            .unwrap(),
        };
        let good = receipt(&node, c.commitment);
        check_receipt(chain_id, bond, &signers, &c, &good).unwrap();
        // The JSON the backend serves.
        let served = serde_json::json!({
            "claimId": good.claim_id, "commitment": good.commitment, "receivedAt": good.received_at,
            "signer": good.signer, "signature": good.signature,
        });
        assert_eq!(parse_receipt(&served).unwrap(), good);
        // Another destination, a stranger, a forged signer field, another bond or chain: refused.
        assert!(check_receipt(chain_id, bond, &signers, &c, &receipt(&node, B256::repeat_byte(0xdd))).is_err());
        assert!(check_receipt(chain_id, bond, &signers, &c, &receipt(&stranger, c.commitment)).is_err());
        let forged = Receipt {
            signer: node.address(),
            ..receipt(&stranger, c.commitment)
        };
        assert!(check_receipt(chain_id, bond, &signers, &c, &forged).is_err());
        assert!(check_receipt(chain_id, Address::repeat_byte(0xb1), &signers, &c, &good).is_err());
        assert!(check_receipt(8_453, bond, &signers, &c, &good).is_err());
        let other = ClaimOnBase {
            id: B256::repeat_byte(2),
            ..c.clone()
        };
        assert!(check_receipt(chain_id, bond, &signers, &other, &good).is_err());
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
