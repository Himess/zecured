# Mainnet

Zecured runs on Base Sepolia and Zcash Testnet. The code is meant to run on mainnet as it is: a
mainnet deployment changes configuration, and brings what code cannot: independent operators, real
bonds and liquidity, and an audit. This page says what was checked against mainnet, what the
configuration changes and why, what a deployment needs, and what is designed but not built. The
decisions are in docs/DECISIONS.md; the parties and the money are in docs/ECONOMICS.md.

## 1. Checked against mainnet

| What | How | Result |
|---|---|---|
| Base contracts on mainnet's Morpho Blue and RISC Zero router | The whole contract suite on a Base mainnet fork: `ZECURED_FORK_NETWORK=base-mainnet ZECURED_FORK_BLOCK=52688338 forge test --isolate` (from `contracts/`; `BASE_MAINNET_RPC_URL` optional) | 533 tests pass and one is skipped (its EIP-712 vectors are for chain id 84532); the invariant campaign holds. Morpho Blue, the AdaptiveCurveIRM, the 62.5 % LLTV and the router are at the same addresses as on Base Sepolia (read on-chain, 2026-10-16) |
| Real proofs on mainnet's verifier | The same suite's real Groth16 seals (a Testnet deposit, an F1 spend) | Both verify through the mainnet router (Groth16 selector `0x73c457ba`, its emergency-stop wrapper not paused): the deposit mints, the theft slashes all five bonds |
| Zcash Mainnet header rules | `config/relay-mainnet.json`, checked against Zebra v7.0.0-rc.0 and 1,600 real Mainnet headers (docs/ref/FACTS.md, D49) | The relay follows the real Mainnet chain under the Mainnet rules, and refuses headers that break them (wrong bits, minimum difficulty after a long gap, times out of bounds, a bad Equihash solution, a hash above the target) |
| Every deployment config | `test/script/DeployConfig.t.sol` reads each `config/zecured-*.json` through the deploy script's checks and the constructors' rules | `config/zecured-base-mainnet.json` passes and has the values below |
| The price feed on the live sources | `test/fork/PriceFeedBaseFork.t.sol` (needs `BASE_MAINNET_RPC_URL`), run at block 52,688,407 | Both sources fresh; `price()` equal to the oracle of Morpho's cbZEC/USDC market on the same report |
| The whole deployment, mainnet configuration | On a local anvil fork of Base mainnet: `DeployPriceFeed` over the live Chainlink and API3 feeds, `DeployRelay` with the Mainnet parameters and a real Mainnet checkpoint, `DeployZecured` with `config/zecured-base-mainnet.json`, then `BondSigners` (five bonds of 50,000 USDC) | Nine contracts on Circle USDC, mainnet's Morpho Blue (a new zZEC/USDC market) and RISC Zero router, the feed as both the market's oracle and the cap's price; at a ZEC price of 1,187.68 USD the bonds cover about 105 ZEC, so the 100 ZEC cap is the limit, and minting is open |

Not done yet: a shadow relay following Zcash Mainnet at Mainnet difficulty (it waits for NU7's
Mainnet height, to be published on 2026-10-20), and a real Mainnet deposit into a Mainnet FROST
vault (a decision for 2026-10-22).

## 2. Configuration

`contracts/config/zecured-base-sepolia.json` (the judge deployment) against
`contracts/config/zecured-base-mainnet.json` (the template). Only what differs:

| Parameter | Judge | Mainnet | Why |
|---|---|---|---|
| `loanToken` | Circle's Base Sepolia USDC | Circle USDC `0x8335…2913` | |
| `coverageBps` (k) | 10,000 | 5,000 | With independent operators the bonds should cover twice what is locked |
| `maxTotalZat` | 1.5 ZEC | 100 ZEC | Start small. The cap is immutable: raising it means a new deployment; within it, the bonds decide |
| `minBond` | 100 USDC | 50,000 USDC | Five equal bonds that cover 100 ZEC at k = 50 % and about 1,265 USD per ZEC |
| `bountyCap` | 50 USDC | 10,000 USDC | 10 % of a slash in both |
| `tierMaxAmountZat` / `tierMinWork` | one tier, 3.5 M (about ten Testnet blocks) | up to 1 / 10 / 100 ZEC: the work of 6 / 20 / 190 Mainnet blocks | Faking a deposit should cost the attacker about three times its value in mining revenue; recompute the raw work from the difficulty at deployment |
| `slashConfirmationWork` | 38 M | the work of 100 Mainnet blocks | at least `max(100 blocks, 3 x bountyCap / block value)`: a fake case gains at most the bounty |
| `finalizeDelay` | 150 s | 30 min | Time for the honest feeders to replace a slow fake branch after a feeder outage |
| `challengeWindow`, `livenessPeriod` | 24 h | 72 h | Time to notice and react, on both sides |
| `commitGrace` | 2 h | 4 h | A payout's expiry (120 blocks of 75 s) plus a reorganisation margin, before NU7 |
| `disputeWindow` / `inclusionWindow` | 24 h / 6 h | 72 h / 48 h | Within the challenge window plus the unbonding, which the deploy script checks |
| `unbondingPeriod` | 54 h | 7 days | Outlasts the challenge window, the liveness period and the commit grace |
| `incidentWindow` | 3 days | 7 days | Cases of one incident share one price |
| `capPriceMaxAge` | 1 h (demo price) | 25 h | The price feed's `maxSourceAge` (a 24-hour heartbeat plus an hour), which the deploy script requires |
| `autoPauseOnCase` | off | on | An open F1/F2 case pauses new minting |
| `journalV2` / `branchIds` | v2 / NU7 `0x77190AD9` | v2 / NU6.3 `0x37A5165B`, NU7 `0x77190AD9` | The epoch of the deposit is part of what is proven; the Testnet build proves the NU7 epoch only |
| `minRefundZat` | 20,000 | 100,000 | A refund must be worth its own payout |
| `refundAbandonPeriod` | 30 days | 2 years | |
| `minSeize` / `minSeizeBps` | 100,000 zat / 0 | 0.05 ZEC / 5 % | No liquidation small enough to grief a vault with dust claims |
| `versionDepositDelay` / `versionViolationDelay` / `versionEstopDelay` | 10 min each | 14 / 7 / 1 days | A new verifier takes effect only after users had time to see it |
| `zcashNetwork` | `zcash-testnet` | `zcash-mainnet` | The deploy script checks the relay's network |

