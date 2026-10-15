# Tier 2: minting zZEC on a proof of the deposit

Status (2026-10-12): **implemented and run on real Testnet data, verified on-chain with a MOCK verifier; the only
mint path of the Base Sepolia deployment (D25).** The statement, the RISC Zero guest, the witness builder, the
Registry entry points (proof, pending activation, finalization), the header relay and the refund path for unminted
deposits are in the repository with tests, including a negative test for every relation of the statement, run
natively and in the zkVM executor (§ 6). A real deposit's proof was produced locally as a RISC Zero succinct (STARK)
receipt, then (2026-10-14) on a rented GPU and wrapped in Groth16: RISC Zero's verifier router accepts the seal, and
`RealGroth16Seal.t.sol` mints the deposit through it on a Base Sepolia fork. Not done yet: the deployment on Base
Sepolia. "Verified" below means checked against a primary source (ZIP text, Zebra or librustzcash source,
live chain data); "estimate" marks numbers not measured here.

**Statement v2 (the guest round, design notes T-09 and H-10b, 2026-10-15).** The witness carries the vault's tag key
K instead of its viewing key, and `VaultOpened` v2 binds K (§ 2.3); a proof has a mode, Activate or Refund (§ 2.1); the
Zcash network is a build parameter with its own guests (§ 2.2); the journal (v2) adds the mode and the epoch's branch
id. Implemented and tested natively for both networks; the zkVM guests are built, and their parity with the native
statement checked, by the `guests` workflow (RISC Zero does not run on the Windows development machine). The v3 Intake
decodes journal v2. Measurements in § 6 are of the v1 guest unless stated.

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
| Visible on Base | vault id, account, amount, tag | the same, plus the anchor header hash, the `VaultOpened` struct hash and (journal v2) the proof's mode and epoch |

The Registry's `MintMode` is fixed at deployment (D25). The Base Sepolia deployment is `ProofOnly`: `attestDeposit`
reverts, so no set of signers and no lying data source can mint unbacked zZEC. The local fork keeps both paths
(`SignerAttested`), which is what lets the tests exercise the rules between them.

## 2. The statement (`tier2/statement`)

The same Rust function, `verify(&Witness) -> Result<Journal, Error>`, runs natively (to build and check witnesses)
and inside the RISC Zero zkVM (the guest proves it accepted the private witness and commits the journal).

**Private witness:** the mode (§ 2.1) and the consensus branch id of the epoch the proof walks (§ 2.2); the
transaction (v6 bytes), the index of its Ironwood action, the note (43-byte recipient, value, rseed), the
transaction's position and SHA-256d path in its block B, B's header, B's ZIP 221 history-tree leaf with its path and
all peaks, a later header T with its block's auth data root, and the vault as the signers described it in
`VaultOpened` (id, owner, account, unified address string, tag key K, amount, expiry height). The witness carries K,
not the viewing key (review H-10): whoever proves a deposit learns which notes and payouts are the vault's (their tags
and payout references), never their amounts or recipients (THREAT_MODEL, "Who sees what").

**Relations checked:**

1. **The transaction.** It is v6 and carries the witness's consensus branch id, one the build allows (v5 and v6
   transactions carry their own; the statement checks it, O5). Activate mode applies the deposit rule D5 strictly: an
   Ironwood bundle with outputs enabled, and no transparent, Sapling or Orchard-pool bundle
   (`check_deposit_transaction`, the very function the signers' P1 applies: K2-c, D27). Refund mode asks only for an
   Ironwood bundle. Its txid (ZIP 244 as modified by ZIP 229, computed from the bytes) is under B's `merkleRoot`.
2. **B is in the history committed by T, in the witness's epoch.** The leaf's `subtree_commitment` is SHA-256d(B's
   header) and it covers a single height; the path hashes the leaf up to one of the peaks; the left fold of the peaks
   is the history tree's root node, and BLAKE2b("ZcashBlockCommit", V3 hash of that node ‖ T's auth data root ‖
   [0]^32) equals T's `hashBlockCommitments`. Every node hash is personalized with the witness's branch id (ZIP 221),
   so the path opens only if B and T are of that epoch (the tree restarts at every network upgrade). Only T is
   public: B stays hidden among the epoch's blocks (23,534 at the anchor below). Activate mode: B's height is at most
   the vault's expiry height.
