#!/usr/bin/env bash
# One prover for the stacks (docs/MAINNET.md § 5): point each stack's prover setting (the backend's DEPOSIT_PROOF_HOST,
# and the maker's `host`) at this wrapper instead of zecured-tier2-host. The STARK (`prove`) runs on
# ZECURED_PROVER_HOST; the Groth16 wrap (`groth16`) on ZECURED_PROVER_GROTH16_HOST when it is set: a build without
# cuda, which wraps with Docker on x86 (docs/TESTING.md § 2b), since a cuda build's own wrap fails. One lock per kind,
# so two STARKs never share the GPU and two wraps never share the memory, while a wrap and the next STARK overlap;
# each coordinator keeps its own queue and order. `image-id` and `network` take no lock and print only what the host
# prints (the maker parses that output). Standard input (the witness) and the exit status pass through.
#   [violation|payment] image-id | network | prove <witness> <receipt> | groth16 <receipt> <seal> | mock-seal ...
set -euo pipefail
host=${ZECURED_PROVER_HOST:?set ZECURED_PROVER_HOST to the zecured-tier2-host build of the network}
locks=${ZECURED_PROVER_LOCKS:-/var/lib/zecured-prover}
mkdir -p "$locks" 2>/dev/null || true
sub=${1:-}
if [[ $sub == violation || $sub == payment ]]; then sub=${2:-}; fi
case $sub in
  image-id | network) exec "$host" "$@" ;;
  groth16) exec flock "$locks/groth16.lock" "${ZECURED_PROVER_GROTH16_HOST:-$host}" "$@" ;;
  *) exec flock "$locks/gpu.lock" "$host" "$@" ;;
esac
