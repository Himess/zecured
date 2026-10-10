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
| Threshold demo | Two signers switched to "compromised" sign a sweep, three refuse, nothing moves |
| Vault verification | The borrower scans the vault with its viewing key (UFVK) |

The Base side of P2 / P3 / P6 has been run against a local fork of Base Sepolia (real Morpho
Blue deployment); the public Base Sepolia deployment follows.

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

## Repository layout

| Path | What |
|---|---|
| `contracts/` | Solidity + Foundry: `ZZEC`, `ZecuredRegistry`, `SettlementAdapter`, mocks, 166 tests |
| `signer/crates/zcash-ops` | Ironwood scanning, PCZT build/prove/sign/extract, ZIP 2005 vault keys |
| `signer/crates/threshold` | FROST DKG and re-randomized signing |
| `signer/crates/policy` | P1 classification, payout ledger (I10), PCZT verification against the plan |
| `signer/crates/node` | A signer node: DKG, owner-request checks, policy-gated signing |
| `signer/crates/coordinator` | Keyless orchestrator: DKG relay, attestations, payouts, confirmations |
| `signer/crates/base-client` | EIP-712 (cross-checked with Solidity), Registry events |
| `backend/` | Public API + privacy proxy (RPC, bundler, paymaster allowlist) |
| `bots/` | Liquidator and market maker |
| `cli/` | `zecured-wallet`: Ironwood CLI wallet and borrower actions |
| `app/` | Next.js app (in progress) |
| `docs/` | [Architecture](docs/ARCHITECTURE.md), [Interfaces](docs/INTERFACES.md), [Threat model](docs/THREAT_MODEL.md), [Decisions](docs/DECISIONS.md), [Jury FAQ](docs/JURY_FAQ.md), spikes |

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
./target/debug/zecured-wallet watch --ufvk "$(./target/debug/zecured-wallet loan viewing-key <vault-id>)" --from <height>
```

### Local Base fork

Everything Base-related can run against a local fork of Base Sepolia:

```bash
anvil --fork-url https://sepolia.base.org --chain-id 84532 --block-time 2
# deploy with contracts/script/DeployZecured.s.sol (see contracts/README.md), then
export ZECURED_DEPLOYMENTS=$PWD/contracts/deployments/anvil.json BASE_SEPOLIA_RPC_URL=http://127.0.0.1:8545
scripts/signer-network.sh init && scripts/signer-network.sh start
./target/debug/zecured-bots market-maker &    # and: zecured-bots liquidator &
```

## Security notes

This is a hackathon build. All five signers are run by the authors, key material is stored
unencrypted on the signers' disks, and the code has not been audited. See
[THREAT_MODEL.md](docs/THREAT_MODEL.md) for what leaks, what doesn't, and why.

## License

MIT
