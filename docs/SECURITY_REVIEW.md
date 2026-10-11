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
| L-8 | Low | A conforming deposit that can never be attested has no refund path | **Open.** Needs an attestation deadline in the Registry |
| L-9 | Low | Concurrent opens could bind one account to two vaults off-chain | **Open.** The Registry still enforces one account ↔ one vault (I4) on-chain |
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

## Remaining risks

- **Data sources (C-1, C-2).** A node is as honest as the chain data it reads. Independent
  operators should each run their own Zcash full node and light wallet server and their own
  Base node or provider, and enable witnesses ([OPERATORS.md](OPERATORS.md)). Until then, on
  testnet, one light wallet server feeds every node. The structural fix is verifying deposits
  on Base itself: a Zcash header relay plus deposit proofs ([TIER2.md](TIER2.md); the relay is
  built and tested, the proof is designed).
- **Three colluding signers** can still steal: the hackathon trust assumption.
- **L-8, L-9, M-5, L-11** as listed above.

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
