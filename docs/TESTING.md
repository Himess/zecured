# Testing and evidence

What is tested automatically, what was run live, which failure scenarios were exercised, and
how to reproduce all of it. Everything here ran on testnets only.

## 1. Automated tests

| Layer | Command | Tests | Covers |
|---|---|---|---|
| Contracts | `cd contracts && forge test --isolate` | 222 | Unit and fork tests against the real Morpho Blue code; handler-based invariants I1, I2, I3 (Base side), I4–I9 and D14 with attacker actions; fuzzing (D14 LTV rule against an independent computation); EIP-712 golden vectors; the Tier 2 Zcash header relay against real testnet headers fetched over the Zcash P2P network (Equihash, NU7 difficulty rules, reorgs) |
| Rust workspace | `cargo test --workspace` | 61 | ZIP 2005 key derivation against published vectors; FROST DKG and re-randomized signing; an offline FROST-signed Ironwood spend; P1 classification; the payout ledger (fees, dust, set-aside claims, I10); PCZT verification (an honest payout passes; another recipient, a split payment, a third-party output readable with the vault's OVK, an extra or a missing vault input, an expiry outside the window, a fee mismatch and the wrong vault are refused); EIP-712 parity with Solidity; D16 request authentication; node-to-node DKG envelopes; scanner reorg detection; paymaster allowlist |
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
| P5 refund | 0.02 TAZ sent to a 0.01 vault is classified non-conforming; owner-signed refund; PCZT verified and FROST-signed by nodes 1–3 | 0.0199 TAZ back at block 4,486,629 |
| P1 + P2 | 0.01 TAZ deposit → `attestDeposit` with 3 signatures → zZEC minted straight into the account's Morpho position → borrow 1 USDC → repay → destination pre-validated → `requestRelease` → payout → `confirmPayout` | 0.0099 TAZ back; vault Closed; zZEC total supply 0 |
| P6 close from collateral | Market maker repays on Morpho through the SettlementAdapter; no outside USDC reaches the account | Market maker 336,667 zat and borrower 653,333 zat in **one** Zcash transaction (block 4,486,717) |
| P3 liquidation | Bot liquidates after a price drop; liquidator output created with `ovk = None` | Liquidator 835,070 zat; the 154,930 zat remainder released to the borrower |
| Threshold demo | Nodes 4 and 5 switched to "compromised" sign a sweep; nodes 1–3 refuse under policy | Threshold not reached; nothing moved |
| D16 | Unsigned request to a node API | Refused; requests signed by the coordinator open vaults normally |
| D14 partial release | 0.01 TAZ vault, borrow 0.1 USDC (25 % LTV), withdraw 0.004 ZEC while the loan stays open (LTV after: 42 %), then repay and release the rest | 390,000 zat received (block 4,486,890, tx `233cf3f68fea8526a1e6fd06983010a44bc823e1ac4526df6bb0e1a1778ad95a`); vault stayed Active with 600,000 zat; then 590,000 zat (block 4,486,900, tx `2dfa25afa503464ab047b2527558bf0419a6c6aa708b5f576549cf61aa2826be`); vault Closed |
| **App, end to end** | Vault opened in the browser; 0.01 TAZ paid to it; the first sponsored UserOp (through the backend proxy to a local alto bundler and mock paymaster) deploys the Safe, authorizes the Registry and the Adapter on Morpho and borrows 0.1 USDC; a 0.004 TAZ partial withdrawal by UserOp; the vault checked with its UFVK (`zecured-wallet watch`); close from collateral signed in the browser (ERC-1271) and executed by the market maker | 390,000 zat (block 4,488,446, tx `4c928da23c7136296c0ac1b7e769ab2fa9b9b2ddf2871eb6ba9b4852af99ca1c`); borrower 337,500 zat and market maker 252,500 zat in one transaction (block 4,488,453, tx `a3d0f0e10df413004daca268f504b11af823493a60df002d009487d0eed6bb06`); vault Closed. The account never held ETH or zZEC |

In every run the account's zZEC balance stayed 0 (I2): zZEC only ever sat in Morpho, the
Registry and the SettlementAdapter.

## 3. Robustness scenarios

| Scenario | How | Result |
|---|---|---|
| Two signers offline during a payout | Nodes 4 and 5 set to `offline` (their API refuses everything) before the D14 partial release | Destination pre-validated, payout signed by nodes 1, 2, 3, confirmed on Base. Nodes 4 and 5 kept following Base and, back online, held the same claim states as the others |
| Signer restart mid-payout | Node 2 killed and restarted between the payout broadcast and its confirmation | Its persisted in-flight record let it check the mined transaction and sign `PayoutConfirmation` |
| Payout expires unmined | TEST ONLY flag `test-withhold-next-broadcast` in the coordinator's data directory: the next payout is signed and recorded but never broadcast | Expired at height 4,486,950; the coordinator rebuilt it, nodes 1–3 verified and signed the new PCZT, mined at block 4,486,956 (tx `9d323f2cbd3ac0fa0397ab4445bd76f54a273cca84de97b50aa92f6fd0f2be82`), confirmed on Base, vault Closed |
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
`x-admin-token`, see `docs/INTERFACES.md` § 4.2). Expiry: `touch signer/data/coordinator/test-withhold-next-broadcast`
before a payout.
