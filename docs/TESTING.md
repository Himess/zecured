# Testing and evidence

What is tested automatically, what was run live, which failure scenarios were exercised, and
how to reproduce all of it. Everything here ran on testnets only.

## 1. Automated tests

| Layer | Command | Tests | Covers |
|---|---|---|---|
| Contracts | `cd contracts && forge test --isolate` | 321 | Unit and fork tests against the real Morpho Blue code; handler-based invariants I1, I2, I3 (Base side), I4–I9, I11–I14 (I13 on its Base side), Tier 3's I15–I17 and D14 with attacker actions, cancellations, claim reuse and uncommitted confirmations included; Tier 3: every `commitSpend` rule (one distinct output per claim: E1), the signer bonds, each violation kind with dismissals, the bounty and the halt, an activation held while a case is open on its vault (E2), a pending deposit neither refundable nor cancellable (E3), and the journal of a real Testnet spend computed by the Rust statement; fuzzing (D14 LTV rule against an independent computation); EIP-712 golden vectors; the Tier 2 Zcash header relay against real testnet headers fetched over the Zcash P2P network (Equihash, NU7 difficulty rules, reorgs); Tier 2 deposit proofs with a real deposit's journal against a relay-verified real header, pending until the relay moves on and then finalized; `ProofOnly` minting and every pending-activation branch on a MOCK relay; the pinned seal selector; the zZEC expectation's settlement; the market maker's ZEC floor; vault cancellation |
| Deposit and violation statements (Tiers 2, 3) | `cargo test -p zecured-deposit-statement`, `tier2/target/release/zecured-tier2-host parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_witness.json` and `... violation parity tier2/statement/tests/fixtures/TEST-ONLY_vault9_f1_witness.json` | 29, and 33 + 10 zkVM runs | The real deposit's witness proves the journal the contracts test; 33 single changes of it (K4: the transaction's shape and branch, the txid path, B's leaf, the history path and peaks, T's commitments, the expiry, the note opening, the recipient, the vault address) each refused with its exact error natively and, through `parity`, by the guest in the RISC Zero executor; transaction shapes a witness cannot carry (an Orchard-pool bundle, v5, another branch id); the changes accepted on purpose (authorizing data, header bytes outside the commitments) and every vault field binding the journal Tier 3: F1 on a real Testnet spend of the TEST-ONLY vault, F2 on its change, F3 with the vault as claimant (only the claim's own output to its exact receiver counts, and a note that does not match its `cmx` pays nothing: E1), every opening field and key bound, the journal ABI |
| Rust workspace | `cargo test --workspace` (plus `-p zecured-node --features demo`, `-p zecured-backend --features test-hooks`) | 249 | ZIP 2005 key derivation against published vectors; FROST DKG and re-randomized signing; an offline FROST-signed Ironwood spend; P1 classification; the payout ledger (fees, dust, set-aside claims, I10); PCZT verification (an honest payout passes; another recipient, a split payment, a third-party output readable with the vault's OVK, an extra or a missing vault input, an expiry outside the window, a fee mismatch and the wrong vault are refused); EIP-712 parity with Solidity; D16 request authentication and replay protection; node-to-node DKG envelopes bound to the transcript; scanner reorg detection; the paymaster allowlist (account binding, CREATE2 sender, EntryPoint, caps); a regression test for every finding of the [security review](SECURITY_REVIEW.md) (payout misattribution, foreign or zero-value vault notes, undecryptable third-party outputs, forged Registry events, witness disagreement, idempotent confirmations, demo-mode gating, lock and overflow safety); Tier 3: the commitment each node derives, the claims a payout must keep (T3-5), the coordinator committing once with the agreed record and reusing it on retry, the watchtower's verdicts |
| App | `cd app && pnpm test` | vitest suite | Unified address encoding (F4Jumble), commitments, EIP-712 owner messages, Safe address prediction, call allowlist, Morpho math, API parsing |

The Rust EIP-712 digests are checked against the same golden vectors as the Solidity tests,
so signers and contracts cannot silently disagree on a message format.

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
| A real proof | `zecured-tier2-host prove` on this 8 GB Apple M2 (CPU only: r0vm has no Metal backend) | A 22.1 M-cycle probe of the same Zcash code produced a verified succinct receipt in 108 min, with the machine swapping heavily. The run on the real deposit's witness (20.2 M cycles) was interrupted when the desktop app froze under that memory pressure; real proofs will be made on an x86 machine, which the Groth16 step needs anyway |
| On-chain | `DepositProofs.t.sol`: the same journal against header 4,488,560 verified by the relay on-chain, MOCK verifier | Vault activated, 266,300 gas without the verifier call; 25 refusal and cancellation tests |
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
