#!/usr/bin/env bash
# The judge deployment (docs/TIER3.md § 8): Zecured v3 with the parameters of
# contracts/config/zecured-base-sepolia.json (proof-only minting against the shared header relay,
# a challenge window of a day, USDC bonds, the coverage cap, the opening fee), the judge
# network's five signers (SIGNER_ADDRESSES), and the guests' reproducible image ids (README,
# "Reproducing the guest image ids"). Writes contracts/deployments/base-sepolia.json; then bond
# the signers (scripts/bond-signers.sh) and register the treasury's receiver and salt behind
# FEE_COMMITMENT with them (POST /recipients), or the opening-fee claims are never paid. Draw them
# before deploying with `cargo run -q -p zecured-recipient -- --address <treasury UA> --out <file>`,
# which prints the commitment and keeps the salt in that file (0600) for the registration. After a
# real deployment, add its Registry to JUDGE_REGISTRIES (signer/crates/base-client/src/theft_demo_guard.rs).
#
#   scripts/deploy-judge.sh                 # simulation only
#   scripts/deploy-judge.sh --broadcast     # sends the deployment
#
# Needs DEPOSIT_IMAGE_ID, VIOLATION_IMAGE_ID, ZCASH_RELAY and FEE_COMMITMENT (the treasury's
# salted Ironwood commitment), in the environment or .env.
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
: "${FEE_COMMITMENT:?FEE_COMMITMENT: the salted Ironwood commitment of the treasury (the fee is on)}"
if [[ -n "${THEFT_DEMO:-}" ]]; then
  echo "THEFT_DEMO is set: the judge deployment never is (scripts/deploy-theft-demo.sh)" >&2
  exit 1
fi

cd "$ROOT/contracts"
DEPLOYMENT_NAME=base-sepolia forge script script/DeployZecured.s.sol --rpc-url "$RPC" "$@"
