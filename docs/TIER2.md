# Tier 2: minting zZEC on a proof of the deposit

Status (2026-10-11): **implemented and run on real Testnet data, verified on-chain with a MOCK verifier.** The
statement, the RISC Zero guest, the witness builder, the Registry entry point, the header relay and the refund path
for unminted deposits are in the repository with tests. A real deposit's proof was produced locally as a RISC Zero
succinct (STARK) receipt. Not done yet: the Groth16 wrap that Base can verify (needs an x86 machine with Docker) and
a deployment on Base Sepolia (needs the relay fed with headers there). "Verified" below means checked against a
primary source (ZIP text, Zebra or librustzcash source, live chain data); "estimate" marks numbers not measured here.

## 1. What changes

In Tier 1 a deposit becomes zZEC when 3 of 5 signers sign a `DepositAttestation` and `attestDeposit` checks the
signatures. Two trust assumptions sit in that step:

- **The signers.** Three colluding signers can attest a deposit that does not exist and mint unbacked zZEC.
- **Their data source.** A lying light wallet server can make honest signers attest a fake deposit (all five read
  the same public NU7 server today; D17 lets each use its own sources).

With `attestDepositWithProof`, zZEC is minted only on a zero-knowledge proof that a note of the vault's amount,
addressed to the vault, is in a Zcash block committed by a header the on-chain relay verified, under enough work.
Nobody has to trust the prover: anyone may build and submit the proof. The payout side (3-of-5 FROST signers
releasing ZEC) is unchanged; see § 7.

| | Tier 1 (`attestDeposit`) | Tier 2 (`attestDepositWithProof`) |
|---|---|---|
| Who vouches for the deposit | 3 of 5 signers | the proof, checked on Base |
| What the signers still vouch for | the deposit | the vault key: all five sign `VaultOpened` at the end of the DKG (D20); one honest signer can refuse a key it did not help create |
| Data source trusted for minting | the signers' light wallet servers | none (the relay checks proof of work itself) |
| Signers online to mint | yes (3 of 5) | no |
| Visible on Base | vault id, account, amount, tag | the same, plus the anchor header hash and the `VaultOpened` struct hash |

Both paths coexist (dual mode). A deployment without a verifier rejects proofs; a proof-only deployment would drop
`attestDeposit`.

## 2. The statement (`tier2/statement`)

The same Rust function, `verify(&Witness) -> Result<Journal, Error>`, runs natively (to build and check witnesses)
and inside the RISC Zero zkVM (the guest proves it accepted the private witness and commits the journal).

**Private witness:** the deposit transaction (v6 bytes), the index of its Ironwood action, the note (43-byte
recipient, value, rseed), the transaction's position and SHA-256d path in its block B, B's header, B's ZIP 221
history-tree leaf with its path and all peaks, a later header T with its block's auth data root, and the vault as
the signers described it in `VaultOpened` (id, owner, account, unified address and full viewing key strings, expiry
height).

**Relations checked:**

1. **Deposit rule D5.** The transaction parses as v6 under the NU7 branch id and has an Ironwood bundle and no
   transparent, Sapling or Orchard parts. Its txid (ZIP 244, computed from the bytes) is under B's `merkleRoot`.
2. **B is in the history committed by T.** The leaf's `subtree_commitment` is SHA-256d(B's header) and it covers a
   single height ≤ the vault's expiry height; the path hashes the leaf up to one of the peaks; the left fold of the
   peaks is the history tree's root node, and BLAKE2b("ZcashBlockCommit", V3 hash of that node ‖ T's auth data root ‖
   [0]^32) equals T's `hashBlockCommitments`. Only T is public: B stays hidden among the epoch's blocks (23,534 at
   the anchor below).
3. **The note.** Recovering the action's ciphertext with the ephemeral secret derived from the witness note (ZIP 212)
   and the note's pk_d succeeds, which means the plaintext is a note whose commitment is the action's cmx and whose
   ephemeral key is the action's: the action creates that note and an incoming viewing key for its recipient
   decrypts it. The recovered note equals the witness note field by field.
