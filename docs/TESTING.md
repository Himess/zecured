# Testing and evidence

What is tested automatically, what was run end to end, which failure scenarios were exercised, and
how to reproduce all of it. Everything here ran on testnets only: Zcash testnet, Base Sepolia itself (the live run of
27 October 2026), and local forks of Base Sepolia and Base mainnet. Amounts are testnet ZEC (TAZ, also written ZEC below); 1 ZEC = 100,000,000 zatoshi (zat). A full
Zcash txid links to ZecBlock's testnet explorer, which shows NU7's v6 transactions and their Ironwood actions (checked
on 2026-10-27).

## At a glance

| Claim | Evidence | Section |
|---|---|---|
| Real zero-knowledge proofs are accepted on Base | Real Groth16 seals of every pinned guest (deposit, F1, `Included`, swap payment) pass RISC Zero's deployed verifier router on Base Sepolia and Base mainnet forks, in 12 tests: `cd contracts && forge test --match-path test/fork/RealSealsV2.t.sol --isolate`. The live router accepts the deposit's seal by `eth_call`: one `cast call`, in the README | § 2b |
| Base checked Zcash's proof of work on a public chain | An earlier version of the header relay (`src/relay` at `f5f1d91`) took real Testnet headers on public Base Sepolia in 274 transactions up to height 4,491,413, the last on 12 October 2026 at 07:32 UTC; one header with its Equihash costs about 620,000 to 670,000 gas: [`0xab07…8FA5`](https://sepolia.basescan.org/address/0xab07317f7B9390394271D9c03FE2E912C0E58FA5) | § 2b |
| The whole system ran live on a public chain | On 27 October 2026, on Base Sepolia and Zcash testnet, from the app in a browser: a 0.05 TAZ deposit minted on its Groth16 proof (made on a rented RTX 4090) through RISC Zero's deployed router, 10 MockUSDC borrowed by a brand-new account that never held ETH, a close from collateral paid in one 3-of-5 FROST Zcash transaction whose delivery was then proven on Base, and a swap of 5 MockUSDC settled on the proof of its Zcash payment | § 2c |
| The flows work end to end | Loans opened, proven, borrowed against, repaid, released, closed from collateral and liquidated; refunds, disputes, payments by the bonds, the swap and the app in a browser; real Zcash testnet transactions, the Base side on local forks of Base Sepolia, proofs checked by a MOCK verifier, and live on Base Sepolia with real proofs | § 2, § 2c |
| Nothing on Base names a Zcash transaction | On the live deployment, none of the 181 Zcash identifiers of its runs (5 wallets, 3 vaults) in its 89 events and 60 transactions' calldata; on the forks, none of the 302 of the v4.1 run in its deployments' 77 events, 55 transactions' calldata and 177 storage words, and none in any earlier scan | § 2c, § 4 |
| Two signers cannot move funds, and three that steal are slashed | The threshold demo (two compromised signers sign a sweep, nothing moves) and the theft demo (three sweep a vault, F1 detected two minutes later, all five bonds slashed, the victim repaid) | § 2 |
| The contracts hold their rules | 804 tests, 35 invariants in four campaigns, 22 symbolic checks (Halmos) | § 1, § 1b |
| How long it takes | Live: the deposit proven on Base 11 min after it was mined (ten Zcash confirmations, then 2 min 10 s of proving on a rented RTX 4090 and its CPU), the loan active 2.5 min later, a swap settled 14.5 min after its order. On a fork, in the browser: the deposit seen 2 min 50 s after its broadcast, its proof on Base after 4 min 25 s (MOCK verifier), the loan active after 7 min 56 s; a deposit's STARK takes 28.1 s on one RTX 4090, and its Groth16 wrap about a minute on an x86 machine | § 2, § 2b, § 2c |

## 1. Automated tests

| Layer | Command | Tests | Covers |
|---|---|---|---|
| Contracts | `cd contracts && forge test --isolate` | 804 (6 skipped: five need a Base mainnet RPC, one measures the relay on a mainnet fork), and four invariant campaigns | Unit and fork tests against the real Morpho Blue code, on a Base Sepolia fork: deposits through the Intake (proofs, pending activations and refunds, the coverage cap, the pause), exits, payouts and their commitments, signed confirmations and proven delivery, bonds with liveness receipts, slashing, compensation, disputes, verifier versions (the Swap kind), pre-liquidation, Zecured Swap, the mainnet price feed, every deployment config; a regression test for every v4 review finding (docs/SECURITY_REVIEW.md); real Groth16 seals of a real deposit and of a real F1 spend, verified by RISC Zero's verifier router. The invariant campaigns (`test/invariant`, `test/unit/ZecuredSwapInvariant.t.sol`): the main one (18 invariants over deposits, gates, exits, payouts, proofs of payouts, disputes, refunds, pre-liquidation and attacks), the bonds (7: receipts, unbonding, reimbursement), compensation through the real slasher and compensator (6) and the swap (4); each run at 64 x 200 for this commit, 12,800 calls, no reverts (docs/INVARIANTS.md) |
| Contracts on a Base mainnet fork | `ZECURED_FORK_NETWORK=base-mainnet ZECURED_FORK_BLOCK=52688338 forge test --isolate` | 533, one skipped (the v3 suites; not yet re-run on v4) | The same suites on mainnet's Morpho Blue and RISC Zero router (same addresses); the real seals verify there too. The skipped test's EIP-712 vectors are for chain id 84532 (docs/MAINNET.md) |
| Contracts, symbolic | `cd contracts && FOUNDRY_PROFILE=halmos halmos` | 22 checks | The zZEC guard from any state; the Registry's I1, I6, I7 and I9 rules, and the rule that only the Intake activates a pending deposit, for exactly its reserved amount (K3); proven delivery's transitions and R-3's expiry (PayoutRules); the bond's effective bond, deposits and R-5's receipt rules; the compensation layers' order; for every value of their inputs (§ 1b) |
| Deposit and violation statements (Tiers 2, 3) | `cargo test -p zecured-deposit-statement` (and with `--features zecured-deposit-statement/mainnet`), `tier2/target/release/zecured-tier2-host parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_witness.json` and `... violation parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_f1_witness.json` (each network's host) | 49 on each network, and 50 (Testnet) or 51 (Mainnet) + 15 zkVM runs (in CI on the Docker-built guests of each network) | Statement v2: the real deposit's witness proves its journal v2 (a cross-language vector for the Intake); K4's single changes of it (the mode, the epoch and branch id, the transaction's shape in each mode, a changed transaction re-anchored in a block of its own, the txid path, B's leaf, the history path and peaks, T's commitments, the expiry in each mode, the note opening, the recipient, the vault address and its network, the vault's amount in each mode, the tag key) each with its outcome natively on both networks and, through `parity`, in the RISC Zero executor; transaction shapes a witness cannot carry (an Orchard-pool bundle, v5, another epoch); the changes accepted on purpose (authorizing data, header bytes outside the commitments), every vault field binding the journal, and the regression of the two-tag attack (one note cannot land under two tag keys, in either mode). Tier 3: F1 on a real Testnet spend of the TEST-ONLY vault, F2 on its change, F3 with the vault as claimant (only the claim's own output to its exact receiver counts, and a note that does not match its `cmx` pays nothing: E1), F4 with the tag key only, `Included`, every opening field and key bound, the epoch, the journal ABI |
| Payment statement (Zecured Swap) | `cargo test -p zecured-payment-statement` (and with `--features zecured-payment-statement/mainnet`), `tier2/target/release/zecured-tier2-host payment parity tier2/payment-statement/tests/fixtures/TEST-ONLY_swap_payment_witness.json` | 4 + 10 (one ignored: the fixture writer) on each network, and 42 (Testnet) or 43 (Mainnet) single changes in the zkVM (in CI on the Docker-built guests) | The synthetic payment (D68) proves its 192-byte journal; each single change of it (epochs, D5, every inclusion relation, the note opening, the memo's prefix and order id, the salt, the amount) gives the same outcome natively and in the RISC Zero executor; the real deposit of the TEST-ONLY vault is refused only at the memo |
| Rust workspace | `cargo test --workspace --locked` (plus `-p zecured-node --features demo`, `-p zecured-backend --features test-hooks`), run in a target directory of its own | 600 in CI on Linux, 597 on Windows, which does not build three Unix-only checks (8 ignored); 52 with `demo` and 81 with `test-hooks` (1 ignored) on Windows, 134 together on Linux | ZIP 2005 key derivation against published vectors; FROST DKG and re-randomized signing; an offline FROST-signed Ironwood spend; P1 classification; the payout ledger and PCZT verification; EIP-712 parity with Solidity (every v4 message: the R-1 confirmation, liveness receipts, operator messages, swap quotes, the owner's `ProveRefund`); D16 request authentication; scanner reorg detection; the paymaster allowlist (with liveness armings and pre-liquidation terms); a regression test for every finding of the [security review](SECURITY_REVIEW.md), and one named after each finding of the review of the v4.1 diff (N-1 to N-12: the confirmation that follows the planned anchor, the F4 rules, a foreign payout's anchor, the arming and case keepers, the treasury keys file, the Ironwood index, the swap payers' check right before the broadcast, the maker's places and reservations); Tier 3: the commitment each node derives, the claims a payout keeps, every v4 event checked against Registry state; v4: the planner's rules (O-4), receipts from the nodes and the backend, proven delivery and the keepers (refunds, expiries, case proofs, burns), invites, `zecured-ops`, the bots' interest accrual (R-24) and their liveness armings, the market maker's nonce lock, the swap crate, the maker and the payment witness builder, the leak scanner; the Zcash network from the configuration (D81: its pairing with the Base chain, encodings, branch ids and epochs, the relay's network id) |
| App | `cd app && pnpm typecheck && pnpm test && pnpm build` | 451 tests (35 files) | Unified address encoding (F4Jumble), commitments, EIP-712 messages and their golden vectors (owner requests, `ProveRefund`, deleverage, swap quotes), liveness receipts, Safe address prediction, the call allowlist, Morpho math and pre-liquidation previews, every ABI fragment against the compiled contracts, API parsing, the evidence file's schema, the replay (a poll inside a step's block skips nothing; its recording holds no secret) |

