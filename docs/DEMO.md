# Demo runbook

## Before the demo

1. `scripts/signer-network.sh start` — 5 signer nodes + backend; check `scripts/signer-network.sh status`.
2. Bots: `zecured-bots market-maker` and `zecured-bots liquidator` (MockUSDC minted to both).
3. A borrower wallet pre-funded in Ironwood (`zecured-wallet balance`); a second vault already
   active for the liquidation scene (liquidation needs a few minutes of confirmations).
4. Oracle price at $40 (the deployment default; the demo control panel sets it).
5. Backup video recorded (see below) in case the testnet or the light wallet server is slow.

## Live flow (≈ 6 minutes)

| Scene | What to show | Proof on screen |
|---|---|---|
| Prepare | Why only Ironwood → Ironwood deposits are accepted (turnstile) | Preparation page |
| Open | Choose 0.01 TAZ → a fresh owner key and smart account → a fresh 3-of-5 vault | ZIP-321 QR, vault address |
| Deposit | Pay the QR from the Ironwood wallet | Vault status: deposit seen → active |
| Borrow | Borrow USDC from a gasless account (first UserOp deploys it) | Position page; the account never held ETH or zZEC |
| What the world sees | Three columns: everyone / signer network / you | Live data |
| Close from collateral | Quote → sign → the market maker repays on Morpho | One Zcash transaction pays the market maker and the borrower |
| Liquidation | Drop the oracle price → the bot liquidates → the rest stays yours | Liquidation claim paid, remaining collateral released |
| Threshold | Switch two signers to "compromised" → sweep attempt → 2 sign, 3 refuse | Votes on the control panel; nothing moves |
| Verify | Owner-signed viewing-key request → scan the vault yourself | UFVK and the vault's notes |
| Tier 2 (optional) | The same deposit activated on a zero-knowledge proof instead of signer signatures: the relay's verified header, the proof's journal, `attestDepositWithProof` | The Base transaction; the journal holds no Zcash data but a later header |

## Theft demo (Tier 3, its own deployment)

Three signers really steal from a vault, and the theft is proven and slashed on Base. This runs on
a separate deployment with its own five signers, where three nodes can be compromised (review
T3-6, docs/TIER3.md § 8). Its binaries refuse to start against the judge deployment.

Setup, once:
1. `THEFT_DEMO=1 scripts/signer-network.sh init`: five fresh node identities in
   `signer/data-theft-demo/` (and the `demo-tier3` build in `target/theft-demo/`).
2. `scripts/deploy-theft-demo.sh` (a simulation), then `scripts/deploy-theft-demo.sh --broadcast`:
   Registry, zZEC, Adapter, `SignerBond` and `ZecuredSlasher` with a 2-minute window
   (`contracts/deployments/base-sepolia-theft-demo.json`).
3. `THEFT_DEMO=1 scripts/signer-network.sh init` again (the network config now names the
   deployment), `THEFT_DEMO=1 scripts/bond-signers.sh`, `THEFT_DEMO=1 scripts/signer-network.sh start`.
4. Open a vault on the theft demo's backend (port 8180) and deposit until it is active; keep its
   viewing key (`loan viewing-key`).

| Scene | Command | On screen |
|---|---|---|
| Compromise three | `POST /admin/nodes/{3,4,5}/mode` with `{"mode":"compromised"}` (header `x-admin-token`) | Three nodes compromised; outside this build the panel stops at two |
| Steal | `POST /admin/vaults/<id>/steal` with `{"to":"<an attacker's unified address>"}` | The sweep's txid on a Zcash explorer: no commitment on Base |
| Detect | `zecured-wallet watch --ufvk-file vault.ufvk --from <birthday> --tier3` | `VIOLATION F1` and the command that builds the witness |
| Prove | `build-violation uncommitted ... --out f1.json`, then `zecured-tier2-host violation prove f1.json f1.receipt` and `... violation groth16 f1.receipt f1.seal.json` | The journal: an anchor header, a tag, a reference; no Zcash transaction, note or address |
| Slash | `cast send <ZecuredSlasher> "proveViolation(uint8,bytes,bytes)" 1 <journal> <seal>`; two minutes later `cast send <ZecuredSlasher> "executeSlash(bytes32)" <caseId>` | All five bonds slashed, the bounty owed to the prover, `halted()` true: no new vault can be activated |

## Two-minute video

| Time | Scene | Narration |
|---|---|---|
| 0:00–0:10 | Title | "Every way to use ZEC in DeFi today starts with unshielding it. Zecured doesn't." |
| 0:10–0:28 | Deposit by QR from Ironwood | "Pay a fresh 3-of-5 threshold vault from your Ironwood notes. No pool crossing, nothing revealed." |
| 0:28–0:45 | Mint and borrow | "A non-transferable collateral receipt is minted straight into a Morpho position for a brand-new, gasless account. Borrow USDC." |
| 0:45–1:02 | What the world sees | "Here's what the world sees: an anonymous account and a loan. No wallet, no balance, no history." |
| 1:02–1:22 | Close from collateral | "No need to bring USDC back. A market maker closes the debt, takes ZEC for it, and the rest returns to a new shielded address — in one transaction." |
| 1:22–1:35 | Liquidation | "If the price drops, a liquidator repays and receives ZEC — shielded." |
| 1:35–1:52 | Threshold demo + UFVK | "Two signers compromised — nothing moves. And you can verify your vault yourself with its viewing key." |
| 1:52–2:00 | Close | "Sell nothing. Reveal nothing. Zecured." |

Rules for the recording: testnet only; no real personal addresses on screen; only features
that work in the build being submitted.
