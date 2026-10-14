#!/usr/bin/env bash
# The judge deployment (docs/TIER3.md § 8): proof-only minting (K1) against the shared header
# relay, Tier 3 with a challenge window of at least a day, the judge network's five signers
# (SIGNER_ADDRESSES), and the guests' reproducible image ids (README, "Reproducing the guest
# image ids"). Writes contracts/deployments/base-sepolia.json; after a real deployment, add its
# Registry to JUDGE_REGISTRIES (signer/crates/base-client/src/theft_demo_guard.rs).
#
#   scripts/deploy-judge.sh                 # simulation only
#   scripts/deploy-judge.sh --broadcast     # sends the deployment
#
# Needs DEPOSIT_IMAGE_ID, VIOLATION_IMAGE_ID and ZCASH_RELAY, in the environment or .env.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env"
  set +a
fi
RPC="${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}"
: "${SIGNER_ADDRESSES:?SIGNER_ADDRESSES: the judge network signers (.env)}"
: "${DEPOSIT_IMAGE_ID:?DEPOSIT_IMAGE_ID: the reproducible deposit guest id}"
: "${VIOLATION_IMAGE_ID:?VIOLATION_IMAGE_ID: the reproducible violation guest id}"
: "${ZCASH_RELAY:?ZCASH_RELAY: the header relay}"
if [[ -n "${THEFT_DEMO:-}" ]]; then
  echo "THEFT_DEMO is set: the judge deployment never is (scripts/deploy-theft-demo.sh)" >&2
  exit 1
fi

cd "$ROOT/contracts"
DEPLOYMENT_NAME=base-sepolia MINT_MODE=proof-only forge script script/DeployZecured.s.sol --rpc-url "$RPC" "$@"