3. **The note.** Recovering the action's ciphertext with the ephemeral secret derived from the witness note (ZIP 212)
   and the note's pk_d succeeds, which means the plaintext is a note whose commitment is the action's cmx and whose
   ephemeral key is the action's: the action creates that note and an incoming viewing key for its recipient
   decrypts it. The recovered note equals the witness note field by field and is a V3 (ZIP 2005) note (O5).
4. **The recipient is the vault's published receiver.** The vault's unified address is an address of the build's
   network whose only receiver is an Orchard-protocol one (D4), and the note pays exactly that receiver, not another
   diversified address of the vault. This is `vault_receiver`, which P1 applies too (K2-c: P1 classifies a note to
   another address of the vault as `WrongReceiver`).
5. **The amount.** Activate mode: the note's value is the vault's amount. Refund mode: any value (the Intake enforces
   a minimum).
6. **The tag and the vault.** The deposit tag is the PRF of the note's cmx under K (§ 4), and `vaultOpenedHash` is the
   EIP-712 struct hash of `VaultOpened(vaultId, owner, account, the vault's amount, expiryHeight, keccak256(address),
   keccak256(K))` (v2, docs/INTERFACES.md § 2.5), computed inside the proof.

Steps 1 and 2 (minus D5) are `check_inclusion(txid, &Inclusion)`, written to be shared: Tier 3's violation statements
prove a spend's inclusion with the same code.

**Public output (journal v2, 256 bytes):** `abi.encode(uint8 mode, uint32 branchId, bytes32 tipHash, bytes32 vaultId,
address account, uint64 amount, bytes32 depositTag, bytes32 vaultOpenedHash)`, with `amount` = the note's value: the
Intake's `DepositJournal` (`contracts/src/interfaces/v3/IZecuredIntake.sol`). The v1 guest committed the six words from
`tipHash` on (192 bytes), always in Activate mode, over `VaultOpened` v1.

### 2.1 Modes (T-09, with T-02)

- **Activate (0)**: the vault's deposit: D5, the vault's amount, mined at or below its expiry height. The Intake
  activates the vault on it, or lands it as a refund when the vault cannot be activated (T-02).
- **Refund (1)**: a note that cannot back the vault (another amount, a late payment, a second deposit, a transaction
  with transparent or other parts) lands as a refund claim instead of an off-chain refund: any v6 transaction of an
  allowed epoch with an Ironwood bundle, a note of any value to the vault's published receiver, at any height. The
  `VaultOpened` struct hash uses the vault's own amount, so the five signatures still check.
- The tag is the same in both modes, so the Intake's `DepositTagUsed` sees both: a note lands once, as collateral or
  as a refund. Notes to the vault's internal (change) address cannot land this way: they do not pay the published
  receiver.
- **Privacy cost.** A refund writes the note's exact value on Base, where an activation writes a denomination many
  vaults share: whoever sent the note knows its amount and time and could find the vault's account. The app asks only
  for the denomination (ZIP 321), so a non-conforming note is a sender's mistake (THREAT_MODEL L25).

### 2.2 The network is a build parameter

A witness never chooses the network: a Testnet note must never be provable to a Mainnet deployment. The statement
crate's cargo feature `mainnet` selects it (`zecured_deposit_statement::NETWORK`); the guests and the host follow with
features of their own (§ 8.1).

| Build | Addresses | Allowed epochs (consensus branch ids) | Guests |
|---|---|---|---|
| default | Testnet | NU7 `0x77190AD9` | deposit-test, violation-test |
| `mainnet` | Mainnet | NU6.3 `0x37A5165B` (Ironwood since 3,428,143), NU7 `0x77190AD9` | deposit-main, violation-main |

