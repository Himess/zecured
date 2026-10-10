# Verified facts (single source for constants)

Every row is verified from a primary source; date = when verified. Unverified items are marked ⚠️.

## Zcash

| Fact | Value | Source | Verified |
|---|---|---|---|
| NU6.3 (Ironwood) testnet activation | 4,134,000 | zcash_protocol CHANGELOG 0.10.0-pre.0 | 2026-10-10 |
| NU6.3 mainnet activation | 3,428,143 | zcash_protocol CHANGELOG 0.10.0 | 2026-10-10 |
| NU7 testnet activation | 4,465,026 | zcash_protocol CHANGELOG 0.11.0-pre.0 | 2026-10-10 |
| NU7 consensus branch ID | `0x77190AD9` | zcash_protocol CHANGELOG 0.11.0-pre.0 | 2026-10-10 |
| NU7 on mainnet | no activation height yet | zcash_protocol CHANGELOG 0.11.0-pre.0 | 2026-10-10 |
| First crates with NU7 testnet support without `zcash_unstable` | zcash_protocol 0.11.0-pre.0 and the matching `-pre` wave (zcash_primitives 0.31.0-pre.1, pczt 0.10.0-pre.1, zcash_keys 0.17.0-pre.1, zcash_address 0.14.0-pre.1, zcash_client_backend 0.25.0-pre.1, zip321 0.10.0-pre.1, orchard 0.16.0) | crates.io, CHANGELOGs | 2026-10-10 |
| Ironwood implementation | inside `orchard`: `ValuePool::Ironwood`, `BundleVersion::ironwood_v3()`, V3 note plaintexts (`NoteVersion::V3`, ZIP 2005), `IronwoodDomain` for note encryption | orchard CHANGELOG 0.15.0 | 2026-10-10 |
| Ironwood receivers | **No separate typecode.** Receivers are scoped to the Orchard protocol, not a pool: an Orchard receiver (UA typecode 0x03) receives Ironwood-pool funds after NU6.3. "Ironwood-only UA" = UA whose only receiver is the Orchard-protocol receiver. | ZIP 326 § Scanning, § Receiving funds into the Ironwood pool | 2026-10-10 |
| Orchard pool after NU6.3 | same-address restriction: an Orchard-pool action's output must go to its spent note's own receiver; nobody can pay an external receiver in the Orchard pool | ZIP 326, orchard 0.15.0 (`Flags::CROSS_ADDRESS_DISABLED`) | 2026-10-10 |
| FROST keys for Orchard protocol | MUST use `use_qsk = true`: participants privately agree on `sk`; `nk = H^nk(sk)`, `qsk = H^qsk(sk)`, `qk = BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)`, `rivk = H^rivk_ext_qk(ak, nk)`; `ak` from DKG with ỹ = 0. No library implements this yet → implemented in `signer/crates/zcash-ops`. | ZIP 2005 § Usage with FROST, § 4.2.3 | 2026-10-10 |
| Relevant ZIPs | 229 (v6 tx), 258 (NU6.3 deployment), 318 (Orchard→Ironwood migration), 326 (NU6.3 wallet consequences), 374 (PCZT), 2005 (Ironwood quantum recoverability), 312 (FROST spend auth), 317 (fees), 218 (25 s blocks), 321 (payment URIs) | zcash/zips | 2026-10-10 |
| reddsa FROST | reddsa 0.6.0 removed the `frost` feature (to move to ZcashFoundation/frost, not yet released) → we use reddsa 0.5.2 `frost` (frost-core / frost-rerandomized 3.0.0) and bridge to orchard 0.16 by bytes | reddsa CHANGELOG | 2026-10-10 |
| Randomizer | alpha comes from the PCZT and is passed as `Randomizer::from_scalar` (same as ZF's zcash-sign / frost-client); frost-rerandomized 3.0 recommends commitment-derived randomizers, which Zcash's sighash ordering does not allow | frost-rerandomized 3.0 src, frost-zcash-demo | 2026-10-10 |

## Base Sepolia (84532)

| Fact | Value | Source | Verified |
|---|---|---|---|
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | docs.morpho.org addresses + on-chain code | 2026-10-10 |
| AdaptiveCurveIRM | `0x46415998764C29aB2a25CbeA6254146D50D22687` (`isIrmEnabled` = true) | docs.morpho.org + on-chain | 2026-10-10 |
| LLTV 62.5% (`625000000000000000`) | enabled | `isLltvEnabled` on-chain | 2026-10-10 |
| Other enabled LLTVs | 0, 38.5, 77, 86, 91.5, 94.5, 96.5, 98 % | on-chain | 2026-10-10 |
| EntryPoint v0.7 / v0.8 | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` / `0x4337084D9E255Ff0702461CF8895CE9E3b5Ff108` (code present) | on-chain | 2026-10-10 |

## Open (⚠️)
- Public testnet lightwalletd / Zaino endpoints on NU7 software; faucet; tooling support (research in progress).
- Paymaster provider (Pimlico vs CDP) and smart account (Safe vs Kernel) — Spike C.
