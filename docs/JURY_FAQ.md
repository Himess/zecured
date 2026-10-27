# Judges' FAQ

**Can I try it?**
Yes, four ways. Watch the video of the live run (https://youtu.be/iyXiao54KWk, 3:55). Open the app
as a guided replay (https://zecured.vercel.app/app/, DEMO): the same app in your browser, replaying our live run of
27 October in place of our servers, each step linked to its real transaction; you press the real buttons. Check a real proof with one `cast call`: RISC
Zero's router on Base Sepolia accepts the Groth16 proof of a real Zcash testnet deposit
([README](../README.md#for-judges-5-minutes), item 3). Or run the whole system on a local fork of Base Sepolia with
real Zcash testnet ([README](../README.md#running-it)). No hosted deployment runs through judging
([D82](DECISIONS.md)): it would need five signer nodes, a coordinator and a GPU prover for 25 days.
[TESTING.md](TESTING.md) records the end-to-end runs (txids and timings) and the real Groth16 seals of every
pinned guest, checked by RISC Zero's router.

**Isn't this just a multisig?**
It's a FROST threshold vault: one shielded address, signatures indistinguishable from a
single signer's, and every signer enforces a written policy on every payout. Two compromised
signers can't move funds: we ran it on Zcash testnet, two signers switched to "compromised" signed a
sweep, the other three refused, and nothing moved. Three that steal are slashed (below).

**How do I pay interest without linking my identity?**
Close from collateral: a market maker repays the debt on Morpho and takes ZEC from the vault;
the rest returns to a new shielded address in the same transaction. No outside funds ever
touch your account.

**What if three signers collude?**
- **They steal.** Signers must commit every payout on Base before they can sign it. Taking ZEC that backs loans
  (without a commitment, or under a false one), or paying any claim to anyone but its owner, is provable on Base
  with a zero-knowledge proof that reveals no Zcash transaction, note or address, and it slashes the signers' bonds
  ([TIER3.md](TIER3.md)); the bonds compensate the victims: liquidators and lenders before the borrower's own
  equity.
- **They stop paying.** The claimant is paid from the bonds 24 hours (72 on mainnet) after any signer acknowledged
  its destination.
- **They confirm a payout they never sent.** A confirmation settles nothing at once: its claimant can dispute it
  within its dispute window, and the claim is payable again unless the payout's inclusion in a Zcash block is
  proven.
- **They mint unbacked zZEC.** They can't: a deposit is activated only on a zero-knowledge proof (RISC Zero) that
  the note is in a Zcash block covered by a header our on-chain relay verified with Equihash, without revealing
  which block ([TIER2.md](TIER2.md)), and new collateral is accepted only while the bonds cover its value
  ([ECONOMICS.md](ECONOMICS.md)).

A deposit of any amount other than the vault's, or paid after its expiry, is proven as a refund and paid back like
any claim. What the design cannot stop: all five signers colluding.

On Zcash testnet a single Equihash ASIC out-mines the whole network about 40 times over, so every
proof-of-work-based guarantee here (Tier 2 minting and Tier 3 slashing) is demonstrative on
testnet. On mainnet the same mechanisms inherit Zcash's proof of work, with confirmation depth
chosen by the value at risk. The five demo signers are run by the team; the bonds demonstrate the
mechanism, not independent economic security.

**Why are all five bonds slashed, not only the thieves'?**
FROST makes three signers' signature look like one, so nobody can tell which three signed a theft: a proven theft
(F1) slashes all five bonds, and every signer has a reason to watch the others. A false payout record (F2) slashes
exactly the signers who signed it. On mainnet, new collateral is accepted only while all the ZEC locked is worth at
most half of the effective bond, and any three signers' bonds make up at least 3/5 of it, so the three it takes to
steal stand to lose at least 1.2 times what they could take. On testnet the bonds are demonstrative.

**Why Base?**
Morpho Blue lends there, and its cbZEC/USDC market shows the demand; Chainlink's and API3's ZEC/USD feeds are there
for mainnet; RISC Zero's verifier router is deployed there; and gas is cheap enough to check Zcash's proof of work
on-chain: a Zcash Mainnet header costs about 600,000 gas, about 0.010 ETH a day at NU7's 25-second blocks and a
base fee of 0.005 gwei ([MAINNET.md](MAINNET.md) § 3).

**How long does a loan take, and what does it cost?**
About eight minutes in our browser run on a fork: the signers saw the deposit 2 min 50 s after its broadcast, its
proof was on Base after 4 min 25 s (MOCK verifier on that fork; a real proof adds its GPU time, 28.1 s for a
deposit's STARK on one RTX 4090, and about a minute for its Groth16 wrap), and the loan was active after 7 min 56 s.
The account's gas is sponsored; the vault keeps a 0.5 % opening fee in ZEC (waived when it would be below 20,000
zatoshi), and the Morpho market sets the interest.

**Does a deposit proof leak anything on Base?**
It reveals a recent Zcash header (the anchor, not the deposit's block), the vault id, the
account, the denomination, a keyed tag and a hash that also depends on the vault's viewing key.
The deposit's block, transaction, note and address stay inside the proof.

**Is zZEC a wrapped ZEC?**
No. Here zZEC is Zecured's non-transferable collateral receipt, minted only into Morpho; it is
not a tradable wrapped ZEC. No account ever holds it: it can only move between Morpho, our
registry and the settlement adapter (enforced per transfer with a one-shot transient
expectation). Each position's zZEC equals exactly what its own vault holds, and the ZEC never
leaves the shielded pool.

**What happens after a partial liquidation?**
The vault tracks an outstanding amount. The liquidator gets only the seized ZEC; the rest stays
yours, and you take it back by repaying or closing from collateral.

**Is this a bridge?**
No. Neither product moves ZEC off Zcash or mints a tradable token for it. Borrowing keeps your ZEC
in a shielded vault and mints only a non-transferable collateral receipt into Morpho. The swap pays
real shielded ZEC on Zcash and releases escrowed USDC on Base on a proof of that payment: nothing is
wrapped, and nobody holds both sides.

**Isn't a Zcash light client on Base a bridge?**
It is the opposite of a lock-and-mint bridge: no asset moves through it and nobody holds coins for it. It lets Base
read Zcash headers, their proof of work checked on-chain, and verify zero-knowledge proofs about shielded
transactions under them. Zecured uses it to mint a non-transferable receipt and to release escrowed USDC; any
contract can use it to check a shielded payment.

**How does the swap work without a custodian?**
The taker's USDC is locked in `ZecuredSwap` for one order. The market maker pays shielded ZEC to the
taker's receiver with `"ZSWP" || orderId` at the start of the note's memo, then proves in the RISC
Zero zkVM that the note is in a Zcash block under a header the relay verified, that it pays the
order's receiver commitment, and at least the order's amount. The contract releases the USDC after
the same readiness wait as a deposit; with no valid proof before the order's deadline, the taker takes
the USDC back ([SWAP.md](SWAP.md)).

**Can one Zcash payment settle two orders?**
No. The memo names one order id, which binds the chain, the contract, the maker and its nonce; a
second order needs a second note, even to the same receiver.

**What if the market maker never pays?**
The taker loses time, not money: once the pay window has passed (at most 6 hours here) the USDC
comes back. A maker stake for mainnet is in [MAINNET.md](MAINNET.md).

**When is a payout final?**
When its inclusion in a Zcash block is proven on Base in zero knowledge (`PayoutProven`), or when its signed
confirmation's dispute window closes undisputed. The signers' confirmation is an optimistic first step that its
claimant can dispute, and a dispute is answered by the inclusion proof. The coordinator queues that proof for every
payout, and anyone can make it, with only the vault's tag key.

**Do you cross pools?**
No. Deposits must be Ironwood → Ironwood (no transparent, Sapling or Orchard-pool parts: the
signers check the full transaction); every payout goes to an Orchard-protocol receiver in the
Ironwood pool. Pool crossings reveal amounts through the turnstile.

**Is there an "Ironwood address"?**
Receivers are scoped to the Orchard protocol, not to a pool (ZIP 326). A vault address is a
unified address whose only receiver is the Orchard-protocol receiver; after NU6.3 it can only
be paid in the Ironwood pool.

**How are the vault keys generated?**
A 5-party FROST DKG per vault (re-randomized FROST over RedPallas). Following ZIP 2005's
"Usage with FROST", the signers agree on `sk` by commit-reveal and derive the viewing keys with
`use_qsk = true`, so the vault stays quantum-recoverable. Our derivation matches 8 independent
test vectors.

**Does every signer hold the vault's key?**
Each holds a FROST share of the spend authorizing key, which no node ever assembles, and the shared `sk` that ZIP
2005's FROST usage requires for the viewing keys: all five can see the vault, none can sign alone. The same `sk`
gives each signer the vault's quantum spending key `qsk`, so a quantum-recovery spend, if a future network upgrade
enables one, would need only one of them: an open point for the mainnet design.

**Who chooses FROST's randomizer?**
The PCZT fixes each spend's `alpha` (`rk = ak + [alpha]G` is in the sighash), and every signer verifies the PCZT
against its own plan before it draws its round-one nonces (`signer/crates/node/src/signing.rs`, `commit`), so a
coordinator cannot choose `alpha` after seeing the commitments ([D6](DECISIONS.md)).

**Can the coordinator cheat?**
It holds no key material. Every signer recomputes the payout plan from its own view of Zcash
and Base and verifies the coordinator's transaction against it: recipients, amounts, inputs,
change, fee, expiry, outgoing-viewing-key visibility, value balance, and that no signature can
be reused for another action. DKG secrets travel node-to-node encrypted.

**What exactly leaks?**
Loan size, collateral denomination, interest, liquidation events and the amount returned to
you at close, on Base: none of it names you or your wallet. Zcash recipients appear on Base only as salted
commitments. On testnet the anonymity sets are small: a deposit hides among the Ironwood transactions of about 3
blocks, a payout or a swap payment among those of 8. Full table in [THREAT_MODEL.md](THREAT_MODEL.md).

**Who liquidates, and when are they paid?**
Anyone, through the SettlementAdapter (zZEC cannot leave Morpho for an outside address). The liquidator repays USDC on
Morpho at once and receives the seized collateral, with Morpho's incentive (about 12.7 % at the 62.5 % LLTV), in
shielded ZEC by the vault's next payout. If the signers stall once one of them acknowledged its destination, the
bonds pay the claim's value plus 5 %, in USDC, after the liveness period (24 hours; 72 on mainnet).

**Did anything run on a public chain?**
Yes. On 27 October 2026 the system ran live on Base Sepolia: a deposit minted on its real Groth16 proof through RISC
Zero's deployed router, a sponsored borrow from an account that never held ETH, a close from collateral paid on
Zcash by 3-of-5 FROST with its delivery then proven on Base, and a swap settled on the proof of its Zcash payment
([TESTING.md](TESTING.md)). Before that, an earlier version of our Zcash header relay took real Testnet headers on
public Base Sepolia, Equihash checked on-chain, in 274 transactions up to Zcash height 4,491,413, the last on 12
October 2026 ([0xab07…8FA5](https://sepolia.basescan.org/address/0xab07317f7B9390394271D9c03FE2E912C0E58FA5)), and
RISC Zero's router there accepts the Groth16 proof of a real deposit today: the one-command check in the
[README](../README.md#for-judges-5-minutes). Liquidations, disputes, refunds and the theft demonstration ran on Base
Sepolia forks.

**Why wait for NU7 on mainnet?**
Nothing in the design needs NU7: the mainnet deposit guest proves NU6.3's and NU7's epochs. The beta waits for NU7's
Mainnet height so that the relay is deployed once with both block spacings (75 s, then 25 s) and ZIP 221's history
tree restarting at the activation ([MAINNET.md](MAINNET.md)).

**What's mocked?**
The ZEC price (`MockZecOracle`, behind `DemoShockOracle`, whose owner-set shock shows a liquidation); the loan token
(MockUSDC, a test token we mint); the verifier of the local-fork runs (`MockRiscZeroVerifier`; the live Base
Sepolia run and the fork tests use RISC Zero's router); the local fork's
paymaster (Pimlico's mock paymaster, behind our allowlist); and signer independence (all five nodes run by us; the
bonds show the mechanism). The demo wallet, the theft demonstration (a separate deployment with a 2-minute
challenge window) and the app's replay are labelled DEMO. Real: Zcash testnet (NU7) Ironwood transactions, FROST
threshold signing, the zero-knowledge statements and their seals, the on-chain Zcash header relay, Morpho Blue's own
deployment (on Base Sepolia, live and on forks), and gasless smart accounts through our own proxy and allowlist. The full list
is in the [README](../README.md#what-is-real-and-what-is-a-mock).