The witness names the epoch of B and T (`branch_id`): the build must allow it, the transaction must carry it, and it
personalizes the history tree's hashes, so B and T are of it. A deployment registers only its network's guests; the
Intake also checks `branchId` against the ids it was deployed with. (NU6.3 is a Testnet epoch too, from 4,134,000 to
NU7; the Testnet build proves the current NU7 epoch only.)

### 2.3 Why the tag key is bound to the vault (design review, Claude Web § 2)

With K and the viewing key's hash as separate witness fields, the statement could not check that both come from the
same UFVK, and the `VaultOpened` signatures would bind only the hash. A borrower could then prove its own deposit
under a made-up K' (tag T1: the vault activates and zZEC is minted), then the same note under the real K (tag T2 ≠ T1:
`DepositTagUsed` does not see it, and the active vault lands it as a refund). One note would back both the zZEC and a
refund, and T1 matches no real note, so not even F1 could be proven. `VaultOpened` v2 therefore signs `tagKeyHash =
keccak256(K)`: the proof derives the tag with the K whose hash it puts in the struct, so a proof under any other K
names a `VaultOpened` nobody signed. K4 checks that another K gives such a journal in both modes, and
`the_same_note_cannot_land_under_two_tags` replays the attack in every order of modes.

**Why the transaction's inclusion path and not the note commitment tree.** The first design (kept in git history)
proved cmx in the Ironwood note commitment tree against a root from B's history leaf: 32 Sinsemilla Merkle hashes,
about 3,300 Pallas point additions. The inclusion path costs SHA-256d (accelerated in RISC Zero) and the ZIP 244
txid, and it also proves the shape of the whole transaction (D5), which the tree path cannot.

**Why the struct hash and not the address hash.** `keccak256(address)` on Base would let anyone who knows the vault's
address, for example an exchange that paid into it, find the vault, its account and its loan. The struct hash also
depends on the tag key, which only the owner, the signers and whoever proves the vault's deposits know.

## 3. On Base

`ZecuredRegistry.attestDepositWithProof(journal, seal, openedSigs)` (D22) checks, in this order:

1. the journal is 192 bytes and the seal starts with the pinned verifier selector (Y1);
2. the vault id, tag and account are unused, and no proof for the vault is pending (I4, I5), nothing is zero;
3. `relay.isInBestChain(tipHash)` and `relay.confirmationWork(tipHash) >= minConfirmationWork`; the deposit's block
   precedes T, so it is at least as deeply buried;
4. all five signers' signatures (sorted) over the Registry-domain digest of `vaultOpenedHash`;
5. `verifier.verify(seal, depositImageId, sha256(journal))`;

then **reserves** the vault id, the tag and the account and records a pending activation (D26, review K3): nothing is
minted yet. `finalizeActivation(vaultId)` (anyone) mints and supplies exactly as `attestDeposit` once the relay's best
chain has gained `finalizeDepth` (6) headers since the proof and `finalizeDelay` (150 s) has passed, with the anchor
still on the best chain under `minConfirmationWork`. `dropPendingActivation(vaultId)` (anyone) frees the vault, the
tag and the account once the anchor has left the best chain; the deposit can then be proven against the chain that
won. `cancelVault` waits while a proof is pending. The point is a lagging feeder: an attacker could feed a branch of
their own and prove a fake deposit in it; the depth and the delay give the honest feeders time to replace that branch
before anything is minted. The coordinator and `zecured-wallet loan prove` both finalize or drop on their own.

