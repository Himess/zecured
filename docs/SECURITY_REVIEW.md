# Security review, 2026-10-11

An internal review of everything off-chain (signer nodes, coordinator, backend, bots, CLI and
app) against [THREAT_MODEL.md](THREAT_MODEL.md), done before submission, followed by fixes.
The contracts are covered by their own test suite (unit, fork against the real Morpho Blue,
handler-based invariants I1–I9 and D14 with attacker actions); they were read here only to
check whether an off-chain bug could reach them.

## Method

- Manual review of every crate and of `app/`, checked against the sources of the
  dependencies actually used (orchard 0.16, pczt 0.10.0-pre.1, zcash_primitives 0.31.0-pre.1,
  frost-core / frost-rerandomized 3.0.0, alloy 2.5).
- Proof-of-concept tests for the ledger findings, run against the unmodified code.
- Every fix comes with a regression test that fails without it (checked by reverting the
  fixes one by one for H-3, H-4, I-1, M-4, C-3, H-5 and M-7).
- After the fixes, the whole flow ran again live on Zcash testnet and a Base Sepolia fork:
  vault opened in the app, sponsored borrow, partial release paid, close from collateral paying
  borrower and market maker in one transaction ([TESTING.md](TESTING.md)).

Severities are rated against the documented claims: one or two compromised signers cannot
move funds, the coordinator cannot move funds or mint, and the signers trust their data
sources for availability, not for authorization.

## Findings and status

