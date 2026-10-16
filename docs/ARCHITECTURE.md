# Architecture

Zecured lets a ZEC holder borrow USDC on Base without the ZEC ever leaving Zcash's shielded
Ironwood pool. This document describes the system as built; interfaces are in
[INTERFACES.md](INTERFACES.md) and build-time decisions in [DECISIONS.md](DECISIONS.md).

## Components

| Component | Where | Role |
|---|---|---|
| Vault | Zcash (Ironwood) | One per loan. Its spend authority is a 3-of-5 FROST key; its address has a single Orchard-protocol receiver, so it is paid in the Ironwood pool. |
| Signer nodes (×5) | `signer/` | Each holds one FROST share per vault and an ECDSA key for its Base messages (`VaultOpened`, payout commitments and confirmations, a pause, a Reimburse destination, a verifier version), and reads Zcash and Base through sources it configures itself (optionally checked against witness sources). Each node enforces the policy below independently. |
| Coordinator | `signer/`, `backend/` | Relays messages, builds PCZTs, aggregates signatures, submits Base transactions. It holds **no** key share: it cannot move funds or mint on its own, and the deposit address it relays is signed by all five signers. It can delay or withhold work (liveness only). |
| `ZecuredRegistry` (with `ZecuredSpendLogic`), `ZZEC`, `SettlementAdapter` | `contracts/` (Base) | Receipt token and accounting on top of a Morpho Blue market: vaults, claims, exits, payout commitments (Tier 3) and disputes of a confirmation. |
| `ZecuredIntake`, `ZcashHeaderRelay`, `ZecuredVersions` | `contracts/` (Base) | Deposit proofs (Tier 2) against relay-verified Zcash headers, through an active verifier version; the gates of minting: the halt, the bonds, the coverage cap, the pause. |
| `SignerBondV2`, `ZecuredSlasher`, `ZecuredCompensator` | `contracts/` (Base) | The signers' USDC bonds and the liveness penalty; violation proofs (Tier 3) and slashing; compensation of a theft's victims ([ECONOMICS.md](ECONOMICS.md)). |
| App + proxy | `app/`, `backend/` | Browser UI; every RPC / bundler / paymaster call goes through our proxy, which logs nothing. |
| Bots | `bots/` | Liquidator and market maker (permissionless roles; ours in the demo). |

## Trust model

- A payout needs 3 of 5 signers. Each signer checks the Base event (at the `safe` block), the
  exact transaction outputs and the vault's books before signing. One or two compromised
  signers cannot move funds. Three colluding signers can, but every payout must be committed on
  Base first, so taking a vault's ZEC is provable and slashes their bonds, which compensate the
  victims (Tier 3, [TIER3.md](TIER3.md)). All five are run by us in this deployment.
- Each signer is as honest as the chain data it reads: independent operators run their own
  Zcash and Base sources ([OPERATORS.md](OPERATORS.md)).
- Minting does not have to trust the signers (Tier 2, [TIER2.md](TIER2.md)): a deposit is
  activated only on a RISC Zero proof that its note is in a Zcash block committed by a header the
  on-chain relay verified, for the vault all five signers described when it opened. The
  coordinator or the borrower proves; the Intake trusts only the proof, from an active verifier
  version (all five signers can add one, after a public delay). New collateral is accepted only
  while the bonds cover its value.
- If the signers stop paying a claim, its claimant is paid from their bonds after
  `livenessPeriod`; a payout they confirm but never send can be disputed (D47, D53).
- The vault address and its viewing key are signed by all five signers when the vault opens;
  the app checks the signatures before showing the address.
- The borrower gets the vault's viewing key and can check the vault independently.
- The smart account's collateral can only be withdrawn by the borrower's own `requestRelease`,
  by a liquidation, or by a close request the borrower signed.

## Vault lifecycle

1. **Open (off-chain).** The browser creates a fresh owner key and counterfactual smart
   account, signs `OpenVault`. The signers run a DKG and agree on the ZIP 2005 `sk`, giving
   the vault its viewing keys and address. The vault has a deposit expiry *height*. Nothing
   touches Base.
2. **Deposit.** The borrower pays the denomination from Ironwood notes. The coordinator (or the
   borrower) proves the deposit and submits it with `attestDepositWithProof`: the vault is
   reserved, and `finalizeActivation` mints zZEC straight into the account's Morpho position once
   the relay has moved on (`finalizeDepth` headers, `finalizeDelay` seconds) and every gate is open.
   A proven deposit that cannot activate the vault (the vault already active, the account bound to
   another vault, a halt, a full cap) lands as a Refund claim instead.
