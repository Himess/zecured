# Economics

Who puts up what, who pays for what, and what the signers' bonds cover. Figures marked *judge* are
the Base Sepolia deployment's (`contracts/config/zecured-base-sepolia.json`); docs/MAINNET.md gives
the mainnet values. The decisions behind each rule are in docs/DECISIONS.md (D40 to D54).

## The parties

| Party | Puts up | Gets |
|---|---|---|
| Borrower | ZEC, sent to a 3-of-5 FROST vault in the Ironwood pool; it never leaves the pool | zZEC (Base, non-transferable) as Morpho collateral, and USDC borrowed against it |
| Lender | USDC supplied to the zZEC / USDC market on Morpho Blue | Morpho's interest (AdaptiveCurveIRM) |
| Liquidator | USDC to repay an unhealthy position | The seized collateral, paid in ZEC on Zcash (a Liquidation claim), with Morpho's incentive: about 12.7 % at the judge market's 62.5 % LLTV |
| Market maker | USDC to repay a borrower's debt when the borrower closes or deleverages from collateral | The repaid value plus a quoted margin (at most 5 %), in ZEC on Zcash |
| Signers (five) | USDC bonds; they run the FROST vaults and pay every claim | The opening fee |

## Bonds

Each of the five signers bonds USDC in `SignerBondV2` (*judge*: at least 100 USDC each). What the
bonds can pay for is the **effective bond**, `min(sum, 5/3 x the three smallest bonds)`: any three
signers, the fewest who can sign, lose at least 3/5 of it if they steal, so a large bond from one
signer cannot stand in for small bonds from the others.

A signer leaves in two steps: it requests an unbonding, which stays slashable until it is withdrawn
`unbondingPeriod` later (*judge*: 54 hours). A full exit needs no open vault, no open violation
case, and the challenge window passed since the last vault settled; a partial exit must keep
`minBond` and the coverage cap. The unbonding period outlasts the challenge window, the liveness
period and the commit grace together, so every claim created before an exit falls due while the
exiting amount can still pay it.

## The coverage cap

New collateral is accepted only while the bonds cover its value. The Intake reserves a proven
deposit only if the zatoshi locked on Base (zZEC supply, pending activations and refunds, and
unpaid claims) stay under `maxTotalZat` (*judge*: 5 TAZ, so that the bonds set the capacity) and,
valued at the high side of the price, within `coverageBps` of the effective bond (*judge*: 100 %;
mainnet at most 50 %). Otherwise a new vault's proof waits: it is sent again once there is room,
or its owner has it proven as a refund, and the vault stays open. A stale cap price delays an
activation the same way and never turns it into a refund. Refunds may push the locked total over
the cap; only new mints stop. A refund still without a destination after `refundDormancy`
(*judge*: 48 hours) can be marked dormant and stops counting until its account gives one, so an
unwanted note cannot hold the cap.

## Fees

- **Opening fee** (*judge*: 0.5 %, waived below 20,000 zatoshi): charged when a vault activates, as a
  Fee claim owed to the signers' treasury; the vault's collateral is its deposit less the fee. A
  0.05 ZEC vault pays 25,000 zatoshi; a 0.01 ZEC vault pays nothing.
- **Zcash fees** (ZIP 317): a payout costs `5,000 x max(2, outputs)` zatoshi, borne by its first
  claim; a Fee claim paid behind another one bears only its own 5,000.
- **Base gas**: the borrower's smart-account operations are sponsored by our paymaster, for an
  allowlist of calls only (docs/INTERFACES.md § 4); the signers pay their own commitments and
  confirmations.

## When the signers misbehave

