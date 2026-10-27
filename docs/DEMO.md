# Demo runbook

No live demo runs through judging (D82); this is how the run of 27 October and its video were made.

## Before the demo

1. `scripts/signer-network.sh start`: 5 signer nodes + backend; check `scripts/signer-network.sh status`.
2. Bots: `zecured-bots market-maker` and `zecured-bots liquidator` (MockUSDC minted to both).
3. A borrower wallet pre-funded in Ironwood (`zecured-wallet balance`); a second vault already
   active for the liquidation scene (liquidation needs a few minutes of confirmations).
4. The MOCK oracle's price: the deploy script starts it at $1,265 (`ZEC_USD_PRICE_CENTS`, near ZEC's price in
   October 2026); the demo control panel moves it.
5. The treasury's receiver and salt behind the deployment's `FEE_COMMITMENT` registered with the
   signers like a market maker's (`POST /recipients`): without it the vaults' opening-fee claims are
   never paid and their vaults never settle.
6. Backup video recorded (see below) in case the testnet or the light wallet server is slow.

## Live flow (about 14 minutes to an active loan on Base Sepolia, then the close and the swap)

| Scene | What to show | Proof on screen |
|---|---|---|
| Prepare | Why only Ironwood → Ironwood deposits are accepted (turnstile) | Preparation page |
| Open | Choose 0.01 TAZ → a fresh owner key and smart account → a fresh 3-of-5 vault | ZIP-321 QR, vault address |
| Deposit | Pay the QR from the Ironwood wallet | Vault status: deposit seen → proven (pending) → active |
| Borrow | Borrow USDC from a gasless account (first UserOp deploys it) | Position page; the account never held ETH or zZEC |
| What the world sees | Three columns: everyone / signer network / you | Live data |
| Close from collateral | Quote → sign → the market maker repays on Morpho | One Zcash transaction pays the market maker and the borrower |
| Liquidation | Drop the oracle price → the bot liquidates → the rest stays yours | Liquidation claim paid, remaining collateral released |
| Threshold | Switch two signers to "compromised" → sweep attempt → 2 sign, 3 refuse | Votes on the control panel; nothing moves |
| Verify | Owner-signed viewing-key request → scan the vault yourself | UFVK and the vault's notes |
| Tier 2 (optional) | How that deposit was activated: a zero-knowledge proof, no signer's word: the relay's verified header, the proof's journal, `ZecuredIntake.attestDepositWithProof`, then `finalizeActivation` | The Base transactions; the journal holds no Zcash data but a later header |

## Theft demo (Tier 3, its own deployment)

Three signers really steal from a vault, and the theft is proven and slashed on Base. This runs on
a separate deployment with its own five signers, where three nodes can be compromised (review
T3-6, docs/TIER3.md § 8). Its binaries refuse to start against the lending deployment (its signers are compiled in).

Setup, once:
1. `THEFT_DEMO=1 scripts/signer-network.sh init`: five fresh node identities in
   `signer/data-theft-demo/` (and the `demo-tier3` build in `target/theft-demo/`).
2. `scripts/deploy-theft-demo.sh` (a simulation), then `scripts/deploy-theft-demo.sh --broadcast`:
   the v4.1 contracts with a 2-minute challenge window and no opening fee
   (`contracts/deployments/base-sepolia-theft-demo.json`).
3. `THEFT_DEMO=1 scripts/signer-network.sh init` again (the network config now names the
   deployment), `THEFT_DEMO=1 scripts/bond-signers.sh`, `THEFT_DEMO=1 scripts/signer-network.sh start`.