| ID | Severity | Finding | Status |
|---|---|---|---|
| C-1 | Critical | A malicious light wallet server could fabricate a deposit, and all nodes used the same one | **Mitigated.** Each node can use its own server (`NODE_LIGHTWALLETD_URL`) and require witness servers (`NODE_LIGHTWALLETD_WITNESS_URLS`) to agree on the deposit's block hash, transaction bytes and tip; the agreed transaction must create the scanned note. Only one public testnet server runs NU7 today, so witnesses are off by default (see "Remaining risks") |
| C-2 | Critical | A malicious Base RPC could forge Registry events | **Fixed.** Logs must come from the Registry and not be removed; every activation, claim and confirmation is checked against Registry state by `eth_call`; an optional witness RPC (`NODE_BASE_RPC_WITNESS_URL`) must return identical evidence |
| C-3 | Critical | The demo "compromised" mode could disable policy checks on any number of nodes | **Fixed.** It exists only in builds with the node's `demo` feature, only on nodes started with `--demo-allow-compromise` (two fixed nodes in the demo setup), behind the coordinator's signature and a constant-time token check |
| C-4 | Critical | The vault address and viewing key reached the user unsigned from the coordinator | **Fixed.** All five signers sign `VaultOpened` (vault, owner, account, amount, expiry, hash of the address, hash of the UFVK); the app and the CLI verify the five signatures against a signer set pinned in the build before showing the address, and check the UFVK against its hash |
| H-1 | High | A coordinator could get two payouts signed from the same notes and confirm one claim with the other's transaction | **Fixed.** Nodes record the txid they signed (equal to the sighash for these v6 transactions, asserted on a real one), refuse a different payout while it can still be mined, and confirm only that txid |
| H-2 | High | Any note at the vault's internal address counted as change: one foreign note could freeze a vault, zero-value notes could inflate fees | **Fixed.** Only the planned change of a vault transaction joins the vault's books; zero-value and foreign notes are ignored; a vault never pays its own addresses |
| H-3 | High | The market maker was never paid if the borrower's share was unrevealed or dust | **Fixed.** After the grace period the market maker is paid alone; the bot refuses closes that leave the borrower dust |
| H-4 | High | Third-party outputs were not checked for decryptability, so a coordinator could make them unspendable | **Fixed.** Every non-zero output must decrypt to exactly the planned note |
| H-5 | High | One failed `confirmPayout` wedged the vault | **Fixed.** Confirmations are idempotent; the coordinator settles claims already paid on Base and retries otherwise |
| H-6 | High (latent) | The app would have taken contract addresses from the backend | **Fixed.** Addresses and signers are pinned at build time; `GET /config` is only a consistency check |
| M-1 | Medium | One misbehaving signer could stall attestations or payouts, or panic the coordinator | **Fixed.** Signatures verified per node, any three valid ones used, FROST cheater detection with retries, bounds checks, supervised coordinator loop |
| M-2 | Medium | A future vault birthday could hide a deposit from every scan | **Fixed.** The birthday must be at or below each node's own tip |
| M-3 | Medium | The paymaster allowlist did not bind sponsorship to Zecured accounts or EntryPoint v0.7 | **Fixed.** Sender must be a vault's account (deployed Safe, or CREATE2-derived from the expected factory data); EntryPoint v0.7 only; caps on batch size and gas; per-sender rate limit |
| M-4 | Medium | DKG messages were not bound to the transcript | **Fixed.** Envelopes are bound to the round-1 transcript; with C-4 the user checks that all five signers agree |
| M-5 | Medium | The light wallet server operator learns which transactions are Zecured deposits and payouts | **Open, documented** as leak vector L21 (an own full node per signer removes it) |
| M-6 | Medium | Vault ids in URL paths reached the app host's logs | **Fixed.** Vault ids live in the URL fragment, links are not prefetched |
| M-7 | Medium | Global locks held across network calls without timeouts | **Fixed.** Timeouts on every light wallet and RPC call; no store lock across network calls |
| M-8 | Medium | Unauthenticated, unbounded work (vault opening, recipients) | **Fixed.** CORS restricted to the app's origins, token-bucket rate limits, cap on vaults awaiting a deposit, signatures checked before any scan |
| M-9 | Medium | Base ingestion was not idempotent | **Fixed.** Events apply once (per-vault last applied position); a bad event is skipped with a warning |
| M-10 | Medium | Refundable notes of expired vaults disappeared after a restart | **Fixed.** They are persisted and late deposits are still found |
| M-11 | Medium | The bots trusted the commitment returned by the backend | **Fixed.** Computed and compared locally |
| L-1 | Low | Node authentication failed open without a configured coordinator; no replay protection | **Fixed.** Nodes refuse to start without one (unless `--insecure-no-auth`); replay cache |
| L-2 | Low | Error chains (URLs, heights) returned to callers or logged | **Fixed.** Fixed public error messages with codes; scrubbed logs |
| L-3 | Low | Key files world-readable | **Fixed.** Files 0600, directories 0700, old files tightened |
| L-4 | Low | The public RPC proxy shared the signers' provider and had no caps | **Fixed.** Optional separate `PROXY_RPC_URL`; batch and `eth_getLogs` range caps |
| L-5 | Low | Lost updates in the coordinator | **Fixed.** Read-modify-write under the lock |
| L-6 | Low | A confirmation could drop a newer in-flight record | **Fixed.** Compare-and-clear |
| L-7 | Low | Signing sessions and pending DKGs never expired | **Fixed.** TTLs and caps |
| L-8 | Low | A conforming deposit that can never be attested has no refund path | **Fixed** (D23). `cancelVault` (3 of 5) retired a never-activated vault on Base; the owner's refund request then paid the deposit back through P5, only once the cancellation was applied. Superseded in v3 (D41): a proven deposit that cannot activate its vault lands as a Refund claim on Base, paid like any claim |
| L-9 | Low | Concurrent opens could bind one account to two vaults off-chain | **Fixed.** Each node allows one DKG in progress per account and per owner and checks again before round 3 saves the vault; the Registry still enforces one account ↔ one vault (I4), and a deposit stranded by a lost race is refundable after cancellation (D23) |
| L-10 | Low | `GET /vaults/:id` exposed more than the app needs | **Fixed.** Minimal view, no lock nesting |
| L-11 | Low | App: `'unsafe-inline'` CSP, identifiable backup filenames, polling pattern | **Partly fixed.** Neutral filenames, staggered polling; nonce-based CSP and passphrase-encrypted keys remain |
| L-12 | Low | A test-only broadcast hook was compiled into the coordinator | **Fixed.** Behind the `test-hooks` feature |
| I-1 | Info | PCZT checks missing for dummy keys and bundle flags (denial of service only) | **Fixed** |
| I-2 | Info | A reorg deeper than the confirmation count freezes the affected vault | **Accepted.** Fails safe |
| I-3 | Info | Market-maker grace period compares wall clock and block time | **Accepted.** Liveness only |
| I-4 | Info | Overflow on server-supplied heights | **Fixed** |
| I-5 | Info | Any fetch error past expiry counted as "not mined" | **Fixed.** Rebuilt only on a definite "not mined" with all inputs unspent |
| I-6 | Info | The backend host holds the coordinator key, the demo token and the MOCK oracle key | **Accepted** for the hackathon (one operator); compromise mode is limited to two nodes |
| I-7 | Info | Network constants are testnet-only | **Accepted.** Testnet only |
| I-8 | Info | Documents overstated some guarantees | **Fixed.** [THREAT_MODEL.md](THREAT_MODEL.md) and [ARCHITECTURE.md](ARCHITECTURE.md) updated |
| I-9 | Info | Re-randomized FROST signing as used | Sound |

