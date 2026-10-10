# Decision log

Decisions taken after docs/PLAN.md v2, with the reason. PLAN.md stays the design reference;
where they differ, this log wins.

| # | Date | Decision | Why | Plan sections affected |
|---|---|---|---|---|
| D1 | 2026-10-10 | Use the NU7-capable `-pre` librustzcash wave (zcash_protocol 0.11.0-pre.0, zcash_primitives 0.31.0-pre.1, pczt 0.10.0-pre.1, zcash_keys 0.17.0-pre.1, zcash_client_backend 0.25.0-pre.1, orchard 0.16) | Testnet is past NU7 (4,465,026, branch `0x77190AD9`); the stable releases only know NU7 behind `zcash_unstable` with a placeholder branch ID and cannot transact on testnet | 13, 14 |
| D2 | 2026-10-10 | FROST(Pallas) from `reddsa` 0.5.2 (`frost` feature, frost-core / frost-rerandomized 3.0), bridged to orchard 0.16 by bytes | reddsa 0.6 removed its `frost` feature; the move to ZcashFoundation/frost is not released yet | 14 |
| D3 | 2026-10-10 | Vault keys follow ZIP 2005 `use_qsk = true` (signers agree on `sk`; `nk`, `qsk`, `qk`, `rivk_ext` derived from it and the DKG `ak`) | ZIP 2005 § Usage with FROST requires it for FROST-generated Orchard-protocol keys; checked against 8 independent vectors | 9.4 |
| D4 | 2026-10-10 | "Ironwood-only address" = a unified address whose only receiver is the Orchard-protocol receiver (typecode 0x03); "extract the Ironwood receiver" = take the UA's Orchard receiver | There is no Ironwood typecode: receivers are scoped to the Orchard protocol (ZIP 326, ZIP 229); after NU6.3 nobody can pay an external receiver in the Orchard pool | 8.1, 9.4, 9.5, 16.5 |
| D5 | 2026-10-10 | P1 "Ironwood → Ironwood" check = the full deposit transaction has no transparent inputs or outputs, no Sapling spends or outputs, no Orchard-pool actions | Strictest reading of "only Ironwood → Ironwood": such a transaction crosses no turnstile and reveals no amount | 9.5 (P1) |
| D6 | 2026-10-10 | Spend randomizer `alpha` comes from the PCZT and is used as the FROST randomizer (`Randomizer::from_scalar`, deprecated `frost_rerandomized::sign`) | `rk = ak + [alpha]G` is in the sighash, so alpha is fixed before FROST round 1; ZF's own zcash-sign / frost-client do the same | 9.4 |
| D7 | 2026-10-11 | Testnet collateral denominations: **0.01 TAZ and 0.05 TAZ** (was 0.1 / 0.5 / 1) | The only working post-NU7 faucet gives 0.125 TAZ per IP per day | 8.2, 11.2 (L3), 19 |
| D8 | 2026-10-10 | Demo "user wallet" is our own CLI wallet, not zcash-devtool / a mobile wallet | zcash-devtool, frost-tools and zingo-cli cannot send on testnet after NU7 yet | 8.1, 19 |
| D9 | 2026-10-10 | Light wallet server `https://testnet.zec.rocks:443`; fallback = Zebra regtest with Zebra's built-in lightwalletd-compatible gRPC | It is the only public endpoint confirmed on NU7 software | 13.2 |
| D10 | 2026-10-11 | GitHub repo `Himess/zecured` is **private**; it becomes public only when Semih decides (the plan's deadline is 2026-10-28) | Owner's instruction | 3.7, 24.4 |