4. **The recipient is the vault.** The vault's unified address is a Testnet address whose only receiver is the
   note's (Orchard-protocol) receiver (D4).

**Public output (journal, 192 bytes):** `abi.encode(bytes32 tipHash, bytes32 vaultId, address account, uint64 amount,
bytes32 depositTag, bytes32 vaultOpenedHash)`, where `vaultOpenedHash` is the EIP-712 struct hash of
`VaultOpened(vaultId, owner, account, amount, expiryHeight, keccak256(address), keccak256(ufvk))` computed inside the
proof, with `amount` = the note's value.

**Why the transaction's inclusion path and not the note commitment tree.** The first design (kept in git history)
proved cmx in the Ironwood note commitment tree against a root from B's history leaf: 32 Sinsemilla Merkle hashes,
about 3,300 Pallas point additions. The inclusion path costs SHA-256d (accelerated in RISC Zero) and the ZIP 244
txid, and it also proves the shape of the whole transaction (D5), which the tree path cannot.

**Why the struct hash and not the address hash.** `keccak256(address)` on Base would let anyone who knows the vault's
address, for example an exchange that paid into it, find the vault, its account and its loan. The struct hash also
depends on the viewing key, which only the owner and the signers know.

## 3. On Base

`ZecuredRegistry.attestDepositWithProof(journal, seal, openedSigs)` (D22) checks, in this order:

1. the vault id, tag and account are unused (I4, I5) and nothing is zero;
2. `relay.isInBestChain(tipHash)` and `relay.confirmationWork(tipHash) >= minConfirmationWork`; the deposit's block
   precedes T, so it is at least as deeply buried;
3. all five signers' signatures (sorted) over the Registry-domain digest of `vaultOpenedHash`;
4. `verifier.verify(seal, depositImageId, sha256(journal))`;

then mints and supplies exactly as `attestDeposit`. The verifier is RISC Zero's router on Base Sepolia
(`0x0b144e07a0826182b6b59788c34b32bfa86fb711`: `verify(bytes,bytes32,bytes32)` present, Groth16 v3.0.0 selector
`0x73c457ba` active, checked on-chain); `MockRiscZeroVerifier` (MOCK) stands in on the local fork, and the deploy
script refuses it anywhere else.

**Confirmations are measured in work.** Testnet minimum-difficulty blocks add 32 units of work against ~490,000 for
an average post-NU7 block (500 blocks to 4,488,603), so a block count would be cheap to inflate. The default
`minConfirmationWork` is 1,000,000: two to three normal blocks.

**Tests** (`contracts/test/fork/DepositProofs.t.sol`, 26 tests): the journal of the real deposit below, anchored to
header 4,488,560, which the real relay verifies on-chain (Equihash, difficulty, time, linkage; fixture cross-checked
with CipherScan) together with 11 more headers for the work on top; refusals for an unknown anchor, too little work,
four signatures, a duplicate or foreign signature, a seal for another journal or image, journals of the wrong length
or with a dirty address word, a reused vault, tag or account; cancellation; the `VaultOpened` golden vector.
Gas: **266,300** without the verifier call (MOCK); RISC Zero's Groth16 verification adds roughly 2.8 × 10^5
(estimate, not measured here).

## 4. The deposit tag (D24)

`depositTag = BLAKE2b-256(key = BLAKE2b-256("Zecured_TagKey__", UFVK string), personal "Zecured_DepTag__", cmx)`.
Signer attestations use the same definition, so a note has one tag on both paths and can never mint twice. The
depositor can compute it (it has the viewing key), the public cannot link it to the note without the viewing key
(L9). The cmx is unique per note; the UFVK string is fixed by the signed `VaultOpened`.

## 5. Unminted deposits: cancellation (D23, L-8)