The verifier is RISC Zero's router on Base Sepolia (`0x0b144e07a0826182b6b59788c34b32bfa86fb711`:
`verify(bytes,bytes32,bytes32)` present, Groth16 v3.0.0 selector `0x73c457ba` active, checked on-chain). The Registry
pins that selector, so the router's owner can add verifiers under new selectors but cannot change the one behind
`0x73c457ba`; the router's emergency stop still works. `MockRiscZeroVerifier` (MOCK, selector `0xffffffff`) stands in
on the local fork, and the deploy script refuses it anywhere else.

**Confirmations are measured in work.** Testnet minimum-difficulty blocks add 32 units of work, against 364,000 to
402,000 for each of the 113 normal blocks before 4,488,559, so a block count would be cheap to inflate. The default
`minConfirmationWork` is 3,500,000: about ten normal blocks (the deploy script refuses less than 1,000,000 outside the
local fork).

**Tests.** `contracts/test/fork/DepositProofs.t.sol` (30 tests): the journal of the real deposit below, anchored to
header 4,488,560, which the real relay verifies on-chain (Equihash, difficulty, time, linkage; fixture cross-checked
with CipherScan), proven with three headers on top and finalized after six more; refusals for an unknown anchor, too
little work, four signatures, a duplicate or foreign signature, a seal for another journal or image or with another
selector, journals of the wrong length or with a dirty address word, a reused or pending vault, tag or account;
cancellation, also while a proof is pending; the `VaultOpened` golden vector. `PendingActivation.t.sol` (7 tests) drives
a MOCK relay through what real headers cannot do in a test: `ProofOnly` refusing even five signatures, not ready by
height or by time, the anchor leaving the best chain before finalization, dropping and proving again.
Gas: **289,256** for the proof with the MOCK verifier, **163,423** for the finalization (mint and Morpho supply). With
the real seal and RISC Zero's router (`RealGroth16Seal.t.sol`): **520,722** for the proof; the router's Groth16 check
costs about 2.4 × 10^5.

## 4. The deposit tag (D24)

