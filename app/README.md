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
cp .env.example .env.local   # optional: defaults work against a local backend
pnpm dev              # http://localhost:3000
pnpm test             # vitest, offline
pnpm build            # production build (includes the TypeScript check)
pnpm typecheck        # tsc --noEmit (app + tests)
```

Requires Node.js ≥ 20.9 and pnpm 10. The backend is optional for browsing: every page renders
without it and says what is unavailable.

## Environment variables

All values are public (inlined into the bundle at build time). Rebuild after changing them.

| Variable | Default | Purpose |
|---|---|---|
| `NEXT_PUBLIC_BACKEND_URL` | `http://127.0.0.1:8080` | Coordinator API and `/proxy/{rpc,bundler,paymaster}`. Also the only extra origin allowed by the CSP `connect-src`. |
| `NEXT_PUBLIC_REGISTRY` | `0x…dEaD` (placeholder) | `ZecuredRegistry`; EIP-712 `verifyingContract` of every owner request. The placeholder matches the local signer network config. |
| `NEXT_PUBLIC_ADAPTER` | `0x…dEaD` (placeholder) | `SettlementAdapter`; EIP-712 domain of `CloseRequest`. |
| `NEXT_PUBLIC_MARKET_ID` | zero (placeholder) | Morpho market id; checked against the market params below. |
| `NEXT_PUBLIC_USDC` | zero (placeholder) | MockUSDC (loan token). |
| `NEXT_PUBLIC_ORACLE` | zero (placeholder) | MockZecOracle (price = ZEC/USD × 1e34). |
| `NEXT_PUBLIC_ZZEC` | zero (placeholder) | zZEC (collateral token). |
| `NEXT_PUBLIC_MORPHO` | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` | Morpho Blue on Base Sepolia. |
| `NEXT_PUBLIC_IRM` | `0x46415998764C29aB2a25CbeA6254146D50D22687` | AdaptiveCurveIRM. |

While any contract address is a placeholder, Base reads and sponsored transactions are
disabled and the UI says so; the off-chain steps (open vault, deposit, refunds, viewing key)
work against the backend.

## Pages

| Route | What it does |
|---|---|
| `/` | Pitch, how it works in three steps, "what the world sees" preview. |
| `/prepare` | Why Ironwood only (pool crossings reveal amounts at the turnstile), migration advice, what counts as a valid deposit, refunds. |
| `/borrow` | Wizard: amount (0.01 / 0.05 TAZ) → fresh key + Safe address, `OpenVault` signature, `POST /vaults` → key backup → verified Ironwood-only vault address, ZIP 321 QR, deadline, live status → borrow up to 40 % LTV. |
| `/loans` | Loan keys held in this browser: export, import (validated against the derived account), forget. |
| `/position/[vaultId]` | Collateral, debt (with accrued interest), health factor, liquidation price, payouts; exits: close from collateral, repay + release, release after liquidation; send USDC; borrow more. |
| `/verify/[vaultId]` | Owner-signed `ViewKeyRequest` → the vault's UFVK (never stored) and how to watch the vault. |
| `/refund/[vaultId]` | Non-conforming notes and owner-signed `RefundRequest`s. |
| `/world` | Everyone / signer network / you, with live Registry events, market totals and the signers' view of your vaults. |
| `/demo` | Admin-token panel: node modes, the sweep (threshold) demo; oracle and bots are placeholders. |

Every exit first asks for the Ironwood destination, salts it, signs `Destination` and has the
signers pre-validate it (`POST /vaults/:id/destination`) before any Base transaction.

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
- **The app checks the backend.** It decodes the vault address itself and refuses to show a QR
  code unless it is an Ironwood-only testnet UA (one Orchard-protocol receiver, D4); it builds
  the ZIP 321 URI itself (no label, message or memo); it computes every commitment itself and
  stops if the signers acknowledge a different one.
- **Sponsorable calls only.** Before asking the paymaster, the app checks each batch against a
  mirror of the paymaster allowlist (INTERFACES § 4.1).
- No `console` logging; fetches use `credentials: "omit"` and `referrerPolicy: "no-referrer"`;
  the backend URL is the only network peer.

## Structure

```
app/                     routes (App Router); pages are thin, logic lives in components/ and lib/
components/              UI: ui.tsx (server-safe primitives), client.tsx (copy, QR, nav, backend status),
                         borrow/BorrowWizard, position/{PositionView,Exits}, DestinationForm, KeyBackup,
                         BorrowForm, VerifyView, RefundView, WorldView, LoansView, DemoPanel
