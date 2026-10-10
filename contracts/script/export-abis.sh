#!/usr/bin/env bash
# Writes the ABIs used by the app and the off-chain services to contracts/abi/<Contract>.json.
# Each file holds only the contract's `abi` array (no bytecode or metadata).
# Re-run after any change to a contract's external interface, and commit contracts/abi/.
#
# Usage (from any directory): contracts/script/export-abis.sh
set -euo pipefail

cd "$(dirname "$0")/.."

contracts=(
  "src/ZZEC.sol:ZZEC"
  "src/ZecuredRegistry.sol:ZecuredRegistry"
  "src/SettlementAdapter.sol:SettlementAdapter"
  "src/mocks/MockUSDC.sol:MockUSDC"
  "src/mocks/MockZecOracle.sol:MockZecOracle"
)

forge build --quiet
mkdir -p abi
for id in "${contracts[@]}"; do
  name="${id##*:}"
  forge inspect "$id" abi --json > "abi/${name}.json"
  echo "abi/${name}.json"
done
