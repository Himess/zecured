#!/usr/bin/env bash
# Snapshots of a live run's public state, for its recording (app/lib/replay/harvest.ts): what the backend serves
# and Base's block, written to data/live-run/snap-<tag>.json. The backend's answers go when it stops (D82); Base keeps
# the rest, which the harvester reads at these blocks. Public data only: no wallet, key or coordinator file is read,
# and nothing but the file's name is printed.
#
#   scripts/live-snapshot.sh t0                       before the recorded vault opens
#   scripts/live-snapshot.sh t1 <taker>               before the swap: the swap wallet's loan-token balance
#   scripts/live-snapshot.sh t2 <vaultId> [orderId]   once the close's payout is proven, before the shutdown
#
# Environment: BACKEND (default http://127.0.0.1:8080), BASE_SEPOLIA_RPC_URL (from the repo-root .env),
# ZECURED_DEPLOYMENTS (default contracts/deployments/base-sepolia.json, for the loan token), LIVE_RUN_DIR
# (default data/live-run).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# An RPC given on the command line wins over the .env's.
given_rpc=${BASE_SEPOLIA_RPC_URL:-}
if [[ -f "$ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env"
  set +a
fi
BACKEND=${BACKEND:-http://127.0.0.1:8080}
RPC=${given_rpc:-${BASE_SEPOLIA_RPC_URL:?set BASE_SEPOLIA_RPC_URL}}
DEPLOYMENT=${ZECURED_DEPLOYMENTS:-$ROOT/contracts/deployments/base-sepolia.json}
OUT=${LIVE_RUN_DIR:-$ROOT/data/live-run}
tag=${1:-}
mkdir -p "$OUT"
chmod 700 "$OUT"

get() { curl -fsS --max-time 30 "$BACKEND$1"; }
rpc() { curl -fsS --max-time 30 -H 'content-type: application/json' -d "$1" "$RPC"; }
block=$(rpc '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}')
at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf '%s' "$block" > "$tmp/block.json"

case $tag in
  t0)
    get /config > "$tmp/config.json"
    get /status > "$tmp/status.json"
    get /demo/status > "$tmp/demo.json"
    get /stats/vaults > "$tmp/stats.json"
    ;;
  t1)
    taker=${2:?give the swap wallet address}
    token=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["contracts"]["LoanToken"])' "$DEPLOYMENT")
    # balanceOf(address): 0x70a08231 and the address in one word.
    data="0x70a08231000000000000000000000000${taker#0x}"
    rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_call\",\"params\":[{\"to\":\"$token\",\"data\":\"$data\"},\"latest\"]}" > "$tmp/balance.json"
    ;;
  t2)
    vault=${2:?give the recorded vault id}
    get /status > "$tmp/status.json"
    get "/vaults/$vault" > "$tmp/vault.json"
    if [[ -n ${3:-} ]]; then get "/swap/orders/$3" > "$tmp/order.json"; fi
    ;;
  *)
    echo "usage: scripts/live-snapshot.sh t0 | t1 <taker> | t2 <vaultId> [orderId]" >&2
    exit 2
    ;;
esac

python3 - "$tag" "$at" "$tmp" "$OUT/snap-$tag.json" <<'PY'
import json, os, sys
tag, at, tmp, out = sys.argv[1:5]
def load(name):
    p = os.path.join(tmp, name)
    return json.load(open(p)) if os.path.exists(p) else None
block = load("block.json")
if "result" not in block:
    sys.exit("Base did not answer eth_blockNumber")
snap = {"at": at, "baseBlock": int(block["result"], 16)}
if tag == "t0":
    snap.update(config=load("config.json"), status=load("status.json"), demo=load("demo.json"), stats=load("stats.json"))
elif tag == "t1":
    balance = load("balance.json")
    if "result" not in balance:
        sys.exit("Base did not answer the balance read")
    snap["takerUsdc"] = str(int(balance["result"], 16) if balance["result"] not in ("0x", "") else 0)
else:
    snap.update(status=load("status.json"), vault=load("vault.json"), order=load("order.json"))
with open(out, "w") as f:
    json.dump(snap, f, indent=2)
os.chmod(out, 0o600)
print("wrote", out)
PY
