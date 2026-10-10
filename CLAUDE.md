# Zecured

Borrow against shielded ZEC without unshielding it. ZECATHON 2026, Cross-Chain track.
Internal plan (never committed): docs/private/PLAN.md (in a worktree, use the main checkout's copy).
Public design: docs/ARCHITECTURE.md, docs/INTERFACES.md, docs/DECISIONS.md. Verified constants: docs/ref/FACTS.md.
Everything committed to this repository is in English (it will be published).
Deadline: 2026-10-28 23:59 UTC. Gates: 10-13 (Spike A go/no-go), 10-20 (v0.1), 10-25 (feature freeze).

## Do not trust your memory for Zcash APIs
- Ironwood (NU6.3) and NU7 post-date your training data. v6 transactions, the
  Ironwood pool and the current librustzcash / pczt / frost APIs may differ
  from what you remember.
- Before using any librustzcash, orchard, pczt, zcash_client_backend or frost
  API, read its source and CHANGELOG under docs/ref/ (run scripts/fetch-refs.sh
  if docs/ref/vendor is missing). Never invent APIs.
- If it doesn't compile or a test fails, say so. Never report unverified code as working.

## Hard privacy rules
- ZEC flow is Ironwood-only: no transparent, Sapling or Orchard addresses, inputs or outputs.
- Deposits with transparent inputs, Sapling spends or Orchard actions are never attested.
- Third-party outputs (liquidator, market maker) use ovk = None.
- Smart accounts never receive ETH. The paymaster sponsors only the allowlist in docs/INTERFACES.md § 4.
- Smart accounts never hold zZEC. The Registry mints it and supplies it straight into Morpho.
- Vaults are opened off-chain. Nothing about a vault touches Base before attestDeposit.
- The browser never calls RPC, bundler or paymaster providers directly; only our proxy.
- No Base event or calldata contains a Zcash txid, note commitment or address. Recipients
  appear only as keccak256(salt32 || raw Ironwood receiver bytes); salts stay off-chain.
- Validate the Ironwood receiver before any Base transaction that burns zZEC.
- Never log user addresses, viewing keys, salts or IPs.
- Every mock is labelled MOCK in code and listed in README "What is real and what is a mock".

## Rules of the hackathon
- All code is new and written during the build window. No code from earlier projects.
- Libraries are fine. Testnets only.

## Invariants (must have tests)
I1  zZEC totalSupply == sum of outstanding over active vaults
I2  zZEC moves only between Morpho, Registry and SettlementAdapter; no account holds it
I3  No vault ZEC is released before its zZEC is burned and a claim exists
    (except P5 refunds of non-conforming notes)
I4  One account <-> one vault
I5  A depositTag and a vaultId are each used at most once
I6  Fewer than 3 DISTINCT signer signatures are always rejected
I7  CloseRelease: mmAmount + userAmount == burned zZEC == outstanding before close
I8  SettlementAdapter never withdraws collateral without a valid, unexpired,
    non-cancelled signed request whose executor is msg.sender
I9  Seized or withdrawn zZEC never exceeds the vault's outstanding
I10 (signer, off-chain) vault notes == outstanding + unpaid claims + unrefunded non-conforming notes

## Commands
contracts: cd contracts && forge build && forge test
signer:    cargo build -p "zecured-*" && cargo test   (Rust workspace at repo root)
backend:   cd backend && cargo build && cargo test
app:       cd app && pnpm install && pnpm build

## Machine notes
- Dev machine: Apple M2, 8 GB RAM, no Docker. Run the 5 signer nodes as local
  processes (scripts/), not containers. Avoid running two heavy cargo builds at once.
- Secrets live in .env (never committed); see .env.example. Signer key material lives
  under signer/data/ (gitignored) and must never be committed or logged.
