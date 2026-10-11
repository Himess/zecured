# Zecured patch

Vendored from `pasta_curves` 0.6.1 (crates.io). The only change: when compiled for the RISC Zero
zkVM (`target_os = "zkvm"`), `Fp` and `Fq` multiplication, squaring and inversion go through
RISC Zero's `bigint2` modular arithmetic accelerator (`src/fields/fp.rs`, `src/fields/fq.rs`,
look for `ZECURED PATCH`). Every other target compiles the original code.
