# Threat model and privacy analysis

Zecured's claim, in full:

> Zecured separates your ZEC wallet, balance and history from your loan. Your ZEC is never
> unshielded and never changes pools. The Base account is fresh and never receives gas from
> outside. A loan can be closed from its collateral, without a single unit of outside money
> entering the account. The USDC you borrow is ordinary USDC; where it goes is your choice.
> What stays public: the loan size, the collateral denomination, interest, liquidation events
> and the amount of ZEC returned to you at close — none of it linked to you.

This document says exactly what each party can see, every leakage vector we know of, what
mitigates it and where that is enforced in the code. The findings of the internal security
review and their status are in [SECURITY_REVIEW.md](SECURITY_REVIEW.md).

## Who sees what

| Observer | Sees | Does not see |
|---|---|---|
| Zcash chain observer | Ordinary Ironwood → Ironwood transactions; no pool crossing anywhere in the flow | Who paid whom, amounts, any link to Zecured |
| Base chain observer | A fresh account with no history; a standard-size zZEC position on Morpho and its debt; release / close / liquidation events; salted recipient commitments | Who owns the account; the user's ZEC wallet, balance or history |
| Signer network (3-of-5) | Vault balances, timing, payout recipients | The user's identity and any ZEC outside the vault |
| Coordinator | Same as the signers (it builds the transactions); it can delay or withhold work | Any key share: it cannot move funds, mint, or hand you a deposit address the signers did not sign |
| The borrower (with the vault UFVK) | The vault's own notes, its payouts to the borrower and its change | Third-party payouts (liquidator, market maker): encrypted with `ovk = None` |
| A deposit prover (our GPU server, or whoever a borrower hands a witness; review H-10a) | The deposit transaction and its note to the vault (value, recipient), the vault's id, owner, account and address, and its tag key K: with K it can tell which Zcash notes are the vault's (their tags) and which transactions are its payouts (their `payoutRef`) | The amounts, recipients and memos of the vault's other notes and payouts; the viewing key (a deposit witness carries K only, statement v2); any key share |
| A violation prover | F1, F2: the vault's UFVK (what the borrower sees); F3: the claimant's own incoming viewing key and its claim; F4 and `Included`: K and, for F4, the claim's salt and receiver | With K alone (F4, `Included`): the amounts and recipients of the vault's notes and payouts; F3: the vault's notes beyond the claim's own output |
| Our backend proxy | Requests and client IPs (never logged) | — |
| The app's host | Page loads and client IPs | Vault ids (kept in the URL fragment) and keys (they stay in the browser) |
| The light wallet server operator | Which transactions the signers look up: Zecured deposits and payouts | Amounts and addresses (shielded); the Base account |
| RPC / bundler / paymaster providers | Account addresses and transactions (public anyway) | The user's IP address (all browser traffic goes through our proxy) |

## Leakage vectors