A proof needs no signer, so the signers may refund the conforming deposit of a vault that was never activated only
after `cancelVault` (3 of 5 signers over `VaultCancellation(vaultId)`) has made the vault `Cancelled`: from then on
neither an attestation nor a proof can activate it. The owner asks with an ordinary `RefundRequest` for that deposit
once its deposit window is over; each node checks that the Registry has not activated the vault, records the refund
and signs the cancellation; the coordinator submits it; the deposit becomes refundable only when the nodes apply the
checked `VaultCancelled` event. This also closes security review finding L-8 for Tier 1.

**Why refunds stay safe while anyone can prove.** The signers refund non-conforming notes (P5) without asking
Base, and a proof can be made for any note that meets the statement, spent or not. This is safe because every note
the statement accepts also meets the signers' P1 rules (same amount, same expiry rule, and D5 at least as strictly:
the statement refuses a transaction that merely carries an empty transparent, Sapling or Orchard bundle, P1 counts
inputs, outputs and actions). So whenever a vault can be minted against, it holds a note P1 classifies as conforming,
the first one in chain order, and that note is never refunded unless the vault is cancelled, which makes it
unmintable first. A mint on a later qualifying note (P1's "second payment") leaves the books unchanged in value: the
conforming note backs the vault and the later one is refundable.

## 6. Measurements on real Testnet data

| What | Result |
|---|---|
| History tree | `hashBlockCommitments` of header 4,488,543 recomputed from public data (23,517 V3 leaves, 4,465,026 .. 4,488,542; compact blocks and tree states from lightwalletd, headers over P2P): **matches the header** (`history-root`) |
| Witness | vault 9 of the local-fork runs: 0.01 TAZ at 4,488,543 (transaction 1 of 2), anchor T = 4,488,560, leaf 23,517 of 23,534, path of 5 nodes to peak 7 of 11; ~5 min to build the first time (per-block summaries are cached), 8 s afterwards |
| Native check | `verify` accepts; the `VaultOpened` struct hash matches an independent computation with `cast` |
| zkVM execution | journal identical to the native computation; **20.2 M cycles** (21 segments of 2^20) |
| Where the cycles go | Sinsemilla note commitments ~56 % (three: the witness note's validity check and two inside recovery), scalar multiplications, hash-to-curve, BLAKE2b ~7 %, SHA-256 ~1 % |
| Optimizations so far | Pallas field multiplication and inversion through RISC Zero's 256-bit modular-multiplication accelerator (vendored `pasta_curves`, `ZECURED_PATCH.md`): ~3× fewer cycles; three redundant note commitments removed (the first run took 36.2 M cycles); witness read as one frame |
| Local proving | a 22.1 M-cycle probe of the same code: succinct receipt in 108 min on this Apple M2 with 8 GB (CPU only, r0vm has no Metal backend, heavy swapping); the real deposit's proof: see `docs/TESTING.md` |
| On-chain | 266,300 gas with the MOCK verifier (§ 3) |

## 7. What still needs trust

- **Payouts.** Releasing ZEC still needs 3 of 5 FROST signers, who can collude to steal a vault. Tier 2 stops unbacked
  minting and lying data sources, not theft.
- **The vault key.** All five signers vouch that the address and viewing key in `VaultOpened` are those of the DKG
  they ran. A vault key controlled by one party could otherwise back a mint that party later spends away.
- **The relay**: its checkpoint (hash, height, 113 context entries) is trusted constructor input; someone must
  submit headers (anyone can: `scripts/relay-feed.sh`); ~6.3 × 10^5 gas per header.
- **RISC Zero**: the soundness of its proof system and the router's governance (its owner can add or remove verifier
  versions; an e-stop wrapper can disable one). The Registry pins the guest image id.
- **Testnet** proof of work is weak; `minConfirmationWork` is a parameter, not a guarantee.

## 8. Remaining work

1. **Groth16 seal.** `risc0-groth16` 3.0.5 proves only with Docker on x86 (or CUDA). The plan keeps the witness on the
   depositor's machine: build the succinct receipt and its `identity_p254` form locally, send only that seal to the
   x86 machine for the Groth16 step, and submit the result. RISC Zero documents its receipts as zero-knowledge proofs
   (`risc0-zkvm` 3.0.6 `Receipt`).
2. **Base Sepolia**: deploy a relay from a recent checkpoint (`script/DeployRelay.s.sol`), keep it fed, redeploy the
   Registry with the router, the image id and the relay.
3. **Prover UX**: a CLI command that builds the witness (headers over P2P, compact blocks, the vault's viewing key
   from `ViewKeyRequest`) and proves; optionally the coordinator as a convenience prover (it already knows the
   viewing key; it cannot forge a proof).
4. **Fewer cycles**: one note commitment instead of three (own ZIP 212 recovery), Pallas point operations through
   RISC Zero's elliptic-curve accelerator instead of field operations, fixed constants precomputed.

## 9. Tier 3 directions

Vault keys derived from a long-lived signer group key by a public tweak (no per-vault `VaultOpened`); bonded signers
slashable on Base for a payout without a matching burn, once Zcash spends can be proven through the same relay; a
finality gadget such as Crosslink for confirmations.

## Appendix: how the history root reaches Base (verified)

- ZIP 221 defines the chain history tree, a Merkle mountain range (MMR) with one leaf per block. Each node carries
  metadata and is hashed with BLAKE2b-256, personalization `"ZcashHistory" ‖ le32(CONSENSUS_BRANCH_ID)`. The header of
  block B_n commits to the MMR over B_x .. B_{n−1}, where x is the most recent upgrade's activation height.
- ZIP 244 § "Block Header Changes": since NU5, `hashBlockCommitments` = BLAKE2b-256 with personalization
  `"ZcashBlockCommit"` over hashLightClientRoot (= hashChainHistoryRoot) ‖ hashAuthDataRoot ‖ [0u8; 32].
- ZIP 258 § "Changes to ZIP 221" (NU6.3) adds `hashEarliestIronwoodRoot`, `hashLatestIronwoodRoot` and
  `nIronwoodTxCount` to every node (`V3`, 277–317 bytes) and leaves `hashBlockCommitments` otherwise unchanged.
- NU7 changes nothing here (none of the ZIPs it deploys mentions the history tree); the NU7 epoch's tree starts at
  4,465,026 with branch id `0x77190AD9`. Zebra v7.0.0-rc.0 (`6d1e414d6f`) builds `V3` trees for NU6.3 and NU7 and
  validates `hashBlockCommitments` against them (`zebra-state/src/service/check.rs`).
- `zcash_history` 0.6.0 implements `V3`; `signer/crates/deposit-witness/src/mmr.rs` builds paths and peaks and is
  tested against `zcash_history::Tree` for 1 to 40 leaves.
- The light wallet protocol serves Ironwood tree states (`TreeState.ironwoodTree`); with all four pool types requested
  a compact block lists every transaction, which the txid Merkle path needs.

## Appendix: choice of proof system

RISC Zero 3.0.6 after comparing, for this statement on an 8 GB Apple M2: SP1 v6.9.0 (no Pallas precompile, local
proving needs more memory, its network sees the guest's stdin), OpenVM (16 GB proving key), and a Halo2 circuit
reusing Orchard's gadgets (fast to prove, but no EVM verifier for Pasta curves without a further wrapping SNARK).
RISC Zero proves locally in 2^18-cycle segments within 8 GB, has a 256-bit modular-multiplication accelerator that
speeds up Pallas arithmetic, and has a verifier router deployed on Base Sepolia.

## References

Vendored sources (`docs/ref/VENDORED.md`): zcash/zips `3af03aa`; zcash/librustzcash `3c76c2d`; zcash/orchard
`616a669`. ZIP 212, 221, 244, 258, 259, 2005; Zebra v7.0.0-rc.0 `6d1e414d6f`; RISC Zero `risc0-zkvm` 3.0.6,
`risc0-groth16` 3.0.5, `risc0-ethereum` deployment for Base Sepolia (checked on-chain with `cast`).