`depositTag = BLAKE2b-256(key = K, personal "Zecured_DepTag__", cmx)`, with the vault's tag key `K =
BLAKE2b-256("Zecured_TagKey__", UFVK string)`. Signer attestations use the same definition, so a note has one tag on
both paths and can never mint twice. The depositor can compute it (it has the viewing key), the public cannot link it
to the note without K (L9). The cmx is unique per note; K is fixed by the signed `VaultOpened` (`tagKeyHash =
keccak256(K)`, § 2.3).

## 5. Unminted deposits: cancellation (D23, L-8)

A proof needs no signer, so the signers may refund the conforming deposit of a vault that was never activated only
after `cancelVault` (3 of 5 signers over `VaultCancellation(vaultId)`) has made the vault `Cancelled`: from then on
neither an attestation nor a proof can activate it. The owner asks with an ordinary `RefundRequest` for that deposit
once its deposit window is over; each node checks that the Registry has not activated the vault, records the refund
and signs the cancellation; the coordinator submits it; the deposit becomes refundable only when the nodes apply the
checked `VaultCancelled` event. This also closes security review finding L-8 for Tier 1.

**Why refunds stay safe while anyone can prove** (review K2, D27). A proof can be made for any note that meets the
statement, spent or not, while P1 rests on the light wallet server's answers. Four rules keep a refund from ever
leaving zZEC unbacked:

- P1 and the statement apply the same predicates (`check_deposit_transaction`, `vault_receiver`), so a deposit P1
  accepts is one a proof can prove, and the reverse.
- Once Base activated a vault, the signers root its lineage at the note named by the `VaultActivated` deposit tag,
  whatever P1 decided; P1's own choice then becomes refundable. A node that cannot see the activated note halts every
  refund of the vault and reports it.
- Before refunding a note of the vault's amount, every node asks the Registry on each of its Base providers: the vault
  must be cancelled, or activated with that note's tag unused. Never while the vault can still be activated (a pending
  proof reserves the tag and keeps the vault unactivated).
- Such a refund waits for twice the deposit depth (2N confirmations), so no reorg brings back a chain where the note
  was the vault's first deposit.

## 6. Measurements on real Testnet data

| What | Result |
|---|---|
| History tree | `hashBlockCommitments` of header 4,488,543 recomputed from public data (23,517 V3 leaves, 4,465,026 .. 4,488,542; compact blocks and tree states from lightwalletd, headers over P2P): **matches the header** (`history-root`) |
| Witness | vault 9 of the local-fork runs: 0.01 TAZ at 4,488,543 (transaction 1 of 2), anchor T = 4,488,560, leaf 23,517 of 23,534, path of 5 nodes to peak 7 of 11; ~5 min to build the first time (per-block summaries are cached), 8 s afterwards |
| Native check | `verify` accepts; the `VaultOpened` struct hash matches an independent computation with `cast` (v2: with Python's hashlib and pycryptodome) |
| Negative tests (K4) | v2: 50 single changes of that witness on a Testnet build, 51 on a Mainnet one (`tier2/k4`), one or more per relation, each with its outcome: refused with its exact error; accepted where the rule does not bind the mode (a late or other-valued note, or a transaction with a transparent output, re-anchored in a block of its own, in Refund mode); or accepted with a `VaultOpened` nobody signed (another tag key). Natively for both networks in `cargo test`, and through `zecured-tier2-host parity` by the guest in the zkVM executor, run by the `guests` workflow on each network's guest (the v1 catalog, 33 changes, ran locally too: 0.1 to 0.4 s each). Plus the transaction shapes a witness cannot carry (an Orchard-pool bundle, v5, another epoch), the changes accepted on purpose (authorizing data, header bytes outside the commitments) and the two-tag regression in `tier2/statement/tests/negatives.rs`. The fixture is the TEST-ONLY witness of this disposable vault |
| zkVM execution | v1 guest: journal identical to the native computation; **20.2 M cycles** (20,153,349; 21 segments of 2^20), 0.5 s in the executor |
| Guest | v1 guest: image id `0x08cc01a2f35fd80bb959e9403a7c08be1506139e3918b4d57b44b4326337bbc0` (2026-10-14 local build, after Tier 3's E1 changed the shared statement crate; any change to the crate changes it, so the deployed id comes from the reproducible build and is fixed with `export-elf`, § 8.1) |
| Where the cycles go | Sinsemilla note commitments ~56 % (three: the witness note's validity check and two inside recovery), scalar multiplications, hash-to-curve, BLAKE2b ~7 %, SHA-256 ~1 % |
| Optimizations so far | Pallas field multiplication and inversion through RISC Zero's 256-bit modular-multiplication accelerator (vendored `pasta_curves`, `ZECURED_PATCH.md`): ~3× fewer cycles; three redundant note commitments removed (the first run took 36.2 M cycles); witness read as one frame |
| Local proving | a 22.1 M-cycle probe of the same code: succinct receipt in 108 min on this Apple M2 with 8 GB (CPU only, r0vm has no Metal backend, heavy swapping); the real deposit's proof: see `docs/TESTING.md` |
| On-chain | 222,380 + 163,179 gas with the MOCK verifier (§ 3) |

## 7. What still needs trust

- **Payouts.** Releasing ZEC still needs 3 of 5 FROST signers, who can collude to steal a vault. Tier 2 stops unbacked
  minting and lying data sources, not theft.
- **The vault key.** All five signers vouch that the address and the tag key (of the viewing key) in `VaultOpened` are
  those of the DKG they ran. A vault key controlled by one party could otherwise back a mint that party later spends
  away.
- **The relay**: its checkpoint (hash, height, 113 context entries) is trusted constructor input; someone must
  submit headers (anyone can: `scripts/relay-feed.sh`); ~6.3 × 10^5 gas per header.
- **RISC Zero**: the soundness of its proof system and the router's governance (its owner can add or remove verifier
  versions; an e-stop wrapper can disable one). The Registry pins the guest image id and the verifier selector.
- **Testnet** proof of work is weak: a single Equihash ASIC out-mines the whole Testnet about 40 times over, so the
  pending activation and `minConfirmationWork` are demonstrative on Testnet; on mainnet they inherit Zcash's proof of
  work, with the depth chosen by the value at risk.

## 8. Remaining work

1. **Groth16 seal.** The STARK runs on a rented NVIDIA GPU: `scripts/gpu-prove.sh` sets the machine up (the host's
   `cuda` feature, no Docker, no guest build) and proves with the pinned guests. Measured on an RTX 4090: 36 s for
   the real deposit, 48 s for an F1 violation (TESTING.md § 2b). `risc0-groth16` 3.0.5 wraps in Groth16 either with
   Docker on x86 or natively with its `cuda` feature. The CUDA prover fails on our receipts (an out-of-bounds read
   in its own kernel, TESTING.md § 2b), so the wrap uses Docker on x86: about a minute per seal. Its input is the
   succinct receipt, which holds no viewing key. Both seals pass RISC Zero's router (done). A deposit witness holds
   the vault's tag key K, not its viewing key (statement v2): the GPU server learns which notes and payouts are the
   vault's, not their amounts or recipients. The private alternative keeps the STARK on the depositor's machine.
2. **Base Sepolia**: deploy a relay from a recent checkpoint (`script/DeployRelay.s.sol`), keep it fed, deploy the
   Registry `ProofOnly` with the router, the image id below and the relay.
3. **Fewer cycles** (optional once proving runs on a GPU): one note commitment instead of three (own ZIP 212
   recovery), Pallas point operations through RISC Zero's elliptic-curve accelerator.

### 8.1 Pinning the guest (D28)

The image id depends on the machine that built the guest: its panic messages carry the source paths of its
dependencies. The Registry pins one image id for good, so the guest is built once and every prover runs that file:

```bash
tier2/target/release/zecured-tier2-host export-elf deposit.bin       # the embedded guest
ZECURED_DEPOSIT_ELF=deposit.bin zecured-tier2-host image-id           # its image id, as any prover computes it
```

A prover built anywhere else (the GPU machine's host has the CUDA prover compiled in) runs the pinned file with
`ZECURED_DEPOSIT_ELF` set (the violation guest: `ZECURED_VIOLATION_ELF`).

**One pair of guests per network** (T-09, § 2.2). The host's `mainnet` feature builds the Mainnet statement natively
and passes `mainnet` to both guests (`tier2/methods/build.rs`, the guests' own `mainnet` feature); without it,
Testnet. `zecured-tier2-host network` prints which, and the host refuses to run if its native statement and its
embedded guests disagree. A deployment pins its own network's pair.

**The deployed ids come from RISC Zero's Docker build** (review E4). With `ZECURED_DOCKER_GUESTS=1`,
`tier2/methods/build.rs` compiles both guests in the pinned `risczero/risc0-guest-builder:r0.1.97.0` image with
`tier2/` as the build context, so the ELFs, and with them the image ids, no longer depend on the machine. The `guests`
workflow (`.github/workflows/guests.yml`) runs that build on every change to the statement, the guests or the vendored
crate and publishes the ids and the two ELFs; the commands to reproduce them are in the README. The statement crate and
both guests are frozen once the deployed ids are taken from that build. Two runs on fresh runners (2026-10-14, the
same source) gave the same ids. After the red-team fixes the reproducible ids are deposit
`0x62feed13b2948aa427275fb6003e717ccc88945d033b99b196936ca99a80cdf5` and violation
`0x7d8c9dd23e813e0e44ffea2b18ec9ec451b52a8a7b634ae998583d49b73cc526`, with both parity suites passing on those guests
in CI. They change with the statement crate until it is frozen. Statement v2 changes both and adds the Mainnet pair:
the workflow now builds both networks (deposit-test, violation-test, deposit-main, violation-main), prints the four
ids and runs both parity suites on each pair; its next run gives the ids to pin.

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
