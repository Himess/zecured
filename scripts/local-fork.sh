#!/usr/bin/env bash
# Local Base Sepolia fork for development and end-to-end tests: anvil forking Base Sepolia, a
# fresh Zecured deployment, gas for the service keys, and a local ERC-4337 bundler + paymaster
# (tools/local-aa), so the app's sponsored UserOps run without any API key.
#
#   scripts/local-fork.sh start    # anvil :8545, deploy (contracts/deployments/anvil.json), AA :4337/:4338
#   scripts/local-fork.sh stop
#   scripts/local-fork.sh status
#
# Then run the signer network against the fork:
#   export ZECURED_DEPLOYMENTS=$PWD/contracts/deployments/anvil.json BASE_SEPOLIA_RPC_URL=http://127.0.0.1:8545 \
#          BUNDLER_URL=http://127.0.0.1:4337 PAYMASTER_URL=http://127.0.0.1:4338
#   scripts/signer-network.sh init && scripts/signer-network.sh start
#
# Keys come from the repo-root .env (DEPLOYER_PRIVATE_KEY, SIGNER_ADDRESSES and the service
# addresses). Nothing here touches a public network: anvil only reads Base Sepolia state.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="$ROOT/data/local-fork"
RPC="http://127.0.0.1:8545"
FORK_URL="${FORK_URL:-https://sepolia.base.org}"

load_env() {
  if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
  fi
}

running() { [[ -f "$STATE/$1.pid" ]] && kill -0 "$(cat "$STATE/$1.pid")" 2>/dev/null; }

wait_for() {
  local what=$1 check=$2
  for _ in $(seq 1 120); do
    if eval "$check" >/dev/null 2>&1; then return 0; fi
    sleep 0.5
  done
  echo "$what did not come up" >&2
  return 1
}

cmd_start() {
  load_env
  mkdir -p "$STATE"
  : "${DEPLOYER_PRIVATE_KEY:?set in .env}" "${SIGNER_ADDRESSES:?set in .env}"

  if running anvil; then
    echo "anvil already running"
  else
    anvil --fork-url "$FORK_URL" --chain-id 84532 --port 8545 --block-time 2 \
      --compute-units-per-second 15 --fork-retry-backoff 1000 --retries 10 --silent \
      > "$STATE/anvil.log" 2>&1 &
    echo $! > "$STATE/anvil.pid"
    wait_for anvil "cast chain-id --rpc-url $RPC"
    echo "anvil forking $FORK_URL at block $(cast block-number --rpc-url "$RPC")"
  fi

  # Gas for every key that sends Base transactions. Smart accounts never get any (I2).
  for var in DEPLOYER_ADDRESS COORDINATOR_ADDRESS BOT_LIQUIDATOR_ADDRESS BOT_MARKET_MAKER_ADDRESS; do
    if [[ -n "${!var:-}" ]]; then
      cast rpc --rpc-url "$RPC" anvil_setBalance "${!var}" 0x8ac7230489e80000 > /dev/null # 10 ETH
    fi
  done

  if [[ ! -f "$STATE/deployed" ]]; then
    (
      cd "$ROOT/contracts"
      DEPLOYMENT_NAME=anvil LENDER_SEED_USDC="${LENDER_SEED_USDC:-100000}" \
        FOUNDRY_BROADCAST="$STATE/broadcast" \
        forge script script/DeployZecured.s.sol --rpc-url "$RPC" --broadcast > "$STATE/deploy.log" 2>&1
    ) || { tail -30 "$STATE/deploy.log" >&2; exit 1; }
    # MockUSDC for the demo bots (the liquidator repays debt, the market maker pays closes).
    local usdc
    usdc="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["contracts"]["MockUSDC"])' \
      "$ROOT/contracts/deployments/anvil.json")"
    for var in BOT_LIQUIDATOR_ADDRESS BOT_MARKET_MAKER_ADDRESS; do
      if [[ -n "${!var:-}" ]]; then
        cast send --rpc-url "$RPC" --private-key "$DEPLOYER_PRIVATE_KEY" "$usdc" "mint(address,uint256)" \
          "${!var}" 1000000000000 > /dev/null # 1,000,000 MockUSDC
      fi
    done
    touch "$STATE/deployed"
    echo "deployed: $(python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["contracts"]["ZecuredRegistry"])' \
      "$ROOT/contracts/deployments/anvil.json") (Registry)"
  fi

  if running aa; then
    echo "local AA already running"
  else
    [[ -d "$ROOT/tools/local-aa/node_modules" ]] || (cd "$ROOT/tools/local-aa" && pnpm install --frozen-lockfile --ignore-scripts)
    node "$ROOT/tools/local-aa/start.mjs" > "$STATE/aa.log" 2>&1 &
    echo $! > "$STATE/aa.pid"
    wait_for "local AA" "grep -q 'local AA ready' '$STATE/aa.log'"
    tail -1 "$STATE/aa.log"
  fi
}

cmd_stop() {
  for p in aa anvil; do
    if running "$p"; then kill "$(cat "$STATE/$p.pid")" && echo "stopped $p"; fi
    rm -f "$STATE/$p.pid"
  done
  # A new anvil starts from Base Sepolia again: the deployment is gone with it.
  rm -f "$STATE/deployed"
}

cmd_status() {
  for p in anvil aa; do
    if running "$p"; then echo "$p running"; else echo "$p stopped"; fi
  done
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  *) echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
