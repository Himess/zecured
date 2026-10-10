#!/usr/bin/env bash
# Run the Zecured signer network locally: 5 signer nodes + the backend (coordinator).
#
#   scripts/signer-network.sh init     # create node identities (keys from .env) and network.json
#   scripts/signer-network.sh start    # build and start everything in the background
#   scripts/signer-network.sh status   # health of every process
#   scripts/signer-network.sh stop
#
# State lives in signer/data/ (gitignored). Logs: signer/data/logs/.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="$ROOT/signer/data"
NETWORK="$DATA/network.json"
LOGS="$DATA/logs"
PIDS="$DATA/pids"
BIN="$ROOT/target/debug"

load_env() {
  if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
  fi
}

admin_token() {
  if [[ ! -f "$DATA/admin-token" ]]; then
    mkdir -p "$DATA"
    head -c 24 /dev/urandom | xxd -p > "$DATA/admin-token"
    chmod 600 "$DATA/admin-token"
  fi
  cat "$DATA/admin-token"
}

build() {
  (cd "$ROOT" && cargo build -q -p zecured-node -p zecured-backend -p zecured-wallet)
}

cmd_init() {
  load_env
  build
  mkdir -p "$DATA"
  local infos=()
  for i in 1 2 3 4 5; do
    local dir="$DATA/node-$i"
    local key_var="SIGNER_${i}_PRIVATE_KEY"
    if [[ ! -f "$dir/identity.json" ]]; then
      NODE_ETH_PRIVATE_KEY="${!key_var:-}" "$BIN/zecured-node" init --index "$i" --data "$dir" \
        --url "http://127.0.0.1:910$i" > "$dir.public.json"
    fi
    infos+=("$dir.public.json")
  done

  local deployments="$ROOT/contracts/deployments/base-sepolia.json"
  local rpc="${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}"
  python3 -I - "$NETWORK" "$deployments" "$rpc" "${LIGHTWALLETD_URL:-https://testnet.zec.rocks:443}" "${infos[@]}" <<'PY'
import json, sys, os, subprocess
out, deployments, rpc, lwd, *infos = sys.argv[1:]
registry = adapter = "0x000000000000000000000000000000000000dEaD"
from_block = None
if os.path.exists(deployments):
    d = json.load(open(deployments))
    registry = d.get("registry") or d.get("ZecuredRegistry") or registry
    adapter = d.get("adapter") or d.get("SettlementAdapter") or adapter
    from_block = d.get("deployBlock") or d.get("startBlock")
if from_block is None:
    from_block = int(subprocess.run(["cast", "block-number", "--rpc-url", rpc], capture_output=True, text=True, check=True).stdout)
cfg = {
    "lightwalletd_url": lwd,
    "base_rpc_url": rpc,
    "chain_id": 84532,
    "registry": registry,
    "adapter": adapter,
    "registry_from_block": int(from_block),
    "confirmations": 3,
    "vault_expiry_blocks": 288,
    "denominations": [1000000, 5000000],
    "nodes": [json.load(open(p)) for p in infos],
}
json.dump(cfg, open(out, "w"), indent=2)
print(f"wrote {out} (registry {registry})")
PY
}

cmd_start() {
  load_env
  [[ -f "$NETWORK" ]] || cmd_init
  build
  mkdir -p "$LOGS" "$PIDS"
  local token
  token="$(admin_token)"
  for i in 1 2 3 4 5; do
    if [[ -f "$PIDS/node-$i" ]] && kill -0 "$(cat "$PIDS/node-$i")" 2>/dev/null; then
      continue
    fi
    DEMO_ADMIN_TOKEN="$token" nohup "$BIN/zecured-node" run --data "$DATA/node-$i" --config "$NETWORK" \
      --bind "127.0.0.1:910$i" > "$LOGS/node-$i.log" 2>&1 &
    echo $! > "$PIDS/node-$i"
  done
  if ! { [[ -f "$PIDS/backend" ]] && kill -0 "$(cat "$PIDS/backend")" 2>/dev/null; }; then
    DEMO_ADMIN_TOKEN="$token" ZECURED_NETWORK_CONFIG="$NETWORK" ZECURED_COORDINATOR_DATA="$DATA/coordinator" \
      nohup "$BIN/zecured-backend" > "$LOGS/backend.log" 2>&1 &
    echo $! > "$PIDS/backend"
  fi
  sleep 2
  cmd_status
}

cmd_stop() {
  for f in "$PIDS"/*; do
    [[ -f "$f" ]] || continue
    kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  echo "stopped"
}

cmd_status() {
  for i in 1 2 3 4 5; do
    printf "node %s  " "$i"
    curl -s --max-time 3 "http://127.0.0.1:910$i/v1/health" || printf "down"
    echo
  done
  printf "backend "
  curl -s --max-time 3 "http://127.0.0.1:8080/health" || printf "down"
  echo
}

case "${1:-}" in
  init) cmd_init ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  *) echo "usage: $0 init|start|status|stop" >&2; exit 2 ;;
esac
