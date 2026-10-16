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
bonds after 24 hours (72 on mainnet); a payout they confirm but never send can be disputed. Nor can
they mint unbacked zZEC: a deposit is activated on a zero-knowledge proof (RISC Zero) that the note
is in a Zcash block committed by a header our on-chain relay verified with Equihash, without
revealing which block ([TIER2.md](TIER2.md)), and new collateral is accepted only while the bonds
cover its value ([ECONOMICS.md](ECONOMICS.md)). What remains outside: deposits of another amount
than the vault's, refunded off-chain on the owner's request until the deployment takes refund
proofs, and all five signers together.

On Zcash testnet a single Equihash ASIC out-mines the whole network about 40 times over, so every
proof-of-work-based guarantee here — Tier 2 minting and Tier 3 slashing — is demonstrative on
testnet. On mainnet the same mechanisms inherit Zcash's proof of work, with confirmation depth
chosen by the value at risk. The five demo signers are run by the team; the bonds demonstrate the
mechanism, not independent economic security.

**Does a deposit proof leak anything on Base?**
It reveals a recent Zcash header (the anchor, not the deposit's block), the vault id, the
account, the denomination, a keyed tag and a hash that also depends on the vault's viewing key.
The deposit's block, transaction, note and address stay inside the proof.

**Isn't zZEC just wrapped ZEC like renZEC?**
No. No account ever holds zZEC: it is minted straight into one Morpho position and can only
move between Morpho, our registry and the settlement adapter (enforced per transfer with a
one-shot transient expectation). Each position's zZEC equals exactly what its own vault holds,
and the ZEC never leaves the shielded pool.

**What happens after a partial liquidation?**
The vault tracks an outstanding amount. The liquidator gets only the seized ZEC; the rest stays
yours, and you take it back by repaying or closing from collateral.

**Is this a bridge or a swap?**
No. The ZEC never leaves Zcash's shielded Ironwood pool, and nothing is sold. Holding ZEC shielded
no longer means giving up liquidity: you can borrow dollars on Base without selling or unshielding
it, and USDC lenders on Base gain a new market backed by Zcash collateral.

**Why not NEAR Intents?**
Today's Zcash-origin bridge routes typically require a transparent sender. That's exactly the
unshielding step we avoid.

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

**What's mocked?**
The price oracle (`MockZecOracle`, behind `DemoShockOracle`, whose owner-set shock shows a
liquidation) and signer independence (all five nodes run by us; the bonds show the mechanism). The
theft demonstration is a separate deployment with a 2-minute challenge window. Everything else is
real: Zcash testnet (NU7) Ironwood transactions, FROST threshold signing, the zero-knowledge deposit
and violation proofs, the on-chain Zcash header relay, Morpho Blue and Circle's testnet USDC on Base
Sepolia, gasless smart accounts through our own proxy, bundler and paymaster.
