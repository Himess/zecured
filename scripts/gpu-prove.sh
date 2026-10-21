#!/usr/bin/env bash
# Proves on a rented NVIDIA GPU machine (docs/TIER2.md § 8, review RQ-17). Runs ON that machine, in a
# copy of the repository (`git archive`), never with signer keys or .env on it.
#
#   scripts/gpu-prove.sh setup                                   # once: Rust, rzup, the native Groth16 prover, the host
#   scripts/gpu-prove.sh prove deposit <witness.json> <out-dir>  # the succinct receipt
#   scripts/gpu-prove.sh prove violation <witness.json> <out-dir>
#   scripts/gpu-prove.sh prove payment <witness.json> <out-dir>     # Zecured Swap's payment guest
#
# The host is built with the `cuda` feature, so the STARK is proven on the GPU. The Groth16 wrap is not made
# here: risc0-groth16 3.0.5's CUDA prover fails on our receipts (docs/TESTING.md § 2b), so the wrap runs with
# Docker on x86 (`zecured-tier2-host groth16`), from the succinct receipt, which holds no viewing key.
# GROTH16=cuda still tries it here, in two processes (`p254`, then `shrink-wrap`). The guests are not
# built here: ZECURED_DEPOSIT_ELF / ZECURED_VIOLATION_ELF / ZECURED_PAYMENT_ELF name the pinned ELFs (the `guests`
# workflow's artifact), so every receipt is for the deployed image id.
#
# A deposit witness holds its vault's tag key K (statement v2), which links the vault's notes and payouts
# on Zcash; an F1, F2 or F3 witness holds a viewing key. Only the team's own disposable vaults are proven
# on rented hardware, and the machine is destroyed afterwards (docs/TIER2.md § 8). The host is built for
# Testnet; a Mainnet prover builds it with `--features cuda,mainnet`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="$ROOT/tier2/target/release/zecured-tier2-host"
export PATH="$HOME/.cargo/bin:$HOME/.risc0/bin:$PATH"

setup() {
  nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv
  command -v nvcc >/dev/null || { echo "nvcc not found: use a CUDA devel image" >&2; exit 1; }
  nvcc --version | tail -1
  # risc0 3.0's kernels do not compile against CUDA 13's headers (CCCL's driver_api.h): build with a
  # CUDA 12 compiler next to the image's toolkit. The driver runs code built for 12.x.
  if nvcc --version | grep -q "release 13"; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq cuda-nvcc-12-8 cuda-cudart-dev-12-8 cuda-cccl-12-8 \
      cuda-driver-dev-12-8 cuda-nvrtc-dev-12-8
    # Every build script must see 12.8: some link through /usr/local/cuda (cust), so it points there too.
    update-alternatives --set cuda /usr/local/cuda-12.8
    export PATH="/usr/local/cuda-12.8/bin:$PATH" CUDA_PATH=/usr/local/cuda-12.8 CUDA_HOME=/usr/local/cuda-12.8 \
      CUDA_ROOT=/usr/local/cuda-12.8
  fi
  # bindgen (circom-witnesscalc, in the Groth16 prover) needs libclang.
  if ! command -v clang >/dev/null; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq clang libclang-dev pkg-config libssl-dev
  fi
  if ! command -v cargo >/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  fi
  rustup toolchain install "$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")" --profile minimal
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
    payment) : "${ZECURED_PAYMENT_ELF:?ZECURED_PAYMENT_ELF: the pinned payment guest}"; prefix=(payment) ;;
    *) echo "statement: deposit | violation | payment" >&2; exit 1 ;;
  esac
  mkdir -p "$out"
  # 2^20-cycle segments suit a 24 GB GPU (the host's default, 2^18, suits an 8 GB laptop).
  export ZECURED_SEGMENT_PO2="${ZECURED_SEGMENT_PO2:-20}"
  "$HOST" ${prefix[@]+"${prefix[@]}"} image-id | tee "$out/$statement.image-id"
  local started
  started=$(date +%s)
  "$HOST" ${prefix[@]+"${prefix[@]}"} prove "$witness" "$out/$statement.receipt"
  echo "$statement succinct: $(( $(date +%s) - started )) s" | tee -a "$out/timings.txt"
  if [[ "${GROTH16:-}" == cuda ]]; then
    "$HOST" ${prefix[@]+"${prefix[@]}"} p254 "$out/$statement.receipt" "$out/$statement.p254"
    started=$(date +%s)
    "$HOST" ${prefix[@]+"${prefix[@]}"} shrink-wrap "$out/$statement.p254" "$out/$statement.receipt" \
      "$out/$statement.seal.json"
    echo "$statement groth16: $(( $(date +%s) - started )) s" | tee -a "$out/timings.txt"
  fi
}

case "${1:-}" in
  setup) setup ;;
  prove) [[ $# -eq 4 ]] || { echo "usage: $0 prove deposit|violation|payment <witness.json> <out-dir>" >&2; exit 1; }
         prove "$2" "$3" "$4" ;;
  *) echo "usage: $0 setup | prove deposit|violation|payment <witness.json> <out-dir>" >&2; exit 1 ;;
esac
