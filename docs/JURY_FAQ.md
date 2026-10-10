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
That's our hackathon trust assumption, stated openly. Roadmap: independent operators, bonds
on Base, automatic slashing via ZK fraud proofs, Base-side verification of Zcash deposits.
Meanwhile you can watch your vault with its viewing key.

**Isn't zZEC just wrapped ZEC like renZEC?**
No. No account ever holds zZEC: it is minted straight into one Morpho position and can only
move between Morpho, our registry and the settlement adapter (enforced per transfer with a
one-shot transient expectation). Each position's zZEC equals exactly what its own vault holds,
and the ZEC never leaves the shielded pool.

**What happens after a partial liquidation?**
The vault tracks an outstanding amount. The liquidator gets only the seized ZEC; the rest stays
yours, and you take it back by repaying or closing from collateral.

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
Price oracle, USDC, and signer independence (all five nodes run by us). Everything else is real:
Zcash testnet (NU7) Ironwood transactions, FROST threshold signing, Morpho Blue on Base Sepolia,
gasless smart accounts through our own proxy.
