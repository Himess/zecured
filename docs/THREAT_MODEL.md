# Threat model and privacy analysis

Zecured's claim, in full:

> Zecured separates your ZEC wallet, balance and history from your loan. Your ZEC is never
> unshielded and never changes pools. The Base account is fresh and never receives gas from
> outside. A loan can be closed from its collateral, without a single unit of outside money
> entering the account. The USDC you borrow is ordinary USDC; where it goes is your choice.
> What stays public: the loan size, the collateral denomination, interest, liquidation events
> and the amount of ZEC returned to you at close — none of it linked to you.

This document says exactly what each party can see, every leakage vector we know of, what
mitigates it and where that is enforced in the code.

## Who sees what

| Observer | Sees | Does not see |
|---|---|---|
| Zcash chain observer | Ordinary Ironwood → Ironwood transactions; no pool crossing anywhere in the flow | Who paid whom, amounts, any link to Zecured |
| Base chain observer | A fresh account with no history; a standard-size zZEC position on Morpho and its debt; release / close / liquidation events; salted recipient commitments | Who owns the account; the user's ZEC wallet, balance or history |
| Signer network (3-of-5) | Vault balances, timing, payout recipients | The user's identity and any ZEC outside the vault |
| Coordinator | Same as the signers (it builds the transactions) | Any key material; it cannot move funds or mint |
| The borrower (with the vault UFVK) | The vault's own notes, its payouts to the borrower and its change | Third-party payouts (liquidator, market maker): encrypted with `ovk = None` |
| Our backend proxy | Requests (never logged) | — |
| RPC / bundler / paymaster providers | Account addresses and transactions (public anyway) | The user's IP address (all browser traffic goes through our proxy) |

## Leakage vectors

