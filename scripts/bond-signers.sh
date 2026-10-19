#!/usr/bin/env bash
# Tier 3 (docs/TIER3.md § 7): each of the five signers deposits its USDC bond (BOND_USDC whole
# USDC each, default the bond's minimum) from its own attestation key, which lives in its node's
# identity file. Nothing is minted until all five have.
#
#   scripts/bond-signers.sh                 # the judge network (signer/data/, deployments/base-sepolia.json)
#   THEFT_DEMO=1 scripts/bond-signers.sh    # the theft demo's network (signer/data-theft-demo/, T3-6)
#
# The keys reach forge through its environment only: never the command line, the terminal or a log.
# DEPLOYER_PRIVATE_KEY (repo-root .env) tops up a signer that lacks the USDC or some gas (on a
# local fork it mints the MockUSDC). Sends real transactions to BASE_SEPOLIA_RPC_URL.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -n "${THEFT_DEMO:-}" ]]; then
  DATA="$ROOT/signer/data-theft-demo"
  DEPLOYMENT="${ZECURED_DEPLOYMENTS:-$ROOT/contracts/deployments/base-sepolia-theft-demo.json}"
else
  DATA="$ROOT/signer/data"
  DEPLOYMENT="${ZECURED_DEPLOYMENTS:-$ROOT/contracts/deployments/base-sepolia.json}"
fi
[[ -f "$DEPLOYMENT" ]] || { echo "no deployment file: $DEPLOYMENT" >&2; exit 1; }

if [[ -f "$ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env"
  set +a
fi
RPC="${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}"

# field FILE KEY...: the JSON value at that key path.
field() {
  python3 -I -c 'import json, sys
v = json.load(open(sys.argv[1]))
for k in sys.argv[2:]:
    v = v[k]
print(v)' "$@"
}
SIGNER_BOND="$(field "$DEPLOYMENT" contracts SignerBond)"
export SIGNER_BOND
for i in 1 2 3 4 5; do
  key="$(field "$DATA/node-$i/identity.json" eth_private_key)"
  export "SIGNER_${i}_PRIVATE_KEY=0x$key"
done
unset key

cd "$ROOT/contracts"
# A local fork (anvil, which keeps Base Sepolia's chain id) records its transactions under data/local-fork,
# never under contracts/broadcast/*/84532, where the real Base Sepolia records live.
if [[ "$RPC" == http://127.0.0.1* || "$RPC" == http://localhost* ]]; then
  export FOUNDRY_BROADCAST="$ROOT/data/local-fork/broadcast-bond"
fi
forge script script/BondSigners.s.sol --rpc-url "$RPC" --broadcast
