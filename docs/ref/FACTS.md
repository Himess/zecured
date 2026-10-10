# Verified facts (single source for constants)

Every row is verified from a primary source; date = when verified. Unverified items are marked ⚠️.

## Zcash

| Fact | Value | Source | Verified |
|---|---|---|---|
| NU6.3 (Ironwood) testnet activation | 4,134,000 (mined 2026-07-04); branch ID `0x37A5165B` | zcash_protocol CHANGELOG 0.10.0-pre.0, ZIP 258 | 2026-10-10 |
| NU6.3 mainnet activation | 3,428,143 | zcash_protocol CHANGELOG 0.10.0 | 2026-10-10 |
| NU7 testnet activation | 4,465,026 (2026-10-04 18:21 UTC) | zcash_protocol CHANGELOG 0.11.0-pre.0, ZIP 259 | 2026-10-10 |
| NU7 consensus branch ID | `0x77190AD9` | zcash_protocol CHANGELOG 0.11.0-pre.0 | 2026-10-10 |
| NU7 on mainnet | target 2026-11-05; height decided 2026-10-20 | NU7 timeline forum post, ZIP 259 | 2026-10-10 |
| NU7 block limits | 25 s target spacing; v4 invalid; ≤330 Orchard, ≤330 Ironwood, ≤300 Sapling per block, and one shared global budget of 330 | ZIP 218, ZIP 2003 | 2026-10-10 |
| Leaving Orchard | value may leave the Orchard pool to any destination (every pool crossing is a ZIP 209 turnstile crossing); Ironwood is only the recommended destination. Orchard can no longer receive (valueBalanceOrchard ≥ 0, same-address change only) | ZIP 258, ZIP 209 | 2026-10-10 |
| First crates with NU7 testnet support without `zcash_unstable` | zcash_protocol 0.11.0-pre.0 and the matching `-pre` wave (zcash_primitives 0.31.0-pre.1, pczt 0.10.0-pre.1, zcash_keys 0.17.0-pre.1, zcash_address 0.14.0-pre.1, zcash_client_backend 0.25.0-pre.1, zip321 0.10.0-pre.1, orchard 0.16.0) | crates.io, CHANGELOGs | 2026-10-10 |
| Ironwood implementation | inside `orchard`: `ValuePool::Ironwood`, `BundleVersion::ironwood_v3()`, V3 note plaintexts (`NoteVersion::V3`, ZIP 2005), `IronwoodDomain` for note encryption | orchard CHANGELOG 0.15.0 | 2026-10-10 |
| Ironwood receivers | **No separate typecode.** Receivers are scoped to the Orchard protocol, not a pool: an Orchard receiver (UA typecode 0x03) receives Ironwood-pool funds after NU6.3. "Ironwood-only UA" = UA whose only receiver is the Orchard-protocol receiver. | ZIP 326 § Scanning, § Receiving funds into the Ironwood pool | 2026-10-10 |
| Orchard pool after NU6.3 | same-address restriction: an Orchard-pool action's output must go to its spent note's own receiver; nobody can pay an external receiver in the Orchard pool | ZIP 326, orchard 0.15.0 (`Flags::CROSS_ADDRESS_DISABLED`) | 2026-10-10 |
| FROST keys for Orchard protocol | MUST use `use_qsk = true`: participants privately agree on `sk`; `nk = H^nk(sk)`, `qsk = H^qsk(sk)`, `qk = BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)`, `rivk = H^rivk_ext_qk(ak, nk)`; `ak` from DKG with ỹ = 0. No library implements this yet → implemented in `signer/crates/zcash-ops`. | ZIP 2005 § Usage with FROST, § 4.2.3 | 2026-10-10 |
| Relevant ZIPs | 229 (v6 tx), 258 (NU6.3 deployment), 318 (Orchard→Ironwood migration), 326 (NU6.3 wallet consequences), 374 (PCZT), 2005 (Ironwood quantum recoverability), 312 (FROST spend auth), 317 (fees), 218 (25 s blocks), 321 (payment URIs) | zcash/zips | 2026-10-10 |
| reddsa FROST | reddsa 0.6.0 removed the `frost` feature (to move to ZcashFoundation/frost, not yet released) → we use reddsa 0.5.2 `frost` (frost-core / frost-rerandomized 3.0.0) and bridge to orchard 0.16 by bytes | reddsa CHANGELOG | 2026-10-10 |
| Randomizer | alpha comes from the PCZT and is passed as `Randomizer::from_scalar` (same as ZF's zcash-sign / frost-client); frost-rerandomized 3.0 recommends commitment-derived randomizers, which Zcash's sighash ordering does not allow | frost-rerandomized 3.0 src, frost-zcash-demo | 2026-10-10 |

| ZIP 2005 use_qsk test vectors | 8 vectors from ZcashFoundation/frost-tools PR #610 (zafe-core, unofficial); our implementation matches all of them | `signer/crates/zcash-ops/tests/fixtures/zip2005_use_qsk.json` | 2026-10-11 |
| v6 version group ID | `0xD884B698` | ZIP 229 | 2026-10-10 |

## Zcash infrastructure (testnet)

| Fact | Value | Source | Verified |
|---|---|---|---|
| Light wallet server | `https://testnet.zec.rocks:443` — ECC lightwalletd v0.5.4 on Zebra 7.0.0-rc.0, branch `0x77190ad9`. **The only public endpoint confirmed post-NU7** (single point of failure → fallback: own Zebra regtest with its built-in lightwalletd-compatible gRPC server) | our `zecured-spike info`; research | 2026-10-10 |
| Dead/old endpoints | lightwalletd.testnet.electriccoin.co:9067 (timeout); zaino.testnet.unsafe.zec.rocks (503, pre-NU7); testnet.lightwalletd.com (no DNS) | probes | 2026-10-10 |
| Zaino | no NU7 release yet (latest 0.10.1; NU7 in zingolabs/zaino#1655) | GitHub | 2026-10-10 |
| Faucet | Valar `https://faucet.testnet.valargroup.dev` — no captcha; 0.125 TAZ per IP and per address per 24 h, global 18.75 TAZ/24 h; UA payouts land in Ironwood (v6, Ironwood actions) | research | 2026-10-10 |
| Wallet tooling | zcash-devtool, frost-tools (ex frost-zcash-demo) and zingo-cli are **not** NU7-ready on testnet; Zkool merged NU7 deps 2026-10-07 (untested) | research | 2026-10-10 |
| Zebra regtest | `network = "Regtest"`; heights under `[network.testnet_parameters.activation_heights]` with keys `Canopy`, `NU5`, `NU6`, `"NU6.1"`, `"NU6.2"`, `"NU6.3"`, `NU7`; mining via `generate`/`generatetoaddress` RPCs or `internal-miner`; Zebra ≥6.4 has `[rpc] lightwalletd_listen_addr` (built-in lightwalletd-compatible gRPC) | Zebra book, research (not yet run) | 2026-10-10 |
| Fees | ZIP-317 marginal fee 5,000 zat/action, 2 grace actions (librustzcash); Zebra ≥6.4 also accepts 1,000 | research | 2026-10-10 |

## Base Sepolia (84532)

| Fact | Value | Source | Verified |
|---|---|---|---|
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | docs.morpho.org addresses + on-chain code | 2026-10-10 |
| AdaptiveCurveIRM | `0x46415998764C29aB2a25CbeA6254146D50D22687` (`isIrmEnabled` = true) | docs.morpho.org + on-chain | 2026-10-10 |
| LLTV 62.5% (`625000000000000000`) | enabled | `isLltvEnabled` on-chain | 2026-10-10 |
| Other enabled LLTVs | 0, 38.5, 77, 86, 91.5, 94.5, 96.5, 98 % | on-chain | 2026-10-10 |
| EntryPoint v0.7 / v0.8 | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` / `0x4337084D9E255Ff0702461CF8895CE9E3b5Ff108` (code present) | on-chain | 2026-10-10 |

## Open (⚠️)
- Paymaster provider (Pimlico vs CDP) and smart account (Safe vs Kernel) — Spike C.
