# Zecured app

Next.js front end of Zecured: **borrow against shielded ZEC. Sell nothing. Reveal nothing.**
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

## Configuration

Every address the app signs for or sends calls to is **pinned in the build** (security review
H-6): a compromised backend must not be able to point the account's Morpho authorization or a
USDC approval at its own contract. The backend's `GET /config` is only checked against the
pins; it supplies the values that carry no authority.

| Variable | Default | Purpose |
|---|---|---|
| `NEXT_PUBLIC_BACKEND_URL` | `http://127.0.0.1:8080` | Coordinator API and `/proxy/{rpc,bundler,paymaster}`. Also the only extra origin allowed by the CSP `connect-src`. |
| `NEXT_PUBLIC_REGISTRY` | none (pin) | `ZecuredRegistry`; EIP-712 `verifyingContract` of every owner request. |
| `NEXT_PUBLIC_ADAPTER` | none (pin) | `SettlementAdapter`; EIP-712 domain of `CloseRequest`. |
| `NEXT_PUBLIC_MARKET_ID` | none (pin) | Morpho market id; must equal `marketIdOf` of the pinned market parameters. |
| `NEXT_PUBLIC_USDC` | none (pin) | MockUSDC (loan token). |
| `NEXT_PUBLIC_ORACLE` | none (pin) | MockZecOracle (price = ZEC/USD × 1e34). |
| `NEXT_PUBLIC_ZZEC` | none (pin) | zZEC (collateral token). |
| `NEXT_PUBLIC_SIGNERS` | none (pin) | The five signer addresses, comma-separated (to verify signer signatures in the app). |
| `NEXT_PUBLIC_MORPHO` | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | Morpho Blue on Base Sepolia (the default is a pin too). |
| `NEXT_PUBLIC_IRM` | `0x46415998764C29aB2a25CbeA6254146D50D22687` | AdaptiveCurveIRM (the default is a pin too). |

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
  confirmations and the deposit window (`vaultExpiryBlocks`).

`components/ConfigGate.tsx` (in the root layout) fetches `GET /config` once before any page
renders, with a compact loading state; after 6 s without an answer the page renders with the
build's configuration. Configuration problems and warnings are listed above every page.

### Pinning a deployment

```bash
jq -r '"NEXT_PUBLIC_REGISTRY=\(.contracts.ZecuredRegistry)
NEXT_PUBLIC_ADAPTER=\(.contracts.SettlementAdapter)
NEXT_PUBLIC_MARKET_ID=\(.market.marketId)
NEXT_PUBLIC_USDC=\(.contracts.MockUSDC)
NEXT_PUBLIC_ORACLE=\(.contracts.MockZecOracle)
NEXT_PUBLIC_ZZEC=\(.contracts.ZZEC)
NEXT_PUBLIC_MORPHO=\(.market.morphoBlue)
NEXT_PUBLIC_IRM=\(.market.irm)
NEXT_PUBLIC_SIGNERS=\(.signers | join(","))"' ../contracts/deployments/anvil.json >> .env.local
```

(`base-sepolia.json` once deployed publicly.) Review the result before building: these
addresses are what the app trusts.

## Pages

