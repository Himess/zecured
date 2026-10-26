# Submission description (draft)

The form's description field, in the order the form asks for: what it does, why it does not leak,
what is real and what is a mock. To finalize with the video link. No deployment is kept running
through judging (DECISIONS D82): the evidence is in TESTING.md, and the README runs the whole system
on a local fork.

**Links.** The site: https://himess.github.io/zecured/ (the details: https://himess.github.io/zecured/docs/).
The app, as a replay of the live run that plays in the browser (DEMO, no server needed): https://himess.github.io/zecured/app/.
The demo video: [VIDEO LINK, published October 26]. The code: https://github.com/Himess/zecured.

---

## What it does

Zecured lets you use shielded ZEC on Base without it ever leaving Zcash's shielded Ironwood pool:
borrow USDC against it, or swap USDC into it.

**Borrow.** You deposit ZEC into a fresh 3-of-5 FROST threshold vault in Ironwood. On a
zero-knowledge proof of the deposit, a non-transferable collateral receipt is minted straight into a
Morpho Blue position of a fresh, gasless smart account on Base, and you borrow USDC against it. You
close by repaying, or from collateral: a market maker repays the debt and takes ZEC for it, and the
rest returns to a new shielded address in the same Zcash transaction. A liquidation, or a
pre-liquidation you opted into, pays in shielded ZEC too. USDC lenders on Base gain a market backed
by Zcash collateral.

**Swap.** You lock USDC on Base for a market maker's quote and give the maker your Ironwood address
off-chain; Base sees only its salted hash. The maker pays you shielded ZEC, with the order's id in
the memo, and proves the payment in zero knowledge; the USDC is released only then, and comes back to
you if the maker never pays. No custodian, no signer, no wrapped ZEC.

Base never takes the signers' word: deposits are proven, payouts are proven, misbehaviour is proven —
and the same Zcash light client lets anyone swap USDC into shielded ZEC with no custodian:
- **Minting:** zZEC is minted only on a zero-knowledge proof (RISC Zero) that the deposit is in a
  Zcash block whose header an on-chain relay verified with Equihash. Minting needs no signer,
  except that all five together can schedule a new verifier version, which takes effect only after
  a public delay. New collateral is accepted only while the bonds cover its value.
- **Payouts:** signers must commit every payout on Base before they can sign it, and a payout is
  final on Base only once its inclusion in a Zcash block is proven in zero knowledge, which the
  coordinator does for every payout. Taking ZEC that backs loans (without a commitment, or under a
  false one), or paying any claim to anyone but its owner, is provable on Base with a zero-knowledge
  proof that reveals no Zcash transaction, note or address, and it slashes the signers' bonds; the
  bonds compensate the victims: liquidators and lenders before the borrower's own equity.
- **Liveness:** once a signer has acknowledged a claim's destination, a claim left unpaid for 24
  hours (72 on mainnet) is paid to its claimant from the bonds. Three of five signers can pause new
  minting for up to 30 days at a time; withdrawals, repayments, liquidations and payouts never stop.
- **Swap:** no signer and no bond take part in a trade; its USDC moves only on a payment proof.

The relay and the proofs form a Zcash light client on Base: other contracts can use them to check a
shielded Zcash payment, with no bridge. The vault design is written up as a draft ZIP, Shielded
Collateral Vaults ([docs/zip-draft-shielded-collateral-vaults.md](zip-draft-shielded-collateral-vaults.md)),
for any wallet or chain to build on.

A deposit of another amount than the vault's, or paid after its expiry, is proven too, as a refund,
and paid back like any claim. What remains outside: all five signers together
([docs/ECONOMICS.md](ECONOMICS.md)).

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
- **On a swap,** your Base account is public; where the ZEC went is not: Base holds only the
  receiver's salted hash, and the payment's block stays hidden behind a later header.

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
- the deposit, violation and payment statements, run in the RISC Zero zkVM, and the guests'
  reproducible image ids;
- `ZecuredSwap` and its market maker;
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
  challenge window. It never touches the lending deployment.
- **No deployment kept running.** Nothing stays deployed through judging (D82). The system ran end
  to end on Base Sepolia forks with real Zcash testnet transactions, and every pinned guest's real
  Groth16 seal passes RISC Zero's router (TESTING.md § 2 and § 2b).