`feeBps` (0.5 %) and the leftover sink (the burn address) are business choices; the compensator can
name a public-good address instead of burning what no victim needs.

## 3. A mainnet deployment

1. **Price feed.** `script/DeployPriceFeed.s.sol` with `config/price-feed-base.json` deploys a
   `ZecuredPriceFeed` (design notes T-10) over Chainlink's ZEC/USD Standard proxy
   (`0x69e5…6ec0`, 18 decimals, 24-hour heartbeat) and API3's ZEC/USD reader proxy (`0x24F2…Ec15`),
   each fresh for 25 hours, agreeing within 5 %. The Standard proxy rather than the SVR-Backup one
   Morpho's cbZEC market reads: the same aggregator, without the liquidation auction's delay. The
   feed is both the market's oracle and the cap's price: export `BASE_ORACLE` (and `CAP_PRICE`, which
   defaults to it); the deploy script checks its primary source, its secondary, that it is not stale
   and that `capPriceMaxAge` is its `maxSourceAge`, and never puts the DemoShockOracle (a testnet demo
   control) in front of the market. A keeper calls `poke()` whenever `state()` differs from
   `recordedState()`, so that the last good price is recorded. API3's plan on Base ends on 2026-12-20:
   renew it, or the feed runs on Chainlink alone.
2. **Relay.** Set `nu7ActivationHeight` in `config/relay-mainnet.json` once NU7's Mainnet height is
   assigned (it is `type(uint32).max` until then, which no chain crosses), deploy
   `ZcashHeaderRelay` with it, and run at least two independent feeders. A Mainnet header costs
   about 600,000 gas, most of it the Equihash check (`test/relay/RelayCostMainnet.t.sol`): on a Base
   mainnet fork at block 52,702,230 (base fee 0.005 gwei) that is about 3.0e-6 ETH of L2 execution
   and 8e-9 ETH of L1 data per header. A batch saves only the per-transaction part and is cheapest
   at 4 to 8 headers (597,000 gas per header at 4, 645,000 at 16, where memory grows). At NU7's
   25-second blocks the relay takes 3,456 headers a day, about 0.010 ETH a day at that base fee
   (0.0035 ETH before NU7). The two feeders should not race for the same headers: one feeds, the
   other takes over when the relay's tip stalls.
3. **Guests.** The `guests` workflow builds the deposit and violation guests for Mainnet; their image
   ids become the initial versions (`DEPOSIT_IMAGE_ID`, `VIOLATION_IMAGE_ID`).
4. **Contracts.** `DEPLOYMENT_NAME=base-mainnet forge script script/DeployZecured.s.sol --broadcast`:
   nine contracts at predicted addresses, whose constructors check the wiring.
5. **Bonds.** Each operator bonds from its own key (`script/BondSigners.s.sol`); nothing is minted
   before all five are in.
6. **Operators.** Five independently run signer nodes (docs/OPERATORS.md), the coordinator, the
   liquidator and the market maker, with lenders supplying the market.
7. **An audit** of the contracts, the guests and the signer policy.

## 4. Designed, not built

- **Adapter registry (T-07b).** If Circle blacklisted the SettlementAdapter, liquidations and closes
  through it would stop, and the remedy today is a migration. The design adds adapters to the
  version registry: all five signers schedule a new adapter, which zZEC and the Registry accept
  after a public delay; three signers remove one at once, never the last.
- **Confirmation by proof (T-05 (b)).** `confirmPayout` would require a proof of the payout's
  inclusion (the violation guest's `Included` kind) instead of three signatures: one GPU proof (about
  a minute or two) and about 350,000 gas per payout, and a confirmed payout would no longer be the
  signers' word. The dispute path (a), deployed, makes a false confirmation answerable instead.
