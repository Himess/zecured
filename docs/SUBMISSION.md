# Submission description (draft)

The form's description field, in the order the form asks for: what it does, why it does not leak,
what is real and what is a mock. To finalize with the deployment addresses, the status page and the
video link.

---

## What it does

Zecured is not a swap or a bridge: your ZEC never leaves Zcash's shielded Ironwood pool, and
nothing is sold. You deposit it into a fresh 3-of-5 FROST threshold vault in Ironwood. A
non-transferable receipt is then minted straight into a Morpho Blue position of a fresh, gasless
smart account on Base, and you borrow USDC against it. You close by repaying, or from collateral:
a market maker repays the debt and takes ZEC for it, and the rest returns to a new shielded
address in the same Zcash transaction. A liquidation pays the liquidator in shielded ZEC too.

Holding ZEC shielded no longer means giving up liquidity: you can borrow dollars on Base without
selling or unshielding it, and USDC lenders on Base gain a new market backed by Zcash collateral.

Base never has to trust the signers on either side:
- **Minting:** zZEC is minted only on a zero-knowledge proof (RISC Zero) that the deposit is in a
  Zcash block whose header an on-chain relay verified with Equihash.
- **Payouts:** signers must commit every payout on Base before they can sign it. Taking ZEC that
  backs loans (without a commitment, or under a false one), or paying any claim to anyone but its
  owner, is provable on Base with a zero-knowledge proof that reveals no Zcash transaction, note
  or address, and it slashes the signers' bonds.

Two things remain outside these proofs:
- refunds of non-conforming deposits;
- liveness: signers refusing to pay, in which case the funds stay in the vault.

## Why it does not leak

**Hidden:**
- **The ZEC stays shielded.** No transparent, Sapling or Orchard-pool input or output is ever
  accepted, so no pool turnstile reveals an amount.
- **Base never sees Zcash data.** No Base event or calldata holds a Zcash transaction id, note
  commitment or address. Recipients appear only as salted hashes; vault notes and payouts only
  as tags keyed with the vault's viewing key.
- **A deposit proof** reveals only a later header, never the deposit's block, transaction or
  note.
- **A fresh account per loan.** Each loan gets a fresh owner key and smart account that never
  holds ETH, so there is no funding trail. The browser talks only to our proxy, never to an RPC,
  bundler or paymaster provider.

**Public**, which we state as scope rather than leave for you to find:
- the Morpho position on Base: a fresh account's collateral and debt, its interest and any
  liquidation;
- the timing between a commitment on Base and its payout on Zcash, narrowed by a random delay;
- the testnet's small anonymity set;
- what the signers see: the vault's notes and the destinations it pays (they are run by us in
  this demo).

## What is real and what is a mock

**Real:**
- Zcash testnet (NU7) Ironwood transactions;
- 3-of-5 FROST threshold signing (re-randomized FROST over RedPallas, ZIP 2005 keys);
- every signer's per-payout policy checks;
- the Zcash header relay, which verifies Equihash on Base;
- the deposit and violation statements, run in the RISC Zero zkVM, and the guests' reproducible
  image ids;
- Morpho Blue and Circle's testnet USDC on Base Sepolia;
- our own RPC proxy, bundler and paymaster with a sponsorship allowlist;
- the bonds and the slasher.

**Not independent, or a demo convenience:**
- **The signers.** All five are run by the team. The bonds show the mechanism, not independent
  economic security.
- **Proof of work.** On Zcash testnet a single Equihash ASIC out-mines the whole network about
  40 times over, so every proof-of-work-based guarantee here (Tier 2 minting and Tier 3
  slashing) is demonstrative on testnet. On mainnet the same mechanisms inherit Zcash's proof of
  work, with confirmation depth chosen by the value at risk.
- **The price.** `MockZecOracle` stands behind `DemoShockOracle`, whose owner-set shock lets the
  demo show a liquidation.
- **The theft demonstration.** It is a separate deployment with its own signers and a 2-minute
  challenge window. It never touches the judge deployment.
