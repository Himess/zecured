# Zecured app

Next.js front end of Zecured: **borrow against shielded ZEC. Only the proof crosses.**
Testnet only (Zcash testnet TAZ, Base Sepolia, MockUSDC). Not a financial product.

There is no browser wallet. For each loan the app generates a fresh owner key in the browser,
derives a 1-owner Safe smart account from it, and sends every Base action as an ERC-4337
UserOp sponsored by our paymaster. The browser talks to exactly one server besides itself:
our backend (`NEXT_PUBLIC_BACKEND_URL`), which serves the coordinator API and proxies Base RPC,
bundler and paymaster calls (docs/INTERFACES.md § 4).

## Quick start

```bash
cd app
pnpm install          # exact versions, every package >= 14 days old (pnpm-workspace.yaml)
cp .env.example .env.local   # then pin the deployment (below); `pnpm dev` also runs unpinned
pnpm dev              # http://localhost:3000
pnpm test             # vitest, offline
pnpm build            # production build (includes the TypeScript check)
pnpm typecheck        # tsc --noEmit (app + tests)
```

Requires Node.js ≥ 20.9 and pnpm 10. The backend is optional for browsing: every page renders
without it and says what is unavailable.

## The judges' replay (DEMO)

No server of ours runs through the judging (D82), so the app also builds as a **replay**: the same pages as a
static site, in which an in-browser simulator (`lib/replay/`) plays the backend, the Base reads, the loan account and
the swap's wallet from a recording (`lib/replay/recording.json`, schema `zecured-replay/1`). The judge presses the
real buttons (open a vault, pay it with demo TAZ, watch its proof land, borrow, close from collateral, buy shielded
ZEC); each action must be the recording's next step. A dock at the bottom of every page says what to do next and
where: "Show me" outlines the control on the page (`components/replay/spotlight.ts`), "Go there" opens its page and
outlines it there. An action the run did not take is answered by a DEMO note naming the next step, never an error.
Every page carries a DEMO band.

```bash
node scripts/build-replay.mjs --dev --port 3001            # next dev in replay mode
NEXT_PUBLIC_BASE_PATH=/zecured/app node scripts/build-replay.mjs   # static export to out/
NEXT_PUBLIC_BASE_PATH=/zecured/app node scripts/check-export.mjs   # its assets, payloads, policy and budget
```

On a machine short of memory, `ZECURED_TYPECHECKED=1` (after `pnpm typecheck`) and `ZECURED_BUILD_CPUS=1` (one
static-generation worker) keep the build within a few hundred megabytes.