| What happens | What Base does | Who is paid, from what |
|---|---|---|
| The signers spend a vault's ZEC without a commitment (F1), withhold a payout's change (F2) | After the challenge window (*judge*: 24 hours), all five bonds (F1) or the signing ones (F2) are slashed, minting halts, the vault is frozen and then compromised | The prover gets a bounty (10 %, capped at 50 USDC, *judge*); `ZecuredCompensator` pays the victims from the rest |
| A payout misdirects a claim (F3) or hides it from its claimant (F4) | The signers who signed it are slashed | F3: the claimant's shortfall joins the compensation layers; F4: the slash only |
| The signers stop paying a claim | Once a signer has acknowledged the claim's destination (a receipt), after `livenessPeriod` (*judge*: 24 hours, deferred by a payout in flight for 2 hours at most) the claimant takes the claim's value plus 5 % from the bonds, pro rata; never at a stale price | The claimant, in USDC; the claim's ZEC then goes to the bonds' Reimburse destination |
| Three signers confirm a payout that never reached Zcash | A confirmation is not final: only a proof of the payout's inclusion is. The claimant disputes within 24 hours; unless the payout's inclusion is proven within 6 hours, the claim is payable again | The claimant, by a new payout or from the bonds |
| Signers go silent before a vault is active | A proof that cannot be activated lands as a refund; a pending activation can be refunded after a 24-hour pause or a halt | The borrower, in ZEC |

**Compensation layers** (`ZecuredCompensator`). A theft's victims are paid from the slashed bonds in
layers, each in full before the next gets anything: (1) the prover's bounty; (2) third parties,
liquidators and market makers, whose claims the vault owed; (3) the lenders: the victim vault's
debt is repaid on Morpho, with a 1 % allowance for interest, and its unbacked zZEC is burned; (4) the
owners: their own claims and their equity. Cases within one incident window (*judge*: 3 days) share
one price. A compromised vault's collateral is burned down to the LLTV as soon as its case is
executed, and again whenever a higher price frees collateral above it (W-12), so that its owner
cannot borrow more against stolen collateral afterwards. Before the execution, during the challenge
window, the owner can still borrow up to the LLTV, and what it borrowed is the compensator's (W-24,
a mainnet design in docs/MAINNET.md § 7). When the bonds cover only
part of its debt, the repayment is made and the collateral stays burned down to the LLTV, so the
repayment cannot be borrowed again. A payout in flight at the theft is proven rather than paid twice:
the tally waits for proofs of committed payouts and of confirmations made just before the freeze. What is left goes
to a fixed sink address, never back to the signers. Freezing a vault never stops anyone repaying its
debt directly on Morpho.

So: *the bonds compensate the victims: liquidators and lenders before the borrower's own equity.*

**Pause.** Three of five signers can pause new minting for up to 30 days at a time; withdrawals,
repayments, liquidations and payouts never stop. It is a power one signer already has, by refusing
to sign a new vault, made explicit and bounded.

**Verifier versions.** Minting needs no signer, except that all five together can schedule a new
verifier version, which takes effect only after a public delay (*judge*: 10 minutes; mainnet 14 days
for a deposit guest, 7 for a violation guest).

## Pre-liquidation (opt-in)

A borrower may agree, once and explicitly, to be deleveraged early instead of waiting for a full
liquidation. Its smart account sets the terms on the SettlementAdapter (`setPreLiquidation`, and
`clearPreLiquidation` to withdraw them): a pre-liquidation LTV `preLltv` below the market's LLTV, a
close factor and an incentive that both grow linearly from their low ends at `preLltv`
(`preLcf1`, `preLif1`) to their high ends at the LLTV (`preLcf2`, `preLif2`), and an expiry. While
the position's LTV is above `preLltv` and at most the LLTV, anyone may repay up to the close factor
of its debt and receives the repaid value times the incentive in ZEC, as a Liquidation claim paid on
Zcash like any other. Above the LLTV, only Morpho's own liquidation applies. The terms are checked
when set: `preLltv` below the LLTV, both incentives between 1 and Morpho's incentive at the LLTV,
both close factors above 0 and at most 1, an expiry in the future.

The app's preset, against the judge market (LLTV 62.5 %, Morpho's incentive about 12.7 %):

| LTV | Who may act | Largest share of the debt repaid at once | The borrower pays, per USDC repaid |
|---|---|---|---|
| up to 55 % | nobody | none | nothing |
| 55 % to 62.5 % | anyone, under the borrower's terms | 10 %, rising to 50 % | 2 %, rising to 5 % |
| above 62.5 % | any liquidator, through Morpho | all of it | about 12.7 % |