## Contracts

A separate review of `ZZEC`, `ZecuredRegistry`, `SettlementAdapter` and the Tier 2 header relay, with
proof-of-concept tests and a deeper invariant campaign (128 runs × 100 calls). No critical or high-severity issue
was found in the production contracts.

| ID | Severity | Finding | Status |
|---|---|---|---|
| R-1 | Medium | Relay: a reorganisation deleted every orphaned height, so a long, light attacker branch on a stale tip could push the honest reorganisation above the transaction gas limit and freeze the relay | **Fixed.** Orphaned heights are no longer cleared; every read is bounded by the tip height. Reorganising back from 3,400 orphaned heights now costs the same as from 8 |
| L-1 | Low | A position at D14's 100,000 zat floor could not be liquidated between LLTV and ~89 % LTV | **Fixed.** A seizure below the D11 floor is accepted when it clears the borrower's whole debt |
| L-2 | Low | A signed close request did not bound the debt the market maker repays (the borrower could borrow more after signing) | **Fixed.** `closeWithCollateral` takes the executor's `maxRepayAssets`; the market maker caps it at its quote plus 10 bps |
| I-1 | Info | Relay `confirmations` is nearly free to inflate on Testnet | **Documented.** Consumers must use `confirmationWork` |
| I-2 | Info | A batch of headers failed when one was already known | **Fixed.** Known headers are skipped |
| I-3 | Info | The MOCK oracle's owner can crash the price and liquidate every vault | **Documented** in [THREAT_MODEL.md](THREAT_MODEL.md) (testnet mock) |
| I-4 | Info | `VaultOpened` was not yet documented | **Fixed** (INTERFACES § 2.5, golden vector in Rust and the app) |

Checked and found sound: the zZEC transfer guard (I2), supply and claim accounting (I1, I3, I7, I9), signature
threshold and malleability (I6), close-request binding and ERC-1271 (I8), Morpho callbacks and reentrancy, the
Morpho authorization surface (no path borrows or withdraws loan assets), D14's LTV rounding, liquidation share caps,
close arithmetic, event privacy, and the relay's consensus rules.

## v3 integration (2026-10-16)

Found while switching the signers and the coordinator to the v3 contracts.

| ID | Severity | Finding | Status |
|---|---|---|---|
| V-1 | Medium | A disputed claim reopened after its inclusion window could never be committed again when its input's claim list also held a claim that stayed confirmed (the opening fee, which nobody can dispute): RT-4 asks for the same list, and every member had to be pending. The bonds then paid it, their reimbursement was blocked the same way, and the vault never settled, so no bond could leave in full; a payout proven after the window kept its vault open the same way | **Fixed** (D57). A new commitment of the input names its whole list again, confirmed claims included, and a later proof still upholds a reopened claim (`DisputedFeePayoutTest`, three more dispute tests) |

## v4 review (2026-10-17)

Four independent read-only reviews of the v3 contracts (the Intake; compensation and the slasher;
the bonds, versions and price feed; the Registry, exits and the Adapter) and an external review of
an earlier commit. Each finding below has a regression test that reproduces it; the tests of the
payout path failed on the v3 code before their fixes (eleven of them, recorded when the fixes were
made), the others assert the opposite of the v3 behaviour their names describe. The invariant
campaigns gained handler actions for liveness, receipts, unbonding, reimbursement, compensation,
proofs of payouts, pre-liquidation and the swap, which no campaign exercised before.