| # | Vector | Mitigation | Enforced in |
|---|---|---|---|
| L1 | Funding the Base account for gas from outside | Paymaster sponsors only an allowlist; the account never receives ETH | `backend/src/allowlist.rs`, `backend/src/proxy.rs` |
| L2 | Depositing with transparent inputs | P1 never counts such a deposit as conforming and no Activate-mode proof accepts it (D5); with journal v2 it is proven in Refund mode and paid back as a claim (on a v1 deployment, P5 refunds it off-chain) | `signer/crates/zcash-ops/src/tx.rs`, `policy/src/classify.rs`, `tier2/statement` |
| L3 | Amount fingerprinting | Standard denominations only (testnet 0.01 / 0.05 / 0.125 TAZ, the last what the Valar faucet pays, D29; 2–3 buckets on mainnet) | node P0 (`dkg::check_open`), app |
| L4 | The deposit transaction revealed on Base | The deposit proof's journal carries only vault id, account, amount, a keyed tag and what L22 lists; no Zcash data ever reaches Base | `tier2/statement`, `policy/src/derive.rs`, contracts |
| L5 | Timing correlation (deposit ↔ mint) | Confirmations before proving, plus a random 0 to `attest_delay_max_blocks` extra confirmations per vault (D15; at least 2 outside a local fork, which the network config enforces); small testnet volume stated openly | coordinator |
| L6 | The return address published on Base | Off-chain owner-signed destination; Base only sees a salted commitment | node `destination`, `ledger::commitment` |
| L7 | Deshielding the returned amount right away | Warning and guidance in the app | app |
| L8 | Reusing a vault | A fresh vault (fresh FROST key) per loan; one account ↔ one vault | DKG per vault; Registry I4 |
| L9 | Linking `depositTag` to the deposit note | Keyed tag: BLAKE2b keyed with a hash of the vault's viewing key, its tag key K, over the note commitment (D24); only holders of K can compute it: the viewing key's holders and whoever proves the vault's deposits | `policy/src/derive.rs`, `tier2/statement` |
| L10 | Our backend and the app's host see client IPs | Nothing is logged; vault ids stay in the URL fragment, so the app's host never sees them; the app and its API can also be served as a Tor onion service, which never sees a visitor's address (infra/README.md) | `backend/`, app routes, `infra/onion/` |
| L11 | Liquidator / market maker addresses | Revealed off-chain only; Base sees salted commitments; a fresh salt per operation | `bots/`, coordinator |
| L12 | Bridges or cross-chain intent routes in the ZEC flow | None: ZEC never leaves Zcash | design |
| L13 | Paying interest/principal from an outside wallet | Close from collateral (P6) needs no outside USDC; the repay-with-USDC path shows a warning | contracts, app |
| L14 | Turnstile: depositing from Orchard/Sapling or paying out to Sapling reveals the amount | P1 accepts only Ironwood-only deposits; every payout goes to an Orchard-protocol receiver in the Ironwood pool; Ironwood preparation screen | `tx::TxShape::is_ironwood_only`, `policy/src/verify.rs` |
| L15 | Providers mapping IP ↔ account | Backend proxy for RPC, bundler and paymaster | `backend/src/proxy.rs` |
| L16 | Sending borrowed USDC straight to a KYC exchange (Morpho's `Borrow` event shows the receiver) | Warning: "this links the loan to your identity" | app |
| L17 | The vault UFVK revealing third-party payouts | Third-party outputs use `ovk = None`; every signer refuses a PCZT where a third-party output is recoverable with the vault OVKs | `policy/src/verify.rs` |
| L18 | Moving exactly the collateral amount into Ironwood and depositing at once | Preparation screen: move a different amount, wait, or use the wallet's ZIP 318 migration | app |
| L19 | Unsalted address hashes on Base | `keccak256(salt32 ‖ receiver43)`; salts only with the signers | `ledger::commitment`, contracts |
| L20 | Registering the vault on Base just before the deposit | Vaults are opened off-chain; the first Base trace is the deposit's proof | coordinator, Registry |
| L21 | The light wallet server learns which transactions the signers look up (deposits, payouts) | Each signer can use its own server; roadmap: every signer on its own full node | node config ([OPERATORS.md](OPERATORS.md)) |
| L22 | A Tier 2 deposit proof revealing the deposit or the vault's address on Base | The journal holds the mode, the epoch's branch id, the anchor header (a later block, not the deposit's), vault id, account, amount, tag and the `VaultOpened` struct hash; the deposit's block, transaction, note and the address and tag key hashes stay inside the proof (D22). Base learns only that the deposit precedes the anchor | `tier2/statement`, `ZecuredIntake.attestDepositWithProof` |
| L23 | A payout's Tier 3 commitment lands on Base seconds before its broadcast, pointing at the next Zcash blocks | A random delay of 1 to `payout_delay_max_blocks` Zcash blocks (8 on the judge deployment) between commitment and broadcast, inside the 120-block expiry (D32, RT-13). Outside a local fork the network config refuses a maximum of 0, and `GET /status` shows both delays | coordinator |
| L24 | Tier 3 records and violation journals linking to Zcash | A commitment holds only keyed tags, a keyed payout reference, salted claim references and output indexes; a journal adds an anchor header and, for F3, the value the claimant received. No txid, note commitment or address (docs/TIER3.md § 11) | `policy/src/derive.rs`, `tier2/statement`, contracts |
| L26 | A swap: the taker's Base account and the order's amount are public, and the maker's payment and proof follow the order | Base holds only the receiver's salted commitment; the payment's block stays behind a later anchor header; the maker pays after a random 1 to `pay_delay_max_blocks` blocks and proves after a random 0 to `prove_delay_max_blocks` more, with no outgoing viewing key; stated in the app: on a swap your Base account is visible, where the ZEC went is not | `swap/maker`, `tier2/payment-statement`, ZecuredSwap |
| L25 | A refund proof (journal v2, Refund mode) writing a non-conforming note's exact value on Base, where an activation writes a shared denomination: the note's sender knows the amount and the time | The app asks only for the denomination (ZIP 321 request built locally), so such a note is a sender's mistake; stated in docs/TIER2.md § 2.1 | app, `tier2/statement` |

## Attacks on the signer network

| Threat | Mitigation | Where |
|---|---|---|
| One or two signers compromised | Payouts need 3 of 5 FROST shares; each signer checks the request independently. Shown live: two "compromised" signers signed a sweep, three refused, nothing moved. The demo's "compromised" mode exists only in demo builds and only on nodes 4 and 5; only the separate theft demo's build allows a third, and it refuses to run against the judge deployment (D35) | `node/src/signing.rs`, `coordinator::demo_drain`, `base-client/src/theft_demo_guard.rs` |
| Forged chain data (a lying light wallet server or Base RPC) | Each node reads its own sources; optional witness sources must agree on the deposit (block hash, transaction bytes, tip); Registry events are checked against contract state | `zcash-ops/src/witness.rs`, `base-client/src/verify.rs` |
| A substituted deposit address or viewing key | All five signers sign `VaultOpened` (the address's hash and the hash of the tag key K, which the viewing key determines: D50); the app and the CLI verify the signatures against a signer set pinned in the build | node DKG, `app/`, `cli/` |
| One payout's transaction attributed to another payout's claims | A node never signs a second payout while one it signed can still be mined, and confirms only the transaction it signed | `node/src/signing.rs` |
| Third-party payouts made unspendable (garbled ciphertext) | Every non-zero output must decrypt to exactly the planned note | `policy/src/verify.rs` |
| Dust or foreign notes sent to the vault to freeze it or inflate fees | Only the planned change of a vault transaction joins the vault's books | `node/src/chain.rs` |
| Malicious coordinator alters a payout (recipient, amount, split, extra input, OVK, fee, expiry) | Every signer recomputes the plan from its own view of both chains and verifies the PCZT against it | `policy/src/verify.rs` (9 adversarial tests) |
| One FROST signature authorizing two actions (equal randomizers) | Refused: no other action may reuse the randomized key of a spend being signed | `policy/src/verify.rs` |
| Burning vault value into fees through an unaccounted spend | Refused: the bundle's value balance must equal the plan's fee exactly | `policy/src/verify.rs` |
| Coordinator reads DKG secrets | Round-2 packages and `sk` contributions are sealed node-to-node (X25519 + ChaCha20-Poly1305); `sk` is agreed by commit-reveal | `node/src/envelope.rs`, `node/src/dkg.rs` |
| Forged owner requests | Each request is an EIP-712 signature checked against the owner key recorded at opening, with one-time nonces and deadlines | node P0 |
| A deposit minted twice, or a refunded deposit also minted | A note has one tag in both proof modes, so it lands on Base once, as collateral or as a Refund claim (D50); the Registry rejects reused vault ids and tags (I5); the signers never refund off-chain a note a deployed guest can land (I30) | `ZecuredIntake`, Registry I5, `node/src/chain.rs` |
| Three signers minting for a deposit that does not exist, or honest signers fooled by a lying data source | Every deposit is minted on a proof (Tier 2, D41), which needs no signer and no light wallet server; the signers only vouch for the vault key, and all five must (D22). The mint waits `finalizeDepth` headers and `finalizeDelay` after the proof, so that honest feeders can replace a fake branch first (K3) | `tier2/`, `ZecuredIntake.attestDepositWithProof`, `finalizeActivation` |
| zZEC escaping its vault (transfers, flash loans, direct liquidation) | zZEC moves only Registry ↔ Morpho ↔ Adapter under a one-shot transient expectation | `contracts/src/ZZEC.sol`, invariant I2 |
| Three colluding signers | Tier 3: every payout is committed on Base before it is signed. Taking ZEC that backs loans, without a commitment or under a false one, or paying a claim to anyone but its owner, is provable with a zero-knowledge proof and slashes the signers' bonds (F1 to F4). A commitment that hides a payout from its claimant under a wrong reference is F4, provable by the borrower for its claims and by the two honest signers for any claim (red-team review, D38). With journal v2 a refund of a non-conforming note is a claim like any other (I30). Refusing to pay is answered by the bonds: a claim left unpaid for `livenessPeriod` is paid from them (T-05), and a payout confirmed but never sent can be disputed (D53). Minting does not need them (Tier 2). On Testnet both rest on weak proof of work | `ZecuredRegistry.commitSpend`, `ZecuredSlasher`, `SignerBondV2`, `ZecuredSpendLogic`, `node/src/signing.rs` |

## Known limitations (hackathon)

- All five signers, the coordinator and the bots are run by us (signer independence is a mock).
- Signer key material (FROST shares, vault `sk`) is stored unencrypted on each node's disk.
- The price oracle is a mock (`MockZecOracle` behind `DemoShockOracle`); USDC is Circle's testnet
  USDC on Base Sepolia (`MockUSDC` only on the local fork). Mainnet uses `ZecuredPriceFeed` over
  Chainlink and API3 instead (D55).
- Only one public light wallet server runs NU7 software today, so every node reads Zcash from it.
  A server that lies can delay payouts or hide a deposit from the signers' books; it cannot get a
  deposit minted, since every deposit is minted on a deposit proof verified on Base
  ([TIER2.md](TIER2.md)) by RISC Zero's verifier router (real Groth16 seals of a real deposit pass
  it in the fork tests, `RealGroth16Seal.t.sol`). Independent operators run their own (the witness
  mechanism is in place).
- The backend host holds the coordinator key, the demo token and the MOCK oracle key (one
  operator during the hackathon). Whoever holds the MOCK oracle key can crash the price and
  liquidate every vault: a real deployment needs a real oracle.
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
- [ ] No Base event or calldata with a Zcash txid, note commitment or address (or an unsalted hash of one); every recipient commitment salted
- [ ] No Base transaction about a vault before its deposit
- [ ] Demo control panel authenticated; `setPrice` owner-only
- [ ] Browser network tab: no third-party RPC / bundler / paymaster calls
- [ ] No address, viewing key, salt or IP in any log or watchdog alarm
- [ ] The whole git history passes a secret scanner before the repository goes public: `gitleaks git .` (clean on 2026-10-14, 136 commits, after allowing five reviewed public test values in `.gitleaksignore`; run it again on the final history)
- [ ] Backend and node logs: no addresses, viewing keys, salts or IPs
- [ ] Demo video: no real personal addresses
- [ ] README "What is real and what is a mock" up to date