The Rust EIP-712 digests are checked against the same golden vectors as the Solidity tests,
so signers and contracts cannot silently disagree on a message format.

## 1b. Symbolic checks (Halmos)

`cd contracts && FOUNDRY_PROFILE=halmos halmos` runs the `check_` functions in
`contracts/test/halmos/` with [Halmos](https://github.com/a16z/halmos) 0.3.3 (settings in
`contracts/halmos.toml`: loops up to 8, and 100 ms for each branch's feasibility query, so that a slow runner explores
the same paths as a fast one). All twenty-two run in about four minutes on a 6-core desktop. Their inputs are symbolic:
callers, recipients, amounts, the function called and the signatures. A passing check holds for
every value of them, where a fuzz test tries samples.

| Check | Property | Paths, time |
|---|---|---|
| **zZEC guard** (`ZZECGuard.t.sol`: the token alone, from any state) | | |
| G1, G2, G7 | Only the Registry mints, and only to itself. Only the Registry burns, and only from itself or the Adapter. The supply changes only then, by exactly the amount | 164 paths, 1 s |
| G3 | The Morpho expectation is only ever cleared, or armed by the Registry or the Adapter for itself | 160 paths, 1 s |
| G4 | A transfer that does not leave Morpho succeeds only as Morpho pulling the Registry's zZEC, whatever allowance was given | 65 paths, 0.4 s |
| G5 | A transfer out of Morpho goes only to the armed recipient, for at most the armed amount, and only once. Only that recipient settles the expectation, and it learns the exact amount | 53 paths, 0.5 s |
| G6 | An induction step. Take any storage in which some address other than the Registry, the Adapter and Morpho holds no zZEC, and those three hold the whole supply. Any call by anyone keeps both. Both hold at deployment, and G3 keeps the expectation's recipient the Registry or the Adapter, so no account ever holds zZEC after any sequence of calls (I2) | 232 paths, 21 s |
| **Registry rules** (`RegistryRules.t.sol`: the real Registry and zZEC, a MOCK Morpho) | | |
| R1 (I6) | The signature check passes only with at least 3 signatures that recover to distinct signers. Its bitmap, which Tier 3 slashes by, names exactly those signers. `setReimburseDestination` behaves the same at its entry point, like `confirmPayout` and `commitSpend` | 1,198 paths, 13 s; at `setReimburseDestination` 315 paths, 20 s |
| R2 (I7) | `recordClose` succeeds only from the Adapter, on an active vault, when the two amounts add up to the outstanding. It burns exactly the outstanding from the Adapter, records exactly the two claims and closes the vault | 22 paths, 2 s |
| R3 (I9) | `recordLiquidation` succeeds only from the Adapter, on an active vault, seizing more than 0 and at most the outstanding. It burns exactly that amount and records exactly one claim for it | 26 paths, 2 s |
| R4 (I1, I3) | Whichever exit runs (release, partial release, liquidation or close), whoever calls it, and with any arguments: releases come only from the vault's account and settlements only from the Adapter. The burn equals the total of the new claims and the drop in the outstanding, and the supply stays equal to the outstanding | 45 paths, 3 s |
| R5 (K3) | From a pending reservation, whichever deposit hook is called by anyone with any arguments: only the Intake's calls succeed; `activate` mints exactly the reserved amount, for exactly that reservation; every other hook leaves the supply alone, and a refund adds exactly one Refund claim of its amount | 43 paths, 1 s |
| **Bond rules** (`SignerBondV2Rules.t.sol`: the real bond, MockUSDC) | | |
| B1 | From any state, the effective bond never exceeds the sum of the bonds | 620 paths, 12 s |
| B2 | The effective bond's second term is the sum of the three smallest bonds, so any three colluders lose at least 3/5 of the effective bond | 427 paths, 17 s (z3) |
| B3 | From any state, a deposit adds exactly its amount to its signer's bond and changes no other bond or unbonding | 7 paths, 1 s |
| B4 (R-5) | Without an arming for the claim's current commitment, `livenessDue` is (0, 0) whatever the claim's kind, commitment, creation and the arming's commitment and time | 83 paths, 4 s |
| B5 (R-5) | Without such an arming, `slashLiveness` never pays, at any time | 23 paths, 3 s |
| B6 (R-5) | Armed for its current commitment at any time, the claim is paid once due, whatever its kind, commitment, creation and the delay | 16 paths, 3 s |
| **Payout rules** (`PayoutRules.t.sol`: the real Registry and spend logic, the slasher's views a MOCK) | | |
| P1 (D76) | Proven delivery: `upholdPayout` succeeds exactly for a claim that is pending, disputed or confirmed by signatures (not final, not closed) and a payout recorded for the claim's input and vault; the claim is then paid for good, and the books (unpaid claims, `lockedZatoshi`'s pending zatoshi, the unproven confirmations) move by exactly that claim | 167 paths, 12 s |
| P1' | Only the slasher proves | 5 paths, 0.2 s |
| P2 (R-3) | `expireConfirmation` makes a confirmation final exactly when its dispute window is over and its vault did not freeze within that window | 32 paths, 2 s |
| **Compensation rules** (`CompensationMath.t.sol`) | | |
| C1 (I25) | The layers take from the funds in order, never more than their totals or the funds, and hold nothing back: either every layer took its whole total or the funds are used up | 47 paths, 23 s |
| C2 (I25) | A layer that took its whole total is paid in full; a layer that took nothing of a non-zero total is paid nothing | 3 paths, 0.03 s |

What these checks do not show:
- **The Registry checks are bounded.** They start from one vault opened through the Registry's own
  Intake hooks, with symbolic amounts and part of its collateral already with the Adapter, and run
  one call. They are not proofs over every sequence of calls; the handler-based invariant suites
  cover longer sequences by fuzzing. The zZEC checks start from any state of the token instead
  (symbolic storage, and an expectation armed by the Registry, the Adapter or no one). Each one
  therefore holds for every single call, wherever it comes in a sequence, and G6 extends I2 to every
  sequence by induction.
- **They do not check the cryptography.** Halmos treats `ecrecover` as an uninterpreted function: a
  signature recovers to whatever address the solver picks. R1 therefore checks the counting for every
  possible recovery, and takes ECDSA itself as given.
- **They do not use the real Morpho.** Halmos has no fork mode, so the Registry checks use
  `MockMorphoForHalmos`: no interest, no debt and no health check. The D14 LTV limit is left to the
  fork tests and the D14 fuzz test.
- **They are not a formal verification or an audit** of the contracts.

