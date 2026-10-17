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
| `eth_getLogs` range | `ZECURED_LOG_RANGE` (default 2,000; a refused range is halved automatically); the nodes, the coordinator and the liquidator also step through Base by it, so a keyed RPC that allows larger ranges speeds them all up (H-11) |
| Base depth before a payout's shares are released | `network.json`: the commitment must be under Base's `safe` head or, while that lags, `commit_depth_secs` old (default 120, at most 180, so that a signing session outlives the wait); `commit_depth_blocks` (default 3) stays a minimum. Seconds, not blocks, so nothing changes with Denim's 200 ms blocks (H-11) |
| Bot intervals, relayer batch and standby | `--every-secs`, `--batch`, `RELAYER_STANDBY_LAG` |
| The liquidator's pre-liquidation round | on by default, `--no-pre-liquidation` turns it off; its profitability estimate, only logged, uses `--eth-usd` (3,000), `--payout-wait-hours` (1) and `--zec-risk-bps-per-hour` (20) |
| The bots' own claims (R-5) | `--claims <file>` (`BOT_CLAIMS_FILE`; default `data/bots/<role>-claims.json`, owner-readable only): the claims whose liveness the liquidator and the lending market maker arm, kept so that a restart resumes them. They fetch a signer's receipt (`POST /claims/:claimId/receipt`) and send `armLiveness` from their own key: keep a little Base ETH on the bot keys. Nothing is armed on a deployment without bonds. The market maker sends every transaction of its key (closes, deleverages, armings) through one nonce lock: send nothing else from the executor key while it runs |
| Cost and capacity of vault openings (H-01) | `network.json` `open_vault`: `stamp_bits` (zero bits of the hashcash stamp every `POST /vaults` carries; 20 by default, at most 32; each bit doubles the work in the user's browser), `pending_slot_secs` (a vault without a seen deposit stops counting against the cap this long after it opened; 1,800 by default, at least 60) and `max_pending` (most vaults waiting for their deposit at once; 512 by default). Missing fields take their defaults; the backend reads them at startup, and the app and the CLI read the stamp's bits from `GET /config` and from `network.json` |
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
| Signer bonds | USDC, `minBond` each (judge: 5 × 100 USDC; theft demo: 5 × 10 USDC per run, slashed), locked until withdrawn; the signers pay gas for `deposit` and `requestUnbond` | 500 USDC (judge) + 50 USDC per theft-demo run, plus a little ETH for the signers' own transactions |
| Coordinator | proofs, activations, commitments, confirmations: a few cents of a cent per vault | 0.02 ETH |
| Paymaster deposit | sponsored UserOps, including each account's deployment | 0.05 ETH |
| Bundler executor, market maker, liquidator | their own transactions | 0.02 ETH each |
| Market maker | repays debts in closes and deleverages | 100–200 USDC |
| Demo vaults | 0.01 to 0.125 TAZ each | ~0.5 TAZ |

## 4. Time-bound risks during judging (G4)

| Risk | When | Effect on us | Mitigation |
|---|---|---|---|
| Base's **Denim** upgrade: 200 ms blocks | Base Sepolia, 21 October 2026, 18:00 UTC (Base's upgrades page; mainnet November) | blocks come 10× faster:<br>• log catch-up needs 10× more requests;<br>• a non-archive RPC keeps the state of the last ~128 blocks, about 25 s | the commitment depth is Base's safe head or seconds (H-11), the liquidator's catch-up is bounded in time, the relay lags and alarms count Zcash blocks, other waits are in seconds; use a keyed Base RPC and raise `ZECURED_LOG_RANGE` to what it allows |
| Zcash mainnet NU7 | decision 20 Oct, target 5 Nov; mainnet only | testnet is already on NU7; light wallet servers may upgrade | watch `testnet.zec.rocks`; keep a second light wallet server configured |
| A single NU7 light wallet server (`testnet.zec.rocks`) | any time | deposits and payouts stall while it is down | our own Zebra and light wallet server on the VM as the fallback endpoint |
| Zebra's end-of-support halt | mainnet only; testnet nodes do not halt | none for testnet | — |
| Public Base RPC limits (200-block logs, rate limits) | any time | slower event sync | adaptive log ranges; a keyed RPC for the backend; a witness RPC per node |
| Faucets (TAZ, USDC) for judges | any time | a judge cannot fund a vault | pre-funded demo wallets; DEMO.md fallback |
| Gas price spikes on Base Sepolia | any time | the relay costs more | watchdog balance alarms; treasury reserve |
| RISC Zero's emergency stop of a verifier | any time | proofs under its router selector revert: deposits wait, slashing and inclusion proofs fail | keep two working violation versions, on different router selectors where RISC Zero offers them (a working one can then go only while the other works, R-22); schedule a replacement, which takes `estopDelay` while every violation verifier is stopped; drop the pending items of a removed version and prove them again |
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

## 8. Operator messages: `zecured-ops` (v4)

