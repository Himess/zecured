# Running Zecured on a server

How the demo deployment runs unattended on one Linux VM through the judging period (reviews
G1–G3). Every service restarts on failure, and a watchdog sends alarms. Everything an operator
may need to change lives in environment files, not in code: endpoints, keys, thresholds and the
alarm channel.

## Layout

| Path | What |
|---|---|
| `/opt/zecured/bin/` | release binaries: `zecured-node`, `zecured-backend` (built with `--features zecured-node/demo` only for the node, D18), `zecured-bots`, `zecured-tier2-host` |
| `/opt/zecured/src/` | a checkout of the submitted tag: scripts, the watchdog, `tools/local-aa` for the bundler, `contracts/deployments/` |
| `/etc/zecured/` | `network.json` and one `.env` file per service, owned by root, group `zecured`, mode 0640 |
| `/var/lib/zecured/` | state: `node-1` .. `node-5`, `coordinator`, `tier2/` (header file and summaries), the watchdog's state |

## Services (`infra/systemd/`)

| Unit | Runs | Configuration |
|---|---|---|
| `zecured-node@1..5` | the five signer nodes, on 127.0.0.1:9101..9105 | `node-N.env`: `DEMO_ADMIN_TOKEN`, `NODE_BASE_RPC_URL`, `NODE_BASE_RPC_WITNESS_URL`, `NODE_LIGHTWALLETD_URL`, `NODE_EXTRA_ARGS` (`--demo-allow-compromise` on nodes 4 and 5 only) |
| `zecured-backend` | coordinator, public API, proxies | `backend.env`: `COORDINATOR_PRIVATE_KEY`, `ZECURED_NETWORK_CONFIG`, `ZECURED_COORDINATOR_DATA`, `ZECURED_DEPLOYMENTS`, `BUNDLER_URL`, `PAYMASTER_*`, `PROXY_RPC_URL`, `APP_ORIGINS`, `DEPOSIT_PROOF_*` |
| `zecured-relayer@1..2` | two independent header relayers (D30): the second on standby | `relayer-N.env`: `RELAYER_PRIVATE_KEY`, `ZECURED_RELAY`, `BASE_SEPOLIA_RPC_URL`; relayer 2 also `RELAYER_STANDBY_LAG=10`, so it pays only when the first falls 10 blocks behind |
| `zecured-market-maker`, `zecured-liquidator` | the bots | `market-maker.env`, `liquidator.env` |
| `zecured-bundler` | our ERC-4337 bundler (alto) | `bundler.env` |
| `zecured-watchdog.timer` | `infra/watchdog/watchdog.py` every 5 minutes | `watchdog.env` (see `watchdog.env.example`) |

Every long-running unit has `Restart=always` and is never rate-limited out of restarting
(`StartLimitIntervalSec=0`). Each runs as the unprivileged user `zecured`, with a read-only system
and write access to its own state only.

## Install

```bash
sudo useradd --system --home /var/lib/zecured --shell /usr/sbin/nologin zecured
sudo install -d -o zecured -g zecured -m 0700 /var/lib/zecured
sudo install -d -o root -g zecured -m 0750 /etc/zecured
sudo cp infra/systemd/* /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now zecured-node@{1..5} zecured-backend zecured-relayer@{1,2} \
  zecured-market-maker zecured-liquidator zecured-bundler zecured-watchdog.timer
```

## Operating it

- **State of everything:** `systemctl list-units 'zecured-*'`; logs: `journalctl -u zecured-backend -f`.
- **The public status page:** `GET /status` on the backend shows the running commit, the deployed
  image ids, the Zcash tip and each node's mode (G5).
- **A watchdog pass by hand:** `sudo -u zecured env $(cat /etc/zecured/watchdog.env | xargs) python3 -I
  /opt/zecured/src/infra/watchdog/watchdog.py` prints every check.
- **Changing an endpoint, key or threshold:** edit the `.env` file, then `systemctl restart` the unit.
