# Zecured

> **Borrow against shielded ZEC. Sell nothing. Reveal nothing.**

Every way to use ZEC in DeFi today starts with unshielding it. Zecured doesn't.

Zecured lets ZEC holders borrow USDC on Base without selling their ZEC and without it ever
leaving the shielded pool. Collateral sits in a fresh **3-of-5 FROST threshold vault** in
Zcash's **Ironwood** pool; a **non-transferable receipt (zZEC)** is minted straight into a
**Morpho Blue** position of a fresh, gasless smart account. Close the loan by repaying in USDC
— or **close it from collateral**: a market maker repays the debt and takes ZEC for it, and
the rest returns to a new shielded address, so no outside funds ever touch your account. If
the price drops, a liquidator repays and receives the seized ZEC, shielded.

Built for ZECATHON 2026, Cross-Chain track. **Testnet experiment — not a financial product.**

## What works today

Verified end to end on **Zcash testnet (NU7)** with five independent signer processes:

| Flow | Result |
|---|---|
| Vault creation | 5-party FROST DKG over HTTP (node-to-node encrypted), ZIP 2005 `use_qsk` keys, Ironwood-only address |
| P1 deposit classification | Ironwood-only deposits of the exact denomination conform; a wrong amount is non-conforming |
| P5 refund | Owner-signed request, PCZT verified by each signer, 3-of-5 FROST signature, ZEC back in the wallet |
| P2 repay + release | zZEC burned on Base → payout to the borrower's new Ironwood address → `confirmPayout` |
| P6 close from collateral | Market maker repays on Morpho; market maker and borrower paid in **one** Zcash transaction |
| P3 liquidation | Bot liquidates after a price drop; liquidator paid with `ovk = None`; the remainder stays the borrower's and is released |
| Partial release (D14) | Part of the ZEC paid out while the loan stays open (LTV after ≤ 50 %), the rest released later |
| Failure handling | Payouts with two signers offline, a signer restarted mid-payout, and a payout that expired unmined and was rebuilt |
| Threshold demo | Two signers switched to "compromised" sign a sweep, three refuse, nothing moves |
| Vault verification | The borrower scans the vault with its viewing key (UFVK) |

The Base side of P2 / P3 / P6 / D14 has been run against a local fork of Base Sepolia (real
Morpho Blue deployment); the public Base Sepolia deployment follows. Evidence (block heights,
txids) and how to reproduce every run: [docs/TESTING.md](docs/TESTING.md).

## What is real and what is a mock

| Real | Mock |
|---|---|
| Shielded Ironwood transactions on Zcash testnet (NU7) | Price oracle (`MockZecOracle`, price set by the demo owner) |
| 3-of-5 FROST threshold vaults (re-randomized FROST, RedPallas), keys per ZIP 2005 | USDC (`MockUSDC`) |
| Per-payout policy checks by every signer (P0–P6) | Signer independence (all 5 nodes run by us) |
| Morpho Blue market on Base Sepolia (the real Morpho deployment) | |
| ZZEC / ZecuredRegistry / SettlementAdapter contracts | |
| Liquidation, close from collateral, refunds of non-conforming deposits | |
| Own RPC / bundler / paymaster proxy with a sponsorship allowlist | |
| Tier 2 deposit statement on real Testnet data, run in the RISC Zero zkVM; Zcash header relay with on-chain Equihash | RISC Zero verifier on the local fork (`MockRiscZeroVerifier`, accepts unproven journals) until Groth16 seals are produced on an x86 machine; Base Sepolia uses the real verifier router |

## Repository layout

