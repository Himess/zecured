# Zecured

> **Borrow against shielded ZEC. Sell nothing. Reveal nothing.**

Zecured lets ZEC holders borrow USDC on Base without selling their ZEC and without it
ever leaving the shielded pool. Collateral sits in a fresh 3-of-5 FROST threshold vault
in Zcash's Ironwood pool; a non-transferable receipt (zZEC) is minted straight into a
Morpho Blue position for a fresh, gasless smart account.

Built for ZECATHON 2026 (Cross-Chain track). **Testnet experiment — not a financial product.**

Status: work in progress. Full design: [docs/PLAN.md](docs/PLAN.md).

## Repository layout

| Path | What |
|---|---|
| `contracts/` | Solidity + Foundry: ZecuredRegistry, ZZEC, SettlementAdapter, mocks |
| `signer/` | Rust: 5 FROST signer nodes + coordinator, policy P0–P6, Zcash ops |
| `backend/` | Rust: coordinator API + RPC/bundler/paymaster proxy (no logs) |
| `bots/` | Liquidation and market-maker bots |
| `app/` | Next.js frontend |
| `infra/`, `scripts/` | Local orchestration, demo and end-to-end scripts |
| `docs/` | Plan, architecture, threat model, jury FAQ, demo, references |

## What is real and what is a mock

| Real | Mock |
|---|---|
| _(filled in as features land)_ | Price oracle (MOCK), USDC (MOCK), signer independence (all 5 nodes run by us) |

## License

MIT
