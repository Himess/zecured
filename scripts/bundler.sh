#!/usr/bin/env bash
# RQ-05: our own ERC-4337 bundler for Base Sepolia (Pimlico's alto, the version the local fork tests
# run: tools/local-aa). Only the backend's proxy talks to it; the proxy applies the paymaster
# allowlist, the sender check and the rate limits first, and only our own paymaster sponsors.
#
#   scripts/bundler.sh            # alto for EntryPoint v0.7 on BUNDLER_PORT (default 4337)
#
# Environment (repo-root .env): BASE_SEPOLIA_RPC_URL; BUNDLER_EXECUTOR_PRIVATE_KEY (sends the bundles;
# the EntryPoint refunds it from our paymaster's deposit) and BUNDLER_UTILITY_PRIVATE_KEY (deploys alto's
# simulation contract once, tops up the executor), both with a little Base Sepolia ETH. The keys reach
# alto through its ALTO_* environment variables, never the command line.
#
# alto listens on every interface: keep BUNDLER_PORT closed to the outside (firewall), so that no one
# but the proxy can submit to it. --safe-mode false because public RPC endpoints refuse
# debug_traceCall, which ERC-7562's checks need; acceptable for a private bundler whose only sponsor is
# our own allowlisted paymaster (docs/DECISIONS.md D30).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Only the variables this script needs: alto is third-party code and must not see the other keys.
env_value() { [[ -f "$ROOT/.env" ]] && grep -E "^$1=" "$ROOT/.env" | tail -1 | cut -d= -f2- || true; }
for name in BASE_SEPOLIA_RPC_URL BUNDLER_EXECUTOR_PRIVATE_KEY BUNDLER_UTILITY_PRIVATE_KEY BUNDLER_PORT; do
  if [[ -z "${!name:-}" ]]; then
    value="$(env_value "$name")"
    [[ -n "$value" ]] && printf -v "$name" '%s' "$value"
  fi
done

RPC="${BASE_SEPOLIA_RPC_URL:?set BASE_SEPOLIA_RPC_URL}"
[[ "$(cast chain-id --rpc-url "$RPC")" == 84532 ]] || { echo "not Base Sepolia" >&2; exit 1; }
ALTO="$ROOT/tools/local-aa/node_modules/@pimlico/alto/esm/cli/alto.js"
[[ -f "$ALTO" ]] || (cd "$ROOT/tools/local-aa" && pnpm install --frozen-lockfile --ignore-scripts)

export ALTO_EXECUTOR_PRIVATE_KEYS="${BUNDLER_EXECUTOR_PRIVATE_KEY:?set BUNDLER_EXECUTOR_PRIVATE_KEY}"
export ALTO_UTILITY_PRIVATE_KEY="${BUNDLER_UTILITY_PRIVATE_KEY:?set BUNDLER_UTILITY_PRIVATE_KEY}"
exec node "$ALTO" \
  --rpc-url "$RPC" \
  --entrypoints 0x0000000071727De22E5E9d8BAf0edAc6f37da032 \
  --chain-type op-stack \
  --safe-mode false \
  --deploy-simulations-contract true \
  --block-time 2000 \
  --port "${BUNDLER_PORT:-4337}" \
  --log-level "${ALTO_LOG_LEVEL:-info}"