| ID | Severity | Finding | Status |
|---|---|---|---|
| R-1 | High | `confirmPayout` signatures could be replayed after a reopening: `PayoutConfirmation(vaultId, claimIdsHash)` named no payout and `reopenDisputedClaim` is open to anyone, so one transaction could reopen a disputed claim and confirm it again with the old signatures; its claimant never saw it pending and had to dispute it in every window | **Fixed** (D76). `PayoutConfirmation(bytes32 vaultId,bytes32 payoutRef,bytes32 claimIdsHash)`; a confirmation needs a payout committed for the vault, each payout is confirmed once, every claim is bound to the payout's input, and a reopened claim is never confirmed by signatures; nodes derive the payout reference from the transaction they signed (`test_R1_*`, handler `attackReplayOrFinal`) |
| R-2 | High | A claim paid on Zcash but not yet confirmed was compensated a second time when its vault was compromised: the tally paid every pending claim and only a disputed claim could be proven | **Fixed** (D77). The slasher records `compromisedAt`; an inclusion proof makes a committed pending claim final on a frozen or compromised vault too; `tallyClaim` waits `compromisedAt + inclusionWindow` for committed claims; an executed F2 or F4 case proves its payout's claims (`test_R2_*`, the proven-delivery orderings) |
| R-3 | Medium | Three colluders could confirm an unpaid claim on a frozen vault; after the compromise its dispute was refused and the tally skipped it | **Fixed** (D77). No confirmation on a frozen vault; a confirmation made within `disputeWindow` before the freeze counts as unpaid unless proven; older ones become final through `expireConfirmation`; the vault's tally waits until `unprovenConfirmations` is zero (`test_R3_*` in three orderings, Halmos PayoutRules P2) |
| R-4 | Low | A payout that landed but was confirmed late could be paid again by the bonds | **Fixed** by proven delivery: a proof makes a committed claim final, and the bonds refuse it (`test_R4_*`) |
| R-5 | High | Liveness asked only for a commitment and its clock ran from `createdAt`: a claimant that never handed the signers its salt and receiver made its own claim undeliverable and took 105 % of its value from the bonds when it chose; three colluders could turn the bonds into USDC with dummy claims of their own vaults | **Fixed** (D61, D70). Liveness needs a signer's `DestinationReceived` receipt for the claim's current commitment (`armLiveness`); `payableAt = max(createdAt, receivedAt)`, the earliest receipt stands, a new commitment voids it. Nodes sign a receipt only after checking the preimage, the receiver and the claim; the backend serves the earliest valid one; the app and the bots arm their claims (`test_R5_*`, LivenessReceiptsTest, Halmos B4, bond campaign B3, the nodes' receipt tests, the bots' arming tests) |
| R-6 | Medium | The bonds pay liveness pro rata from all five signers, but three chose the reimbursement destination, moving about 0.4 of the claim's value to colluders | **Fixed** (D79). `setReimburseDestination` takes all five signatures (Halmos `check_I6_reimburseDestinationNeedsAllFiveSigners`, `test_R6_*`); a reimbursement per signer share is a mainnet design (docs/MAINNET.md § 4) |
| R-7 | Medium | An executed F3 case left its claim pending, so its claimant could take the shortfall and the whole claim again from the bonds | **Fixed** (D61, D79). An executed F3 closes the claim in the Registry (`closeMisdirectedClaim`, with `F3_GAS` reserved so a starved execution cannot skip it), and the bonds refuse a claim the compensator owes (`test_R7_*`, compensation campaign `R7_R20_compensation`) |
| R-8 | Low | A refund opened on a pending vault accrued liveness although no payout could be committed for it | **Fixed** (D62), differently from the proposal: no claim ever exists on a pending vault (the refund waits for it), so its clock starts once it opens (`test_R8_*`, invariant I32) |
| R-9 | Low | The bonds paid at the oracle's last price however stale | **Fixed** (D61). A bond whose oracle reports a feed state refuses liveness while it is stale (`PriceStale`); the deploy script requires the check on mainnet (`test_R9_*`). Morpho lending at a stale price stays a mainnet item (docs/MAINNET.md § 4) |
| R-10 | Low | `commitGrace` could be shorter than a committed payout's lifetime (120 s on the theft demo and the local fork) | **Fixed** (D64, D71). The deploy script requires twice a payout's 136 blocks; the planner commits no claim within one payout lifetime of its due time (`test_R10_*`, planner tests) |
| R-11 | Medium | A full cap turned a new vault's proof into a refund that cancelled the vault for good, and refunds without a destination counted against the cap: one unwanted note could fill the judge cap for 30 days | **Fixed** (D62). A new vault's proof reverts `CapExceeded` and waits (or is proven as a refund at its owner's request, D72); a refund without a destination becomes dormant after `refundDormancy` and leaves the cap; the judge cap is 5 TAZ (`test_R11_*`, invariant I33) |
| R-12 | Medium | A proof landing as a refund opened its claim in the proof's own transaction, skipping the readiness wait, and `refundPending` checked only the best chain: a short-lived branch during a feeder outage could leave a refund claim for good | **Fixed** (D62). Refunds wait like activations (`finalizeRefund`, `dropPendingRefund`), no payout spends a reserved note, and `refundPending` checks readiness and the anchor in both branches (`test_R12_*`, invariants I13/I20, `attackSpendPendingRefund`) |
| R-13 | Medium | With a bond short, a pending activation could neither finalize, be refunded nor be dropped; renewing a lapsed pause in the same block reset the pause clock | **Fixed** (D62). The account converts its pending activation once `readyTime + pendingRefundDelay` has passed, whatever gate holds it (`test_R13_*`) |
| R-14 | Medium | A version was checked only when a proof landed: a removed (broken) guest's pending activations still minted | **Fixed** (D63). Pending activations, refunds and swap settlements keep their version; finalizing needs it active, and anyone drops them once it is not (`test_R14_*`, `test_finalize_versionRemoved`) |
| R-15 | Medium (mainnet) | A note above the largest work tier could never be refunded: proofs refused it, and the nodes' `can_land` counted every note as landable, so the off-chain refund refused it too | **Fixed**. `can_land` is false above the Intake's largest tier, so the off-chain refund (P5) applies (policy tests) |
| R-16 | Medium | Tag squatting: three signers could record a victim's future deposit tag as a change tag, so the victim's proof failed for ever and no theft was provable | **Fixed** (D78). A proven deposit overrides the change record (`ChangeTagOverridden`): another vault's record loses its claim set and its claims may be bound again; the vault's own record keeps its claim set; F1 and `upholdPayout` count only the note's own vault; the nodes check the event (`test_R16_*`, three tests; the nodes' event tests) |
| R-17 | Low | A pending activation could be dropped while a case was open on the vault | **Fixed** (D62). Both drops revert while a case is open (`test_R17_*`) |
| R-18 | Low | Nothing enforced work tiers that never decrease, positive delays, or `disputeWindow + inclusionWindow <= unbondingPeriod` | **Fixed** (D64). The constructors and the deploy script check them (`test_R18_*`) |
| R-19 | Medium | Until its settlement, the owner of a compromised vault could borrow against stolen collateral | **Fixed** (D79). `executeSlash` burns the collateral down to the LLTV at once (`burnCollateral`, retried by anyone if starved of gas); equity stays that of the outstanding at the compromise (`test_R19_*`) |
| R-20 | Medium | Partly covered or unauthorized victim vaults never closed, so no bond could ever leave in full | **Fixed** (D79). A settled victim left active is marked (`VaultCompensated`): out of `openVaultCount` and `lockedZatoshi`, burnable again and retired once its debt is gone (`test_R20_*`) |
| R-21 | Low | An abandoned refund of a compromised vault could not be compensated | **Fixed** (D79). `tallyClaim` takes written-off refunds until the incident is finalized (`test_R21_*`) |
| R-22 | High | "The last active violation version" counted versions whose verifier the router had stopped: three colluders could remove the only working one, ending every slash and inclusion proof | **Fixed** (D63). A working violation version goes only while another works; stopped ones go freely; verifier reads revert when starved of gas (`test_R22_*`) |
| R-23 | Medium | Three signers could remove every deposit version, making thefts of unproven deposits unprovable | **Fixed** (D63). The last working deposit version takes all five signers (`test_R23_*`) |
| R-24 | Medium (mainnet) | The liquidator read Morpho's stored borrow totals, which omit the interest since the market's last update: a position that crossed the LLTV through interest alone was never liquidated, and closes were quoted on the stale debt | **Fixed**. The bots accrue the totals to the latest block as Morpho's `expectedMarketBalances` does, shared by the liquidator, the pre-liquidation round and the quotes (`AccruedDebtTest`, `r24_fork_vector`, `r24_on_a_fork`) |
| R-25 | Low | Documentation drift after D57 and D54 | **Fixed** (TIER3 § 4, INTERFACES § 2.2 and § 3.3, TIER2 § 2.2, the `ZecuredEIP712` NatSpec) |