3. **Borrow.** The account's first UserOp (sponsored) deploys it, authorizes the Registry and
   the Adapter on Morpho, and borrows.
4. **Exit.** One of:
   - **Repay + release (P2):** the account repays and calls `requestRelease`.
   - **Close from collateral (P6):** a market maker repays the debt and is paid in ZEC from
     the vault; the borrower's remainder goes to a fresh Ironwood address in the same
     transaction. No outside funds touch the account.
   - **Liquidation (P3):** a liquidator repays through the Adapter and receives the seized
     ZEC; the vault stays open for the borrower's remainder.
5. **Refunds.** A proven deposit that lands as a Refund claim is paid like any claim, once the
   account gives its destination (D41). With journal v2, as on the judge deployment, a deposit of
   another amount or paid late is proven in Refund mode and lands the same way. A deposit no
   deployed guest can prove (on a deployment without refund proofs, another amount than the
   vault's) is refunded off-chain on the owner's signed request (P5); a note a deployed guest can
   prove is never refunded off-chain (I30).

## Signer policy

| Rule | Condition | Signs |
|---|---|---|
| **P0** (every request) | right vault, right signer key, unused nonce, deadline not passed **when accepted**; an accepted request is completed even if signing is slow | — |
| **P1** deposit attestation | the vault's notes in chain order (height, tx index, action index); the **first** note with: a deposit transaction that only touches Ironwood (no transparent, Sapling or Orchard-pool parts), value = denomination, height ≤ expiry height, ≥ N confirmations. Classification happens once and never changes. | `DepositAttestation` for that note only |
| **P2** release | `ReleaseRequested`; commitment matches the pre-validated destination | borrower: amount − fee |
| **P3** liquidation | `LiquidationRelease`; commitment matches the liquidator's revealed recipient | liquidator: seized − fee (`ovk = None`) |
| **P4** anything else | — | never |
| **P5** refund | a non-conforming note, owner-signed request, Ironwood destination | depositor: note − fee |
| **P6** close | `CloseRelease`; both commitments match revealed recipients | market maker (`ovk = None`) + borrower (remainder − fee) in one transaction |

Every payout returns the vault's still-unpaid value to the vault's internal address.

### Payout queue

- Each Base event creates one claim per recipient (a close creates two). Claims are paid in
  Base order, one transaction at a time; the next starts once the previous change is mined.
- One transaction settles up to 8 claims of the vault (the Registry's limit; review H-06): the
  first payable claim (or close pair), then the later payable claims in Base order. A claim that
  would pay a receiver of the transaction a second time, or name one of its salts again (E1), is
  skipped for a later transaction, and the claims after it still ride along: a liquidation split
  into many small claims to one receiver (review H-06) delays only itself.
- The transaction's ZIP-317 fee (5,000 zat per output, at least 10,000) is deducted from the first
  claim's output; the others are paid in full. A claim rides along only while the first one stays
  above the fee. zZEC is always burned in full on Base.
- A claim whose recipient is unknown is set aside (its value stays in the vault) and the queue
  moves on. Exception: a close waits up to 2 minutes for the market maker's recipient so both
  outputs share one transaction; after that the borrower is paid alone. A close pair is never
  split to ride along.
- A claim not larger than the fee of a transaction paying it alone is dust and stays in the vault
  (it never rides along on another claim's fee either). The Adapter enforces a minimum liquidation
  size, so dust cannot be used to clog the queue.
- Claim ids are never paid twice. A transaction that misses its expiry height is rebuilt for
  the same claims. Zcash transaction ids are never written to Base.

## Invariants

I1 to I31, where each is enforced and which tests check it: [INVARIANTS.md](INVARIANTS.md). The
original ten: zZEC supply equals the outstanding of active vaults (I1); zZEC moves only between
Morpho, the Registry and the Adapter (I2); no ZEC leaves a vault before its zZEC is burned and a
claim exists (I3); one account, one vault (I4); a deposit tag and a vault id are used once (I5);
fewer than three distinct signer signatures are always rejected (I6); a close splits exactly the
outstanding (I7); the Adapter acts only on a valid signed request of the account (I8); nothing
seizes more than the outstanding (I9); the signers' books hold what Base owes (I10).

## Privacy

What each observer can and cannot see, and every leakage vector we considered, is in
[THREAT_MODEL.md](THREAT_MODEL.md) (in progress). In short: the ZEC never leaves the Ironwood
pool and never crosses pools; Base sees a fresh gasless account, a standard-size position, and
salted commitments instead of Zcash recipients; third-party payouts are encrypted without an
outgoing viewing key so the borrower's vault viewing key reveals nothing about them.