| Path | What |
|---|---|
| `contracts/` | Solidity + Foundry: `ZZEC`, `ZecuredRegistry` (attestations, Tier 2 deposit proofs, cancellation), `SettlementAdapter`, mocks, and the Zcash header relay (`src/relay/`); 253 tests |
| `signer/crates/zcash-ops` | Ironwood scanning, PCZT build/prove/sign/extract, ZIP 2005 vault keys |
| `signer/crates/threshold` | FROST DKG and re-randomized signing |
| `signer/crates/policy` | P1 classification, payout ledger (I10), PCZT verification against the plan |
| `signer/crates/node` | A signer node: DKG, owner-request checks, policy-gated signing |
| `signer/crates/coordinator` | Keyless orchestrator: DKG relay, attestations, payouts, confirmations |
| `signer/crates/base-client` | EIP-712 (cross-checked with Solidity), Registry events |
| `signer/crates/deposit-witness` | Tier 2: the ZIP 221 history tree (V3, Ironwood) from public chain data, and the witness of a deposit proof |
| `tier2/` | Tier 2: the deposit statement (`statement/`), the RISC Zero guest and host (separate Cargo workspace) |
| `backend/` | Public API + privacy proxy (RPC, bundler, paymaster allowlist) |
| `bots/` | Liquidator and market maker |
| `cli/` | `zecured-wallet`: Ironwood CLI wallet and borrower actions |
| `app/` | Next.js app (in progress) |
| `docs/` | [Architecture](docs/ARCHITECTURE.md), [Interfaces](docs/INTERFACES.md), [Threat model](docs/THREAT_MODEL.md), [Decisions](docs/DECISIONS.md), [Testing and evidence](docs/TESTING.md), [Security review](docs/SECURITY_REVIEW.md), [Tier 2 design](docs/TIER2.md), [Operators](docs/OPERATORS.md), [Jury FAQ](docs/JURY_FAQ.md), spikes |

## Running it

Prerequisites: Rust (the repo pins 1.99.0 via `rust-toolchain.toml`), Foundry, `jq`, Python 3.
The Zcash side talks to `https://testnet.zec.rocks:443` (the public light wallet server on NU7).

```bash
# Contracts
cd contracts && forge test --isolate && cd ..

# Rust (signer, backend, bots, CLI)
cargo test -p zecured-threshold -p zecured-zcash-ops -p zecured-policy -p zecured-base -p zecured-node -p zecured-backend

# Secrets: copy .env.example to .env and fill in (never commit .env)
scripts/signer-network.sh init     # node identities + signer/data/network.json
scripts/signer-network.sh start    # 5 signer nodes + backend (logs in signer/data/logs/)

# A borrower, from the command line
./target/debug/zecured-wallet init                       # Ironwood wallet; fund it with testnet TAZ
./target/debug/zecured-wallet loan open --amount 0.01    # opens a vault, prints the ZIP-321 URI
./target/debug/zecured-wallet loan deposit <vault-id>
./target/debug/zecured-wallet loan status <vault-id>
./target/debug/zecured-wallet loan viewing-key <vault-id> > ufvk.txt   # keep it local
./target/debug/zecured-wallet watch --ufvk-file ufvk.txt --from <height> --tier3
```

### Local Base fork

Everything Base-related can run against a local fork of Base Sepolia, including the app's
sponsored UserOps (a local alto bundler and mock paymaster replace the hosted ones, no API key):

```bash
scripts/local-fork.sh start   # anvil fork, fresh deployment (contracts/deployments/anvil.json), bundler :4337, paymaster :4338
export ZECURED_DEPLOYMENTS=$PWD/contracts/deployments/anvil.json BASE_SEPOLIA_RPC_URL=http://127.0.0.1:8545 \
       BUNDLER_URL=http://127.0.0.1:4337 PAYMASTER_URL=http://127.0.0.1:4338
scripts/signer-network.sh init && scripts/signer-network.sh start
./target/debug/zecured-bots market-maker &    # and: zecured-bots liquidator &
```

### Tier 2: minting on a deposit proof

```bash
(cd tier2 && cargo build --release)           # RISC Zero guest and host (rzup toolchain)
./target/debug/update-headers data/tier2/headers_nu7.json   # NU7-epoch headers over P2P (first run: see docs/TIER2.md)
TIER2=1 scripts/local-fork.sh start           # + header relay fed from the Testnet, image id, MOCK verifier
DEPOSIT_PROOF_HOST=$PWD/tier2/target/release/zecured-tier2-host DEPOSIT_PROOF_MOCK=true \
  scripts/signer-network.sh start             # the coordinator proves deposits instead of attesting them
```

On the local fork the proof is checked by `MockRiscZeroVerifier` (MOCK); the real seal is a
succinct receipt wrapped in Groth16 (`zecured-tier2-host prove`, then `groth16` on an x86 machine
with Docker), checked by RISC Zero's verifier router on Base Sepolia.

## Security notes

This is a hackathon build. All five signers are run by the authors, key material is stored
unencrypted on the signers' disks (files readable by their owner only), and the code has not
been audited by a third party. An internal security review of the off-chain code, its fixes
and the remaining risks are in [SECURITY_REVIEW.md](docs/SECURITY_REVIEW.md); see
[THREAT_MODEL.md](docs/THREAT_MODEL.md) for what leaks, what doesn't, and why.

## License

MIT