None of the zZEC and Registry checks passes vacuously: each was run against a deliberately broken
copy of the contract (the v2 Registry, before the round of docs/DECISIONS.md D40) and returned a
counterexample. The bond and compensation checks are backed by the fuzz tests named in their
files:

| Broken rule in `src/` | Caught by |
|---|---|
| zZEC: anyone, not only Morpho, may move the Registry's zZEC into Morpho | G4 |
| zZEC: a transfer out of Morpho may exceed the armed amount | G5 |
| zZEC: the transfer out of Morpho does not consume the expectation | G5 |
| zZEC: mint to any address | G1 |
| zZEC: burn from any address | G6 |
| zZEC: `expectFromMorpho` arms the Registry as recipient, whoever calls | G3 |
| Registry: the same signer may sign twice (`<` for `<=` in the order check) | R1, at both entry points |
| Registry: signer 4 sets signer 3's bit | R1 |
| Registry: a close whose two amounts add up to less than the outstanding | R2 |
| Registry: a liquidation burns the whole outstanding | R3 |
| Registry: a partial release records a claim one zatoshi larger than its burn | R4 |

The first version of the zZEC checks started from states built by calls (mint, supply, withdraw to
the Adapter). The `expectFromMorpho` mutation broke that setup: every path reverted before reaching
an assertion, and Halmos reported an error instead of a counterexample. The zZEC checks now start
from symbolic storage, which no change to the token can break.

## 2. End-to-end runs

The runs in the first table used the v2 contracts (signer attestations, 2026-10-11 to 10-14); the v3 runs on a local
fork follow it (2026-10-16), then the v4 runs (2026-10-19 and 10-20) and v4.1's (2026-10-21). On 27 October the whole
system ran live on Base Sepolia (§ 2c); no deployment was kept running after it (D82).

Setup: Zcash **testnet** after NU7 through `testnet.zec.rocks`, five signer node processes plus
the coordinator talking over HTTP, and the Base side on a local fork of Base Sepolia (the real
Morpho Blue deployment, a fresh Zecured deployment) started with `scripts/local-fork.sh`.
The CLI's Base commands are TEST ONLY: the loan owner's key acts as the account and pays its
own fork gas. The app uses a fresh, gasless Safe instead.

