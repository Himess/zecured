#!/usr/bin/env bash
# One prover for two stacks (docs/MAINNET.md § 5): point each stack's prover setting (the backend's DEPOSIT_PROOF_HOST,
# and the maker's) at this wrapper instead of zecured-tier2-host. It holds one exclusive lock for a whole proof, so the
# stacks' proofs take turns on the GPU instead of running at once; each coordinator keeps its own queue and order.
# Standard input (the witness) and the exit status pass through.
set -euo pipefail
host=${ZECURED_PROVER_HOST:?set ZECURED_PROVER_HOST to the zecured-tier2-host build of the network}
exec flock /var/lib/zecured-prover/lock "$host" "$@"
