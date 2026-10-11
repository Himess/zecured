#!/usr/bin/env bash
# Run the Zecured signer network locally: 5 signer nodes + the backend (coordinator).
#
#   scripts/signer-network.sh init     # create node identities (keys from .env) and network.json
#   scripts/signer-network.sh start    # build and start everything in the background
#   scripts/signer-network.sh status   # health of every process
#   scripts/signer-network.sh stop
#
# State lives in signer/data/ (gitignored). Logs: signer/data/logs/.
#
# DEMO build: the node binary is built with the `demo` feature and nodes 4 and 5 are started
# with --demo-allow-compromise, so the demo panel can switch them (and only them) to
# "compromised" for the threshold demo. "offline" works on every node. Never deploy this build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="$ROOT/signer/data"
NETWORK="$DATA/network.json"
LOGS="$DATA/logs"
PIDS="$DATA/pids"
BIN="$ROOT/target/debug"

# Values already set in the environment (e.g. a local fork, see scripts/local-fork.sh) win over .env.
OVERRIDABLE=(BASE_SEPOLIA_RPC_URL ZECURED_DEPLOYMENTS BUNDLER_URL PAYMASTER_URL)

load_env() {
  local kept=() var
  for var in "${OVERRIDABLE[@]}"; do
    [[ -n "${!var:-}" ]] && kept+=("$var=${!var}")
  done
  if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
  fi
  for var in "${kept[@]+"${kept[@]}"}"; do
    export "${var?}"
  done
  return 0
}

admin_token() {
  if [[ ! -f "$DATA/admin-token" ]]; then
    mkdir -p "$DATA"
    head -c 24 /dev/urandom | xxd -p > "$DATA/admin-token"
    chmod 600 "$DATA/admin-token"
  fi
  cat "$DATA/admin-token"
}

# Nodes that the demo panel may switch to "compromised" (two at most: below the 3-of-5 threshold).
DEMO_COMPROMISABLE_NODES=" 4 5 "

build() {
  # Two builds: only the node binary gets the `demo` feature.
  (cd "$ROOT" && cargo build -q -p zecured-node --features demo && cargo build -q -p zecured-backend -p zecured-wallet)
}

cmd_init() {
  load_env
  if [[ -z "${COORDINATOR_ADDRESS:-}" ]]; then
    echo "COORDINATOR_ADDRESS is not set (the address of COORDINATOR_PRIVATE_KEY, in .env):" >&2
    echo "nodes refuse to start without it, as every request must be signed by the coordinator (D16)." >&2
    exit 1
  fi
  export COORDINATOR_ADDRESS
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

  local deployments="${ZECURED_DEPLOYMENTS:-$ROOT/contracts/deployments/base-sepolia.json}"
  local rpc="${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}"
  python3 -I - "$NETWORK" "$deployments" "$rpc" "${LIGHTWALLETD_URL:-https://testnet.zec.rocks:443}" "${infos[@]}" <<'PY'
import json, sys, os, subprocess
out, deployments, rpc, lwd, *infos = sys.argv[1:]
registry = adapter = "0x000000000000000000000000000000000000dEaD"
from_block = None
if os.path.exists(deployments):
    d = json.load(open(deployments))
    registry = d["contracts"]["ZecuredRegistry"]
    adapter = d["contracts"]["SettlementAdapter"]
    from_block = d["block"]
if from_block is None:
    from_block = int(subprocess.run(["cast", "block-number", "--rpc-url", rpc], capture_output=True, text=True, check=True).stdout)
cfg = {
    "lightwalletd_url": lwd,
    "base_rpc_url": rpc,
    "chain_id": 84532,
    "registry": registry,
    "adapter": adapter,
    "registry_from_block": int(from_block),
    # S12: ZIP 315's depth for an untrusted TXO (a deposit from someone else's wallet).
    "confirmations": int(os.environ.get("CONFIRMATIONS", "10")),
    "vault_expiry_blocks": 288,
    # D7 vault sizes, and 0.125 TAZ: what the Valar testnet faucet pays (S13), so a judge can
    # fund a vault straight from the faucet.
    "denominations": [1000000, 5000000, 12500000],
    "nodes": [json.load(open(p)) for p in infos],
    # D16: nodes only accept requests signed by the coordinator (and refuse to start without it).
    "coordinator": os.environ["COORDINATOR_ADDRESS"],
    # D15: random extra confirmations before attesting (0 for the demo).
    "attest_delay_max_blocks": int(os.environ.get("ATTEST_DELAY_MAX_BLOCKS", "0")),
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
    local demo_flags=()
    if [[ "$DEMO_COMPROMISABLE_NODES" == *" $i "* ]]; then
      demo_flags=(--demo-allow-compromise)
    fi
    DEMO_ADMIN_TOKEN="$token" nohup "$BIN/zecured-node" run --data "$DATA/node-$i" --config "$NETWORK" \
      --bind "127.0.0.1:910$i" ${demo_flags[@]+"${demo_flags[@]}"} > "$LOGS/node-$i.log" 2>&1 &
    echo $! > "$PIDS/node-$i"
  done
  if ! { [[ -f "$PIDS/backend" ]] && kill -0 "$(cat "$PIDS/backend")" 2>/dev/null; }; then
    DEMO_ADMIN_TOKEN="$token" ZECURED_NETWORK_CONFIG="$NETWORK" ZECURED_COORDINATOR_DATA="$DATA/coordinator" \
      ZECURED_DEPLOYMENTS="${ZECURED_DEPLOYMENTS:-$ROOT/contracts/deployments/base-sepolia.json}" \
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