| Route | What it does |
|---|---|
| `/` | Pitch, how it works in three steps, "what the world sees" preview. |
| `/prepare` | Why Ironwood only (pool crossings reveal amounts at the turnstile), migration advice, what counts as a valid deposit, refunds. |
| `/borrow` | Wizard: amount (the signers' denominations from `GET /config`) → fresh key + Safe address, `OpenVault` signature, `POST /vaults` → key backup → Ironwood-only vault address signed by all five signers (checked in the browser), ZIP 321 QR, deadline, live status → borrow up to 40 % LTV. |
| `/loans` | Loan keys held in this browser: export, import (validated against the derived account), forget. |
| `/position#<vaultId>` | Collateral, debt (with accrued interest), health factor, liquidation price, payouts; exits: close from collateral, repay + release, release after liquidation; withdraw part of the ZEC (D14); send USDC; borrow more. |
| `/verify#<vaultId>` | Owner-signed `ViewKeyRequest` → the vault's UFVK (never stored), what it shows and hides, and the `zecured-wallet watch` command from the vault's birthday height. |
| `/refund#<vaultId>` | Non-conforming notes and owner-signed `RefundRequest`s. |
| `/world` | Everyone / signer network / you, with live Registry events, market totals and the signers' view of your vaults. |
| `/demo` | Admin-token panel: node modes, the sweep (threshold) demo, the MOCK oracle price (presets $40 and $20); bot status is a placeholder. |

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

## Privacy in the browser

- **Only our backend.** A Content Security Policy (next.config.ts) limits `connect-src` to the
  app and the backend origin, so the page cannot reach an RPC, bundler, paymaster or any third
  party even by mistake. Fonts are self-hosted (`geist`), the QR code is drawn locally, there
  are no analytics, and the app links to no block explorer (opening one would tell it which
  account you care about).
- **Keys stay local.** The owner key lives in `localStorage` (every access wrapped in
  try/catch) and leaves the device only in a backup the user downloads. Salts and destination
  addresses are sent to the signers and never stored; only the resulting commitment is kept to
  resume an interrupted exit. The UFVK is shown once and never stored.
- **The app checks the backend.** It shows a vault's address, as text or QR code, only once all
  five signers' `VaultOpened` signatures (INTERFACES § 2.5) verify against the signer set pinned
  in the build (`NEXT_PUBLIC_SIGNERS`), and a viewing key only if it hashes to the `ufvkHash` they
  signed (security review C-4; a development build without the pin falls back to the backend's set
  and says so, a production build refuses). It decodes the vault address itself and refuses to show a QR
  code unless it is an Ironwood-only testnet UA (one Orchard-protocol receiver, D4); it builds
  the ZIP 321 URI itself (no label, message or memo); it computes every commitment itself and
  stops if the signers acknowledge a different one.
- **Sponsorable calls only.** Before asking the paymaster, the app checks each batch against a
  mirror of the paymaster allowlist (INTERFACES § 4.1), built from the pinned addresses.
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
                         ConfigGate (loads GET /config before pages render), borrow/BorrowWizard,
                         position/{PositionView,Exits,PartialRelease}, DestinationForm, KeyBackup,
                         BorrowForm, PayoutNotice, VerifyView, RefundView, WorldView, LoansView, DemoPanel
lib/config.ts            pinned addresses, GET /config consistency check and runtime values, market id
lib/api.ts               typed client for the coordinator API (§ 4) incl. GET /config and the oracle demo
                         control, response normalization, errors, loadRuntimeConfig()
lib/eip712.ts            domains and types of § 2, owner signing, Safe ERC-1271 signing of CloseRequest
lib/owner.ts             owner flows: open vault, destination, refund, viewing key; vault address check
lib/signers.ts           the signers' VaultOpened signatures on a vault (C-4): checked against the pinned set
lib/keys.ts              per-loan owner keys: create, store, export/import, wipe
lib/safe.ts              pinned Safe 1.4.1 + 4337 module config and its counterfactual address (no RPC)
lib/account.ts           smart-account layer: LoanAccount interface, sponsored UserOps via the proxy
lib/calls.ts             call batches (borrow, repay+release, release, partial release, transfer,
                         invalidateNonce), allowlist mirror, Registry / Morpho revert explanations
lib/chain.ts             Base reads through /proxy/rpc: position, market, oracle, Registry events, release claims
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

`pnpm test` runs about 270 offline unit tests (vitest, no network). Cross-checks against independent sources:

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
| Owner keys, EIP-712 signing, Safe address derivation, UA decoding, commitments | MockUSDC and MockZecOracle (labelled in the UI) |
| Coordinator API client (against the running backend's shapes) | Sponsored UserOps: wired and tested against a fake bundler / paymaster; not yet run against a live bundler (`TODO(live)` in lib/account.ts) |
| Base reads through `/proxy/rpc` (unit-tested against a mocked node; the running proxy answered live Base Sepolia reads in a manual check) | Demo panel: the oracle it sets is the MOCK MockZecOracle; bot status is a placeholder |
| Demo panel price control (`GET` / `POST /admin/oracle`, the backend holds the owner key) | Close from collateral: needs the market-maker bot behind `GET /quotes/close` / `POST /closes` |

## Open points

- Owner keys live in `localStorage` under a CSP that allows inline scripts (Next.js without
  nonces); a nonce-based CSP and optional passphrase encryption of stored keys are not done (L-11).
