# Tier 2: proving deposits on Base without trusting the signers

Status: design, plus a first building block (the header relay in `contracts/src/relay/`). Nothing here is wired into
the Registry. "Verified" below means checked against a primary source (ZIP text, Zebra or librustzcash source, or
live chain data); "inferred" means reasoned from those sources but not yet tested end to end.

## 1. Where we are and what changes

Today (Tier 1) a deposit becomes zZEC when 3 of 5 signers sign a `DepositAttestation(vaultId, account, amount,
depositTag)` and `ZecuredRegistry.attestDeposit` checks the signatures. Two trust assumptions sit inside that step:

- **The signers.** Three colluding signers can attest a deposit that does not exist and mint unbacked zZEC.
- **Their data source.** All five read the chain through the same light wallet server (`testnet.zec.rocks`, the only
  public NU7 endpoint; THREAT_MODEL "Known limitations"). A lying server could make honest signers attest a fake
  deposit.

Tier 2 replaces the attestation with a proof checked on Base: zZEC is minted only if a note of the right value,
addressed to the vault, is committed in a Zcash block that is on the proof-of-work-verified best chain and buried
deeply enough. The payout side (FROST signers releasing ZEC) is unchanged; see § 6.

## 2. The statement

A deposit proof for vault V convinces the Registry of:

> There is an Ironwood note *n* = (d, pk_d, v, ρ, ψ, rcm) with v = the vault's denomination, addressed to vault V's
> Orchard-protocol receiver, whose commitment cmx is a leaf of the Ironwood note commitment tree whose root
> rt is the final Ironwood tree root of some block m; block m is committed by the header H, H is on the relay's best
> chain with enough work on top of it, and t = Tag(n, V).

| | Values |
|---|---|
| **Public inputs** | header hash H (or its `hashBlockCommitments`); vault key commitment K (§ 5); denomination v; deposit tag t |
| **Private witness** | the note (d, pk_d, v, ρ, ψ, rcm, or rseed); its tree position and 32 Merkle siblings; the vault's full viewing key (ak, nk, rivk); the history-tree (MMR) leaf of block m, its path and peaks; H's `hashChainHistoryRoot` and `hashAuthDataRoot` |

Relations checked inside the proof:

1. **Note commitment.** cm = NoteCommit^Orchard_rcm(g_d, pk_d, v, ρ, ψ), cmx = Extract_P(cm). ZIP 2005 leaves
   NoteCommit^Orchard and DeriveNullifier unchanged for Ironwood notes; it only changes how rcm is derived from rseed
   (lead byte `0x03`, `pre_rcm` over g_d, pk_d, v, ρ, ψ; ZIP 2005 § "Changes to the Protocol Specification"). The
   circuit can take rcm as witness, as the Orchard action circuit does. (Verified, ZIP 2005.)
2. **Merkle path.** cmx is at the given position of a depth-32 tree with root rt, using MerkleCRH^Orchard (Sinsemilla).
   The Ironwood tree is a second Orchard-shaped tree: ZIP 258 caps it at 2^MerkleDepth^Orchard leaves, the `orchard`
   crate (0.16, vendored `616a669`) has no Ironwood-specific tree code, and Zebra stores its root as an
   `orchard::tree::Root`. (Verified in sources; the empty-leaf value being the Orchard one is inferred.)
3. **Recipient is the vault.** ivk = Commit^ivk_rivk(Extract_P(ak), nk), g_d = DiversifyHash(d), pk_d = [ivk] g_d,
   and K = Hash(ak, nk, rivk). Only the full viewing key is needed, not any spending key.
4. **Anchor is committed by H.** rt equals `hashLatestIronwoodRoot` of the MMR leaf of block m; that leaf is in the MMR
   whose root is `hashChainHistoryRoot`; and H's `hashBlockCommitments` = BLAKE2b-256("ZcashBlockCommit",
   hashChainHistoryRoot ‖ hashAuthDataRoot ‖ [0]^32). See § 3.
5. **Tag.** t = Tag(n, V) as in § 4.

The Registry then checks: `relay.isInBestChain(H)` and `relay.confirmationWork(H)` ≥ W_min (§ 6); K is the attested
key of `vaultId` (§ 5); t and `vaultId` are unused (I5); then it mints exactly as `attestDeposit` does today.

**Confirmations.** The note is in the tree of block m, and m precedes H, so whatever burial the Registry requires of
H holds at least as strongly for the note's block. Any Ironwood note, even one created before NU7, can be proven
against a post-NU7 anchor: the Ironwood tree is cumulative since NU6.3.

## 3. How the Ironwood root reaches Base

