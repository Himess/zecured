#!/usr/bin/env bash
# Proves on a rented NVIDIA GPU machine (docs/TIER2.md § 8, review RQ-17). Runs ON that machine, in a
# copy of the repository (`git archive`), never with signer keys or .env on it.
#
#   scripts/gpu-prove.sh setup                                   # once: Rust, rzup, the native Groth16 prover, the host
#   scripts/gpu-prove.sh prove deposit <witness.json> <out-dir>  # succinct receipt, then Groth16 seal
#   scripts/gpu-prove.sh prove violation <witness.json> <out-dir>
#
# The host is built with the `cuda` feature: the STARK is proven on the GPU and wrapped in Groth16
# natively (the `risc0-groth16` rzup component), so the machine needs no Docker. The guests are not
# built here: ZECURED_DEPOSIT_ELF / ZECURED_VIOLATION_ELF name the pinned ELFs (the `guests`
# workflow's artifact), so every seal is for the deployed image id.
#
# A witness holds its vault's viewing key: only the team's own disposable vaults are proven on
# rented hardware, and the machine is destroyed afterwards (docs/TIER2.md § 8).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="$ROOT/tier2/target/release/zecured-tier2-host"
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"

setup() {
  nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv
  command -v nvcc >/dev/null || { echo "nvcc not found: use a CUDA devel image" >&2; exit 1; }
  nvcc --version | tail -1
  if ! command -v cargo >/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  fi
  if ! command -v rzup >/dev/null; then
    curl -sSfL https://risczero.com/install | bash
  fi
  # risc0-groth16 3.0.5 looks for version 0.1.0 of the component (src/prove/cuda.rs).
  rzup install risc0-groth16 0.1.0
  cd "$ROOT/tier2"
  # No guest build (pinned ELFs) and no LTO: the prover is GPU-bound, the build is not.
  RISC0_SKIP_BUILD=1 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_DEBUG=0 \
    cargo build --release --locked -p zecured-tier2-host --features cuda
}

prove() {
  local statement="$1" witness="$2" out="$3" prefix=()
  case "$statement" in
    deposit) : "${ZECURED_DEPOSIT_ELF:?ZECURED_DEPOSIT_ELF: the pinned deposit guest}" ;;
    violation) : "${ZECURED_VIOLATION_ELF:?ZECURED_VIOLATION_ELF: the pinned violation guest}"; prefix=(violation) ;;
    *) echo "statement: deposit | violation" >&2; exit 1 ;;
  esac
  mkdir -p "$out"
  # 2^20-cycle segments suit a 24 GB GPU (the host's default, 2^18, suits an 8 GB laptop).
  export ZECURED_SEGMENT_PO2="${ZECURED_SEGMENT_PO2:-20}"
  "$HOST" ${prefix[@]+"${prefix[@]}"} image-id | tee "$out/$statement.image-id"
  local started
  started=$(date +%s)
  "$HOST" ${prefix[@]+"${prefix[@]}"} prove "$witness" "$out/$statement.receipt"
  echo "$statement succinct: $(( $(date +%s) - started )) s" | tee -a "$out/timings.txt"
  started=$(date +%s)
  "$HOST" ${prefix[@]+"${prefix[@]}"} groth16 "$out/$statement.receipt" "$out/$statement.seal.json"
  echo "$statement groth16: $(( $(date +%s) - started )) s" | tee -a "$out/timings.txt"
}

case "${1:-}" in
  setup) setup ;;
  prove) [[ $# -eq 4 ]] || { echo "usage: $0 prove deposit|violation <witness.json> <out-dir>" >&2; exit 1; }
         prove "$2" "$3" "$4" ;;
  *) echo "usage: $0 setup | prove deposit|violation <witness.json> <out-dir>" >&2; exit 1 ;;
esac
