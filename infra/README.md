# Running Zecured on a server

How a deployment runs unattended on one Linux VM (reviews G1–G3). None is kept running through
the hackathon's judging period (docs/DECISIONS.md D82). Every service restarts on failure, and a
watchdog sends alarms. Everything an operator may need to change lives in environment files, not in
code: endpoints, keys, thresholds and the alarm channel.

## Layout

| Path | What |
|---|---|
| `/opt/zecured/bin/` | release binaries: `zecured-node`, `zecured-backend` (built with `--features zecured-node/demo` only for the node, D18), `zecured-bots`, `zecured-tier2-host` |
| `/opt/zecured/src/` | a checkout of the submitted tag: scripts, the watchdog, `tools/local-aa` for the bundler, `contracts/deployments/` |
| `/etc/zecured/` | `network.json` and one `.env` file per service, owned by root, group `zecured`, mode 0640; `app.env` holds the `NEXT_PUBLIC_*` variables the app is built with |
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

## Onion service

The app and its API are also served as one Tor onion service, so that a user can reach them without
showing an IP address to anyone, us included. Tor hands connections to an nginx site on
127.0.0.1:8081 (`infra/onion/`), which serves a second build of the app at `/` and the backend at
`/api/`. Every visitor reaches nginx from 127.0.0.1, and its access log is off. The backend's rate
limits count requests per route and per sender, never per address, so Tor users are limited no
differently.

```bash
sudo apt install tor nginx
sudo install -d /etc/tor/torrc.d
grep -q '^%include /etc/tor/torrc.d' /etc/tor/torrc || echo '%include /etc/tor/torrc.d/*.conf' | sudo tee -a /etc/tor/torrc
sudo cp infra/onion/zecured.torrc /etc/tor/torrc.d/zecured.conf
sudo systemctl restart tor
ONION=$(sudo cat /var/lib/tor/zecured/hostname)       # the address to publish in README.md

sudo cp infra/onion/zecured-onion.nginx.conf /etc/nginx/sites-enabled/zecured-onion
sudo nginx -t && sudo systemctl reload nginx

# The onion build: the app's build variables (app.env), with only the backend URL changed.
sudo -u zecured cp -r /opt/zecured/src/app /opt/zecured/app-onion
cd /opt/zecured/app-onion
sudo -u zecured env $(cat /etc/zecured/app.env | xargs) NEXT_PUBLIC_BACKEND_URL="http://$ONION/api" \
  sh -c 'pnpm install --frozen-lockfile && pnpm build'
sudo cp /opt/zecured/src/infra/systemd/zecured-app-onion.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now zecured-app-onion
```

Then add `http://$ONION` to the backend's `APP_ORIGINS` (`/etc/zecured/backend.env`) and restart
`zecured-backend`. Where the public app is served over TLS, the header
`Onion-Location: http://<onion address>$request_uri` makes Tor Browser offer the onion address.

## A second stack (the mainnet beta)

The mainnet beta (docs/MAINNET.md § 5) runs beside the judge deployment on the same host as a second,
independent stack: its own units, configuration, state and keys, sharing only the binaries and the prover.

```bash
sudo infra/stack.sh mainnet 920       # zecured-mainnet-* units: /etc/zecured-mainnet/, /var/lib/zecured-mainnet/,
                                      # nodes on 127.0.0.1:9201..9205
sudo install -d -o zecured -g zecured -m 0700 /var/lib/zecured-mainnet
sudo install -d -o root -g zecured -m 0750 /etc/zecured-mainnet
sudo systemctl daemon-reload
```

Its environment files differ from the public stack's in every endpoint and key: Base mainnet and two independent Zcash
Mainnet light wallet servers, its own coordinator, relayer and bot keys, its own `BACKEND_BIND`, bundler and market
maker ports, and a `network.json` for Zcash Mainnet. Its watchdog (`zecured-mainnet-watchdog.timer`) watches it as the
first one watches the public stack.

**One prover for both.** Set each stack's prover (the backend's `DEPOSIT_PROOF_HOST`, and the maker's) to
`/opt/zecured/src/infra/prover-lock.sh`, with `ZECURED_PROVER_HOST` naming that network's `zecured-tier2-host` build
(the Mainnet one is built with `--features mainnet`). The wrapper holds one lock for each whole proof, so the stacks
take turns on the GPU; each coordinator keeps its own queue.

```bash
sudo install -d -o zecured -g zecured -m 0700 /var/lib/zecured-prover
```