This is the crux, and the answer is positive: **since NU6.3, every block header commits to the Ironwood tree root of
every earlier block of the same network-upgrade epoch.**

**Verified:**

- ZIP 221 defines the chain history tree, a Merkle mountain range (MMR) with one leaf per block. Each node carries
  metadata and is hashed with BLAKE2b-256, personalization `"ZcashHistory" ‖ le32(CONSENSUS_BRANCH_ID)`. The header
  of block B_n commits to the MMR over B_x .. B_{n−1}, where x is the most recent upgrade's activation height
  (ZIP 221 § "Tree Node specification", § "Block header semantics").
- ZIP 244 § "Block Header Changes": since NU5, `hashBlockCommitments` = BLAKE2b-256 with personalization
  `"ZcashBlockCommit"` over hashLightClientRoot (= hashChainHistoryRoot) ‖ hashAuthDataRoot ‖ [0u8; 32].
- ZIP 258 § "Changes to ZIP 221" (NU6.3) adds three fields to every node: `hashEarliestIronwoodRoot`,
  `hashLatestIronwoodRoot` (for a leaf: "the note commitment root of the final Ironwood treestate" of that block) and
  `nIronwoodTxCount`. It states that `hashBlockCommitments` "is unaffected beyond this". Nodes are 277–317 bytes.
- NU7 changes nothing here: none of the ZIPs it deploys (ZIP 259: 207 rev. 2, 214 rev. 3, 218, 235, 237, 2003,
  2008) mentions the history tree or `hashBlockCommitments`, and Zebra uses the same `V3` trees for NU7.
- Implementation: `zcash_history` 0.5.0 (2026-07-09) adds `V3` node data with the Ironwood fields
  (`MAX_NODE_DATA_SIZE` = 317; librustzcash `3c76c2d`). Zebra v7.0.0-rc.0 (`6d1e414d6f`) builds `V3`
  ("IronwoodOnward") trees for NU6.3 and NU7 (`zebra-chain/src/history_tree.rs`), fills the leaf from the block's
  final Ironwood root (`primitives/zcash_history.rs`), starts a new tree at each upgrade's activation block, and
  validates `hashBlockCommitments` from the previous block's history root and the block's auth data root
  (`zebra-state/src/service/check.rs`, `block_commitment_is_valid_for_chain_history`).
- Data availability: the light wallet protocol returns Ironwood tree states (`TreeState.ironwoodTree`, field 7;
  `ShieldedProtocol.ironwood = 2` for subtree roots). `GetTreeState` on `testnet.zec.rocks` returns a 322-byte
  Ironwood frontier at height 4,488,000.

**Inferred, not yet tested:**

- I have not recomputed a real `hashBlockCommitments` from chain data. That needs, for every block of the epoch, its
  hash, time, nBits, the three tree roots and three transaction counts, plus the block's `hashAuthDataRoot`
  (ZIP 244 auth digests of all its transactions). All of it is public (full blocks, or compact blocks plus tree
  states). This is the first task of phase 2 (§ 7), and it must be checked against real Testnet headers.
- The NU7 epoch MMR starts at 4,465,026 and uses NU7's branch ID `0x77190AD9` in the personalization. The
  activation block itself commits to the NU6.3 epoch's tree. An anchor therefore needs m ≥ 4,465,026 for headers in
  the NU7 epoch.

**Two ways to check relation 4:**

| | (A) On Base, in Solidity | (B) Inside the proof |
|---|---|---|
| What goes on chain | MMR leaf, siblings, peaks (≈ 300 bytes each, ~log2(blocks since NU7) + peaks ≈ 20–40 nodes) | nothing extra; H is a public input |
| Cost | BLAKE2b-256 via the 0x09 precompile (~5 compressions per node) plus 6–12 KB calldata: roughly 1.5–2.5 × 10^5 gas (estimate) | ~200 BLAKE2b compressions in the zkVM (small next to Sinsemilla) |
| Privacy | reveals the anchor block m (not the note) | reveals only H |

(B) is simpler and more private, and is the recommended path with a zkVM. (A) fits a Halo2 circuit, where BLAKE2b
is costly. It can be shared: an "anchor registry" stores verified roots that many deposits reuse.

## 4. The deposit tag

Requirements: **unique per note** (one note cannot mint twice), **deterministic** (the prover cannot pick another
tag for the same note), **unlinkable** to anything visible on Zcash, and computable by whoever proves.