- **A dispute bond, a launch condition.** On the judge deployment a claimant disputes a
  confirmation for free, once per confirmation. Mainnet launches with a small bond on each dispute,
  returned when the dispute reopens the claim and paid to the prover when an inclusion proof upholds
  the confirmation. Routine inclusion proofs of every payout make disputes rare.
- **Reimbursement per signer share.** The bonds pay an overdue claim pro rata, but its ZEC goes
  back to one destination the signers give together (all five of them on the judge deployment).
  Mainnet splits it: each signer registers its own reimbursement destination, and the claim's ZEC
  is paid back per share, one output per signer. Every commitment of a claim's input carries the
  same claim set (RT-4), so the split must be fixed when the bonds pay, as per-signer sub-claims of
  that input, not chosen at payout time.
- **Vault pool (T-12).** Vaults made ahead of time (`VaultPooled`, signed by all five) and assigned
  to a user by a second message (`VaultAssigned`), also signed by all five, so that opening a vault
  is immediate and the DKG leaves the user's path. Assignment must take all five: with three, three
  colluders could assign one pool vault to a user and to themselves and land the user's deposit
  first.
- **Price feed risks.** When the two sources disagree by more than 5 %, the market takes the lower
  price and the cap the higher: either source can pull liquidations forward, never mint more. The
  feed does not read Chainlink's L2 sequencer uptime feed: mainnet should treat the price as stale
  while Base's sequencer is down and for a grace period after it restarts, so that nothing is
  liquidated on prices users could not react to. While the feed is stale the market keeps the last
  good price: Morpho goes on lending against it, new minting stops at the cap and the bonds pay no
  liveness claim. A haircut that grows with staleness is an option to evaluate during the beta.
- **Swap: a maker stake and a verifiable delivery of the destination.** On the judge deployment a
  maker that never pays costs its taker only time: the USDC comes back after the pay window. A
  mainnet maker would stake USDC, slashable for an order it did not pay although it had received the
  order's destination. That needs the delivery to be provable without putting the receiver on
  Base: either the maker signs a receipt when it gets the salt and receiver (as the signers do for
  liveness), or the taker posts them encrypted to the maker's key with a proof that they open the
  order's commitment.
- **Share refresh and repair through the nodes.** The threshold library refreshes every share of
  a vault and repairs a lost one, but no node runs either yet (docs/TIER3.md § 5). Mainnet needs
  both as node rounds, their private messages sealed node to node as in the DKG, and a runbook: a
  refresh after any suspected share leak, a repair when a signer loses its share.
- **Fee schedule.** ZIP 317's marginal fee (5,000 zatoshi) and grace actions (2) are constants of
  the spend logic; a change of Zcash's fee rules would take a new deployment.

## 5. Mainnet beta

A capped, invite-only beta on Base mainnet and Zcash Mainnet, unaudited, and labelled so in the
app: "Mainnet beta — unaudited — invite only — small caps".

- **Invitations.** Vault openings (the coordinator's `POST /vaults`) and swap quotes need an invite
  code. Only the codes' hashes are stored, their uses are counted, and one switch closes new
  openings; exits and payouts never stop. No contract change.
- **Configuration** (`config/zecured-base-mainnet-beta.json`, derived from the mainnet template and
  checked by the deploy script): `maxTotalZat` 1 ZEC, vaults of 0.05 and 0.1 ZEC, `minBond` 300
  USDC, `coverageBps` 5,000, `feeBps` 50, the template's windows and work tiers, `bountyCap` 100
  USDC, `refundDormancy` 7 days.
- **Relay:** NU7's Mainnet height (to be assigned on 2026-10-20) in `config/relay-mainnet.json`, a
  checkpoint near the tip, two feeders (§ 3).
- **Price:** `ZecuredPriceFeed` over Chainlink and API3; API3's plan runs until 2026-12-20, past
  the judging period.
- **Guests:** deposit-main and violation-main v2 with real seals before the beta; payment-main
  before the swap opens on mainnet.
- **Swap:** a 24-hour pay window, finalization after 1,800 seconds and 6 headers, the Intake's
  mainnet work tiers, the NU6.3 and NU7 branch ids, `maxOrderUsdc` 250, no fee, and a 14-day delay
  for a new Swap version.
- **Operations:** a second stack with its own data directories and units, two independent
  Mainnet light wallet servers, the watchdog on both stacks, one prover and queue for both.

**NU7 activation (about 2026-11-05).** Before it: the relay's activation height, spacing (25 s)
and averaging window (102 blocks from NU7); ZIP 221's history tree restarting at the activation, so
a proof's leaf and peaks are of its own epoch; the guests' branch ids (deposit-main proves both
NU6.3 and NU7). During it the activation window is watched; if anything is off, the operators
pause new minting with `zecured-ops`, and exits go on.

## 6. What stays true on mainnet

The bonds cover what the cap admits, not more; five colluding signers can take every vault, and a
new verifier version can only follow their public delay; proof of work protects a deposit proof
only by the work it demands; and someone must build each witness and run the prover. docs/ECONOMICS.md
§ "What the bonds do not cover" says the same in numbers.