### Zecured Swap (the design's attack list)

| ID | Attack | Status |
|---|---|---|
| S-1 | One payment for two orders | **Prevented**: the memo binds one order id (K4 memo changes; `test_prove_aJournalOfAnotherOrder`) |
| S-2 | A wrong receiver or too little | **Prevented**: the commitment and the amount are checked (`test_prove_wrongReceiverOrTooLittle`) |
| S-3 | An Orchard-pool, Sapling or transparent part | **Prevented**: D5 in the statement (K4) |
| S-4 | A fake branch | **Prevented**: work tiers, the readiness wait, drops and stale-record replacement (`test_prove_anchorAndWorkTiers`, `test_finalize_anchorReorgedOut`, `test_refund_afterAFakeProofIsDropped`) |
| S-5 | A journal replayed on another order, contract or chain | **Prevented**: the order id binds them (`test_orderId_formulaAndVector`) |
| S-6 | A quote replayed | **Prevented**: the nonce is spent at `open`; a quote naming a taker is that taker's (`test_open_replayOfAQuote`, `test_open_takerBinding`) |
| S-7 | A proof at the deadline | **Defined**: a proof must come by the deadline, and no refund while one stands (`test_prove_byTheDeadline`, `test_refund_notWhileAProofStands`) |
| S-8 | A broken guest | **Contained**: three signers remove the Swap version at once; its pending settlements go stale (`test_finalize_versionRemoved`) |
| S-9 | RISC Zero's emergency stop | **Accepted payer risk**: refund after the deadline (`test_refund_afterARouterEstop`; docs/SWAP.md) |
| S-10 | Rounding | Exact zatoshi; the fee rounds down |
| S-11 | Reentrancy | **Prevented**: `nonReentrant`, SafeERC20, ERC-1271 by static call (`test_reentrancy`, `test_open_erc1271MakerCannotMeddle`) |
| S-12 | A leak through events | No event or calldata holds a Zcash identifier (`test_events_carryNoZcashIdentifier`; the leak scanner) |

