# Architecture

Zecured lets a ZEC holder borrow USDC on Base without the ZEC ever leaving Zcash's shielded
Ironwood pool. This document describes the system as built; interfaces are in
[INTERFACES.md](INTERFACES.md) and build-time decisions in [DECISIONS.md](DECISIONS.md).

## Components

| Component | Where | Role |
|---|---|---|
| Vault | Zcash (Ironwood) | One per loan. Its spend authority is a 3-of-5 FROST key; its address has a single Orchard-protocol receiver, so it is paid in the Ironwood pool. |
| Signer nodes (×5) | `signer/` | Each holds one FROST share per vault and an ECDSA key for Base attestations, and reads Zcash and Base through sources it configures itself (optionally checked against witness sources). Each node enforces the policy below independently. |
| Coordinator | `signer/`, `backend/` | Relays messages, builds PCZTs, aggregates signatures, submits Base transactions. It holds **no** key share: it cannot move funds or mint on its own, and the deposit address it relays is signed by all five signers. It can delay or withhold work (liveness only). |
| `ZecuredRegistry`, `ZZEC`, `SettlementAdapter` | `contracts/` (Base) | Receipt token and accounting on top of a Morpho Blue market. |
| App + proxy | `app/`, `backend/` | Browser UI; every RPC / bundler / paymaster call goes through our proxy, which logs nothing. |
| Bots | `bots/` | Liquidator and market maker (permissionless roles; ours in the demo). |

## Trust model

- A payout needs 3 of 5 signers. Each signer checks the Base event (at the `safe` block), the
  exact transaction outputs and the vault's books before signing. One or two compromised
  signers cannot move funds; three colluding signers can (hackathon trust assumption, all
  five run by us).
- Each signer is as honest as the chain data it reads: independent operators run their own
  Zcash and Base sources ([OPERATORS.md](OPERATORS.md)).
- Minting does not have to trust the signers (Tier 2, [TIER2.md](TIER2.md)): a deposit can be
  activated on a RISC Zero proof that its note is in a Zcash block committed by a header the
  on-chain relay verified, for the vault all five signers described when it opened. The
  coordinator or the borrower proves; the Registry trusts only the proof.
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
2. **Deposit (P1).** The borrower pays the denomination from Ironwood notes. After N
   confirmations the signers classify the vault's notes and attest the conforming one;
   `attestDeposit` mints zZEC straight into the account's Morpho position. Or (Tier 2) a
   deposit proof is submitted with `attestDepositWithProof`, with the same effect.
3. **Borrow.** The account's first UserOp (sponsored) deploys it, authorizes the Registry and
   the Adapter on Morpho, and borrows.
4. **Exit.** One of:
   - **Repay + release (P2):** the account repays and calls `requestRelease`.
   - **Close from collateral (P6):** a market maker repays the debt and is paid in ZEC from
     the vault; the borrower's remainder goes to a fresh Ironwood address in the same
     transaction. No outside funds touch the account.
   - **Liquidation (P3):** a liquidator repays through the Adapter and receives the seized
     ZEC; the vault stays open for the borrower's remainder.
5. **Refunds (P5).** Any non-conforming deposit can be refunded on the owner's signed request;
   the conforming deposit of a vault that was never activated, once the signers cancelled the
   vault on Base (D23).

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
- One transaction settles up to 8 claims of the vault (the Registry's limit; review İ-01): the
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

| | |
|---|---|
| I1 | zZEC total supply = Σ `outstanding` over active vaults |
| I2 | zZEC moves only between Morpho, Registry and Adapter; no account holds it |
| I3 | No vault ZEC is released before its zZEC is burned and a claim exists (except P5 refunds) |
| I4 | One account ↔ one vault |
| I5 | A `depositTag` and a `vaultId` are each used at most once |
| I6 | Fewer than 3 distinct signer signatures are always rejected |
| I7 | `CloseRelease`: `mmAmount + userAmount` = burned zZEC = `outstanding` before the close |
| I8 | The Adapter never withdraws collateral without a valid, unexpired, non-cancelled signed request whose executor is the caller |
| I9 | Seized or withdrawn zZEC never exceeds the vault's `outstanding` |
| I10 | (signers) conforming-lineage notes = `outstanding` + unpaid claims; non-conforming notes are tracked separately until refunded |

## Privacy

What each observer can and cannot see, and every leakage vector we considered, is in
[THREAT_MODEL.md](THREAT_MODEL.md) (in progress). In short: the ZEC never leaves the Ironwood
pool and never crosses pools; Base sees a fresh gasless account, a standard-size position, and
salted commitments instead of Zcash recipients; third-party payouts are encrypted without an
outgoing viewing key so the borrower's vault viewing key reveals nothing about them.