The replay never publishes a private key, a salt or a viewing key: its loan carries a dummy key that controls
nothing, the viewing-key route answers `replay_unavailable`, and a destination the judge types stays in the browser.
Every hook point tests the replay flag (`lib/replay/flag.ts`, `NEXT_PUBLIC_REPLAY=1`); a live build, where the
variable is unset, never takes a replay path at runtime (`lib/replay/hooks.test.ts`), although the simulator's code
still ships in its bundle. The recording is our live run of 27 October on Base Sepolia and Zcash testnet
(`lib/replay/recording.json`, harvested by `lib/replay/harvest.ts` from the run's snapshots and from Base): its real
transaction hashes, each on-chain step linked to its explorer, and the real signers' signatures of its vault. The
placeholder (`lib/replay/placeholder.ts`: dummy hashes, no explorer links, five DEMO signer keys) stays for the tests.

## Configuration

Every address the app signs for or sends calls to is **pinned in the build** (security review
H-6): a compromised backend must not be able to point the account's Morpho authorization or a
USDC approval at its own contract. The one exception is the signer bond, which the pinned
Registry names on-chain (`signerBond()`, immutable; the Intake must name the same one): the
account only ever calls its `slashLiveness` (T-05), which approves nothing and moves nothing out
of the account. The backend's `GET /config` is only checked against the pins; it supplies the
values that carry no authority.

| Variable | Default | Purpose |
|---|---|---|
| `NEXT_PUBLIC_BACKEND_URL` | `http://127.0.0.1:8080` | Coordinator API and `/proxy/{rpc,bundler,paymaster}`. Also the only extra origin allowed by the CSP `connect-src`. |
| `NEXT_PUBLIC_REGISTRY` | none (pin) | `ZecuredRegistry`; EIP-712 `verifyingContract` of every owner request; also serves the dispute path (`IZecuredDisputes`) through its fallback. |
| `NEXT_PUBLIC_ADAPTER` | none (pin) | `SettlementAdapter`; EIP-712 domain of `CloseRequest`. |
| `NEXT_PUBLIC_INTAKE` | none (pin) | `ZecuredIntake` (v3): pending activations, and the account's `refundPending`. Must be the Intake the Registry names. |
| `NEXT_PUBLIC_MARKET_ID` | none (pin) | Morpho market id; must equal `marketIdOf` of the pinned market parameters. |
| `NEXT_PUBLIC_USDC` | none (pin) | The loan token: on testnet a MockUSDC we mint (MOCK: no value), on a local fork and on Base Sepolia alike; on mainnet Circle's USDC. |
| `NEXT_PUBLIC_ORACLE` | none (pin) | The market's oracle (price = ZEC/USD × 1e34): on testnets the DemoShockOracle (MOCK / DEMO) around a MockZecOracle (MOCK). |
| `NEXT_PUBLIC_ZZEC` | none (pin) | zZEC (collateral token). |
| `NEXT_PUBLIC_SIGNERS` | none (pin) | The five signer addresses, comma-separated (to verify signer signatures in the app). |
| `NEXT_PUBLIC_MORPHO` | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | Morpho Blue on Base Sepolia (the default is a pin too). |
| `NEXT_PUBLIC_IRM` | `0x46415998764C29aB2a25CbeA6254146D50D22687` | AdaptiveCurveIRM (the default is a pin too). |
| `NEXT_PUBLIC_ZECURED_DEPLOYMENT` | `testnet` | v4: `testnet` (Base Sepolia, Zcash testnet, TAZ) or `mainnet-beta` (Base 8453, Zcash mainnet, ZEC): the band "Mainnet beta — unaudited — invite only — small caps" on every page, an invite code sent with `POST /vaults`, the demo path off, the evidence page's mainnet section first. Any other value keeps Base disabled. |
| `NEXT_PUBLIC_SWAP` | none | v4: ZecuredSwap, pinned like the other contracts (the user's own wallet sends `open` and `refund` to it); empty: the Buy page says the build has no swap. |
| `NEXT_PUBLIC_DEMO_PATH` | off | v4, P-1 (DEMO, testnet only): `1` shows "Pay it with demo TAZ" while the backend's demo wallet is on (`GET /demo/status`). Ignored on the mainnet beta. |

All values are public (inlined into the bundle at build time). Rebuild after changing them.

- **Production build** (`next build` / `next start`): a missing pin keeps Base reads and
  sponsored transactions disabled (and owner signatures too while the Registry is unpinned).
- **Development** (`next dev`): unpinned values are taken from `GET /config`, and every page
  shows an "unpinned development configuration" warning.
- **Any disagreement** between a pin and `GET /config` (or a backend inconsistent with itself,
  another chain, another LLTV) is a hard stop: owner signatures, Base reads and sponsored
  transactions are disabled, and the reason is shown.
- From `GET /config` at runtime: `sponsorship` (bundler, paymaster and allowlist configured;
  without it `loanAccount().send()` refuses before signing), the collateral denominations, the
  confirmations, the deposit window (`vaultExpiryBlocks`) and the deployment's public v3
  parameters (`v3`: the opening fee shown before a deposit, the refund abandon period, the
  pending-refund delay and the liveness period the pages explain). They carry no authority: the
  contracts enforce their own.
- From Base, through the pinned Registry and Intake (`lib/chain.ts` `readLinks`, once per
  session): the slasher, the compensator and the relay the app only reads, the bond (whose
  `slashLiveness` the account calls), the dispute and inclusion windows. A Registry that names
  another Intake than the pinned one, or another bond than the Intake's, is refused.
- From `GET /status`, every minute: the minting gates (paused, halted, bonds short, the coverage
  cap's room per denomination), shown as a banner on every page.

`components/ConfigGate.tsx` (in the root layout) fetches `GET /config` once before any page
renders, with a compact loading state; after 6 s without an answer the page renders with the
build's configuration. Configuration problems and warnings are listed above every page.

### Pinning a deployment

```bash
jq -r '"NEXT_PUBLIC_REGISTRY=\(.contracts.ZecuredRegistry)
NEXT_PUBLIC_ADAPTER=\(.contracts.SettlementAdapter)
NEXT_PUBLIC_INTAKE=\(.contracts.ZecuredIntake)
NEXT_PUBLIC_MARKET_ID=\(.market.marketId)
NEXT_PUBLIC_USDC=\(.market.loanToken)
NEXT_PUBLIC_ORACLE=\(.market.oracle)
NEXT_PUBLIC_ZZEC=\(.contracts.ZZEC)
NEXT_PUBLIC_MORPHO=\(.market.morphoBlue)
NEXT_PUBLIC_IRM=\(.market.irm)
NEXT_PUBLIC_SIGNERS=\(.signers | join(","))"' ../contracts/deployments/anvil.json >> .env.local
```

(`base-sepolia.json`: the Base Sepolia deployment of 27 October.) Or, with the swap contract and the demo path too (testnet only), without
jq: `node scripts/pin-deployment.mjs base-sepolia http://127.0.0.1:8080 > .env.local`. Review the result before
building: these addresses are what the app trusts.

## Pages

| Route | What it does |
|---|---|
| `/` | Pitch, how it works in three steps, "what the world sees" preview. |
| `/prepare` | Why Ironwood only (pool crossings reveal amounts at the turnstile), migration advice, what counts as a valid deposit, refunds. |
| `/borrow` | Wizard: amount (the signers' denominations from `GET /config`, with the opening fee when the deployment charges one; a size whose deposit would only come back as a refund now, halted or over the coverage cap, cannot be opened) → fresh key + Safe address, `OpenVault` signature, `POST /vaults` → key backup → Ironwood-only vault address signed by all five signers (checked in the browser), ZIP 321 QR, deadline, live status (seen, proven on Base and waiting to be activated, active) → borrow up to 40 % LTV. |
| `/loans` | Loan keys held in this browser: export, import (validated against the derived account), forget. |
| `/position#<vaultId>` | Collateral, debt (with accrued interest), health factor, liquidation price; a pending activation (readiness height and time, what holds it, turning it into a refund after a long pause or a halt); refunds; every claim in plain words, with "My payout did not arrive" (a dispute) within the dispute window and a reopening once the signers' time to prove the payout passed, and for an overdue claim of the account what the signers' bonds would pay for it now, with "Claim from the bonds"; a frozen or compromised vault, and what the compensator owes it (read-only); exits: close from collateral, repay + release, release after liquidation; withdraw part of the ZEC (D14); send USDC; borrow more. |
| `/verify#<vaultId>` | Owner-signed `ViewKeyRequest` → the vault's UFVK (never stored), what it shows and hides, and the `zecured-wallet watch` command from the vault's birthday height. |
| `/refund#<vaultId>` | The vault's Refund claims on Base (why each landed, its destination step, reviving an abandoned one), its pending refunds (R-12: proven, their claims not open yet) and its pending activation. There is no refund outside Base. |
| `/world` | Everyone / signer network / you, with live Registry events, market totals and the signers' view of your vaults. |
| `/buy` | v4, Zecured Swap (Buy side; Sell stays on the command line): a quote from the maker through the backend, the user's own wallet approves and opens the order, the destination (an Ironwood address and salt behind the order's commitment) goes to the maker through the backend, the order is followed from Base events until its payment is proven, and refunded after its deadline otherwise. |
| `/market` | v4, P-4: supplied, borrowed, utilization, rates, the locked collateral, the signers' bonds against it and the compensation order; optional lending from the user's own wallet (EIP-1193). |
| `/status` | v4, P-2: the signers' modes, the Zcash tip against the header relay, the deposit-proof queue, the minting gates, the coverage cap's fill, ΣB and B_eff, the anonymity set of each vault size, the proof versions and the running commit. |
| `/evidence` | v4, P-3: the backend's evidence file (`zecured-evidence/1`, lib/evidence.ts): the manifest, every flow's transactions linked to the explorers, the leak scan, the measured durations. |
| `/demo` | Admin-token panel: node modes, the sweep (threshold) demo, the MOCK oracle price (presets $1,265 and $632.50); bot status is a placeholder. |

The vault id travels in the URL **fragment**, never in the path or the query (security review
M-6): browsers do not send fragments to the server, so the app host's logs and prefetches never
tie an IP to a vault. Links to vault pages also set `prefetch={false}`. Old
`/position/<vaultId>` links no longer exist.

Every exit first asks for the Ironwood destination, salts it, signs `Destination` and has the
signers pre-validate it (`POST /vaults/:id/destination`) before any Base transaction. A partial
withdrawal (`requestPartialRelease`, D14) works the same way: the app computes the largest
amount that keeps the loan at or below 50 % LTV with at least 0.001 TAZ left in the vault
(`maxPartialRelease`, with a 0.1 % debt margin for interest accruing until inclusion), shows the
LTV after, then sends `[setAuthorization(Registry) if missing, requestPartialRelease]` as one
sponsored UserOp and follows the new Release claim until it is paid.

v3 (contracts/src/interfaces/v3, docs/DECISIONS.md D40 to D54):

- **Refunds (T-02, v4 R-11 and R-12).** A deposit that cannot back a loan (a second payment, a
  deposit while minting is halted, a Refund-mode proof) lands on Base as a pending refund
  (`RefundPending`), which waits like an activation until `finalizeRefund` opens its Refund claim;
  the vault pages list it as pending until then. A new vault's deposit that the coverage cap has
  no room for is no longer refunded: its proof waits and is sent again once there is room, or its
  owner asks for a Refund-mode proof instead ("Refund my deposit instead",
  `POST /vaults/:id/prove-refund`). A refund's destination is pre-validated exactly like an exit's, then set with one sponsored
  UserOp, `Registry.setRefundDestination(claimId, commitment)`; the same revives a refund written
  off after `refundAbandonPeriod` without a destination. The reason it landed is read from the
  Intake's `DepositRefunded` event of the claim's creation second.
- **Pending activations (K3, T-03).** A proven deposit waits for the relay and a short delay;
  anyone activates it then (the backend's keeper does). While minting is paused it waits; once a
  pause by signatures has lasted `pendingRefundDelay` (or after a halt), its account can turn it
  into a refund: `Intake.refundPending(vaultId)`, sponsored for the vault's own account only.
- **Disputes (T-05 (a)).** For `disputeWindow` after the signers confirm a claim paid, its
  claimant can dispute the confirmation ("My payout did not arrive",
  `Registry.disputePayout(claimId)`, served by its fallback); the signers then have
  `inclusionWindow` to prove the payout is in a Zcash block, and otherwise the claim can be
  reopened (`reopenDisputedClaim`) and is paid again. Both are sponsored for the claim's own
  claimant only.
- **Overdue claims (T-05).** When one of the account's own claims with a destination is still
  unpaid `livenessPeriod` after it became payable (up to `commitGrace` longer while a payout is in
  flight), the vault page shows what the signers' bonds would pay for it now (`livenessDue`: its
  value in USDC at the current price plus `livenessPremiumBps`) and offers "Claim from the bonds":
  `SignerBond.slashLiveness(claimId)`, sponsored for the claim's own claimant, and only when the
  bonds can pay it at once. The USDC goes to the loan account; the claim's ZEC then goes to the
  bonds instead, to reimburse them. Before it is due, the page says from when.
- **The minting gates (T-01, T-03).** A banner says when new deposits wait (a pause, short
  bonds) or would come back as refunds (a halt, a full cap), with the project's two standing
  sentences: "Three of five signers can pause new minting for up to 30 days at a time;
  withdrawals, repayments, liquidations and payouts never stop." and "... the bonds compensate
  the victims: liquidators and lenders before the borrower's own equity." None of the gates
  ever stops an exit.
- **The opening fee (T-06)** is shown before a deposit (`fee = amount * feeBps / 10,000`, waived
  below `minFeeZat`) and appears among the vault's claims as its Fee claim.
- **A compromised vault (T-04)**: its exits stop while a theft case is open on it; once one is
  executed, the compensator pays its claims in USDC, and the vault page shows its tally, the
  account's equity and each claim's entitlement (read-only).

## Privacy in the browser

- **Only our backend.** A Content Security Policy (next.config.ts) limits `connect-src` to the
  app and the backend origin, so the page cannot reach an RPC, bundler, paymaster or any third
  party even by mistake. Fonts are self-hosted (Archivo and Martian Mono, OFL, from `app/fonts/`
  through `next/font/local`), the QR code is drawn locally, and there are no analytics. The app
  fetches nothing from a block explorer. On testnet it links a vault's Base transactions to
  Basescan, a link you choose to open; the mainnet beta links none, since opening one would tell the
  explorer which account you care about.
- **Keys stay local.** The owner key lives in `localStorage` (every access wrapped in
  try/catch) and leaves the device only in a backup the user downloads. Salts and destination
  addresses are sent to the signers and never stored; only the resulting commitment is kept to
  resume an interrupted exit. The UFVK is shown once and never stored.
- **The app checks the backend.** It shows a vault's address, as text or QR code, only once all
  five signers' `VaultOpened` signatures (INTERFACES § 2.5) verify against the signer set pinned
  in the build (`NEXT_PUBLIC_SIGNERS`), and a viewing key only if its tag key hashes to the
  `tagKeyHash` they signed (VaultOpened v2; security review C-4; a development build without the pin falls back to the backend's set
  and says so, a production build refuses). It decodes the vault address itself and refuses to show a QR
  code unless it is an Ironwood-only testnet UA (one Orchard-protocol receiver, D4); it builds
  the ZIP 321 URI itself (no label, message or memo); it computes every commitment itself and
  stops if the signers acknowledge a different one.
- **Sponsorable calls only.** Before asking the paymaster, the app checks each batch against a
  mirror of the paymaster allowlist (INTERFACES § 4.1), built from the pinned addresses and,
  for a payment by the bonds only, the bond the pinned Registry names.
- **Vault ids stay out of URLs sent anywhere** (fragment routes, no prefetch; M-6). Key backups
  download under a neutral random name, never the account address, and lists of this browser's
  vaults (My loans, What the world sees) fetch them one by one at randomized intervals instead of
  in one burst (L-11).
- **Warnings where they matter:** the amount step explains that fixed denominations hide the
  amount (L3) and, with the deposit step, not to move exactly the collateral into Ironwood and
  deposit right away (L18); borrowing and sending USDC warn about KYC exchanges (L16); the repay
  path about topping up from outside (L13); payouts (release, partial release, close, refund)
  about deshielding or moving the same amount right away (L7). The viewing-key page says what
  the UFVK shows and hides and to treat it as sensitive.
- No `console` logging; fetches use `credentials: "omit"` and `referrerPolicy: "no-referrer"`;
  the backend URL is the only network peer.

## Structure

```
app/                     routes (App Router); pages are thin, logic lives in components/ and lib/
components/              UI: ui.tsx (server-safe primitives), client.tsx (copy, QR, nav, backend status),
                         ConfigGate (loads GET /config before pages render), MintingBanner (GET /status),
                         borrow/BorrowWizard, position/{PositionView,Exits,PartialRelease,Ledger},
                         DestinationForm, KeyBackup, BorrowForm, PayoutNotice, VerifyView, RefundView,
                         WorldView, LoansView, DemoPanel
lib/config.ts            pinned addresses, GET /config consistency check and runtime values, market id
lib/api.ts               typed client for the coordinator API (§ 4) incl. GET /config, GET /status's
                         minting gates and the oracle demo control, response normalization, errors,
                         loadRuntimeConfig()
lib/eip712.ts            domains and types of § 2, owner signing, Safe ERC-1271 signing of CloseRequest
lib/owner.ts             owner flows: open vault, destination (an exit's or a refund's), viewing key;
                         vault address check
lib/signers.ts           the signers' VaultOpened signatures on a vault (C-4): checked against the pinned set
lib/keys.ts              per-loan owner keys: create, store, export/import, wipe
lib/safe.ts              pinned Safe 1.4.1 + 4337 module config and its counterfactual address (no RPC)
lib/account.ts           smart-account layer: LoanAccount interface, sponsored UserOps via the proxy
lib/calls.ts             call batches (borrow, repay+release, release, partial release, transfer,
                         invalidateNonce, refund destination, refundPending, dispute, reopen, claim from
                         the bonds), allowlist mirror, Registry / Intake / dispute / bond / Morpho
                         revert explanations
lib/chain.ts             Base reads through /proxy/rpc: position, market, oracle, Registry events, release
                         claims; v3: the vault's ledger (claims, pending activation, freeze), refund
                         reasons, compensation, and the contracts the pinned Registry and Intake name
lib/v3.ts                v3 in plain words: opening fee, minting banner and gate, pending activation,
                         refunds, claims, disputes, payments by the bonds
lib/morpho.ts            Morpho share math, interest accrual, health factor, liquidation price, D14 partial
                         release limits, formatting
lib/zcash.ts             zatoshi ↔ ZEC, ZIP 321 URIs, testnet UA sanity check
lib/ua.ts, f4jumble.ts   unified address decoder (Bech32m, F4Jumble, TLV) for the Orchard receiver
lib/commitment.ts        keccak256(salt32 || receiver43) and destination preparation
lib/hooks.ts             React hooks: local loans, URL fragment, polling (paused while the tab is hidden,
                         staggered for lists of vaults), actions
lib/routes.ts            vault page links with the vault id in the URL fragment
lib/__fixtures__/        zcash-test-vectors JSON (UA, F4Jumble) and the contracts' dry-run addresses
```

`AGENTS.md` / `CLAUDE.md` are generated by `next dev` (Next.js 16): they point coding agents at
the version-matched docs in `node_modules/next/dist/docs/`.

## Tests

`pnpm test` runs the offline unit tests (vitest, no network; the count is in docs/TESTING.md § 1). Cross-checks
against independent sources:

- **v3 contracts:** every ABI fragment the app encodes or decodes (Registry, the dispute path,
  Intake, slasher, bond, compensator) against the compiled ABIs in `contracts/abi`, the enums it
  reads as numbers (vault and claim statuses and kinds, `MintBlock`, `RefundReason`) against the
  Solidity interfaces, the opening fee against the Registry's own lines, and the deployment
  fixture is a simulated v3 deployment on a Base Sepolia fork (its EIP-712 domain separators
  computed by the contracts). The ledger reader, the refund-reason search (block bisection,
  2,000-block log windows) and the compensation reader run against a mocked JSON-RPC node; the
  refund, pending-refund, dispute, reopen and bond-payment UserOps run through the faked bundler
  and paymaster and pass the allowlist mirror.

- **EIP-712:** the type strings are parsed from `docs/INTERFACES.md` and compared with the app's
  types and with viem's type hashes; domain separators and digests match the Solidity golden
  vectors (`contracts/test/unit/ZecuredEIP712.t.sol`) and the dry-run deployment; owner-request
  signatures are byte-identical to alloy's (`cast wallet sign --data`); the Safe-wrapped
  `CloseRequest` signature matches a `safeHash` and signature computed with `cast`; the
  `VaultOpened` digest and signature match the Rust signer's vector (`zecured-base`,
  `vault_opened_golden_vector`, also equal to `cast wallet sign --data`).
- **Vault signatures (C-4):** all five pinned signers must sign; the backend's own address, viewing
  key or signer set, four signatures, duplicates, another Registry, request or expiry are refused;
  production builds without `NEXT_PUBLIC_SIGNERS` open no vault; development fallbacks are marked.
- **Unified addresses:** all 60 `unified_address` and 8 `f4jumble` vectors of
  zcash-test-vectors (identical to the copies in librustzcash), re-encoding, testnet and
  Revision 2 addresses, and the ZIP 316 rejection rules the decoder enforces.
- **Commitments:** `keccak256(salt || receiver)` against `cast keccak` of the signer's test inputs.
- **Smart account:** the counterfactual address equals permissionless.js' computation for the
  pinned config (and differs from its default MultiSend config); a full UserOp run against a fake
  bundler, paymaster and RPC checks the deployment, the ERC-7677 sponsorship, Pimlico gas
  pricing, that only methods the backend proxy allows are used, and that the UserOp passes a
  mirror of the backend's allowlist decoder.
- **Configuration:** pins vs `GET /config` (agreeing, contradicting, a compromised backend's
  self-consistent config with an attacker's Adapter or Morpho, another chain, LLTV or market
  id), development fallback, the loader's memoization, failure and 6 s timeout.
- **Partial release (D14):** `MIN_RELEASE` and `MAX_WITHDRAW_LTV_BPS` read from
  `contracts/src/ZecuredRegistry.sol`; the Registry's checks in its order; exact boundaries
  (50.0 % passes, one zatoshi more fails); the closed-form maximum checked by a window search
  over awkward prices, debts and a low-LLTV market; the ABI fragments against
  `contracts/abi/ZecuredRegistry.json` (selector `0x6990cc7d`).
- **Morpho math** (share rounding, accrual, health, LIF), **Base reads** against a mocked
  JSON-RPC node, the **API client** (wire format, normalization of the documented response
  shapes, strict `GET /config`, errors, timeouts), **key storage** (broken or missing storage
  never throws), fragment links, request staggering, ZIP 321 and zatoshi formatting.

## What is real and what is a mock (app)

| Real | Mock / not live yet |
|---|---|
| Owner keys, EIP-712 signing, Safe address derivation, UA decoding, commitments | The market's price on testnets: the DemoShockOracle (MOCK / DEMO) around a MockZecOracle (MOCK), labelled in the UI; MockUSDC (MOCK) as the loan token on testnet |
| Coordinator API client (against the running backend's shapes) | Sponsorship on a local fork: Pimlico's mock paymaster (MOCK, `tools/local-aa`), which sponsors whatever the backend's allowlist lets through |
| Sponsored UserOps, the v3 ones (refund destination, pending refund, dispute, reopen, claim from the bonds) included: run in the browser on local forks of Base Sepolia, through the backend proxy and a local alto bundler (docs/TESTING.md § 2), and tested against a fake bundler and paymaster | Demo panel: the oracle it sets is the MOCK MockZecOracle; bot status is a placeholder |
| Base reads through `/proxy/rpc`, the v3 ledger, refund reasons and compensation included: unit-tested against a mocked node, and run in the browser against the v4 deployments on local forks and against the live Base Sepolia deployment of 27 October (not kept running, D82) | DEMO: the judges' replay (`lib/replay/`, `NEXT_PUBLIC_REPLAY=1`), a simulator that plays a recording in the browser: the live run of 27 October |
| The minting banner and the borrow gate (GET /status, read by the backend from the Intake and the bond) | |
| Demo panel price control (`GET` / `POST /admin/oracle`, the backend holds the owner key; refused where the market's oracle is a real feed, as on Base mainnet) | |
| Close from collateral and deleverage: signed in the browser (the Safe's ERC-1271) and executed by the market-maker bot behind `GET /quotes/close` / `POST /closes` (docs/TESTING.md § 2) | |
| A vault's proof trail (`lib/journey.ts`): its own events on Base, read through the proxy | |

## Open points

- Owner keys live in `localStorage` under a CSP that allows inline scripts (Next.js without
  nonces); a nonce-based CSP and optional passphrase encryption of stored keys are not done (L-11).