4. Open a vault on the theft demo's backend (port 8180) and deposit until it is active; keep its
   viewing key (`loan viewing-key`). Borrow once from it (the app's first transaction, or `loan borrow`): that
   authorizes the Registry on Morpho, without which the compensator cannot retire the vault's zZEC and pays the
   victim nothing.

| Scene | Command | On screen |
|---|---|---|
| Compromise three | `POST /admin/nodes/{3,4,5}/mode` with `{"mode":"compromised"}` (header `x-admin-token`) | Three nodes compromised; outside this build the panel stops at two |
| Steal | `POST /admin/vaults/<id>/steal` with `{"to":"<an attacker's unified address>"}` | The sweep's txid on a Zcash explorer: no commitment on Base |
| Detect | `zecured-wallet watch --ufvk-file vault.ufvk --from <birthday> --tier3` | `VIOLATION F1` and the command that builds the witness |
| Prove | `build-violation uncommitted ... --out f1.json`, then `zecured-tier2-host violation prove f1.json f1.receipt` and `... violation groth16 f1.receipt f1.seal.json` | The journal: an anchor header, a tag, a reference; no Zcash transaction, note or address |
| Slash | `cast send <ZecuredSlasher> "proveViolation(uint8,bytes,bytes,bytes32)" 1 <journal> <seal> <violation image id>`; two minutes later `cast send <ZecuredSlasher> "executeSlash(bytes32)" <caseId>` | All five bonds slashed, the bounty owed to the prover, `halted()` true: no new vault can be activated |
| Compensate | `cast send <ZecuredCompensator> "tallyVault(bytes32)" <vault id>`; once the incident window is over (10 minutes here), `finalizeIncident(uint256)` with `currentIncident()`, then `settleVault(bytes32)` and `withdrawEquity(bytes32)` | The slashed bonds repay the vault's debt, its unbacked zZEC is retired, and the rest of its value goes to its account in USDC: the victim is made whole up to the bonds |

## Pay it with demo TAZ (P-1, DEMO)

On a testnet deployment the deposit step offers "Pay it with demo TAZ" to a visitor without testnet ZEC:
the operators' demo wallet pays the vault its exact amount from its own Ironwood notes. It is a real
Zcash testnet payment, proven on Base like any deposit. Each vault gets one demo payment, within a
daily budget, after a second of proof of work in the browser. When the loan is closed its ZEC goes
back to the demo wallet, unless the visitor gives an Ironwood address of their own (the destination
form offers "Send my ZEC back to the demo wallet"; the swap offers "DEMO: use the demo wallet's Ironwood address").

Operators: run `zecured-wallet --wallet data/demo/wallet.json init`, fund the address it prints with
TAZ, then set `DEMO_WALLET_SPENDING_KEY` (the file's `spending_key`: secret, never logged) and
`DEMO_WALLET_BIRTHDAY` (the file's `birthday`), optionally `DEMO_DAILY_CAP_ZAT`,
`DEMO_LOW_BALANCE_ZAT` and `DEMO_ALARM_WEBHOOK`. Restart the backend, build the app with
`NEXT_PUBLIC_DEMO_PATH=1`, and check that `GET /demo/status` says `enabled`. The demo path never
runs on Base mainnet, and the mainnet-beta app never shows it.

**Public pages.** `/status`: the signers, the relay, the proof queue, the minting gates, the cap's
fill, the bonds (ΣB, B_eff), the anonymity sets, the proof versions and the running commit.
`/evidence`: the deployment's manifest and real transactions, from `ZECURED_EVIDENCE`. `/market`: the
market's size and rates, the collateral, the bonds, the compensation order, and optional lending.
`/buy`: the swap's Buy side.

## Recording the video

The submitted video runs 3 min 55 s (the live run of 27 October) and follows the build being submitted. Rules for the recording:

- Testnet only, and only features that work in the build being submitted.
- No real personal addresses on screen, and never a viewing key, a salt, an owner-key backup, the admin token,
  `.env`, a server's address or a terminal's history.
- The DEMO and MOCK labels stay visible in every shot.
- Every cut in time is marked with the real time that passed (for example "+4 min, real time").
- 1080p, captions burned in and also as a `.vtt` file.