| Flow | What happened | Evidence |
|---|---|---|
| Spike A | 3-of-5 FROST vault spends an Ironwood note | Zcash testnet block 4,486,491 |
| Tier 3: a payout committed before it is signed (2026-10-12) | 0.01 TAZ vault `0x59d5e63e…` attested on the fork; `requestRelease`; the three chosen nodes each derived and signed the payout's `SpendCommit`, the coordinator recorded it with `commitSpend`, every node checked the record three blocks deep before releasing its share, the payout was broadcast, mined, and `confirmPayout` accepted the committed claim; the vault settled (`openVaultCount` 0). `zecured-wallet watch --tier3` with the vault's UFVK: "lineage note 0xe43d283f spent by its committed payout: ok" | deposit [`bd6daa8e120b391625c6b962684875f761e36e2fa0d7e0bc1eeb34e53bacaf9a`](https://testnet.zecblock.com/tx/bd6daa8e120b391625c6b962684875f761e36e2fa0d7e0bc1eeb34e53bacaf9a) (block 4,490,621); payout [`19eab8ea2907ca1f24e2a516885a4e8fb091ce78468febd92da9afd334eddca8`](https://testnet.zecblock.com/tx/19eab8ea2907ca1f24e2a516885a4e8fb091ce78468febd92da9afd334eddca8) (block 4,490,655); `SpendCommitted` with keyed `payoutRef` `0xcdf694fd…`, input tag `0xe43d283f…`, signer bitmap `0x0d`, no txid, note or address on Base |
| P5 refund | 0.02 TAZ sent to a 0.01 vault is classified non-conforming; owner-signed refund; PCZT verified and FROST-signed by nodes 1–3 | 0.0199 TAZ back at block 4,486,629 |
| P1 + P2 | 0.01 TAZ deposit → `attestDeposit` with 3 signatures → zZEC minted straight into the account's Morpho position → borrow 1 USDC → repay → destination pre-validated → `requestRelease` → payout → `confirmPayout` | 0.0099 TAZ back; vault Closed; zZEC total supply 0 |
| P6 close from collateral | Market maker repays on Morpho through the SettlementAdapter; no outside USDC reaches the account | Market maker 336,667 zat and borrower 653,333 zat in **one** Zcash transaction (block 4,486,717) |
| P3 liquidation | Bot liquidates after a price drop; liquidator output created with `ovk = None` | Liquidator 835,070 zat; the 154,930 zat remainder released to the borrower |
| Threshold demo | Nodes 4 and 5 switched to "compromised" sign a sweep; nodes 1–3 refuse under policy | Threshold not reached; nothing moved |
| D16 | Unsigned request to a node API | Refused; requests signed by the coordinator open vaults normally |
| D14 partial release | 0.01 TAZ vault, borrow 0.1 USDC (25 % LTV), withdraw 0.004 ZEC while the loan stays open (LTV after: 42 %), then repay and release the rest | 390,000 zat received (block 4,486,890, tx [`233cf3f68fea8526a1e6fd06983010a44bc823e1ac4526df6bb0e1a1778ad95a`](https://testnet.zecblock.com/tx/233cf3f68fea8526a1e6fd06983010a44bc823e1ac4526df6bb0e1a1778ad95a)); vault stayed Active with 600,000 zat; then 590,000 zat (block 4,486,900, tx [`2dfa25afa503464ab047b2527558bf0419a6c6aa708b5f576549cf61aa2826be`](https://testnet.zecblock.com/tx/2dfa25afa503464ab047b2527558bf0419a6c6aa708b5f576549cf61aa2826be)); vault Closed |
| **App, end to end** | Vault opened in the browser; 0.01 TAZ paid to it; the first sponsored UserOp (through the backend proxy to a local alto bundler and mock paymaster) deploys the Safe, authorizes the Registry and the Adapter on Morpho and borrows 0.1 USDC; a 0.004 TAZ partial withdrawal by UserOp; the vault checked with its UFVK (`zecured-wallet watch`); close from collateral signed in the browser (ERC-1271) and executed by the market maker | 390,000 zat (block 4,488,446, tx [`4c928da23c7136296c0ac1b7e769ab2fa9b9b2ddf2871eb6ba9b4852af99ca1c`](https://testnet.zecblock.com/tx/4c928da23c7136296c0ac1b7e769ab2fa9b9b2ddf2871eb6ba9b4852af99ca1c)); borrower 337,500 zat and market maker 252,500 zat in one transaction (block 4,488,453, tx [`a3d0f0e10df413004daca268f504b11af823493a60df002d009487d0eed6bb06`](https://testnet.zecblock.com/tx/a3d0f0e10df413004daca268f504b11af823493a60df002d009487d0eed6bb06)); vault Closed. The account never held ETH or zZEC |
| **After the security fixes** | The same app flow on a fresh deployment with the fixed signers, coordinator and paymaster allowlist | 390,000 zat (block 4,488,561, tx [`1558eea9fb60a5827a05c2d24436a38dacdc72997217e5fbaf048eca2a87d2ed`](https://testnet.zecblock.com/tx/1558eea9fb60a5827a05c2d24436a38dacdc72997217e5fbaf048eca2a87d2ed)); borrower 337,500 zat and market maker 252,500 zat in one transaction (block 4,488,577, tx [`6166f6f392fd6d9568774b6f637079f46be99ca4aca5a16c7881ed9afd40804a`](https://testnet.zecblock.com/tx/6166f6f392fd6d9568774b6f637079f46be99ca4aca5a16c7881ed9afd40804a)); vault Closed |

In every run the account's zZEC balance stayed 0 (I2): zZEC only ever sat in Morpho, the
Registry and the SettlementAdapter.

### v3 on a local fork (2026-10-16)

A fresh v3 deployment on an anvil fork of Base Sepolia (block 48,206,967): the nine contracts, the real header relay
checkpointed at the Testnet tip and fed over P2P, `MockRiscZeroVerifier` with the v2 Testnet guests' image ids as the
active versions, five signer node processes and the coordinator, all on the Windows development machine. The Zcash side
is real Testnet: TAZ from the Valar faucet; every deposit and payout below is a Testnet transaction. The seals are MOCK
(`mock-host`: the native statement on the real witness, since RISC Zero does not run on Windows); everything else is
the code a testnet deployment runs.

| Flow | What happened | Evidence |
|---|---|---|
| Deposit proof, pending activation, activation | 0.01 TAZ paid to vault A. The coordinator built the witness from Testnet data (the first one, with the NU7 epoch's summaries, in about 8 minutes), proved it into the Intake (pending until the relay had gained 6 headers and 150 s had passed), then finalized it: 1,000,000 zat of zZEC straight into the account's Morpho collateral | deposit [`dd07aa9e68ed547fd092f8449ca2be5204e99e8bb683b2fe1b5f0b3975524388`](https://testnet.zecblock.com/tx/dd07aa9e68ed547fd092f8449ca2be5204e99e8bb683b2fe1b5f0b3975524388) (block 4,509,466) |
| Borrow, repay, release | 0.1 USDC borrowed and repaid; `requestRelease`; the payout committed on Base, FROST-signed by nodes 1, 2 and 4, confirmed: vault closed, zZEC supply 0 | payout [`d41c2feb4ab540b811d1d3ac38e98b8c29e21b79f8e0bc6694bcd4e2f0718dde`](https://testnet.zecblock.com/tx/d41c2feb4ab540b811d1d3ac38e98b8c29e21b79f8e0bc6694bcd4e2f0718dde) (block 4,509,505, 990,000 zat) |
| Refund by proof (T-02) | 0.0123 TAZ (another amount), then 0.01 TAZ paid to vault B. Once B was active the coordinator proved the first note in Refund mode: a Refund claim of 1,230,000 zat, its destination given from the account, paid by nodes 1, 2 and 3 | deposits [`66fa53d4d191312cd42fe2d83d52977be59137b63e55723e684e97125b821b71`](https://testnet.zecblock.com/tx/66fa53d4d191312cd42fe2d83d52977be59137b63e55723e684e97125b821b71), [`95d51562985c38349cfe33ca2b04c8971637368a80e4efc8a6cac7b3df72fde6`](https://testnet.zecblock.com/tx/95d51562985c38349cfe33ca2b04c8971637368a80e4efc8a6cac7b3df72fde6); refund [`86e1ffedc5877ccf963fe3b6fb54c9a1055b4c0ed6bd2c5a13fec56548b36463`](https://testnet.zecblock.com/tx/86e1ffedc5877ccf963fe3b6fb54c9a1055b4c0ed6bd2c5a13fec56548b36463) (block 4,509,534, 1,220,000 zat) |
| Dispute, reopening, the bonds, a late proof (D53, T-05, D57) | Vault A's confirmed release disputed although it had arrived. The slash's confirmation work (38 M) took about 45 minutes to gather, longer than the fork's 20-minute inclusion window, so the claim was reopened on its original liveness schedule and the account had the bonds pay it (13.2825 USDC). Then the coordinator proved the payout's inclusion and the claim was upheld as paid: the vault settled again | the payout above |
| Release after a refund | Vault B's collateral released and paid by nodes 1, 2 and 3 | payout [`97b7c1deb34ebd4569985e1ac5aa1d00097ba3f4873a7d3ee680122573994896`](https://testnet.zecblock.com/tx/97b7c1deb34ebd4569985e1ac5aa1d00097ba3f4873a7d3ee680122573994896) (block 4,509,573, 990,000 zat) |

Not in this run: the opening fee (the fork's config charges none), liquidation and close from collateral, and the
app's sponsored UserOps. Twice the coordinator asked for signatures right after a new Base event that the nodes had
not applied yet; they refused, and its next pass, about 80 seconds later, went through.

### v4 on a local fork (2026-10-19 and 2026-10-20)

Two runs of the v4 code on fresh anvil forks of Base Sepolia (blocks 48,316,593 and 48,379,206), set up as the v3 run:
a fresh v4 deployment and ZecuredSwap, the real header relay fed over P2P, the MOCK verifier with the pinned Testnet
guests' image ids (deposit, violation and payment), five signer node processes, the coordinator, the liquidator, the
lending market maker and the swap maker, all on the Windows development machine; Zcash is real Testnet (TAZ from the
Valar faucet). The fork's configuration charges the 0.5 % opening fee (`ANVIL_FEE_BPS=50`, a 20,000 zatoshi floor), and
its payout lifetime is 240 s (its liveness period is 600 s, see below). Times are UTC; a payout's Zcash transaction is
named by the first 8 hex digits of its txid and its block.

| Flow | What happened | Evidence |
|---|---|---|
| Opening fee (D48) | 0.05 TAZ vault: deposit proven and accepted about 9 minutes after its broadcast, activated 3 minutes later with a Fee claim of 25,000 zatoshi to the treasury's commitment (collateral 4,975,000). The Fee claim was paid at once, alone, so it bore its payout's whole ZIP 317 fee (10,000), as ECONOMICS says; borrow 18 USDC, repay, release | fee payout `c7ee110e` (block 4,518,214, 15,000 zatoshi to the treasury); release committed 09:55:07, confirmed 09:58:18 |
| Liquidation | 0.01 TAZ vault borrowed to 57.7 % LTV; the demo shock set to 90 % at 09:46:14; the liquidator liquidated at 09:46:22 and armed its claim's liveness with a signer's receipt (R-5) at 09:47:53 | Liquidation claim 722,472 zatoshi; payout `dc7c669a` (block 4,518,234), ovk none |
| Pre-liquidation (D60) | 0.01 TAZ vault borrowed to 49.8 % LTV with the app's preset terms (55 %, close factor 10 to 50 %, incentive 2 to 5 %); shock 87 % at 09:48:20 (LTV 57.2 %): the bot pre-liquidated at 09:48:25, repaying 1.384 USDC for 129,401 zatoshi (it logged that the round lost money on gas, as designed: logged, not enforced) | payout `71e2f625` (block 4,518,234) |
| Close from collateral | `loan close` on a 0.01 TAZ vault with 5 USDC of debt: the market maker repaid through the SettlementAdapter in one Base transaction | CloseUser 541,138 + CloseMarketMaker 458,862 zatoshi = the outstanding 1,000,000 (I7); one payout, `4a3f0840` for the market maker (block 4,518,244) |
| Zecured Swap, Buy | Quote, `open` with the taker's USDC and receiver commitment, destination to the maker; the maker paid with memo `"ZSWP" \|\| orderId` and ovk none, proved the payment (MOCK seal) and settled | order `0x21660b10`: 0.01 ZEC for 11.115555 USDC; paid 09:52:27, proven 09:57:39 (anchor 4,518,235), settled 10:00:14 |
| Zecured Swap, Sell (CLI) | `zecured-wallet swap sell`: the maker locked USDC, the wallet paid 0.01 ZEC, waited for the order's work on the relay, proved and settled | order `0x6fc71e83`: payment `d5aaef42` (block 4,523,265), settled 20:54:23 |
| Swap refund | A Buy order without a destination: `refund` refused before the pay window ended (`PayDeadlineNotPassed`); after the window (the fork's clock moved 7,300 s on) it returned the taker's 12.7765 USDC | order `0x5b4f2e87` |
| Two signers down | Nodes 4 and 5 offline: a 0.01 TAZ vault borrowed, repaid and released; nodes 1, 2 and 3 committed, signed and confirmed the payout | payout `b8fac240` (block 4,523,234), confirmed 20:34:02 |
| One signer down at opening | Node 5 switched offline from the admin panel: `loan open` refused at once ("Not enough signers are available"), no vault created; node 5 back, openings work again | 22:53:43, refused in 1 s |
| Dispute, then proof (T-05, D57, section 3) | A confirmed release disputed by its claimant at 20:35:24; the coordinator queued the payout's inclusion proof at dispute priority (20:36:59), proved it (20:38:06) and Base took it: the claim is final (20:38:15), 2 min 51 s after the dispute | payout `398fda25` (block 4,523,236) |
| Proven delivery, routine (D76) | A partial release (0.004 ZEC) of a vault that stays open: confirmed 21:04:35, its inclusion proven on Base 21:08:53 by the coordinator's routine proof, inside the 10-minute dispute window | payout `5c71277d` (block 4,523,295), 390,000 zatoshi |
| Receipts and liveness (R-5, T-05) | Release requested with three signers up, then nodes 3, 4 and 5 offline: nothing could be committed. `slashLiveness` before arming: refused (`NotLivenessEligible`); `loan arm` with a signer's receipt at 20:46:31; once due (20:55:10) `slashLiveness` paid the claimant 13.2825 USDC (value × price × 1.05). Back online, the signers did not pay the claim again; the five gave its Reimburse destination (R-6, `zecured-ops`), and the signers paid that | bond payment 20:55:15; Reimburse payout `2b6e77ae` (block 4,523,295), 990,000 zatoshi |
| Refund lifecycle (R-11, R-12) | The coverage cap made full (the MOCK base price raised to 50,000 USD): the deposit of a new vault was proven but waited; the owner asked for it back (`loan prove-refund`); the refund proof was accepted as pending (21:10:29), finalized once the relay moved on (21:13:04), given a destination and paid | Refund claim 1,000,000 zatoshi, payout committed 21:16:59, confirmed 21:20:08 |
| Underwater liquidation | The liquidator stopped, the shock lowered in two steps to 56.25 % (LTV about 107 %), the liquidator restarted: it liquidated the whole collateral; Morpho realized the bad debt (about 1.29 USDC of the lenders' 100,000) | Liquidation claim 1,000,000 zatoshi, payout confirmed 21:51:05 |
| Browser, gasless | The app (`next dev`, pinned to the fork's deployment) in a browser: a 0.01 TAZ loan, its owner key and Safe account made in the browser, the vault's five signatures checked there; paid from the CLI wallet; the first UserOp deployed the account, authorized the Registry and the Adapter on Morpho and borrowed 4 USDC (577,222 gas); the signers validated the destination; "Repay with USDC, then release" in one UserOp (401,324 gas); the app fetched a signer's receipt and armed the claim's liveness with a sponsored UserOp. Every request of the browser went to the backend only (its RPC, bundler and paymaster proxy) | deposit `5bfdaa09` 22:53:06, activated 23:02:00; borrow `0xb750ec91`; release payout `1182cbdc` (block 4,523,628), committed 23:12:31, confirmed 23:16:01 |
| Demo path (P-1) | A vault opened by the CLI; `POST /demo/vaults/:id/pay` with a 20-bit stamp: the demo wallet paid the vault, the vault activated; a second request was refused (`demo_already_paid`) | demo payment `85890de1`; activated 20:25:52 |
| Operators' drill (O-3) | `scripts/ops-drill.sh --send`: a pause with two signatures refused, with three sent; the unpause sent; a Swap version scheduled by all five and the removal of the last working deposit version simulated (three signatures refused, R-23) | each stage in 0 to 4 s |
| Leak scan (P-6) | `zecured-leak-scan` over the second run's deployment and swap records, with `--blocks --anvil-state`: 298 identifiers of the six test wallets and six vaults | 159 events, 108 transactions' calldata, 320 storage words: **0 matches** |

Found and fixed during these runs: the fork's fee setup lacked a fee floor (the Registry refused it); a local fork's bond
transactions had been committed as Base Sepolia's (removed; the bond script now records a fork's under `data/`); the
fork's 600-second liveness period was shorter than a payout's lifetime, so R-10 never let the signers commit a claim the
bots armed at once (the fork now sets `payout_lifetime_secs`); the fork's slash work (38,000,000, about 45 minutes of
Testnet blocks) never let a delivery proof beat its 10-minute dispute window (now 3,000,000); the wallet stopped on a
swap payment the server did not know yet, and reused an owner request's nonce after a request two signers had kept;
the coordinator proved an upheld payout a second time before the slasher's event arrived, and called
`finalizeActivation` on every pass while a liveness payment had left a bond below the minimum; two scripts did not
parse in bash 5.2. The first run's fork stopped after 28 hours when the machine ran out of memory.

**The theft demo (T3-6) on the second fork (2026-10-20).** Its own deployment on the same fork (a copy of the anvil
configuration with `theftDemo` set and the demo's 10 USDC bonds, `DEPLOYMENT_NAME=anvil-theft-demo`), its own five
signers with fresh keys and its own coordinator, the `demo-tier3` build (nodes on ports 9201 to 9205, backend on 8180),
the same relay and the MOCK verifier; DEMO.md's scenes:

| Scene | What happened | Evidence |
|---|---|---|
| Setup | All five bonded (5 × 10 USDC); a 0.01 TAZ vault opened on the demo's backend and paid: its deposit proven and accepted 7 minutes after the broadcast, the vault activated 3.5 minutes later | activated 22:32:16 |
| Compromise three | `POST /admin/nodes/{3,4,5}/mode` with `compromised`: all three accepted (outside this build the panel stops at two) | |
| Steal | `POST /admin/vaults/<id>/steal` to a test wallet of ours: the three compromised nodes signed the sweep in 8 s, with no commitment on Base | sweep `a8c1f1ba` (block 4,523,546), broadcast 22:33:24 |
| Detect | `zecured-wallet watch --tier3` with the vault's viewing key: `VIOLATION F1`, the vault's lineage note spent by a transaction no commitment names, and the command that builds the witness | 22:34:50, the block after the sweep |
| Prove | `build-violation uncommitted` anchored at 4,523,547: the witness in 68 s; the MOCK host sealed its journal (the zkVM proves such a witness for real, § 2b) | the journal: kind 1, the anchor, the payout's reference, the note's tag; no Zcash transaction, note or address |
| Slash | `proveViolation` once the anchor had the fork's slash work (22:38:08, 363,360 gas with the MOCK verifier): the vault frozen at once; `executeSlash` after the 2-minute window (22:40:14, 522,976 gas) | all five bonds 0, `halted()` true, the 5 USDC bounty cap owed to the prover, the vault compromised |

The compensation then ran as far as it goes for this victim: `tallyVault`, `finalizeIncident` (the incident at
$1,265, every layer covered at 100 %) and `settleVault`. The victim was the CLI's account, which had never borrowed and
so had never authorized the Registry on Morpho: by design its share is zero, since an unauthorized vault's zZEC cannot
be retired (the app's first UserOp gives that authorization). The leak scan of this deployment (222 identifiers of two
wallets and the vault; 48 events, 25 transactions' calldata, 115 storage words) found **0 matches**.

Found and fixed: `signer-network.sh init` gave a node without a key in `.env` an empty key, which the node refuses, so
a network of fresh keys (the theft demo's) could not be initialized. In the browser run, the app warned that a correct
deposit "cannot back the loan" and kept asking for the payment until its proof was on Base (V-10).

Closing and deleveraging from the app could not run on the second fork: its swap refund test had moved the fork's
clock 7,300 s ahead, so a request the browser dated by its own clock had already expired there (`RequestExpired`), and
the coordinator's R-3 keeper, which reads its own clock, saw confirmations as disputable for longer than Base did.

**A third fork (2026-10-20 23:30 to 21 00:20 UTC).** A fresh fork (block 48,385,520), its clock in step with real
time, with the V-10 fix, ran what the second could not, the app in a browser and the theft demo again:

| Flow | What happened | Evidence |
|---|---|---|
| Deposit in the browser (V-10) | No warning while the deposit waited. The signers saw it with its confirmations 2 min 50 s after the broadcast, and the page said "Deposit received on Zcash" and stopped asking for the payment; proof on Base after 4 min 25 s, active after 7 min 56 s | deposit `9ede7868` 23:42:32, activated 23:50:28 |
| Borrow with protection | The first UserOp deployed the account, authorized Morpho, borrowed 4 USDC and set pre-liquidation terms (the standard band, from 55 % LTV) | `0x503ff334`, 729,131 gas |
| Deleverage | The market maker's quote: 2 USDC of debt for 0.00159684 TAZ (margin 1 %; 1 USDC was refused, its collateral below the 0.001 ZEC minimum); signed in the browser (the Safe's ERC-1271) and executed: LTV 31.6 % → 18.8 % | `0xb42ab487` 23:53:19, 372,438 gas; its payout confirmed 00:01:45 |
| Close from collateral | Quote: the market maker repays the last 2 USDC and receives 0.00159684 TAZ, the borrower's remainder 0.00680632 TAZ; signed in the browser and executed | `0xfcf84e1a` 23:55:22 |
| Liveness from the browser | The deleverage's payout was confirmed only after 5 min 49 s (slow Testnet blocks), and by then the fork's 10-minute liveness period was too close: R-10 kept both close claims uncommitted. The app had armed the borrower's claim with a signer's receipt at 23:55; once it was due (00:05:24) the page offered "Claim from the bonds", and a sponsored UserOp paid 9.040494 USDC, the claim's value at $1,265 plus 5 % | `0x2835db42`, 257,048 gas |
| Theft demo with compensation | Its own deployment again; the victim borrowed 3 USDC first, which authorized the Registry on Morpho. Sweep by nodes 3, 4 and 5; F1 detected two minutes later; witness in 64 s; `proveViolation` 00:04:40; `executeSlash` 00:06:47 (600,002 gas: with debt, the collateral is burned down to the LLTV, R-19); then `tallyVault`, `finalizeIncident` once the 10-minute incident window was over (00:16:53), `settleVault` (the debt repaid from the slashed bonds, the vault's zZEC retired) and `withdrawEquity` | sweep `43ea5e91` (block 4,523,730); the victim's account 3 → 12.65 USDC, the full value of its 0.01 ZEC at $1,265; 32.35 USDC left with the compensator |

The leak scan of both deployments (the lending one with the browser's vault and two wallets: 225 identifiers, 63
events, 38 transactions' calldata, 140 storage words; the theft demo's: 240 identifiers, 63 events, 30 calldata, 139
storage words) found **0 matches**.

Found and fixed: after the deleverage, the position page said "Part of the collateral was liquidated": a deleverage and a
pre-liquidation pay through a Liquidation claim like a liquidation, which the page cannot tell apart, so it now says
that part of the collateral went to whoever repaid part of the debt.

### v4.1 on a local fork (2026-10-21)

A fresh v4.1 deployment on an anvil fork of Base Sepolia (block 48,411,390), a relay fed from P2P, the MOCK verifier with
the pinned Testnet image ids, five nodes and the coordinator with the treasury keys (W-6), the swap maker, real Zcash
Testnet:

| Flow | Result | Evidence |
|---|---|---|
| Two deposits (0.01 TAZ) | Broadcast 14:15:17, seen by the signers 14:19, proof on Base 14:21:24, active 14:24:17 | deposits `4cc54e5e`, `3b42fdc8` |
| Borrow and partial release | 2 USDC; 0.004 ZEC committed 14:28:24, broadcast 14:28:26, confirmed by signatures | payout `cfb10c40` (block 4,525,841) |
| A late payout's anchor (W-10) | Planned at 4,525,837 (this fork's network config had no payout delay, so `r` = 0); the payout waited in the mempool and was mined at 4,525,841, late, so its routine delivery proof anchored at `h + 1 + r` = 4,525,842, queued 14:34:51 and final on Base 14:36:15 | proof `0x8d259151` |
| Earliest-receipt armings (W-4) | The coordinator armed both claims with their earliest receipts | coordinator log |
| Reimbursement end to end (W-6) | Nodes 3 to 5 stopped; the release went overdue and its claimant took it from the bonds (`slashLiveness`, 157,919 gas). `zecured-ops` refused to sign without the treasury keys, and refused a destination prepared with other keys ("not the claim's treasury destination: not signed (W-6)"); three operators signed the derived one, sent 3 of 3. With the nodes back, the coordinator registered the derived recipient (14:46:17); the payout was committed 14:47:29, held one block (L23), confirmed 14:50:27, and the treasury's wallet found the note: 990,000 zatoshi at block 4,525,883 | `0x00a16af0`, `0xaa12d8fc` |
| A planned anchor (W-10) | With `payout_delay_max_blocks` 8: planned at 4,525,892 (`r` = 2), the payout mined at 4,525,883. Its proof anchored at exactly 4,525,892, the plan, nine blocks above the payout's block and not at `h + 1`; queued 15:00:37, ready 15:02:16, by which time the confirmation had expired undisputed (R-3), so the proof was not sent and its anchor was read from the seal file's journal | seal file |
| Swap Buy, the maker (W-7, W-10, W-19) | The maker quoted only once its engine had passed once (W-7: the relay's lag comes from a pass); a cap of 3 open orders per taker. 0.005 ZEC for 6.388 USDC: scheduled at 4,525,923 (a two-block delay), anchor planned at 4,525,928 (`r` = 1); paid 15:07:54, mined at 4,525,926, proven on Base 15:16:32 under the header at 4,525,928, the plan, not `h + 1` | open `0x9b67ca47` |
| Swap Sell, the CLI (W-9, W-10) | The CLI took ZecuredSwap from the pinned record (`anvil.swap.json`). 0.003 ZEC for 3.757 USDC: scheduled at 4,525,929 (four blocks), anchor planned at 4,525,933 (`r` = 2); the payment was mined at 4,525,933 itself, so late, and its proof anchored at `h + 1 + r` = 4,525,936 (MOCK seal). Both orders settled, at 15:20:39 and 15:20:41; the CLI's whole Sell took 12 min 13 s | open `0xa4196792`, proof `0x69874164` |
| Leak scan (P-6) | The lending and swap deployments, three wallets (the main one, the maker's, the treasury's) and two vaults: 302 identifiers; 77 events, 55 transactions' calldata, 177 storage words | **0 matches** |
| The Ironwood index (W-11) | Testnet's whole Ironwood history from 4,134,000 (about 392,000 blocks, 231,000 Ironwood transactions, an 8.3 MB index) read in 27 s on the first run; a later run in 1 s | — |

Found and fixed: on Windows a debug build of the CLI overflowed the main thread's 1 MiB stack while proving a payment
(Halo 2 ran inside `main`'s future, since v4); the wallet and the Sell payer now prove on a blocking thread, as the
coordinator always did. And a claim was armed eleven minutes after a node could give its receipt: the arming keeper had
counted its first, empty look (no receipt yet) as a try and waited the ten-minute keeper period; the keepers that wait
on the nodes (armings, treasury recipients) now look again every 30 seconds.

## 2b. Tier 2 deposit proofs (docs/TIER2.md)

| What | How | Result |
|---|---|---|
| History tree from public data | `history-root --tip 4488543` (23,517 V3 leaves from compact blocks, tree states and P2P headers) | Recomputed `hashBlockCommitments` equals the real header's |
| Witness of a real deposit | `build-witness` for vault 9 (0.01 TAZ at 4,488,543), anchor 4,488,560 | Native `verify` accepts; `VaultOpened` struct hash equal to `cast`'s |
| The guest on that witness | `zecured-tier2-host execute` | Journal byte for byte equal to the native one; 20.2 M cycles (36.2 M before removing redundant note commitments) |
| A real proof | `zecured-tier2-host prove` on this 8 GB Apple M2 (CPU only: r0vm has no Metal backend) | A 22.1 M-cycle probe of the same Zcash code produced a verified succinct receipt in 108 min, with the machine swapping heavily. The run on the real deposit's witness (20.2 M cycles) was interrupted when the desktop app froze under that memory pressure; the real proofs were then made on a rented GPU (below) |
| Proofs on a rented GPU (2026-10-14) | `scripts/gpu-prove.sh` on one RTX 4090 (a 48 GB board), the CI's pinned guests (their SHA-256 and image ids checked on the machine), 2^20-cycle segments | The real deposit (TEST-ONLY vault 9): a verified succinct receipt in 36 s (22.0 M cycles with padding, 21 segments, at most 13 GB of GPU memory). The F1 violation: 48 s (28.8 M cycles, 28 segments). The Poseidon254 identity step: 0.6 s. About 40 minutes of machine time in all, about $0.45. CUDA 13.1's headers do not compile risc0 3.0's kernels, so the script builds with CUDA 12.8 |
| Groth16 on that GPU | `risc0-groth16` 3.0.5's CUDA prover (`risc0-groth16-sys` 0.1.0, rzup component 0.1.0), also run alone in a fresh process (`p254`, then `shrink-wrap`) | Fails every time with "illegal memory access". `compute-sanitizer` places it in the prover's `witness_into_poly` kernel, which reads 50–115 GB past the witness buffer. The GPU memory passed a write-and-verify test over all 46 GB. So the Groth16 wrap goes RISC Zero's default way, Docker on x86, from the succinct receipt, which holds no viewing key |
| Groth16 with Docker on x86 (2026-10-14) | `zecured-tier2-host groth16` (r0vm 3.0.6, RISC Zero's `risc0-groth16-prover:v2025-04-03.1` image) on a 15-vCPU x86 VM, from the two succinct receipts | Deposit seal in 61.5 s, F1 seal in 64.7 s, at most 9 GB of RAM; both verified by the host. About 20 minutes of machine time, about $0.08 |
| STARK on a CPU (2026-10-14) | `zecured-tier2-host prove` (r0vm 3.0.6, no GPU) on the same x86 VM: 15 vCPU of an i9-11900KB, 48 GB RAM, 2^20-cycle segments, the real deposit's witness | Stopped after 38 min without a receipt: the segments were proven (at most 9.9 GB of RAM) and the recursion was still running. The same proof takes 36 s on the GPU. A server without a GPU would keep a judge's deposit waiting well over half an hour |
| Real seals on-chain | `RealGroth16Seal.t.sol` on a Base Sepolia fork, and an `eth_call` to the live router | RISC Zero's router (`0x0b14…b711`, Groth16 v3.0.0 selector `0x73c457ba`) accepts both seals, and refuses them for a journal changed by one zatoshi or by one beneficiary byte. The deposit, through the real relay, the real router and the deposit guest's verifier version, lands as a pending activation and is minted straight into Morpho by `finalizeActivation`: `attestDepositWithProof` costs 676,458 gas (the version lookup, the five `VaultOpened` signatures and the reservation included), `finalizeActivation` 251,507. The F1 case slashes all five bonds and pays the bounty to the journal's beneficiary: `proveViolation` costs 623,893 gas (re-measured 2026-10-21 on v4.1). Both seals verify the same way through the router on a Base mainnet fork |
| Statement v2's seals on a CPU (2026-10-20 and 21, O-8) | `zecured-tier2-host prove`, then `groth16`, with the pinned Testnet ELFs (their SHA-256 and image ids checked on the machine) on a 4-vCPU x86 server without a GPU, 2^18-cycle segments; the Groth16 container capped at 5 GB of RAM and 3 CPUs | deposit-test on the TEST-ONLY vault 9 witness: a verified succinct receipt in 7 h 58 min (26.3 M cycles with padding, 101 segments, at most 2.4 GB of RAM), its journal byte for byte the native one, Groth16 in 5 min 21 s; the `Included` witness of vault 9's payout (derived from the F1 fixture, K only, not committed): 57 min 44 s (3.2 M cycles, 13 segments), Groth16 in 6 min 2 s. A deployment that proves its users' deposits needs a GPU (next row) |
| Statement v2's proofs on a rented GPU (2026-10-21, O-8) | `scripts/gpu-prove.sh` on one RTX 4090 (24 GB) with 24 vCPU, the same pinned ELFs, 2^20-cycle segments; the host built with CUDA 12.8 in 14 min | Verified succinct receipts: deposit 28.1 s (22.0 M cycles, 21 segments), `Included` 4.0 s (3.1 M, 3), payment 28.1 s (22.0 M, 21), F1 36.9 s (28.8 M, 28). About 30 minutes of machine time, about $0.35. The Groth16 wraps of the payment and F1 receipts on the 4-vCPU server: 7 min 29 s and 7 min 38 s |
| Statement v2's seals on-chain | `RealSealsV2.t.sol` (12 tests) on a Base Sepolia fork, and on a Base mainnet fork | RISC Zero's router accepts all four seals. The deposit, through the real relay and an Intake reading journal v2 with NU7's branch id, lands (`attestDepositWithProof` 675,341 gas, real verification included) and `finalizeActivation` mints it into Morpho. The F1 seal slashes all five bonds and pays the bounty (`proveViolation` 623,907 gas: v4.1's per-note F1 count adds a storage slot, W-2). The `Included` seal makes a claim committed to that payout final with no confirmation (`proveInclusion` 381,156 gas) and upholds a disputed confirmation. The payment seal settles the synthetic fixture's order on a ZecuredSwap that takes the router (`proveSettlement` 388,043 gas; its anchor on a MOCK relay, the fixture's header being synthetic). Each seal proves no other amount, epoch, beneficiary, payout, order or image id |
| The live router, by anyone (2026-10-27) | `cast call` of `verify(seal, imageId, sha256(journal))` on RISC Zero's router on Base Sepolia (`0x0b14…b711`), with the deposit-test seal of the row above (published as `site/evidence/deposit-test.seal`) and its journal v2 | Accepted (an empty return); the same call with one digit of the journal's digest changed reverts with `VerificationFailed` (`0x439cc0cd`) |
| On-chain | `DepositProofs.t.sol`: the same journal against header 4,488,560 verified by the relay on-chain, MOCK verifier | Vault reserved and then activated: 446,148 gas for the proof with the MOCK verifier (676,458 with the real router, row above), 251,507 for `finalizeActivation` (v4.1: its version must be working, W-13, and its note free of F1 cases, W-2); 20 tests, among them every refusal (a seal or image id that is not an active version, the journal's length and words, the anchor's work, the signatures) and the journal v2 golden vector |
| Headers over P2P (Rust) | `update-headers` / `build-witness --update-headers` | 969 new headers from Testnet peers; the rebuilt witness gives the same journal |

## 2c. Live on Base Sepolia (2026-10-27)

The whole system on a public chain for one day, then stopped (D82). The contracts of
`contracts/deployments/base-sepolia.json` (with its `.swap`, `.relay` and `.paymaster` records), verified on Basescan,
the loan token our MockUSDC (D109); RISC Zero's deployed router (selector `0x73c457ba`) checked every proof. The five
signer processes, the coordinator, the prover, our bundler and paymaster proxy, the market maker and the swap maker
ran on one rented machine: the STARKs on its RTX 4090, the Groth16 wraps on its CPU. The app was the live build,
published at an unlisted address that talked to that backend; every step below was pressed in a browser.

- **Rehearsal**, 12:40 to 13:51 UTC: vault `0xd03284d0658761da6fc5983cb473fac7bdd932cff38e23aa541e401f4cb80647`
  closed from collateral by the market maker
  ([`0xbf8852f9…a603`](https://sepolia.basescan.org/tx/0xbf8852f90d59a3e0637a13db2db91adcd34954079d832aec0936f9f09674a603)),
  its payout's delivery proven at 13:51:35; swap order `0xaf8f05b7…52a7`, 2 MockUSDC for 156,537 zat, settled; and a
  0.01 TAZ vault opened from a check of the app, left unpaid to expire.
- **The recorded run**, 17:52 to 19:36 UTC, in the video and in the judges' replay (`app/lib/replay/recording.json`):
  vault `0x64197b7179a4d1b617bad0808d9ef54c05aa5a99c0a1d672e90da63611a20862`, 0.05 TAZ paid by the demo wallet; the
  close's payout and the swap's payment went to the CLI wallet.

| Step of the recorded run | UTC | After the previous step | Transaction |
|---|---|---|---|
| Vault opened, signed by 5 of 5 | 17:52:31 | | |
| Deposit mined: 0.05 TAZ, Ironwood → Ironwood | 18:00:23 | | [`e5eaf50d…75825c`](https://testnet.zecblock.com/tx/e5eaf50dc641b30f25c2cdc7785099979c66fa3d6ced5c7baaf5ad580d75825c) (block 4,548,243) |
| Ten confirmations | 18:05:01 | 4 min 38 s | |
| Deposit proven on Base (STARK and Groth16 wrap: 2 min 10 s) | 18:11:34 | 6 min 33 s | [`0x92af7a0f…4384b`](https://sepolia.basescan.org/tx/0x92af7a0f3260c389799961c49a4971e76e5938b3eaac3e02c70bfa6b1464384b) |
| Activated: zZEC minted into Morpho | 18:14:06 | 2 min 32 s | [`0xbfe27bc5…f9e219`](https://sepolia.basescan.org/tx/0xbfe27bc562805b8d29cd853cac428e63769a83aa64088502580a9f7e63f9e219) |
| Borrowed 10 MockUSDC, gas sponsored | 18:18:13 | 4 min 7 s | [`0x0e17d8c2…469c9d`](https://sepolia.basescan.org/tx/0x0e17d8c2a2ee01206b9372998e43dc71bd6c59416d30ba6ff39479d7b9469c9d) (UserOp `0x657fd818…774c66`) |
| Closed from collateral: 0.04176581 TAZ to the borrower, 0.00798419 TAZ to the market maker | 18:21:33 | 3 min 20 s | [`0xf9c5f4bc…2059e3`](https://sepolia.basescan.org/tx/0xf9c5f4bcfda141209063d1e7a6e03c4f991d4995d845d88bee80c2ad102059e3) |
| Payouts committed (2 claims) | 18:25:11 | 3 min 38 s | [`0x7fdbf471…2761a7`](https://sepolia.basescan.org/tx/0x7fdbf4711abefc85427f9c5602fff64cac33d529c18323fee8878fdcb72761a7) |
| Both paid in one 3-of-5 FROST transaction | 18:29:07 | 3 min 56 s | [`0e82a053…3e29fa`](https://testnet.zecblock.com/tx/0e82a05392014ada0859325e570666bb205553849c9ffc6e529b41cda93e29fa) (block 4,548,312) |
| Their delivery proven on Base: the claims final | 19:36:21 | 67 min 14 s | [`0xee567c75…3edce9`](https://sepolia.basescan.org/tx/0xee567c750be2e9f60096590b5f9fcbbb7af5e2792e17b64fed636fcbee3edce9) |
| Swap: the order opened from the user's own wallet, 5 MockUSDC locked | 18:30:41 | | [`0x27198f05…180e1a`](https://sepolia.basescan.org/tx/0x27198f0529e57f14b69d92ecaf0d828cf68c3b474c807ec936baf0ed6f180e1a) (approval [`0x2f7cc959…ceacd8`](https://sepolia.basescan.org/tx/0x2f7cc9593f1e64005906e7e1b1f3ef4dc1e42d25bcf2b3c8e7c657cb8dceacd8)) |
| Swap: the maker paid 391,343 zat | 18:32:45 | 2 min 4 s | [`fa6ce860…245ee0`](https://testnet.zecblock.com/tx/fa6ce860bce2a39b3be793d36d6e767b1e9cd738b0d716bc9185ea7797245ee0) (block 4,548,320) |
| Swap: the payment proven on Base | 18:42:27 | 9 min 42 s | [`0xac5ad94f…c318fa`](https://sepolia.basescan.org/tx/0xac5ad94fc62de4dbe76d9b17243f39713bffe6f8fc29a2e8167699c394c318fa) |
| Swap: settled, the tokens released to the maker | 18:45:12 | 2 min 45 s | [`0xd1c1989d…038d69`](https://sepolia.basescan.org/tx/0xd1c1989d699bb55d0b78d04a61f7ca77996043c4d139769d48b307bed7038d69) |

Times are block times. The vault's opening fee, 25,000 zat, was paid alone to the treasury (15,000 zat after its
network fee, [`76918843…2b82a0`](https://testnet.zecblock.com/tx/769188431dddce153a4769bfeb0f6994c100d34bd0c74bc3a7b03dd1bb2b82a0),
block 4,548,282). The CLI wallet received 4,166,581 zat from the close (its 4,176,581 less the payout's 10,000 fee)
and 391,343 zat from the swap. Each payout's delivery proof took about 1 min 20 s to make; the coordinator queued it
about an hour after the payment (priority `RoutineInclusion`). Zcash testnet reorganized four times that day (the maker
saw the chain change below heights 4,547,942, 4,548,266, 4,548,471 and 4,548,495): the signers and the maker rescanned
and carried on, and no step was affected. Feeding the relay cost the relayer about 0.0025 ETH an hour.

**Leak scan of the live deployment**, once both runs were over (blocks 50,858,809 to 51,039,726): `zecured-leak-scan`
with the coordinator's three vaults and the five wallets of the runs (the demo wallet, the treasury, the market maker,
the swap maker and the CLI wallet): 181 identifiers, 89 events and 60 transactions' calldata, **0 matches**. The
deployment record's 13 contracts went in two records of 7 and 6, since the public RPC takes at most 10 addresses per
`eth_getLogs`.

## 3. Robustness scenarios

| Scenario | How | Result |
|---|---|---|
| Two signers offline during a payout | Nodes 4 and 5 set to `offline` (their API refuses everything) before the D14 partial release | Destination pre-validated, payout signed by nodes 1, 2, 3, confirmed on Base. Nodes 4 and 5 kept following Base and, back online, held the same claim states as the others |
| Signer restart mid-payout | Node 2 killed and restarted between the payout broadcast and its confirmation | Its persisted in-flight record let it check the mined transaction and sign `PayoutConfirmation` |
| Payout expires unmined | TEST ONLY flag `test-withhold-next-broadcast` in the coordinator's data directory (only in a backend built with `--features test-hooks`): the next payout is signed and recorded but never broadcast | Expired at height 4,486,950; the coordinator rebuilt it, nodes 1–3 verified and signed the new PCZT, mined at block 4,486,956 (tx [`9d323f2cbd3ac0fa0397ab4445bd76f54a273cca84de97b50aa92f6fd0f2be82`](https://testnet.zecblock.com/tx/9d323f2cbd3ac0fa0397ab4445bd76f54a273cca84de97b50aa92f6fd0f2be82)), confirmed on Base, vault Closed |
| Lost broadcast response | The coordinator stores a signed payout before broadcasting it and rebroadcasts it on every pass until it is mined or expires | Nothing new is built for the same claims meanwhile; a second payout is impossible anyway (I10, nullifiers) |
| Zcash reorg | The scanner refuses a block whose `prev_hash` is not the hash it scanned last | Unit test; the node then rescans the vault from its birthday |
| Base reorg | Nodes and the coordinator ingest Registry events only up to Base's `safe` head | By construction |
| Out-of-policy PCZT | Wrong expiry window, recipients, amounts, fees or value balance | Refused by every honest node (first run on testnet: all five refused a PCZT outside the expiry window; adversarial unit tests) |

## 4. Privacy checks

- Service logs (five nodes, backend, bots) were scanned for unified addresses, viewing keys,
  salts, Base addresses and IP addresses: none, apart from each service's own bind address.
  Vaults appear in logs only as an 8-hex-digit prefix of the vault id.
- Error messages returned or logged by the signers never embed receivers, viewing keys or salts.
- Base events and calldata carry only salted receiver commitments, never a Zcash address,
  txid or note commitment (enforced by the contract interfaces, checked on each deployment by
  the leak scanner below).
- **Leak scanner** (`tools/leak-scan`). It gathers, in memory only, the identifiers of our own
  runs: for every note of the test wallets and of the coordinator's vaults, the transaction id
  and the spending transaction's id (both byte orders), the note commitment, the nullifier and
  the raw receiver; for every vault, its viewing key, tag key and address. It then searches
  every event and every calldata of the deployment's contracts since their deployment block
  and, on a local fork, every storage word (`anvil_dumpState`, in slot order so that a value
  written over two slots stays contiguous). It prints counts only: a match is named by its kind
  and where it was found, never by its value, and fails the run.

  ```bash
  cargo run -p zecured-leak-scan -- --deployment contracts/deployments/anvil.json       --rpc http://127.0.0.1:8545 --wallet data/wallet/wallet.json       --coordinator signer/data/coordinator --blocks --anvil-state
  ```

  Self-check on a local anvil (2026-10-17): a probe contract's event, its creation and a call
  to it were read (1 event, 2 calldata, 1 storage word); one of our own Testnet vaults'
  address, sent to the probe as calldata, was reported as "vault address in the calldata of
  transaction …" with exit status 1, and the value itself was not printed.
- Payout transactions are ordinary Ironwood transactions: the txids above show nothing but
  shielded actions on a block explorer.

## 5. Reproducing

```bash
scripts/local-fork.sh start
export ZECURED_DEPLOYMENTS=$PWD/contracts/deployments/anvil.json BASE_SEPOLIA_RPC_URL=http://127.0.0.1:8545 \
       BUNDLER_URL=http://127.0.0.1:4337 PAYMASTER_URL=http://127.0.0.1:4338
scripts/signer-network.sh init && scripts/signer-network.sh start

W=./target/debug/zecured-wallet           # fund its address with testnet TAZ first
$W loan open --amount 0.01                # prints the vault id and ZIP-321 URI
$W loan deposit <vault-id>
$W loan status <vault-id>                 # wait for "active"
$W loan borrow <vault-id> --usdc 0.1      # TEST ONLY: owner key as the account
$W loan partial-release <vault-id> --amount 0.004
$W loan repay-release <vault-id>
$W balance                                # payouts arrive as new notes
```

Node failures: `POST /admin/nodes/:index/mode` with `{ "mode": "offline" }` (header
`x-admin-token`, see `docs/INTERFACES.md` § 4.2). Expiry: build the backend with
`cargo build -p zecured-backend --features test-hooks`, then
`touch signer/data/coordinator/test-withhold-next-broadcast` before a payout.