| Candidate | Unique | Unlinkable | Notes |
|---|---|---|---|
| nf (the note's nullifier) | yes | **no** | When the vault spends the note, nf appears on Zcash and links the Base mint to the payout transaction |
| BLAKE2b(nf) | yes | **no** | Anyone can hash published nullifiers |
| Today: BLAKE2b keyed by H(vault `sk`) over nf (`policy/src/derive.rs`) | yes | yes | `sk` is shared by the signers only; the depositor cannot compute it |
| **BLAKE2b keyed by H(nk) over nf** (recommended) | yes | yes | nk is in the vault's full viewing key, which the borrower receives (ARCHITECTURE "Trust model") |
| H(vaultId) | per vault | yes | Works only with one deposit per vault and one vault per key; no protection if keys are reused |

Why the recommended tag works:

- Uniqueness: nf is unique per note (DeriveNullifier over the unique ρ; ρ is itself the nullifier spent in the same
  action). nk is fixed by the vault's address, because a different (ak, nk, rivk) with the same ivk would break the
  binding of Commit^ivk. Deriving nf in the proof adds a Poseidon hash and one fixed-base scalar multiplication. Using
  ρ instead of nf is equally unique and cheaper.
- What it reveals: to the public, nothing (a PRF output under nk). The signers and the borrower know nk and can link
  t to the note, but they already see the vault's notes. If a borrower leaks the UFVK, the tag becomes linkable,
  but so does the vault's whole history.

## 5. Who proves, and with what

**The depositor proves.** The depositor built the note, so it knows the plaintext (or rseed) and simply opens the
commitment. **No trial decryption is needed**: an IVK only serves to find notes, and the depositor already has the
note. The vault's full viewing key is needed for relation 3 and the tag. The signers do not need to be online to
mint, and cannot block a mint.

**Binding the vault key.** A proof against a key the depositor controls would mint zZEC for ZEC the depositor can
spend back. So K = Hash(ak, nk, rivk) must be attested once, at vault opening, by the signers ("we ran the DKG for
this vault; this is its key"). The attestation is presented with the first deposit, so nothing touches Base earlier
(L20), and the borrower can check it against the vault UFVK. This is a smaller trust assumption than today's: signers
vouch for a key, not for a deposit.

**Proof systems:**

| | RISC Zero zkVM | SP1 zkVM | Halo2 with Orchard gadgets |
|---|---|---|---|
| Statement written as | Rust, reusing `orchard` / `pasta_curves` / `sinsemilla` and a BLAKE2b crate | same | a circuit, reusing the audited Sinsemilla, ECC and Poseidon chips |
| Verified on Base Sepolia by | `RiscZeroVerifierRouter` `0x0b144e07a0826182b6b59788c34b32bfa86fb711`: on-chain check at block 47,973,492 shows selector `0x73c457ba` (Groth16 v3.0.0) routed to its e-stop wrapper `0x724d375B5b622e15f0E64e9Deb76a4cB17877797`, `0xbb001d44` (v2.2.0) to `0x7BE0Ca3734bB4B0aC66dD05714D25476bd658a9F` | `SP1VerifierGateway` (Groth16) `0x397A5f7f3dBd538f23DE225B51f532c34448dA9B` (code present) | no EVM verifier: Pasta-curve IPA proofs would need recursion into a BN254 SNARK |
| Verification gas | ≈ 2.3–2.5 × 10^5 (Groth16, 4 pairings at EIP-1108 prices plus input handling; estimate, not measured) | similar | n/a |
| Proving | heaviest: Pallas arithmetic in software | similar | fast (comparable to an Orchard action) |

**Size of the computation (estimate, not measured; see § 8).** The Merkle path is 32 MerkleCRH hashes. Each hashes
520 bits as 52 Sinsemilla chunks, i.e. 104 incomplete point additions, so about 3,300 additions in all. NoteCommit
adds about 220 and Commit^ivk about 100, plus three 255-bit scalar multiplications (rcm, rivk, ivk) and one
hash-to-curve for g_d. In projective coordinates that is on the order of 5 × 10^4 Pallas base-field
multiplications. RISC Zero has no Pasta accelerator, so each multiplication costs hundreds of RISC-V cycles (8 × 32-bit
limbs): **order 10^7 cycles**, plus ~10^6 for BLAKE2b in option (B). Patching the field arithmetic onto RISC Zero's
256-bit modular-multiplication accelerator could cut this several-fold. At that size proving takes minutes on a
laptop CPU and much less on a GPU or proving network, plus a fixed cost for the Groth16 wrap. These figures must be
replaced by measurements before any commitment.

## 6. What still needs trust or stays out of scope

- **Payouts.** Releasing ZEC still needs 3 of 5 FROST signers, who can collude to steal a vault. Tier 2 only stops
  them from minting unbacked zZEC, and stops a lying data source from fooling honest signers.
- **Vault key attestation** (§ 5): signers vouch that K is a vault key they hold.
- **The relay**: its checkpoint is trusted; it needs someone to submit headers (anyone can); ~6.3 × 10^5 gas per
  header (contracts/src/relay/README.md) at 3,456 headers a day. Batching headers in a zkVM proof (Equihash + retarget
  for N headers in one receipt) would amortise this.
- **Testnet minimum-difficulty blocks** cost almost nothing (work 32 vs ~380,000 for a normal block), so on Testnet
  "N confirmations" must be measured in work (`confirmationWork`), and W_min should be a multiple of the work of a
  normal block. Mainnet has no such rule, but the usual majority-hash-power assumption remains.
- **Policy rules outside the minimal statement.** P1 also requires an Ironwood-only deposit transaction (D5), a height
  ≤ the vault's expiry height and "first conforming note" ordering. Proving D5 needs the deposit transaction itself
  (ZIP 244 txid, path to `hashMerkleRoot`); the first-note rule needs the vault's full note history. In Tier 2 these
  could stay app or wallet policy, with non-conforming notes still refunded under P5 by the signers.
- **Base-side privacy.** Unchanged and not worse: no Zcash transaction id, note commitment or address reaches Base.
  Option (A) would reveal an anchor block. The mint timing (L5) is unchanged.
- **Tier 3 directions:** vault keys derived from a long-lived signer group key by a public tweak (no per-vault
  attestation); bonded signers slashable on Base for a payout without a matching burn, once Zcash spends can be proven
  through the same relay; a finality gadget such as Crosslink for confirmations.

## 7. Phased plan

| Phase | Deposits | What it removes |
|---|---|---|
| 0 (today) | 3-of-5 attestation; signers read one light wallet server | — |
| 1 (this repository) | unchanged on Base. Relay deployed and fed by a relayer; each signer cross-checks that a deposit's block is on the relay's best chain, off-chain only (putting the block on Base would leak it, L4) | trust in a single data source |
| 2 | recompute `hashBlockCommitments` of real Testnet blocks from public data (MMR library + test vectors); specify K, its attestation, and the tag; write the zkVM guest; measure cycles and proving time; build a prover for the CLI and the app | — (preparation) |
| 3 | Registry accepts `proveDeposit(vaultId, account, proof)` alongside `attestDeposit`; the paymaster allowlist adds the selector | signers as deposit attesters (dual mode) |
| 4 | proof-only deposits; `attestDeposit` removed; P5 refunds unchanged | signer trust for minting |
| 5 (Tier 3) | payouts: bonds and slashing, derived vault keys | trust in signers for custody, partially |

## 8. Optional measurement: not done

A RISC Zero executor run of a Sinsemilla Merkle path was planned. The toolchain is not actually present on the
build machine: `~/.cargo/bin/r0vm` and `~/.cargo/bin/cargo-risczero` are dangling symlinks into a missing
`~/.risc0` (a v2.1.0 install from June 2025), and `rzup` is absent. Installing it means downloading binaries, which
this task excluded, so no cycle counts were measured. The § 5 figures are analytical. Phase 2 starts with this
measurement.

## References

Vendored sources (`docs/ref/VENDORED.md`): zcash/zips `3af03aa` (2026-10-08); zcash/librustzcash `3c76c2d`;
zcash/orchard `616a669`.

- ZIP 221 (FlyClient, chain history MMR), ZIP 244 (§ Block Header Changes), ZIP 258 (NU6.3 deployment, § Changes to
  ZIP 221), ZIP 259 (NU7 deployment), ZIP 218 (25 s spacing), ZIP 2005 (Ironwood quantum recoverability),
  ZIP 229 (v6 transactions), ZIP 326 (NU6.3 wallets).
- Zebra v7.0.0-rc.0, commit `6d1e414d6f55e4180d0e47baaa934bf97d5b4fec`: `zebra-chain/src/history_tree.rs`,
  `zebra-chain/src/primitives/zcash_history.rs`, `zebra-chain/src/block/commitment.rs`,
  `zebra-state/src/service/check.rs`.
- librustzcash `zcash_history` 0.5.0 (`src/node_data.rs`, CHANGELOG), `zcash_client_backend/lightwallet-protocol/
  walletrpc/service.proto` (`TreeState.ironwoodTree`).
- RISC Zero: `risc0/risc0-ethereum` `contracts/deployment.toml` (main, last changed in `85c59d5c5fb5`, 2026-04-29),
  checked on chain with `cast call` against `https://sepolia.base.org`.
- Succinct: `succinctlabs/sp1-contracts` `contracts/deployments/84532.json` (main).
- This repository: `contracts/src/relay/README.md`, `signer/crates/policy/src/derive.rs`, docs/ARCHITECTURE.md,
  docs/THREAT_MODEL.md.