On a fork of the judge market with the real Morpho Blue, a position borrowed to 50 % LTV at $40
enters the band at $36 (LTV 55.6 %): one pre-liquidation there repays about 13 % of the debt for
about a 2.2 % incentive and brings the LTV back under 55 %; if the price then falls past the LLTV,
the borrower who opted in keeps more collateral after the liquidation than one who did not. Each
pre-liquidation must seize, and leave, at least the dust floor (0.001 ZEC on the judge
deployment): with the preset, a 0.01 ZEC vault can be pre-liquidated only from about 56.4 % LTV,
and a vault below about 0.003 ZEC never; liquidation is unaffected.

The pre-liquidator is paid in ZEC on Zcash, after the signers' next payout of the vault, so it
carries the price for that wait and the payout's Zcash fee; its bot logs a profitability estimate
and acts regardless. Its claim is an ordinary Liquidation claim, covered by the bonds like any
third party's. Lenders gain from positions brought back before they reach the LLTV.

## Zecured Swap

The second product needs no signer in a trade and no bond: USDC escrowed in `ZecuredSwap` on Base
is released by a zero-knowledge proof of a shielded ZEC payment, made by whoever paid
(docs/SWAP.md). The signers'
only part is the list of verifier versions it accepts (`ZecuredVersions`, the Swap kind: added by all
five after a public delay, removed by three at once).

| Party | Puts up | Gets | Risk |
|---|---|---|---|
| Taker buying ZEC | USDC, locked when it opens the order | ZEC at its own Ironwood address, paid by the maker | time only: if the maker never pays, the USDC comes back once the order's pay window has passed (at most 6 hours on the judge deployment, 24 hours in the beta) |
| Maker (Buy) | the ZEC, paid first, and the payment's proof | the taker's USDC, once the proof has finalized | the ZEC paid, if its proof cannot land before the deadline (the verifier's emergency stop, a stalled relay): the order then refunds the taker |
| Taker selling ZEC (CLI) | the ZEC paid to the maker, and its proof | the maker's locked USDC | likewise: the payer proves before the deadline |

The maker quotes from a price feed plus a spread, within its inventory and the per-order caps
(`maxOrderUsdc`: *judge* 200, *beta* 250 USDC). Neither the judge deployment nor the beta charges a
protocol fee (`feeBps` 0). A settlement costs the payer one zkVM proof and two Base transactions
(prove, then finalize once the anchor is deep and old enough). On mainnet a maker stake would put
a price on an unpaid order (docs/MAINNET.md § 4).

## Mainnet signer economics (design)

With the mainnet template (docs/MAINNET.md § 2): five bonds of 50,000 USDC (250,000 USDC in all),
coverage k = 50 % and a cap of 100 ZEC. A 0.5 % opening fee on a full cap of new vaults earns
0.5 ZEC; if the cap turns over four times a year, the signers earn about 2 ZEC a year on 250,000
USDC of bonded capital, about 1 % a year at a ZEC price near 1,200 USD. The opening fee alone does
not pay for the capital. Options, all design:

- **Yield-bearing bonds:** bonds held as ERC-4626 shares of a lending vault and counted at a
  haircut in the effective bond, so that the capital earns while it backs the vaults.
- **A custody fee by time:** a small fee per day on the outstanding collateral, charged at exit as
  a Fee claim.
- **Signer cells:** several independent five-signer cells, each with its own cap and bonds, so
  that capital grows with demand instead of one cell's bonds bounding the protocol.

Liveness payments shrink the coverage until the bonds are topped up: the USDC paid for an overdue
claim leaves the bonds, the effective bond falls, and new minting stops earlier until the signers
add to their bonds.

## What the bonds do not cover

- A theft larger than the bonds. The coverage cap keeps the locked value within the bonds' reach at
  activation; a later price rise can outgrow them until new minting stops at the cap.
- All five signers together: they hold every vault's key and can add a verifier version that
  accepts anything after its delay. Users see the scheduled version on Base for the whole delay
  (*mainnet*: 7 to 14 days), long enough to exit or claim from the bonds.
- Liveness beyond the bonds' size: if every claim falls due at once, the bonds pay claims in the
  order their claimants come, until they run out.
- Testnet proof of work is cheap to forge; on mainnet the work required on top of a deposit grows
  with its value (docs/MAINNET.md).
- Proofs need a prover: someone must build the witness and run the zkVM. The borrower's CLI does it for
  a theft from its own vault (docs/DEMO.md); the signers do it for their inclusion proofs.
