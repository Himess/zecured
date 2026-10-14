#!/usr/bin/env bash
# The theft demo's own deployment (review T3-6, docs/TIER3.md § 8): THEFT_DEMO=true,
# DEPLOYMENT_NAME=base-sepolia-theft-demo, a 2-minute challenge window, and the theft-demo
# network's own five signers (`THEFT_DEMO=1 scripts/signer-network.sh init` creates them). The judge
# network's signers (SIGNER_ADDRESSES in .env) go to the deploy script as JUDGE_SIGNER_ADDRESSES,
# and it refuses any overlap.
#
#   scripts/deploy-theft-demo.sh                 # simulation only
#   scripts/deploy-theft-demo.sh --broadcast     # sends the deployment
#
# Needs DEPOSIT_IMAGE_ID and VIOLATION_IMAGE_ID (the reproducible build's ids, README) and
# ZCASH_RELAY (the shared header relay), in the environment or .env.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env"
  set +a
fi
RPC="${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}"
JUDGE="${SIGNER_ADDRESSES:?SIGNER_ADDRESSES: the judge network signers (.env)}"
: "${DEPOSIT_IMAGE_ID:?DEPOSIT_IMAGE_ID: the reproducible deposit guest id}"
: "${VIOLATION_IMAGE_ID:?VIOLATION_IMAGE_ID: the reproducible violation guest id}"
: "${ZCASH_RELAY:?ZCASH_RELAY: the header relay}"

# The theft-demo nodes' attestation addresses, ascending (the deploy script's order).
THEFT_SIGNERS="$(python3 -I - "$ROOT/signer/data-theft-demo" <<'PY'
import glob, json, sys
files = sorted(glob.glob(sys.argv[1] + "/node-*.public.json"))
addrs = sorted((json.load(open(f))["eth"] for f in files), key=lambda a: int(a, 16))
if len(addrs) != 5:
    sys.exit(f"{len(addrs)} theft-demo node identities: run THEFT_DEMO=1 scripts/signer-network.sh init")
print(",".join(addrs))
PY
)"

cd "$ROOT/contracts"
DEPLOYMENT_NAME=base-sepolia-theft-demo THEFT_DEMO=true JUDGE_SIGNER_ADDRESSES="$JUDGE" \
  SIGNER_ADDRESSES="$THEFT_SIGNERS" forge script script/DeployZecured.s.sol --rpc-url "$RPC" "$@"
