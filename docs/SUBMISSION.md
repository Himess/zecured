# Submission description

The text for the submission form, in the order the form asks for: what it does, why it does not leak, what is real
and what is a mock. Every link is absolute, so it works from the form.

**Links.** The site: https://himess.github.io/zecured/ (the details: https://himess.github.io/zecured/docs/). The
app as a guided replay in your browser, with sample data (DEMO, no server needed): https://himess.github.io/zecured/app/.
The code: https://github.com/Himess/zecured.

**Team.** Himess (https://github.com/Himess). Built from scratch for ZECATHON 2026, Cross-Chain track; first commit
10 October 2026.

## Short version

**Zecured: borrow against shielded ZEC. Only the proof crosses.** Borrow USDC on Base against ZEC that never leaves
Zcash's shielded pool, or swap USDC into shielded ZEC. Base sees zero-knowledge proofs, never your Zcash address,
transaction or balance. The ZEC sits in a fresh 3-of-5 FROST vault; Base verifies Zcash's headers itself and mints
a non-transferable collateral receipt on a RISC Zero proof of the deposit; a brand-new, gasless account borrows on
Morpho Blue. Testnet only: the five signers are run by the team, and the ZEC price, the loan token (MockUSDC) and
the verifier in our end-to-end runs are mocks; a real proof of a real deposit passes RISC Zero's live router in one
command.

## For judges (5 minutes)

- **Watch** the two-minute video. Its link goes here as soon as it is published.
- **Click through** the app as a guided replay in your browser, with sample data in place of our servers (DEMO):
  https://himess.github.io/zecured/app/
- **Check a real proof in one command** (Foundry's `cast`, nothing to clone): RISC Zero's verifier router as deployed
  on Base Sepolia accepts the Groth16 proof of a real Zcash testnet deposit (the TEST-ONLY vault 9, block 4,488,543):

  ```bash
  cast call 0x0b144E07A0826182B6b59788c34b32Bfa86Fb711 "verify(bytes,bytes32,bytes32)" \
    $(curl -sL https://himess.github.io/zecured/evidence/deposit-test.seal) \
    0x406c6d268f29270226c775db1fe4bec04acf51ba919e250078125d71c10bff41 \
    0x1a9dcbc10182a251b4e24fdc01740df36a90f7eb9397cb89be1a9316d35867f4 \
    --rpc-url https://sepolia.base.org && echo "accepted by RISC Zero's router"
  ```

  Change one digit of the last hash and the router reverts. Every guest, with minting and slashing through the real
  router, on a fork: `cd contracts && forge test --match-path test/fork/RealSealsV2.t.sol --isolate`.
- **Real and mock:** Zcash testnet transactions, the FROST vaults, the on-chain Zcash header relay, the RISC Zero
  proofs and Morpho Blue are real; the ZEC price, the loan token (MockUSDC), the verifier inside the end-to-end runs
  and the signers' independence are not.
- **Details:** what ran and how to reproduce it, https://github.com/Himess/zecured/blob/main/docs/TESTING.md; the
  judges' FAQ, https://github.com/Himess/zecured/blob/main/docs/JURY_FAQ.md.

---

## What it does

Zecured lets you borrow USDC on Base against ZEC that never leaves Ironwood, Zcash's newest shielded pool, and swap
USDC into shielded ZEC, with no bridge and no wrapped ZEC. Using ZEC in DeFi usually means unshielding it: a
transparent address and an amount go public, a bridge holds the coins and mints a copy that matches the deposit, and
the gas for a new wallet ties it to an old one. Zecured removes all three. The ZEC sits in a fresh 3-of-5 FROST
threshold vault; Base verifies Zcash's block headers (Equihash) itself and mints a non-transferable collateral
receipt only on a RISC Zero proof that the deposit is in one of those blocks; a brand-new, gasless smart account
borrows USDC on Morpho Blue. The same Zcash light client releases a swap's escrowed USDC only on a zero-knowledge
proof that the market maker paid your shielded address.

People already borrow against ZEC on Base: Morpho's cbZEC/USDC market, backed by Coinbase's custodial wrapped ZEC
and public on-chain, held about USD 2.5 M of supplied USDC when we read it on 16 October 2026. Zecured is that
market for shielded ZEC.

Built from scratch for ZECATHON 2026 (Cross-Chain track), first commit 10 October: 804 contract tests, 35
invariants, 22 symbolic checks, real Groth16 proofs of every guest accepted by RISC Zero's router on Base forks,
end-to-end runs with real Zcash testnet transactions, and a draft ZIP for shielded collateral vaults. Try it: the
two-minute video, the app as a guided replay in your browser (https://himess.github.io/zecured/app/), or the whole
system on a local fork (https://github.com/Himess/zecured#running-it).

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

**Who you have to trust.** Base never takes the signers' word: deposits are proven, payouts are proven, and
misbehaviour is proven.
- **Minting:** zZEC is minted only on a zero-knowledge proof (RISC Zero) that the deposit is in a
  Zcash block whose header an on-chain relay verified with Equihash. Minting needs no signer,
  except that all five together can schedule a new verifier version, which takes effect only after
  a public delay. New collateral is accepted only while the bonds cover its value.
- **Payouts:** signers must commit every payout on Base before they can sign it, and a payout is final once its
  inclusion in a Zcash block is proven in zero knowledge, or once its dispute window closes undisputed: the signed
  confirmation is an optimistic first step, and the coordinator queues an inclusion proof for every payout. Taking
  ZEC that backs loans (without a commitment, or under a false one), or paying any claim to anyone but its owner,
  is provable on Base with a zero-knowledge proof that reveals no Zcash transaction, note or address, and it
  slashes the signers' bonds; the bonds compensate the victims: liquidators and lenders before the borrower's own
  equity.
- **Liveness:** once a signer has acknowledged a claim's destination, a claim left unpaid for 24
  hours (72 on mainnet) is paid to its claimant from the bonds. Three of five signers can pause new
  minting for up to 30 days at a time; withdrawals, repayments, liquidations and payouts never stop.
- **Swap:** no signer and no bond take part in a trade; its USDC moves only on a payment proof.

**Why stealing does not pay.** On mainnet, new collateral is accepted only while all the ZEC locked is worth at most
half of the signers' effective bond, and any three signers' bonds make up at least 3/5 of it: the three it takes to
steal stand to lose at least 1.2 times what they could take, and since FROST hides which three signed, all five are
slashed. On testnet the bonds are demonstrative.

The relay and the proofs form a Zcash light client on Base: other contracts can use them to check a
shielded Zcash payment, with no bridge. The vault design is written up as a draft ZIP, Shielded
Collateral Vaults (https://github.com/Himess/zecured/blob/main/docs/zip-draft-shielded-collateral-vaults.md),
for any wallet or chain to build on.

A deposit of any amount other than the vault's, or paid after its expiry, is proven too, as a refund, and paid back
like any claim. What the design cannot stop: all five signers colluding
(https://github.com/Himess/zecured/blob/main/docs/ECONOMICS.md).

## Why it does not leak

**Hidden:**
- **The ZEC stays shielded.** No transparent, Sapling or Orchard-pool input or output is ever
  accepted, so no pool turnstile reveals an amount.
- **Base never sees Zcash data.** No Base event or calldata holds a Zcash transaction id, note
  commitment or address. Recipients appear only as salted hashes; vault notes and payouts only
  as tags keyed with the vault's viewing key.
- **A deposit proof** reveals a later header, never the deposit's block, transaction or note; besides it, Base sees
  only the vault id, its account, the standard amount, a keyed tag and the hash of the vault's signed opening
  message.
- **A fresh account per loan.** Each loan gets a fresh owner key and smart account that never
  holds ETH, so there is no funding trail. The browser talks only to our proxy, never to an RPC,
  bundler or paymaster provider.
- **On a swap,** your Base account is public; where the ZEC went is not: Base holds only the
  receiver's salted hash, and the payment's block stays hidden behind a later header.

**Public**, which we state as scope rather than leave for you to find:
- the Morpho position on Base: a fresh account's collateral and debt, its interest and any
  liquidation;
- the timing between a commitment on Base and its payout on Zcash, blurred by a random delay;
- the anonymity sets, which are small on testnet: a deposit's proof places it among the Ironwood transactions of
  about 3 blocks (a random 0 to 2 extra confirmations), a payout or a swap payment among those of 8 blocks;
- what the signers see: the vault's notes and the destinations it pays (they are run by us in
  this demo).

## What is real and what is a mock

**Real:**
- Zcash testnet (NU7) Ironwood transactions;
- 3-of-5 FROST threshold signing (re-randomized FROST over RedPallas, ZIP 2005 keys);
- every signer's per-payout policy checks;
- the Zcash header relay, which verifies Equihash on Base (an earlier version took real Testnet headers on public
  Base Sepolia in 274 transactions, the last on 12 October 2026: 0xab07317f7B9390394271D9c03FE2E912C0E58FA5);
- the deposit, violation and payment statements, run in the RISC Zero zkVM, and the guests'
  reproducible image ids;
- real Groth16 seals of every pinned guest, accepted by RISC Zero's deployed verifier router on Base Sepolia and
  Base mainnet forks, and the deposit's seal by the live router itself (the one-command check above);
- `ZecuredSwap` and its market maker;
- Morpho Blue's own deployment on Base Sepolia (on forks);
- our own RPC proxy, bundler and paymaster with a sponsorship allowlist;
- the bonds and the slasher.

**Mock, demo, or not independent:**
- **The signers.** All five are run by the team. The bonds show the mechanism, not independent
  economic security.
- **Proof of work.** On Zcash testnet a single Equihash ASIC out-mines the whole network about
  40 times over, so every proof-of-work-based guarantee here (Tier 2 minting and Tier 3
  slashing) is demonstrative on testnet. On mainnet the same mechanisms inherit Zcash's proof of
  work, with confirmation depth chosen by the value at risk.
- **The price.** `MockZecOracle` stands behind `DemoShockOracle`, whose owner-set shock lets the
  demo show a liquidation.
- **The loan token.** MockUSDC, a test token we mint (MOCK: no value). The contracts take any 6-decimal USDC; the
  mainnet configuration names Circle's USDC.
- **The verifier in the end-to-end runs.** `MockRiscZeroVerifier` (MOCK), since RISC Zero does not run on our
  Windows machine; the real seals pass the real router separately (above).
- **Sponsorship on the local fork.** Pimlico's mock paymaster, behind our proxy's allowlist.
- **The demo wallet (DEMO).** On testnet it pays a vault real TAZ for a visitor without testnet ZEC.
- **The theft demonstration (DEMO).** It is a separate deployment with its own signers and a 2-minute
  challenge window. It never touches the lending deployment.
- **The app's replay (DEMO).** A static build of the app whose in-browser simulator plays sample data in place of
  our servers; it carries no private key, salt or viewing key.
- **No deployment kept running.** Nothing stays deployed through judging (D82). The system ran end to end on Base
  Sepolia forks with real Zcash testnet transactions, checking proofs with a MOCK verifier; separately, the real
  Groth16 seal of every pinned guest passes RISC Zero's deployed router
  (https://github.com/Himess/zecured/blob/main/docs/TESTING.md, § 2 and § 2b).
