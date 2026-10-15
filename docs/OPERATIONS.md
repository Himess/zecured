# Operations through the judging period

The code is frozen at the submission tag on 28 October. From then until 23 November the
deployment must run untouched, with only operations allowed:
- restarts;
- topping up balances;
- changing endpoints, keys and thresholds in configuration.

This page lists what that takes (reviews G1–G7). The server setup is in
[infra/README.md](../infra/README.md); the signer node itself is in
[OPERATORS.md](OPERATORS.md).

## 1. What can change without a code change (G1)

| What | Where |
|---|---|
| Base RPC endpoints (primary and witness, per node; the backend's; the proxy's; the bots') | `node-N.env` (`NODE_BASE_RPC_URL`, `NODE_BASE_RPC_WITNESS_URL`), `backend.env`, `relayer-N.env`, bot env files |
| Light wallet servers | `network.json` (`lightwalletd_url`), per node `NODE_LIGHTWALLETD_URL` and `NODE_LIGHTWALLETD_WITNESS_URLS` |
| `eth_getLogs` range | `ZECURED_LOG_RANGE` (default 2,000; a refused range is halved automatically) |
| Base depth before a payout's shares are released | `network.json` `commit_depth_blocks` (default 3) |
| Bot intervals, relayer batch and standby | `--every-secs`, `--batch`, `RELAYER_STANDBY_LAG` |
| Deposit proofs run at once (H-12) | `DEPOSIT_PROOF_CONCURRENCY` in `backend.env` (1 by default, 2 at most); the others wait in a first-in-first-out queue, one job per vault, and `GET /status` shows `depositProofs: { queued, running, concurrency }` |
| Alarm thresholds and channel | `watchdog.env` |

## 2. Restarts, health and alarms (G2)

Every service runs under systemd with `Restart=always` and is never rate-limited out of
restarting. The watchdog runs every 5 minutes and alarms once when a check fails, again every
`REPEAT_HOURS` while it keeps failing, and once when it recovers. Alarms name a check and a
number, never a user address, viewing key, salt or IP.

| Alarm | Meaning | What to do |
|---|---|---|
| `backend /status fails` | the backend is down or unreachable | `systemctl status zecured-backend`; `journalctl -u zecured-backend -n 200`; restart |
| `signer node N: unreachable` | node N is down | restart `zecured-node@N`. Four nodes still pay out, since payouts need three, but no new vault can open, because opening one needs all five |
| `signer node N: compromised` or `offline` | the demo panel left node N in a demo mode | switch it back to `normal` from the panel |
| `header relay K blocks behind` | the relayers are not keeping up | check `zecured-relayer@1` and the relayer's ETH; relayer 2 takes over at its standby lag |
| `X has … ETH (floor …)` | an operational account runs low on gas | top it up from the treasury |
| `paymaster deposit …` | sponsored UserOps are about to stop | `depositTo` the paymaster at the EntryPoint |
| `X has … USDC` | the market maker cannot repay debts in closes | send it USDC (Circle faucet) |
| `a signer bond is missing` | a signer withdrew its bond: new vaults cannot activate | the signer bonds again (`scripts/bond-signers.sh`) |

## 3. Balances until 23 November (G3)

These are measured costs at Base Sepolia's usual 0.006 gwei. A higher gas price scales every line.

| Account | Cost | Needed from deployment (about 20 Oct) to 23 Nov |
|---|---|---|
| Relayer 1 | ~590,000 gas per Zcash header, 3,456 headers a day at NU7's 25 s blocks: ~0.012 ETH a day | ~0.45 ETH, plus ~0.03 ETH to catch up from the relay's tip (4,491,413) |
| Relayer 2 (standby) | only while relayer 1 is down | 0.05 ETH reserve |
| Signer bonds | 5 × 0.01 ETH per deployment, locked until withdrawal | 0.05 ETH (judge) + 0.05 ETH per theft-demo run (slashed) |
| Coordinator | proofs, activations, commitments, confirmations: a few cents of a cent per vault | 0.02 ETH |
| Paymaster deposit | sponsored UserOps, including each account's deployment | 0.05 ETH |
| Bundler executor, market maker, liquidator | their own transactions | 0.02 ETH each |
| Market maker | repays debts in closes and deleverages | 100–200 USDC |
| Demo vaults | 0.01 to 0.125 TAZ each | ~0.5 TAZ |

## 4. Time-bound risks during judging (G4)

| Risk | When | Effect on us | Mitigation |
|---|---|---|---|
| Base's **Denim** upgrade: 200 ms blocks | Base Sepolia, targeted for November 2026 (no date yet) | blocks come 10× faster:<br>• `commit_depth_blocks` covers less time;<br>• log catch-up needs 10× more requests | raise `commit_depth_blocks` (e.g. 30); log ranges adapt; other waits are in seconds |
| Zcash mainnet NU7 | decision 20 Oct, target 5 Nov; mainnet only | testnet is already on NU7; light wallet servers may upgrade | watch `testnet.zec.rocks`; keep a second light wallet server configured |
| A single NU7 light wallet server (`testnet.zec.rocks`) | any time | deposits and payouts stall while it is down | our own Zebra and light wallet server on the VM as the fallback endpoint |
| Zebra's end-of-support halt | mainnet only; testnet nodes do not halt | none for testnet | — |
| Public Base RPC limits (200-block logs, rate limits) | any time | slower event sync | adaptive log ranges; a keyed RPC for the backend; a witness RPC per node |
| Faucets (TAZ, USDC) for judges | any time | a judge cannot fund a vault | pre-funded demo wallets; DEMO.md fallback |
| Gas price spikes on Base Sepolia | any time | the relay costs more | watchdog balance alarms; treasury reserve |
| Server billing, disk and logs | monthly | the server stops | prepay through 23 Nov; journald size limit |

## 5. The running code is the submitted code (G5)

The submission commit carries a tag. The server builds and runs exactly that tag, and the guests
come from the reproducible build (README, "Reproducing the guest image ids"). `GET /status`
shows:
- the commit the backend was built from, and whether the tree was clean;
- the deployed image ids;
- the Zcash tip;
- each node's mode.

## 6. Audits the operator runs (Tier 3)

- **Each signer, for every vault:**
  1. `zecured-node export-audit --data <node dir> --vault <id> --out <dir>` writes the vault's
     viewing key and every claim's salt and receiver, in files only the operator can read.
  2. `zecured-wallet watch --ufvk-file <..>.ufvk --from <birthday> --tier3 --destinations
     <..>-destinations.json` reports an uncommitted spend (F1), a withheld change (F2) and a claim
     committed under a wrong reference (F4), with the command that builds the proof.

  This is the honest minority's job: with three colluding signers, the other two can still prove
  any of these.
- **The bots:** `zecured-wallet --wallet <bot wallet> watch-claims --destinations <bot's
  destinations file> --from <height> --base-from <block>` checks every payment of the liquidator's
  and the market maker's claims (F3). The destination files hold salts: back them up, never share
  them.

## 7. Soak test (G6)

Before the submission, the full deployment runs untouched for at least 48 hours, with the
watchdog on and nothing restarted by hand. We record:
- restarts (`systemctl show -p NRestarts`);
- the memory of each service;
- the balance burn rate against § 3;
- the relay lag;
- every alarm.