lib/config.ts            NEXT_PUBLIC_* config, placeholders, market params and id
lib/api.ts               typed client for the coordinator API (§ 4), response normalization, errors
lib/eip712.ts            domains and types of § 2, owner signing, Safe ERC-1271 signing of CloseRequest
lib/owner.ts             owner flows: open vault, destination, refund, viewing key; vault address check
lib/keys.ts              per-loan owner keys: create, store, export/import, wipe
lib/safe.ts              pinned Safe 1.4.1 + 4337 module config and its counterfactual address (no RPC)
lib/account.ts           smart-account layer: LoanAccount interface, sponsored UserOps via the proxy
lib/calls.ts             call batches (borrow, repay+release, release, transfer, invalidateNonce) + allowlist mirror
lib/chain.ts             Base reads through /proxy/rpc: position, market, oracle, Registry events
lib/morpho.ts            Morpho share math, interest accrual, health factor, liquidation price, formatting
lib/zcash.ts             zatoshi ↔ ZEC, ZIP 321 URIs, testnet UA sanity check
lib/ua.ts, f4jumble.ts   unified address decoder (Bech32m, F4Jumble, TLV) for the Orchard receiver
lib/commitment.ts        keccak256(salt32 || receiver43) and destination preparation
lib/hooks.ts             React hooks: local loans, polling (paused while the tab is hidden), actions
lib/__fixtures__/        zcash-test-vectors JSON (UA, F4Jumble) and the contracts' dry-run addresses
```

`AGENTS.md` / `CLAUDE.md` are generated by `next dev` (Next.js 16): they point coding agents at
the version-matched docs in `node_modules/next/dist/docs/`.

## Tests

`pnpm test` runs about 200 offline unit tests (vitest, no network). Cross-checks against independent sources:

- **EIP-712:** the type strings are parsed from `docs/INTERFACES.md` and compared with the app's
  types and with viem's type hashes; domain separators and digests match the Solidity golden
  vectors (`contracts/test/unit/ZecuredEIP712.t.sol`) and the dry-run deployment; owner-request
  signatures are byte-identical to alloy's (`cast wallet sign --data`); the Safe-wrapped
  `CloseRequest` signature matches a `safeHash` and signature computed with `cast`.
- **Unified addresses:** all 60 `unified_address` and 8 `f4jumble` vectors of
  zcash-test-vectors (identical to the copies in librustzcash), re-encoding, testnet and
  Revision 2 addresses, and the ZIP 316 rejection rules the decoder enforces.
- **Commitments:** `keccak256(salt || receiver)` against `cast keccak` of the signer's test inputs.
- **Smart account:** the counterfactual address equals permissionless.js' computation for the
  pinned config (and differs from its default MultiSend config); a full UserOp run against a fake
  bundler, paymaster and RPC checks the deployment, the ERC-7677 sponsorship, Pimlico gas
  pricing, that only methods the backend proxy allows are used, and that the UserOp passes a
  mirror of the backend's allowlist decoder.
- **Morpho math** (share rounding, accrual, health, LIF), **Base reads** against a mocked
  JSON-RPC node, the **API client** (wire format, normalization of both response shapes,
  errors, timeouts), **key storage** (broken or missing storage never throws), ZIP 321 and
  zatoshi formatting.

## What is real and what is a mock (app)

| Real | Mock / not live yet |
|---|---|
| Owner keys, EIP-712 signing, Safe address derivation, UA decoding, commitments | MockUSDC and MockZecOracle (labelled in the UI) |
| Coordinator API client (against the running backend's shapes) | Sponsored UserOps: wired and tested against a fake bundler / paymaster, never run live (no bundler / paymaster keys, contracts not deployed) |
| Base reads through `/proxy/rpc` (unit-tested against a mocked node; the running proxy answered live Base Sepolia reads in a manual check) | Demo panel: oracle price control and bot status are placeholders |
| | Close from collateral: needs the market-maker bot behind `GET /quotes/close` / `POST /closes` |

## Open points

- `GET /vaults/:id`: the backend returns `paid: bool` per claim (INTERFACES: `status`), adds
  `address`, `outstanding`, `payoutInFlight`, and omits `confirmations` / `deposit`. The client
  accepts both shapes. A `tipHeight` field would let the app show blocks left before the deposit
  deadline (supported if present); a `birthdayHeight` would give the exact UFVK scan start.
- Owner signatures must use the Registry address the signer network is configured with; set
  `NEXT_PUBLIC_REGISTRY` together with the backend after deployment.
