# Testing and evidence

What is tested automatically, what was run live, which failure scenarios were exercised, and
how to reproduce all of it. Everything here ran on testnets only.

## 1. Automated tests

| Layer | Command | Tests | Covers |
|---|---|---|---|
| Contracts | `cd contracts && forge test --isolate` | 340 | Unit and fork tests against the real Morpho Blue code; real Groth16 seals of a real deposit and of a real F1 spend, verified by RISC Zero's verifier router on the fork (the deposit minted through the real relay in `ProofOnly` mode, the F1 case slashing all five bonds); handler-based invariants I1, I2, I3 (Base side), I4–I9, I11–I14 (I13 on its Base side), Tier 3's I15–I17 and D14 with attacker actions, cancellations, claim reuse and uncommitted confirmations included; Tier 3: every `commitSpend` rule (one distinct output per claim: E1), the signer bonds, each violation kind with dismissals, the bounty and the halt, an activation held while a case is open on its vault (E2), a pending deposit neither refundable nor cancellable (E3), the red-team review's proofs of concept reversed (F4 on a wrong claim reference, one claim set per note, a pending deposit dropped after a halt, dust written off, one lineage note per tag), and the journal of a real Testnet spend computed by the Rust statement; fuzzing (D14 LTV rule against an independent computation); EIP-712 golden vectors; the Tier 2 Zcash header relay against real testnet headers fetched over the Zcash P2P network (Equihash, NU7 difficulty rules, reorgs); Tier 2 deposit proofs with a real deposit's journal against a relay-verified real header, pending until the relay moves on and then finalized; `ProofOnly` minting and every pending-activation branch on a MOCK relay; the pinned seal selector; the zZEC expectation's settlement; the market maker's ZEC floor; vault cancellation |
| Contracts, symbolic | `cd contracts && FOUNDRY_PROFILE=halmos halmos` | 10 checks | The zZEC guard from any state, and the Registry's I1, I3, I6, I7 and I9 rules, for every value of their inputs (§ 1b) |
| Deposit and violation statements (Tiers 2, 3) | `cargo test -p zecured-deposit-statement`, `tier2/target/release/zecured-tier2-host parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_witness.json` and `... violation parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_f1_witness.json` | 32, and 33 + 11 zkVM runs (locally and in CI on the Docker-built guests) | The real deposit's witness proves the journal the contracts test; 33 single changes of it (K4: the transaction's shape and branch, the txid path, B's leaf, the history path and peaks, T's commitments, the expiry, the note opening, the recipient, the vault address) each refused with its exact error natively and, through `parity`, by the guest in the RISC Zero executor; transaction shapes a witness cannot carry (an Orchard-pool bundle, v5, another branch id); the changes accepted on purpose (authorizing data, header bytes outside the commitments) and every vault field binding the journal Tier 3: F1 on a real Testnet spend of the TEST-ONLY vault, F2 on its change, F3 with the vault as claimant (only the claim's own output to its exact receiver counts, and a note that does not match its `cmx` pays nothing: E1), every opening field and key bound, the journal ABI |
| Rust workspace | `cargo test --workspace` (plus `-p zecured-node --features demo`, `-p zecured-backend --features test-hooks`) | 266 | ZIP 2005 key derivation against published vectors; FROST DKG and re-randomized signing; an offline FROST-signed Ironwood spend; P1 classification; the payout ledger (fees, dust, set-aside claims, I10); PCZT verification (an honest payout passes; another recipient, a split payment, a third-party output readable with the vault's OVK, an extra or a missing vault input, an expiry outside the window, a fee mismatch and the wrong vault are refused); EIP-712 parity with Solidity; D16 request authentication and replay protection; node-to-node DKG envelopes bound to the transcript; scanner reorg detection; the paymaster allowlist (account binding, CREATE2 sender, EntryPoint, caps); a regression test for every finding of the [security review](SECURITY_REVIEW.md) (payout misattribution, foreign or zero-value vault notes, undecryptable third-party outputs, forged Registry events, witness disagreement, idempotent confirmations, demo-mode gating, lock and overflow safety); Tier 3: the commitment each node derives, the claims a payout must keep (T3-5), the coordinator committing once with the agreed record and reusing it on retry, the watchtower's verdicts |
| App | `cd app && pnpm test` | vitest suite | Unified address encoding (F4Jumble), commitments, EIP-712 owner messages, Safe address prediction, call allowlist, Morpho math, API parsing |

The Rust EIP-712 digests are checked against the same golden vectors as the Solidity tests,
so signers and contracts cannot silently disagree on a message format.

## 1b. Symbolic checks (Halmos)

`cd contracts && FOUNDRY_PROFILE=halmos halmos` runs the `check_` functions in
`contracts/test/halmos/` with [Halmos](https://github.com/a16z/halmos) 0.3.3 (settings in
`contracts/halmos.toml`). All ten run in under a minute on the 8 GB M2. Their inputs are symbolic:
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
| R1 (I6) | The signature check passes only with at least 3 signatures (5 for `attestDepositWithProof`) that recover to distinct signers. Its bitmap, which Tier 3 slashes by, names exactly those signers. `cancelVault` behaves the same at its entry point | 1,198 paths, 7 s; at `cancelVault` 309 paths, 3 s |
| R2 (I7) | `recordClose` succeeds only from the Adapter, on an active vault, when the two amounts add up to the outstanding. It burns exactly the outstanding from the Adapter, records exactly the two claims and closes the vault | 27 paths, 1 s |
| R3 (I9) | `recordLiquidation` succeeds only from the Adapter, on an active vault, seizing more than 0 and at most the outstanding. It burns exactly that amount and records exactly one claim for it | 27 paths, 1 s |
| R4 (I1, I3) | Whichever exit runs (release, partial release, liquidation or close), whoever calls it, and with any arguments: releases come only from the vault's account and settlements only from the Adapter. The burn equals the total of the new claims and the drop in the outstanding, and the supply stays equal to the outstanding | 48 paths, 1 s |

What these checks do not show:
- **The Registry checks are bounded.** They start from one vault activated through the real
  `attestDeposit`, with symbolic amounts and part of its collateral already with the Adapter, and
  run one call. They are not proofs over every sequence of calls; the handler-based invariant suites
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

None of the checks passes vacuously. Each one was run against a deliberately broken copy of the
contract and returned a counterexample:

| Broken rule in `src/` | Caught by |
|---|---|
| zZEC: anyone, not only Morpho, may move the Registry's zZEC into Morpho | G4 |
| zZEC: a transfer out of Morpho may exceed the armed amount | G5 |
| zZEC: the transfer out of Morpho does not consume the expectation | G5 |
| zZEC: mint to any address | G1 |
| zZEC: burn from any address | G6 |
| zZEC: `expectFromMorpho` arms the Registry as recipient, whoever calls | G3 |
| Registry: the same signer may sign twice (`<` for `<=` in the order check) | R1, and R1 at `cancelVault` |
| Registry: signer 4 sets signer 3's bit | R1 |
| Registry: a close whose two amounts add up to less than the outstanding | R2 |
| Registry: a liquidation burns the whole outstanding | R3 |
| Registry: a partial release records a claim one zatoshi larger than its burn | R4 |

The first version of the zZEC checks started from states built by calls (mint, supply, withdraw to
the Adapter). The `expectFromMorpho` mutation broke that setup: every path reverted before reaching
an assertion, and Halmos reported an error instead of a counterexample. The zZEC checks now start
from symbolic storage, which no change to the token can break.

## 2. Live end-to-end runs

Setup: Zcash **testnet** after NU7 through `testnet.zec.rocks`, five signer node processes plus
the coordinator talking over HTTP, and the Base side on a local fork of Base Sepolia (the real
Morpho Blue deployment, a fresh Zecured deployment) started with `scripts/local-fork.sh`.
The CLI's Base commands are TEST ONLY: the loan owner's key acts as the account and pays its
own fork gas. The app uses a fresh, gasless Safe instead.

| Flow | What happened | Evidence |
|---|---|---|
| Spike A | 3-of-5 FROST vault spends an Ironwood note | Zcash testnet block 4,486,491 |
| Tier 3: a payout committed before it is signed (2026-10-12) | 0.01 TAZ vault `0x59d5e63e…` attested on the fork; `requestRelease`; the three chosen nodes each derived and signed the payout's `SpendCommit`, the coordinator recorded it with `commitSpend`, every node checked the record three blocks deep before releasing its share, the payout was broadcast, mined, and `confirmPayout` accepted the committed claim; the vault settled (`openVaultCount` 0). `zecured-wallet watch --tier3` with the vault's UFVK: "lineage note 0xe43d283f spent by its committed payout: ok" | deposit `bd6daa8e120b391625c6b962684875f761e36e2fa0d7e0bc1eeb34e53bacaf9a` (block 4,490,621); payout `19eab8ea2907ca1f24e2a516885a4e8fb091ce78468febd92da9afd334eddca8` (block 4,490,655); `SpendCommitted` with keyed `payoutRef` `0xcdf694fd…`, input tag `0xe43d283f…`, signer bitmap `0x0d`, no txid, note or address on Base |
| P5 refund | 0.02 TAZ sent to a 0.01 vault is classified non-conforming; owner-signed refund; PCZT verified and FROST-signed by nodes 1–3 | 0.0199 TAZ back at block 4,486,629 |
| P1 + P2 | 0.01 TAZ deposit → `attestDeposit` with 3 signatures → zZEC minted straight into the account's Morpho position → borrow 1 USDC → repay → destination pre-validated → `requestRelease` → payout → `confirmPayout` | 0.0099 TAZ back; vault Closed; zZEC total supply 0 |
| P6 close from collateral | Market maker repays on Morpho through the SettlementAdapter; no outside USDC reaches the account | Market maker 336,667 zat and borrower 653,333 zat in **one** Zcash transaction (block 4,486,717) |
| P3 liquidation | Bot liquidates after a price drop; liquidator output created with `ovk = None` | Liquidator 835,070 zat; the 154,930 zat remainder released to the borrower |
| Threshold demo | Nodes 4 and 5 switched to "compromised" sign a sweep; nodes 1–3 refuse under policy | Threshold not reached; nothing moved |
| D16 | Unsigned request to a node API | Refused; requests signed by the coordinator open vaults normally |
| D14 partial release | 0.01 TAZ vault, borrow 0.1 USDC (25 % LTV), withdraw 0.004 ZEC while the loan stays open (LTV after: 42 %), then repay and release the rest | 390,000 zat received (block 4,486,890, tx `233cf3f68fea8526a1e6fd06983010a44bc823e1ac4526df6bb0e1a1778ad95a`); vault stayed Active with 600,000 zat; then 590,000 zat (block 4,486,900, tx `2dfa25afa503464ab047b2527558bf0419a6c6aa708b5f576549cf61aa2826be`); vault Closed |
| **App, end to end** | Vault opened in the browser; 0.01 TAZ paid to it; the first sponsored UserOp (through the backend proxy to a local alto bundler and mock paymaster) deploys the Safe, authorizes the Registry and the Adapter on Morpho and borrows 0.1 USDC; a 0.004 TAZ partial withdrawal by UserOp; the vault checked with its UFVK (`zecured-wallet watch`); close from collateral signed in the browser (ERC-1271) and executed by the market maker | 390,000 zat (block 4,488,446, tx `4c928da23c7136296c0ac1b7e769ab2fa9b9b2ddf2871eb6ba9b4852af99ca1c`); borrower 337,500 zat and market maker 252,500 zat in one transaction (block 4,488,453, tx `a3d0f0e10df413004daca268f504b11af823493a60df002d009487d0eed6bb06`); vault Closed. The account never held ETH or zZEC |
| **After the security fixes** | The same app flow on a fresh deployment with the fixed signers, coordinator and paymaster allowlist | 390,000 zat (block 4,488,561, tx `1558eea9fb60a5827a05c2d24436a38dacdc72997217e5fbaf048eca2a87d2ed`); borrower 337,500 zat and market maker 252,500 zat in one transaction (block 4,488,577, tx `6166f6f392fd6d9568774b6f637079f46be99ca4aca5a16c7881ed9afd40804a`); vault Closed |

In every run the account's zZEC balance stayed 0 (I2): zZEC only ever sat in Morpho, the
Registry and the SettlementAdapter.

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
| Real seals on-chain | `RealGroth16Seal.t.sol` on a Base Sepolia fork, and an `eth_call` to the live router | RISC Zero's router (`0x0b14…b711`, Groth16 v3.0.0 selector `0x73c457ba`) accepts both seals, and refuses them for a journal changed by one zatoshi or by one beneficiary byte. The deposit, through the real relay and the real router in `ProofOnly` mode, is activated and minted straight into Morpho: `attestDepositWithProof` costs 520,722 gas. The F1 case slashes all five bonds and pays the bounty to the journal's beneficiary: `proveViolation` costs 471,158 gas |
| On-chain | `DepositProofs.t.sol`: the same journal against header 4,488,560 verified by the relay on-chain, MOCK verifier | Vault activated: 289,256 gas for the proof with the MOCK verifier (520,722 with the real router, row above); 25 refusal and cancellation tests |
| Headers over P2P (Rust) | `update-headers` / `build-witness --update-headers` | 969 new headers from Testnet peers; the rebuilt witness gives the same journal |

## 3. Robustness scenarios

| Scenario | How | Result |
|---|---|---|
| Two signers offline during a payout | Nodes 4 and 5 set to `offline` (their API refuses everything) before the D14 partial release | Destination pre-validated, payout signed by nodes 1, 2, 3, confirmed on Base. Nodes 4 and 5 kept following Base and, back online, held the same claim states as the others |
| Signer restart mid-payout | Node 2 killed and restarted between the payout broadcast and its confirmation | Its persisted in-flight record let it check the mined transaction and sign `PayoutConfirmation` |
| Payout expires unmined | TEST ONLY flag `test-withhold-next-broadcast` in the coordinator's data directory (only in a backend built with `--features test-hooks`): the next payout is signed and recorded but never broadcast | Expired at height 4,486,950; the coordinator rebuilt it, nodes 1–3 verified and signed the new PCZT, mined at block 4,486,956 (tx `9d323f2cbd3ac0fa0397ab4445bd76f54a273cca84de97b50aa92f6fd0f2be82`), confirmed on Base, vault Closed |
| Lost broadcast response | The coordinator stores a signed payout before broadcasting it and rebroadcasts it on every pass until it is mined or expires | Nothing new is built for the same claims meanwhile; a second payout is impossible anyway (I10, nullifiers) |
| Zcash reorg | The scanner refuses a block whose `prev_hash` is not the hash it scanned last | Unit test; the node then rescans the vault from its birthday |
| Base reorg | Nodes and the coordinator ingest Registry events only up to Base's `safe` head | By construction |
| Out-of-policy PCZT | Wrong expiry window, recipients, amounts, fees or value balance | Refused by every honest node (first live run: all five refused a PCZT outside the expiry window; adversarial unit tests) |

## 4. Privacy checks

- Service logs (five nodes, backend, bots) were scanned for unified addresses, viewing keys,
  salts, Base addresses and IP addresses: none, apart from each service's own bind address.
  Vaults appear in logs only as an 8-hex-digit prefix of the vault id.
- Error messages returned or logged by the signers never embed receivers, viewing keys or salts.
- Base events and calldata carry only salted receiver commitments, never a Zcash address,
  txid or note commitment (enforced by the contract interfaces).
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