One tool prepares, signs, collects and submits every EIP-712 message the signers sign as operators
(`ops/`, D74). The signing layer is the one the nodes' liveness receipts use.

| Message | Contract | Signatures |
|---|---|---|
| `PauseMinting`, `UnpauseMinting` | Intake | 3 of 5 |
| `ScheduleVersion` (Deposit, Violation or Swap) | ZecuredVersions | 5 of 5 |
| `RemoveVersion` | ZecuredVersions | 3; all 5 for the last working deposit version; never the last working violation version; 3 for a version that is not working |
| `ReimburseDestination` | Registry | 5 of 5 |

1. `zecured-ops prepare <message> … --out req.json` (`--ttl`, default 6 hours): reads the contract and
   the nonce through the Registry of the network config, and refuses unless the contract's own hash
   view gives the same digest.
2. Each signer: `zecured-ops sign req.json --identity signer/data/node-<i>/identity.json --out
   sig.<i>.json` shows what it signs and signs with `--yes` (`--key-env VAR` reads the key from an
   environment variable instead). Keys are never printed.
3. `zecured-ops collect req.json sig.*.json --out bundle.json` (a bundle can be collected again to add
   signatures).
4. `zecured-ops submit bundle.json --dry-run`, then for real with `--sender-key-env VAR` or
   `--sender-identity <file>`. Before sending it checks the chain, the deployment's contract, the
   contract's hash view, the current nonce, the Registry's signers and how many signatures the
   message needs now (for a removal, through `isWorking`).

`zecured-ops versions` lists the deployment's versions and which of them work. Options: `--config` or
`ZECURED_NETWORK_CONFIG`, `--rpc` or `ZECURED_OPS_RPC` (scrubbed from every error).

**The drill** (`scripts/ops-drill.sh`) runs dry by default; `--send`, on a local fork only, sends the
pause and the unpause and keeps the version stages as dry runs (`DEPLOYER_PRIVATE_KEY` pays;
optional `DRILL_REIMBURSE_CLAIM` and `DRILL_REIMBURSE_COMMITMENT` for a bond-paid claim,
`DRILL_SELECTOR`).

## 9. Invites (the mainnet beta)

`--invites` (or `ZECURED_INVITES`) names the operator's invite file; unset, openings are open to all
(the judge deployment). Its format: `{"openings":true,"inviteOnly":true,"codes":[{"hash":"0x<keccak256
of the code>","uses":1}]}`; hash a code with `cast keccak "<code>"` (surrounding whitespace is trimmed).
The file is read again on every opening and every `/config`, so it changes without a restart; a
broken file is refused at startup, and one that becomes unreadable later closes openings. Uses are
counted in the coordinator's data directory (`invites-used.json`, hashes only). `openings: false`
closes new vaults for everyone; exits, payouts, refunds and proofs never stop. The market maker has
its own invite list for quotes (docs/SWAP.md).

## 10. Watchdog settings (v4)

Besides the v3 checks: Base mainnet ETH and USDC (`MAINNET_RPC_URL`, `WATCH_ETH_MAINNET`,
`MIN_ETH_MAINNET[_<NAME>]`, `MAINNET_USDC`, `WATCH_USDC_MAINNET`, `MIN_USDC_MAINNET_<NAME>`); the
demo wallet's TAZ (`WATCH_DEMO_TAZ=1`, `DEMO_STATUS_URL`, from the backend's demo status); balances
only a service reports (`WATCH_JSON=name|url|field|floor`, for the market maker's inventory); and the
coverage cap's fill (`REGISTRY`, `INTAKE`, `CAP_FILL_ALARM`, an alarm at 70 % by default, R-11). No
alarm names an address, a key or a URL. The swap maker's inventory comes from its operators-only endpoint
(`"status_bind": "127.0.0.1:8096"` in `maker.json`, a loopback address other than `bind`, never added to
the backend's proxy):
`WATCH_JSON=swap_maker_zec|http://127.0.0.1:8096/inventory|zecFreeZat|50000000,swap_maker_usdc|http://127.0.0.1:8096/inventory|usdcFree|100000000`
(floors of 0.5 ZEC and 100 USDC; adjust them).

## 11. The coordinator's keepers (v4)

On every vault pass, before its scan, the coordinator: finalizes the refunds that are ready and drops
the stale ones (R-12), also for a compromised vault, whose tally waits for them (V-4); expires
confirmations whose dispute window has ended while the vault still has unproven ones (R-3); uses each
executed F2 or F4 case to make its payout's claims final (`proveInclusionByCase`, R-2); and runs a
compromised, still-active vault's collateral burn again when no `CollateralBurned` was seen (R-19).
Each call is simulated first and sent only when it would change something; a refused one is retried
after ten minutes, and none of them ever holds a payout back. Gas comes from the coordinator key:
about one transaction per expiring confirmation, plus rare case and burn transactions. Logs name a
vault by its id prefix only.

