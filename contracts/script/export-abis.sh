#!/usr/bin/env bash
# Writes the ABIs used by the app and the off-chain services to contracts/abi/<Contract>.json.
# Each file holds only the contract's `abi` array (no bytecode or metadata).
# IZecuredDisputes and IZecuredRefunds are served at the Registry's address (its fallback runs
# ZecuredSpendLogic's code), so the Registry's own ABI does not list those functions (the refund events
# stay in the Registry's ABI).
# Re-run after any change to a contract's external interface, and commit contracts/abi/.
#
# Usage (from any directory): contracts/script/export-abis.sh
set -euo pipefail

cd "$(dirname "$0")/.."

contracts=(
  "src/ZZEC.sol:ZZEC"
  "src/ZecuredRegistry.sol:ZecuredRegistry"
  "src/interfaces/v3/IZecuredDisputes.sol:IZecuredDisputes"
  "src/interfaces/v3/IZecuredRefunds.sol:IZecuredRefunds"
  "src/SettlementAdapter.sol:SettlementAdapter"
  "src/ZecuredIntake.sol:ZecuredIntake"
  "src/SignerBondV2.sol:SignerBondV2"
  "src/ZecuredSlasher.sol:ZecuredSlasher"
  "src/ZecuredCompensator.sol:ZecuredCompensator"
  "src/ZecuredVersions.sol:ZecuredVersions"
  "src/ZecuredSwap.sol:ZecuredSwap"
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
