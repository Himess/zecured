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

## Zcash header rules by network (header relay parameters)

Z = Zebra `v7.0.0-rc.0` (commit `6d1e414d6f55e4180d0e47baaa934bf97d5b4fec`), paths under
https://github.com/ZcashFoundation/zebra/blob/v7.0.0-rc.0/. ZIPs and the protocol specification: https://github.com/zcash/zips
at commit `929a92f7383e036b6f6a48f43ec88c5bdd130003` (2026-10-14, the tip of `main` on 2026-10-15). `contracts/config/relay-<network>.json` holds these values; `ZcashHeaderRelay.RelayParams` repeats the sources.

| Fact | Value | Source | Verified |
|---|---|---|---|
| PoWLimit, Mainnet | 2^243 − 1, used rounded through its compact form: nBits `0x1f07ffff`, target `0x07ffff` × 2^224 | Z `zebra-chain/src/work/difficulty.rs` (`ParameterDifficulty for Network::target_difficulty_limit`); specification § 5.3 `PoWLimit` (`protocol/protocol.tex`) | 2026-10-15 |
| PoWLimit, Testnet | 2^251 − 1, rounded: nBits `0x2007ffff`, target `0x07ffff` × 2^232 | Z `zebra-chain/src/parameters/network/testnet.rs` (`ParametersBuilder::default`); specification § 5.3 | 2026-10-15 |
| Max time rule (time ≤ median-time-past + 90 min) | Mainnet: every height in Zebra (the specification: height ≥ 2); Testnet: height ≥ 653,606 | Z `zebra-chain/src/parameters/network.rs` (`is_max_block_time_enforced`: `Network::Mainnet => true`), `network_upgrade.rs` (`TESTNET_MAX_TIME_START_HEIGHT`), `zebra-state/src/service/check.rs` (`difficulty_threshold_and_time_are_valid`); specification § 7.6 | 2026-10-15 |
| Minimum-difficulty blocks | Testnet only, from height 299,188: a block more than 6 × PoWTargetSpacing (18 × from NU7; 450 s either way after Blossom) after its parent carries ToCompact(PoWLimit). Never on Mainnet | Z `network_upgrade.rs` (`minimum_difficulty_spacing_for_height` returns None for Mainnet; `TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT`), `zebra-state/src/service/check/difficulty.rs` (`expected_difficulty_threshold` asserts a test network); specification § 7.7.3 ("not apply to Mainnet"); ZIP 218 § "Minimum difficulty blocks on Testnet" | 2026-10-15 |
| Blossom activation (75 s spacing from here) | Mainnet 653,600; Testnet 584,000 | Z `zebra-chain/src/parameters/constants.rs` (`activation_heights`); specification § 5.3 | 2026-10-15 |
| Spacing and averaging window before NU7 | 75 s, 17 blocks (both networks; 150 s before Blossom) | Z `network_upgrade.rs` (`POST_BLOSSOM_POW_TARGET_SPACING`, `POW_AVERAGING_WINDOW`); specification § 5.3 | 2026-10-15 |
| Spacing and averaging window from NU7 | 25 s, 102 blocks (`PostNU7PoWTargetSpacing`, `PostNU7PoWAveragingWindow`); chosen by the candidate block's height | ZIP 218 § "Block target spacing", § "Averaging window"; Z `network_upgrade.rs` (`POST_NU7_POW_TARGET_SPACING`, `POST_NU7_POW_AVERAGING_WINDOW`, `averaging_window_for_height`) | 2026-10-15 |
| NU7 Mainnet activation height | **Not assigned yet**: absent from Zebra v7.0.0-rc.0 and from Zebra `main` at `6cc6d360e51fe07e661c26f02230e06d81b468c7` (committed 2026-10-14, checked 2026-10-15; every file above is identical there to v7.0.0-rc.0, and rc.0 is the latest v7 tag); ZIP 259 says "TBD (To be set on OCT 20)" and requires a multiple of 3. The Mainnet relay configuration uses `type(uint32).max` until then | Z `zebra-chain/src/parameters/constants.rs`; ZIP 259 (`zips/zip-0259.md`, § "NU7 network constants") | 2026-10-15 |
| Rules that do not depend on the network | median of 11 times, damping 4, bounds +16 % / −32 %, 90 min, the 2-hour clock rule, Equihash (200, 9), version ≥ 4 | Z `zebra-state/src/service/check/difficulty.rs` (constants), `zebra-chain/src/block/header.rs` (`time_is_valid_at`), `zebra-consensus/src/block/check.rs`; specification § 7.6, § 7.7 | 2026-10-15 |
| Real Mainnet data | 1,600 headers 3,497,446 .. 3,499,045 (P2P; SHA256d and `hashPrevBlock` link them to the hashes Zebra v7.0.0-rc.0's `zebra-chain/src/parameters/checkpoint/main-checkpoints.txt` lists at 3,497,445, 3,497,845, 3,498,245, 3,498,645, 3,499,045). With the Mainnet parameters the Python model passes every time, nBits, Equihash and PoW check of the 1,572 headers with their 28-block context. Of the 1,487 with a full 113-block context: a 16-block window reproduces 2 nBits, an 18- or 102-block window or a 25 s spacing none; the Testnet minimum-difficulty rule would refuse exactly the 7 blocks that came 451 s to 762 s after their parents. PoWLimit is not exercised (targets ~1.8 × 10^8 below it) | `contracts/test/relay/tools/zcash_header_check.py chain --network mainnet --equihash`; the variants with the same model's functions | 2026-10-15 |

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

## Zcash infrastructure (mainnet)

| Fact | Value | Source | Verified |
|---|---|---|---|
| P2P network | magic `24e92764`, port 8233; DNS seeders `dnsseed.str4d.xyz`, `dnsseed.z.cash`, `mainnet.seeder.shieldedinfra.net`, `mainnet.seeder.zfnd.org`, `seeder.zec.rocks` | Zebra v7.0.0-rc.0 `zebra-chain/src/parameters/constants.rs` (`magics`), `zebra-network/src/config.rs` (`DEFAULT_MAINNET_PEERS`) | 2026-10-15 |
| P2P protocol versions | Mainnet peers require ≥ 170,160 (NU6.3); NU7 uses 170,190 on Mainnet and 170,180 on Testnet; Zebra 7.0.0-rc.0 advertises 170,180. Mainnet `getheaders` was answered only by `/Zakura:1.5.0/` and `/Zakura:1.6.0/` peers (protocol 170,190); the Zebra peers tried (4.3.0, 5.0.0, 5.1.0, 6.2.3, 6.4.2) closed the connection on it and a `/MagicBean:6.20.0/` (zcashd) peer timed out | `zebra-network/src/protocol/external/types.rs`, `zebra-network/src/constants.rs`; ZIP 259; `tools/zcash_p2p_headers.py --network mainnet` | 2026-10-15 |
| Mainnet block API | CipherScan `https://api.mainnet.cipherscan.app/api/block/<height>` (same fields as the Testnet API) | probe; `tools/build_fixture.py --network mainnet --cipherscan` | 2026-10-15 |
| Light wallet servers' chain name | `GetLightdInfo.chain_name` is `main` on Mainnet (`https://zec.rocks:443` and `https://na.zec.rocks:443`: ECC lightwalletd v0.5.3, branch `0x37a5165b`, height 3,523,050) and `test` on Testnet (`https://testnet.zec.rocks:443`: v0.5.4, branch `0x77190ad9`, height 4,518,055); the stack refuses a server whose chain is not its network's (D81) | `zecured-spike info` | 2026-10-19 |

## Base Sepolia (84532)

| Fact | Value | Source | Verified |
|---|---|---|---|
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | docs.morpho.org addresses + on-chain code | 2026-10-10 |
| AdaptiveCurveIRM | `0x46415998764C29aB2a25CbeA6254146D50D22687` (`isIrmEnabled` = true) | docs.morpho.org + on-chain | 2026-10-10 |
| LLTV 62.5% (`625000000000000000`) | enabled | `isLltvEnabled` on-chain | 2026-10-10 |
| Other enabled LLTVs | 0, 38.5, 77, 86, 91.5, 94.5, 96.5, 98 % | on-chain | 2026-10-10 |
| EntryPoint v0.7 / v0.8 | `0x0000000071727De22E5E9d8BAf0edAc6f37da032` / `0x4337084D9E255Ff0702461CF8895CE9E3b5Ff108` (code present) | on-chain | 2026-10-10 |

## Base mainnet (8453): ZEC/USD price sources (ZecuredPriceFeed, design notes T-10)

"On-chain" = read-only `eth_call`s on `https://mainnet.base.org` at blocks 52,687,626 to 52,688,407 (2026-10-16, about
15:30 UTC). CL-RDD = Chainlink's reference data directory for Base,
`https://reference-data-directory.vercel.app/feeds-ethereum-mainnet-base-1.json`, the data the Feed Addresses page of
docs.chain.link shows (data.chain.link refuses automated reads). `contracts/config/price-feed-base.json` holds the deploy
values; `test/fork/PriceFeedBaseFork.t.sol` checks them against a Base mainnet fork.

| Fact | Value | Source | Verified |
|---|---|---|---|
| Chainlink ZEC / USD: Standard proxy | `0x69e5BC4988a9AF30Ec827C5609c0D41028446ec0` (phase 1 = the DualAggregator below). ZecuredPriceFeed's primary | CL-RDD entry `zec-usd-shared-svr`, `proxyAddress`; the docs label `proxyAddress` "Standard Proxy" and `secondaryProxyAddress` "SVR-Backup Proxy" for a `-shared-svr` feed on a network that also has `-shared-svr-2` feeds, as Base does (smartcontractkit/documentation `src/features/feeds/components/Tables.tsx`, `src/features/feeds/utils/svrDetection.ts`); on-chain `aggregator()` | 2026-10-16 |
| Chainlink ZEC / USD: SVR-Backup proxy | `0xC4a161651546983083AeDDDEc5878A2eAf604890` (phase 2, same DualAggregator); Morpho's cbZEC/USDC market reads this one | CL-RDD `secondaryProxyAddress`; on-chain | 2026-10-16 |
| SVR | On by default for some feeds on Base; "Protocols can also opt out at any time by reading from the 'Standard' proxy"; when the private route fails, the SVR feed falls back to the standard price after a configurable delay | https://docs.chain.link/data-feeds/svr-feeds | 2026-10-16 |
| Chainlink aggregator | `0xb00e68fb3754EE8CC7B5F61348a2f09d53fB2e0e`, `typeAndVersion` "DualAggregator 1.0.0"; read access checked (`checkEnabled` true, `hasAccess` true for both proxies, false for another contract): contracts read a proxy, whose `accessController` is zero | CL-RDD `contractAddress`; on-chain | 2026-10-16 |
| Chainlink ZEC / USD parameters | 18 decimals, description "ZEC / USD", heartbeat 86,400 s, deviation threshold 0.5 %, market risk category "medium", answer bounds 1 to 95,780,971,304,118,053,647,396,689,196,894,323,976,171,195,136,475,135 (no practical clamp) | CL-RDD (`decimals`, `heartbeat`, `threshold`, `feedCategory`, `maxSubmissionValue`); on-chain `decimals`, `description`, `minAnswer`, `maxAnswer` | 2026-10-16 |
| Chainlink ZEC / USD sample | answer 1,188,013,688,814,189,200,000 (USD 1,188.01), `updatedAt` 1,792,164,439, 160 s before block 52,687,626 | on-chain `latestRoundData` | 2026-10-16 |
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | on-chain | 2026-10-16 |
| Morpho cbZEC/USDC market, 62.5 % LLTV (about USD 2.5 M supplied; the other two cbZEC markets on Base hold under USD 20,000) | id `0xe458a501c1103ca3b256f2bcb45d223263fb34b6349d82675494ce3a94ce1327`: loan USDC `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`, collateral cbZEC `0xB2000000000000000000008501b13360000cb2EC` (8 decimals), oracle `0xa3CB2d7204F47a468B82C6fAb1CA9675e9F7A272`, IRM `0x46415998764C29aB2a25CbeA6254146D50D22687`, LLTV 62.5 %. The oracle is a MorphoChainlinkOracleV2: `BASE_FEED_1` = the SVR-Backup proxy, no other feed, `SCALE_FACTOR` 1e16 = 10^(36 + 6 − 8 − 18); its `price()` is the answer × 1e16, exactly what ZecuredPriceFeed computes | Morpho API `https://blue-api.morpho.org/graphql` (markets on chain 8453 matching "ZEC"); on-chain `idToMarketParams`, `BASE_FEED_1`, `SCALE_FACTOR`, `price` | 2026-10-16 |
| API3 ZEC/USD dAPI | stage "active"; providers blocksize, coingecko, coinmetrics, coinpaprika, ncfx, nodary, shiftmarkets; Base (`base`) an active chain | `@api3/dapi-management` 4.41.0, `dist/data/dapis.json` and `dist/data/chains.json` (https://unpkg.com/@api3/dapi-management@4.41.0/) | 2026-10-16 |
| API3 contracts on Base | Api3ServerV1 `0x709944a48cAf83535e43471680fDA4905FB3920a`, Api3ServerV1OevExtension `0x2E787629c3B7545271240A87197108823ce8303b`, Api3ReaderProxyV1Factory `0x1Fe170311Cb13e3bfB96DeC8FAadcf97BF843381`, Api3MarketV2 `0x3f5C77BB36a16118ccC9Ca83ddEe8A01b6C01811`, AirseekerRegistry `0x7B42df2563E128Ae3F68e2CFB1904808F61C8F12` | `@api3/contracts` 42.0.1, `dist/deployments/addresses.json`; code on-chain | 2026-10-16 |
| API3 ZEC/USD reader proxy (Chainlink-compatible) | `0x24F223087c9a85b1eF998a53744fdDd0c026Ec15`, the communal Api3ReaderProxyV1 (an ERC1967 proxy API3 can upgrade; implementation `0xcE4C817Ae58CeF979a91793f549B0F46468fe5d5`): `decimals` 18, `version` 4913, `dapiName` "ZEC/USD", `description` "", `latestRoundData` with roundId 0 and `updatedAt` = when the oracles signed the data. ZecuredPriceFeed's secondary | address from `computeCommunalApi3ReaderProxyV1Address(8453, "ZEC/USD")` (`@api3/contracts` 42.0.1 `dist/src/proxy.js`), recomputed with `cast`; code and values on-chain; behaviour from `api3-server-v1/proxies/Api3ReaderProxyV1.sol` in the same package | 2026-10-16 |
| API3 ZEC/USD on Base: update parameters and plan | 2.5 % deviation (2,500,000 where 1e8 = 100 %), 86,400 s heartbeat; the current plan ends at 1,797,786,413 = 2026-12-20 17:06:53 UTC and nothing is queued after it (API3 Market offers 5, 2.5, 1, 0.5 or 0.25 % and a 24 h heartbeat only, heartbeat updates come 2 minutes early) | on-chain AirseekerRegistry `dapiNameToUpdateParameters`, Api3MarketV2 `dapiNameToCurrentSubscriptionId`, `subscriptions`; https://docs.api3.org/llms-full.txt (update parameters) | 2026-10-16 |
| API3 ZEC/USD sample | answer 1,187,679,000,000,000,000,000 (USD 1,187.68), `updatedAt` 1,792,164,131 | on-chain `read`, `latestRoundData` | 2026-10-16 |
| Pyth ZEC/USD | feed id `0xbe9b59d178f0d6a97ab4c343bff2aa69caa1eaae3e9048a65788c529b125bb24` ("Crypto.ZEC/USD") | https://hermes.pyth.network/v2/price_feeds?query=ZEC&asset_type=crypto | 2026-10-16 |
| Pyth on Base | **Not usable as the secondary today**: ZEC/USD is not pushed on Base. Contract `0x8250f4aF4B972684F7b336503E2D6dFeDeB1487a` (version 1.4.6): last ZEC/USD update 2026-09-04 19:29:54 UTC; the upgraded Pyth Core contract the docs list for Base, `0xbC16aee60f64864882BC6C4E428e148Fc0E272F5` (version 1.4.5-alpha.1): 2026-08-21 10:23:25 UTC; ETH/USD minutes old on both. No deployed `PythAggregatorV3` for ZEC/USD was found: Pyth as a source needs that adapter deployed and someone pushing updates | on-chain `getPriceUnsafe`, `version`; https://docs.pyth.network/price-feeds/core/upgrade/contracts | 2026-10-16 |
| RedStone on Base | No ZEC feed: RedStone's Base push relayer manifest lists 29 feeds, none for ZEC | `packages/relayer-remote-config/main/relayer-manifests-multi-feed/baseMultiFeed.json` in https://github.com/redstone-finance/redstone-oracles-monorepo (main) | 2026-10-16 |

## Base mainnet (8453): the contracts Zecured uses

Read with `eth_call` on `https://mainnet.base.org` on 2026-10-16 (block about 52,688,000).

| Fact | Value | Verified |
|---|---|---|
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` (same address as Base Sepolia) | 2026-10-16 |
| AdaptiveCurveIRM | `0x46415998764C29aB2a25CbeA6254146D50D22687`, `isIrmEnabled` true | 2026-10-16 |
| LLTV 62.5 % | `isLltvEnabled(625000000000000000)` true | 2026-10-16 |
| Circle USDC | `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`, 6 decimals | 2026-10-16 |
| RISC Zero verifier router | `0x0b144E07A0826182B6b59788c34b32Bfa86Fb711` (same address as Base Sepolia); `getVerifier(0x73c457ba)` = `0x9F9994Eb4Cb5200198FEfb470f8b50301662e696`, an emergency-stop wrapper, `paused()` false. On Base Sepolia the same selector's wrapper is `0x724d375B5b622e15f0E64e9Deb76a4cB17877797` around the Groth16 verifier `0x2a098988600d87650Fb061FfAff08B97149Fa84D` | 2026-10-16 |

## Open (⚠️)
- Paymaster provider (Pimlico vs CDP) and smart account (Safe vs Kernel) — Spike C.
- ⚠️ API3's ZEC/USD plan on Base ends 2026-12-20 17:06:53 UTC: a mainnet deployment renews it (or buys its own) or runs ZecuredPriceFeed PrimaryOnly after that date.