### Found while integrating v4

| ID | Severity | Finding | Status |
|---|---|---|---|
| V-2 | Medium | A refund to an existing vault could override that same vault's false change record, and the override cleared the tag's claim set: a later commitment of the note could leave out a claim bound to it, which a proof of that payout then made final although it was never paid | **Fixed** before merging: the vault's own record keeps its claim set (`test_R16_theVaultsOwnRecordKeepsItsClaimSet`; the exploit now hits `ClaimSetMismatch`) |
| V-3 | Low | The nodes refused a `ChangeTagOverridden` naming the deposit's own vault, which R-12's refunds make legitimate | **Fixed**: the nodes accept it when the tag was the vault's change record, and not a deposit tag, before the event (`a_refund_to_the_same_vault_may_override_its_own_change_record`) |
| V-4 | Medium | A refund reserved before its vault was compromised could neither open (the vault is compromised) nor be dropped (its anchor stays valid), so the compensator never saw that depositor | **Open**: being fixed before the contract freeze |
| V-5 | Low | The lending market maker sent its liveness armings and its closes from one key through two providers: a nonce race could fail a user's close | **Open**: being fixed before the contract freeze |

## Remaining risks

- **Data sources (C-1, C-2).** A node is as honest as the chain data it reads. Independent
  operators should each run their own Zcash full node and light wallet server and their own
  Base node or provider, and enable witnesses ([OPERATORS.md](OPERATORS.md)). Until then, on
  testnet, one light wallet server feeds every node. The structural fix is verifying deposits
  on Base itself: a Zcash header relay plus deposit proofs ([TIER2.md](TIER2.md); the relay is
  built and tested, the proof is designed).
- **Three colluding signers** can still steal: the hackathon trust assumption.
- **M-5, L-11** as listed above.

## Checked and found sound

FROST nonces are fresh per session and never reused; each node signs only the sighash and
randomizers of a PCZT it verified itself. PCZT verification covers the transaction version,
branch id, expiry window, pools (Ironwood only), value commitments, nullifiers and randomized
keys of vault spends, the exact input set, outputs against the plan, outgoing-viewing-key
visibility, value balance against the fee and randomized-key reuse. The payout ledger pays
claims in Base order, one transaction at a time, from the whole vault, so two payouts built from
the same state can never both be mined. Owner requests are EIP-712 messages bound to the vault
with one-time nonces and deadlines. The deposit tag is keyed with the vault secret. The
browser talks only to our backend (enforced by the Content Security Policy), and the proxy
forwards no client headers.