| # | Vector | Mitigation | Enforced in |
|---|---|---|---|
| L1 | Funding the Base account for gas from outside | Paymaster sponsors only an allowlist; the account never receives ETH | `backend/src/allowlist.rs`, `backend/src/proxy.rs` |
| L2 | Depositing with transparent inputs | P1 never attests such a deposit; P5 refunds it | `signer/crates/zcash-ops/src/tx.rs`, `policy/src/classify.rs` |
| L3 | Amount fingerprinting | Standard denominations only (testnet 0.01 / 0.05 TAZ; 2–3 buckets on mainnet) | node P0 (`dkg::check_open`), app |
| L4 | The deposit transaction revealed on Base | The attestation carries only vault id, account, amount and a keyed tag; no Zcash data ever reaches Base | `policy/src/derive.rs`, contracts |
| L5 | Timing correlation (deposit ↔ mint) | Confirmations before attesting; optional random delay (roadmap); small testnet volume stated openly | coordinator |
| L6 | The return address published on Base | Off-chain owner-signed destination; Base only sees a salted commitment | node `destination`, `ledger::commitment` |
| L7 | Deshielding the returned amount right away | Warning and guidance in the app | app |
| L8 | Reusing a vault | A fresh vault (fresh FROST key) per loan; one account ↔ one vault | DKG per vault; Registry I4 |
| L9 | Linking `depositTag` to the note's nullifier | Keyed tag (key derived from the vault secret) | `policy/src/derive.rs` |
| L10 | Our backend sees client IPs | Nothing is logged; Tor support is roadmap | `backend/` |
| L11 | Liquidator / market maker addresses | Revealed off-chain only; Base sees salted commitments; a fresh salt per operation | `bots/`, coordinator |
| L12 | Bridges or NEAR Intents in the ZEC flow | None: ZEC never leaves Zcash | design |
| L13 | Paying interest/principal from an outside wallet | Close from collateral (P6) needs no outside USDC; the repay-with-USDC path shows a warning | contracts, app |
| L14 | Turnstile: depositing from Orchard/Sapling or paying out to Sapling reveals the amount | P1 accepts only Ironwood-only deposits; every payout goes to an Orchard-protocol receiver in the Ironwood pool; Ironwood preparation screen | `tx::TxShape::is_ironwood_only`, `policy/src/verify.rs` |
| L15 | Providers mapping IP ↔ account | Backend proxy for RPC, bundler and paymaster | `backend/src/proxy.rs` |
| L16 | Sending borrowed USDC straight to a KYC exchange (Morpho's `Borrow` event shows the receiver) | Warning: "this links the loan to your identity" | app |
| L17 | The vault UFVK revealing third-party payouts | Third-party outputs use `ovk = None`; every signer refuses a PCZT where a third-party output is recoverable with the vault OVKs | `policy/src/verify.rs` |
| L18 | Moving exactly the collateral amount into Ironwood and depositing at once | Preparation screen: move a different amount, wait, or use the wallet's ZIP 318 migration | app |
| L19 | Unsalted address hashes on Base | `keccak256(salt32 ‖ receiver43)`; salts only with the signers | `ledger::commitment`, contracts |
| L20 | Registering the vault on Base just before the deposit | Vaults are opened off-chain; the first Base trace is the attested deposit | coordinator, Registry |

## Attacks on the signer network

| Threat | Mitigation | Where |
|---|---|---|
| One or two signers compromised | Payouts need 3 of 5 FROST shares; each signer checks the request independently. Shown live: two "compromised" signers signed a sweep, three refused, nothing moved | `node/src/signing.rs`, `coordinator::demo_drain` |
| Malicious coordinator alters a payout (recipient, amount, split, extra input, OVK, fee, expiry) | Every signer recomputes the plan from its own view of both chains and verifies the PCZT against it | `policy/src/verify.rs` (9 adversarial tests) |
| One FROST signature authorizing two actions (equal randomizers) | Refused: no other action may reuse the randomized key of a spend being signed | `policy/src/verify.rs` |
| Burning vault value into fees through an unaccounted spend | Refused: the bundle's value balance must equal the plan's fee exactly | `policy/src/verify.rs` |
| Coordinator reads DKG secrets | Round-2 packages and `sk` contributions are sealed node-to-node (X25519 + ChaCha20-Poly1305); `sk` is agreed by commit-reveal | `node/src/envelope.rs`, `node/src/dkg.rs` |
| Forged owner requests | Each request is an EIP-712 signature checked against the owner key recorded at opening, with one-time nonces and deadlines | node P0 |
| A deposit minted twice, or a refunded deposit also minted | P1 marks exactly one conforming note; decisions are final; the Registry rejects reused vault ids and tags | `policy/src/classify.rs`, Registry I5 |
| zZEC escaping its vault (transfers, flash loans, direct liquidation) | zZEC moves only Registry ↔ Morpho ↔ Adapter under a one-shot transient expectation | `contracts/src/ZZEC.sol`, invariant I2 |
| Three colluding signers | Can steal: this is the hackathon trust assumption. Roadmap: independent operators, bonds, slashing, Base-side verification of Zcash deposits | — |

## Known limitations (hackathon)

- All five signers, the coordinator and the bots are run by us (signer independence is a mock).
- Signer key material (FROST shares, vault `sk`) is stored unencrypted on each node's disk.
- The price oracle and USDC are mocks.
- Only one public light wallet server runs NU7 software today; the signers trust it for data
  availability (not for authorization).
- Testnet's low volume shrinks the anonymity set.

## Intentionally public

- On Base: loan size, collateral denomination, interest, close and liquidation events, the
  USDC receiver of a borrow, the market maker's address (an expected public role) — none
  linked to the borrower's identity or ZEC wallet.
- The ZEC amount returned at close can be computed from Base events; the app advises not to
  deshield it immediately (L7).

## Pre-submission checklist

- [ ] No transparent, Sapling or Orchard-pool address or spend anywhere in a flow
- [ ] Every Zcash output in the Ironwood pool
- [ ] Third-party outputs `ovk = None`
- [ ] No smart account ever received ETH; outside USDC only in the warned repay-with-USDC path
- [ ] No account ever held zZEC (token transfers on the explorer)
- [ ] No Base event or calldata with a Zcash txid, note commitment or address; every recipient commitment salted
- [ ] No Base transaction about a vault before its deposit
- [ ] Demo control panel authenticated; `setPrice` owner-only
- [ ] Browser network tab: no third-party RPC / bundler / paymaster calls
- [ ] Backend and node logs: no addresses, viewing keys, salts or IPs
- [ ] Demo video: no real personal addresses
- [ ] README "What is real and what is a mock" up to date
