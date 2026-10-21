# Jury FAQ

**Isn't this just a multisig?**
It's a FROST threshold vault: one shielded address, signatures indistinguishable from a
single signer's, and every signer enforces a written policy on every payout. Two compromised
signers can't move funds — we show it live: two signers switched to "compromised" sign a
sweep, the other three refuse, nothing moves.

**How do I pay interest without linking my identity?**
Close from collateral: a market maker repays the debt on Morpho and takes ZEC from the vault;
the rest returns to a new shielded address in the same transaction. No outside funds ever
touch your account.

**What if three signers collude?**
Signers must commit every payout on Base before they can sign it. Taking ZEC that backs loans —
without a commitment or under a false one — or paying any claim to anyone but its owner is provable
on Base with a zero-knowledge proof that reveals no Zcash transaction, note or address, and it
slashes the signers' bonds ([TIER3.md](TIER3.md)); the bonds compensate the victims: liquidators and
lenders before the borrower's own equity. If they just stop paying, the claimant is paid from the
bonds 24 hours (72 on mainnet) after any signer acknowledged its destination; and a payout is final
on Base only once its inclusion in a Zcash block is proven, so confirming a payout they never sent
settles nothing, and its claimant can dispute it. Nor can
they mint unbacked zZEC: a deposit is activated on a zero-knowledge proof (RISC Zero) that the note
is in a Zcash block committed by a header our on-chain relay verified with Equihash, without
revealing which block ([TIER2.md](TIER2.md)), and new collateral is accepted only while the bonds
cover its value ([ECONOMICS.md](ECONOMICS.md)). A deposit of another amount than the vault's, or
paid after its expiry, is proven as a refund and paid back like any claim. What remains outside:
all five signers together.

On Zcash testnet a single Equihash ASIC out-mines the whole network about 40 times over, so every
proof-of-work-based guarantee here — Tier 2 minting and Tier 3 slashing — is demonstrative on
testnet. On mainnet the same mechanisms inherit Zcash's proof of work, with confirmation depth
chosen by the value at risk. The five demo signers are run by the team; the bonds demonstrate the
mechanism, not independent economic security.

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
When its inclusion in a Zcash block is proven on Base (`PayoutProven`). The signers' signed
confirmation comes first and can be disputed by its claimant; the coordinator proves every payout,
and anyone can, with only the vault's tag key.

**Do you cross pools?**
No. Deposits must be Ironwood → Ironwood (no transparent, Sapling or Orchard-pool parts — the
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

**Can the coordinator cheat?**
It holds no key material. Every signer recomputes the payout plan from its own view of Zcash
and Base and verifies the coordinator's transaction against it — recipients, amounts, inputs,
change, fee, expiry, outgoing-viewing-key visibility, value balance, and that no signature can
be reused for another action. DKG secrets travel node-to-node encrypted.

**What exactly leaks?**
Loan size, collateral denomination, interest, liquidation events and the amount returned to
you at close, on Base — unlinkable to you. Zcash recipients appear on Base only as salted
commitments. Full table in [THREAT_MODEL.md](THREAT_MODEL.md).

**Can I try it live?**
Not on a hosted deployment: none is kept running through judging ([D82](DECISIONS.md)). The
README runs the whole system, app included, on a local fork of Base Sepolia with real Zcash
testnet; [TESTING.md](TESTING.md) records the end-to-end runs (short txids and timings) and the
real Groth16 seals of every pinned guest, checked by RISC Zero's router.

**What's mocked?**
The price oracle (`MockZecOracle`, behind `DemoShockOracle`, whose owner-set shock shows a
liquidation) and signer independence (all five nodes run by us; the bonds show the mechanism). The
theft demonstration is a separate deployment with a 2-minute challenge window. Everything else is
real: Zcash testnet (NU7) Ironwood transactions, FROST threshold signing, the zero-knowledge deposit
and violation proofs, the on-chain Zcash header relay, Morpho Blue's own deployment (on Base Sepolia
forks, where a MockUSDC stands in for Circle's testnet USDC), gasless smart accounts through our own
proxy, bundler and paymaster.
