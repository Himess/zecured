# Tier 3: theft of collateral is provable on Base and slashes bonds

Status (2026-10-14): **implemented, not yet deployed** (Deploy 2). Contracts: Registry v2
(`commitSpend`), `SignerBond`, `ZecuredSlasher`. Signers: the commitment in FROST round 1 and the
round-2 checks (T3-4, T3-5). The violation statement, its zkVM guest, `build-violation` (F1, F2, F3)
and the watcher (`zecured-wallet watch --tier3`) are done, and a real Testnet payout went through the
whole protocol (`docs/TESTING.md` § 2); the review's E1 to E3 are in (D34), and so are the fixes of
the red-team review of 2026-10-14 (D38), including the fourth violation kind, F4. Still to come: the
theft demo deployment (T3-6). This document fixes what the contracts, the signers and the violation
guest agree on:
- the message the signers sign;
- the state the Registry keeps;
- the four violation statements;
- the bond rules.

It follows the review's decisions T3-1 to T3-7 and S4 to S6 (D31). The interfaces are:
- `contracts/src/libraries/ZecuredEIP712.sol` (`SpendCommit`);
- `contracts/src/interfaces/IZecuredSpendCommits.sol`;
- `ISignerBond.sol`;
- `IZecuredSlasher.sol`.

The derivations are `zecured_policy::derive::payout_ref` and `zecured_policy::ledger::claim_ref`, and
the same functions in `tier2/statement` (`violation.rs`).

## 1. The idea

A 3-of-5 FROST signature on a Zcash spend says nothing about who signed: three colluding signers
can spend a vault and nobody can point at them. Tier 3 changes the order of signing. In a fully
shielded v6 transaction the signature digest *is* the txid (ZIP 244, as modified for v6 by
ZIP 229: with no transparent inputs, the shielded sighash is the txid digest; pinned by zcash-ops'
`frost_spend` test), so every signer knows the payout's txid in FROST round 1, before any share
leaves it. The signers first **commit** the
payout on Base (which lineage note it spends, which claims it pays, which change note it returns),
and only then release their round-2 shares. Every value then has an owner who can check it:

| Where the value of a payout goes | Who checks it | How a lie is proven |
|---|---|---|
| A spend nobody committed | anyone with the vault's viewing key | F1 |
| The change back to the vault | anyone with the vault's viewing key | F2 |
| Each claim's payment | the claimant, with its own wallet's viewing key | F3 |
| The reference each claim is committed under | the borrower (its own claims) and every signer, with the vault's viewing key and the claim's salt | F4 |

Conservation on Base (`input = Σ claims + change`) leaves no value unaccounted for, so a theft
is always one of these. F4 exists because the Registry cannot check a claim's salted reference: a
commitment under a wrong one would hide the payout from its claimant, so F3 could never be proven
(red-team finding RT-1). Each proof is a zero-knowledge proof that reveals no Zcash transaction,
note or address. It slashes the signers' bonds and halts minting.

> **Verified in:** Base verifies every shielded ZEC deposit with a zero-knowledge proof that never
> reveals which Zcash block it was in.
>
> **Accountable out:** Signers must commit every payout on Base before they can sign it. Taking ZEC
> that backs loans — without a commitment or under a false one — or paying any claim to anyone but
> its owner is provable on Base with a zero-knowledge proof that reveals no Zcash transaction, note or
> address, and it slashes the signers' bonds. Two things remain outside these proofs: refunds of
> non-conforming deposits, and liveness — signers refusing to pay, in which case the funds stay in
> the vault.

## 2. References: what Base sees is keyed or salted

`K = BLAKE2b-256(personal "Zecured_TagKey__", UFVK string)`, the vault's tag key (D24). A txid is
always in internal byte order (the ZIP 244 / ZIP 229 digest itself, not the reversed display form).

| Value | Definition | Code |
|---|---|---|
| `noteTag(cmx)` | `BLAKE2b-256(key K, personal "Zecured_DepTag__", cmx)`: the D24 deposit tag, used for every lineage note (deposit and change) | `derive::deposit_tag` |
| `payoutRef(txid)` | `BLAKE2b-256(key K, personal "Zecured_PayRef__", txid)` | `derive::payout_ref` |
| `claimRef_i` | `keccak256(salt_i ‖ txid)`, `salt_i` the salt of claim *i*'s recipient commitment `keccak256(salt_i ‖ receiver_i)` | `ledger::claim_ref` |

Without the UFVK, a tag or a `payoutRef` links to nothing; without `salt_i`, a `claimRef` links to
nothing. A claimant knows its salt but not `K`: `claimRef` is how it finds and names the payout.

## 3. What the signers sign: `SpendCommit` (EIP-712, Registry domain)

```
SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)
```

- `inputTag`: the `noteTag` of the lineage note the payout spends. A payout spends the whole
  conforming lineage (`zecured_policy::ledger`), and the lineage is always a single unspent note
  (the activated deposit or the latest change), so one tag; a node refuses a plan with more inputs.
- `claimIds`: the claims it pays, at least one (T3-1: no "change only" commitment), in plan order;
  `claimIds[0]` bears the fee. `claimRefs[i]` belongs to `claimIds[i]`.
- `claimOutputs[i]`: the index of the payout's action whose output pays `claimIds[i]` (E1). The
  nodes read it from the PCZT they verified; the indexes are distinct, so no output pays two claims.
- `payoutRef`: of the payout's txid.
- `changeTag`, `changeValue`: the change note returned to the vault's internal address (both zero for
  a payout that empties the vault).

Golden vector: `contracts/test/unit/ZecuredEIP712.t.sol` (`GOLDEN_SPEND_COMMIT_DIGEST`, checked
against Foundry's EIP-712 implementation) and `zecured-base` `spend_commit_golden_vector`.

## 4. What the Registry keeps (v2, Deploy 2): `IZecuredSpendCommits`

| State | Set by | Meaning |
|---|---|---|
| `lineageOf(tag) = (vaultId, value)` | a pending activation (deposit tag; removed if dropped), `commitSpend` (change tag) | the notes the vault's zZEC rests on |
| `spendRecord(payoutRef) = (recordHash, vaultId, inputTag, changeTag, changeValue, claimCount, signerBitmap)` | `commitSpend` | one committed payout |
| `claimPayment(claimRef) = (claimId, payoutRef, expected, outputIndex)` | `commitSpend` | what the claimant must receive, and in which output |
| `claimInput(claimId)` | the first `commitSpend` naming the claim | the lineage note the claim is bound to |
| `inputClaimSet(tag)` | the first `commitSpend` from the note | the hash of its claim list: a later commitment from the note (a rebuild of the same payout) must carry exactly that list (RT-4) |
| `claimRefOf(payoutRef, commitment)` | `commitSpend` | the reference the payout's commitment gives the claim of that recipient commitment (F4) |

`commitSpend(c, sigs)`, sent by anyone, requires:
- at least 3 sorted signatures of distinct signers (I6);
- `payoutRef` never committed;
- `inputTag` a lineage note of the vault;
- 1 to 8 distinct claims, each of the vault, `Pending` and bound to no other input;
- unused claim refs, one per claim;
- distinct output indexes, one per claim (E1), and distinct recipient commitments (a payout never
  names two claims of one commitment, so `claimRefOf` is unambiguous);
- the claim list of the note's first commitment, if it has one (RT-4);
- **conservation**: `lineageOf(inputTag).value == Σ claim.amount + changeValue`, exact because the fee
  is paid out of a claim;
- a change tag exactly when there is change, not yet a lineage note;
- a fee-bearing claim larger than the fee. The Registry computes the fee as the signers' plan does:
  ZIP 317 with one input, `5,000 × max(2, claims + (change > 0))` zatoshi. ZIP 317 is mempool policy,
  not consensus: if the network's fee policy changed, this constant could make an honest payout look
  short. That is no risk within the hackathon; on mainnet the fee constants must be updatable behind
  a timelock (roadmap).

`commitSpend` then:
- records the change as a lineage note;
- records `expected = amount − fee` for `claimIds[0]` and `amount` for the others;
- binds every claim to `inputTag`;
- emits `SpendCommitted`.

`confirmPayout` additionally requires each claim to have been committed.

`finalizeActivation` reverts with `ViolationCaseOpen` while the slasher has a case open on the vault
(`openCasesOf(vaultId)`, E2). A deposit note stolen while its activation is pending (it is a lineage
note from the proof on, so the theft is F1) therefore never backs zZEC. The hold is per vault, so a
false case cannot stop the system; if the case is dismissed the activation goes on, and the global
halt still needs an executed case. A pending deposit is also neither refundable (its tag counts as
used for the nodes' P5 guard) nor cancellable (`cancelVault` refuses a vault with a pending proof);
once a pending activation is dropped, cancelling is possible again (E3).

Each of the following is prevented, and each has a test:

- **Replaying a claim** on successive change notes (commit and mine C from t0, never confirm it,
  commit C again from t1): the claim stays bound to t0.
- **A commitment with no claim** written after an uncommitted theft, to have the F1 case dismissed
  while no claim is pending (T3-1).
- **Minting after a slash**: activation reverts once the bond contract has halted.
- **Two claims paid by one note** (E1): each claim names its own output, the Registry refuses a
  repeated index, and F3 reads only that output, so a second claim to the same receiver cannot hide
  behind the first one's payment (`test_F3_aSecondClaimToTheSameReceiverIsNotMasked`).
- **Activating a deposit a case says was stolen** (E2), and **refunding or cancelling a pending
  deposit** (E3).
- **Hiding a payout from its claimant** under a wrong reference (RT-1): provable as F4.
- **Two claim sets from one note** (RT-4), which could bind a claim for ever to a note a payout
  without it spent.
- **A pending deposit frozen by a halt** (RT-6): once minting has halted, any pending activation can
  be dropped, then the vault cancelled and its deposit refunded.
- **A dust claim keeping a vault open for ever** (RT-8): a claim of a closed vault, never committed
  and at most 10,000 zat (the smallest payout's fee, so it can never bear one), can be written off
  by anyone (`writeOffDust`); its value stays in the vault.

Each of these has a regression test (`contracts/test/fork/RedTeamRegressions.t.sol`, the review's
proofs of concept with their outcome reversed).

## 5. The signing protocol

**Round 1 (each node).**
1. Verify the PCZT against the node's own plan (unchanged).
2. Compute `inputTag`, `payoutRef(sighash)` and the `claimRefs`, plus the change tag from the change
   output (it already checks that output decrypts).
3. Check with `eth_call` that `inputTag` is this vault's lineage note.
4. Sign `SpendCommit` with its ECDSA attestation key.
5. Return the signature with its FROST commitments.

**Coordinator.** Collect at least 3 `SpendCommit` signatures and send `commitSpend`. Wait until it is
a few Base blocks deep, then run round 2, handing every node the signatures it used.

**Round 2 (each node, T3-4).** Release the share only when both hold:
- the node holds **every** signature the `commitSpend` used;
- it sees `spendRecord(payoutRef).recordHash` equal to the struct hash it signed, on **two independent
  RPC providers**, at least a few blocks deep.

Holding the signatures lets anyone resubmit the commitment, since `commitSpend` needs no
permission. The coordinator keeps every in-flight payout's commitment and its signatures and, on
every pass, submits it again if Base no longer has it (a reorg dropped it, RT-5); a case opened in
the meantime is dismissed once it is back. The nodes read the record on their primary Base RPC and on
a witness RPC (set by default outside the local fork).

**Node policy.**
- T3-5 (liveness): a rebuilt payout always carries the claims already committed against its input,
  and only those: the planner reads the claims bound to the lineage note on Base (`claimInput`, every
  provider agreeing) and plans exactly them first (RT-3); a node also checks the plan against the
  note's committed claim list (`inputClaimSet`).
- E1: a payout never pays one receiver twice. The only payout with two claims is a close (borrower
  and market maker); if both name the same receiver, the borrower is paid first and the market maker
  in the next payout.
- H-1: a node never signs a second payout while one it signed can still be mined, which it assumes
  for 10 blocks past the payout's expiry height (a reorg margin, RT-4). A claim is therefore never
  committed to two payouts that could both land.

**Refunds of non-conforming notes (P5)** need no commitment. Those notes are never lineage notes, so
no zZEC rests on them (§ 10).

## 6. The violation statements: one guest, one entry point (T3-3)

There is one RISC Zero guest with one image id, and `kind ∈ {Uncommitted, WithheldChange, Misdirected}`.

**Common to every kind.** The guest first proves, with the Tier 2 inclusion code (`check_inclusion`),
that a transaction X is in a block committed by an anchor header on the relay; it computes X's txid
from its bytes. The journal is `abi.encode(ViolationJournal{kind, anchor, beneficiary, ref, noteTag,
value, destCommitment, outputIndex})`, eight words; the fields a kind does not use are zero. The reward
goes to `beneficiary`, which is bound inside the proof, so nobody can copy a seal from the mempool and
redirect the reward (`test_prove_revertsOnABadJournalOrSeal`: a seal for another beneficiary fails).

**F1 Uncommitted.**
- Private input: the vault's UFVK, a lineage note's opening, and the index of the action of X that spends it.
- The guest shows: the note's nullifier (`Note::nullifier` under the UFVK's full viewing key) is
  revealed by X; `noteTag = noteTag(cmx)` and `ref = payoutRef(txid_X)`, both under the UFVK's K.
- Base checks: `lineageOf(noteTag)` exists, and `spendRecord(ref).inputTag ≠ noteTag`.
- Slashes all five bonds: FROST hides which three signed.

**F2 WithheldChange.**
- Private input: the vault's UFVK.
- The guest shows: `ref = payoutRef(txid_X)`, and no action of X carries a note whose tag is `noteTag`
  and which decrypts, under the vault's internal IVK, to a note of value `value`. Honest change goes to
  the internal scope; change sent to the vault's external address does not count, since the signers
  would take it for a deposit to refund (RT-9).
- Base checks: `spendRecord(ref)` has change, with `changeTag == noteTag` and `changeValue == value`.
- Slashes the record's signers (`signerBitmap`): they signed a false record.

**F3 Misdirected (any claimant).**
- Private input: the claimant's salt, receiver and wallet IVK, and the claim's output index
  (`claimPayment(ref).outputIndex`).
- The guest shows:
  - `ref = keccak256(salt ‖ txid_X)` and `destCommitment = keccak256(salt ‖ receiver)`;
  - the IVK owns the receiver (`pk_d = [ivk] g_d`);
  - `value` is what action `outputIndex` of X pays **exactly that receiver** (E1): its note,
    decrypted under the IVK with librustzcash's checked decryption, which rebuilds the note and
    requires its commitment to equal the action's `cmx`. A note to another address of the same
    wallet, a ciphertext that decrypts to more than its `cmx` commits, or an index X does not have,
    all count as 0. Honest nodes always pay the committed receiver in the committed output, so this
    raises no false alarm.
- Base checks: `claimPayment(ref)` exists for `outputIndex`, its claim's commitment is
  `destCommitment`, and `value < expected`.
- Slashes the payout's signers.

**F4 WrongClaimRef (the borrower, and every signer).**
- Private input: the vault's UFVK, and a claim's salt and receiver.
- The guest shows: `ref = payoutRef(txid_X)`, `noteTag = keccak256(salt ‖ txid_X)` (the reference an
  honest commitment of X gives the claim) and `destCommitment = keccak256(salt ‖ receiver)`.
- Base checks: `spendRecord(ref)` exists, and `claimRefOf(ref, destCommitment)` is set and differs
  from `noteTag`.
- Slashes the record's signers: they signed a reference that hides X from the claimant.

**Cases.** `proveViolation(kind, journal, seal)`:
- checks the seal (pinned selector, violation image id) and the anchor (best chain, at least
  `slashConfirmationWork`);
- checks the kind's condition and a beneficiary that is not a vault's smart account (smart accounts
  never receive ETH);
- then opens case `keccak256(kind, ref, noteTag)`: one case per violation, whoever proves it first.

`executeSlash(caseId)`, after the challenge window:
- checks the anchor and the condition again;
- dismisses an F1 case whose commitment appeared meanwhile. That is an honest resubmission after a
  Base reorg; a lying late commitment is F2's to catch, and needs a pending claim (T3-1);
- dismisses any case whose anchor left the best chain;
- otherwise slashes.

**Who can prove.**
- F1 and F2: anyone holding the vault's viewing key. That is the borrower (the app keeps the UFVK from
  the vault's opening, encrypted on the user's side, S6), a watcher the borrower gave it to, and the
  signers themselves, so an honest minority is a natural watcher.
- F3: every claimant, borrower or third party. A third party finds X by testing
  `keccak256(salt ‖ txid)` against its committed `claimRef` for each new Zcash transaction.
- F4: whoever holds the vault's UFVK and the claim's salt: the borrower for its own claims, and the
  signers for every claim. With three colluders, two honest signers remain to prove it.

**Tools.**
- **Any UFVK holder:** `zecured-wallet watch --ufvk-file <vault UFVK> --tier3` checks every spend of
  the vault's lineage (F1, F2).
- **The borrower** adds `--claims-of <vault id>` to check its own claims:
  - its loan file keeps the salt of every destination it gave;
  - for each committed payout X, the watcher checks that the commitment names the claim by
    `keccak256(salt ‖ txid_X)` (F4);
  - it then decrypts the named output with the wallet's own viewing key (F3).
- **A signer's operator** checks every claim of a vault the same way, with
  `zecured-node export-audit` (the vault's UFVK and every claim's salt and receiver, in operator-only
  files) and `watch --tier3 --destinations`.
- **A third-party claimant** (the liquidator, the market maker), which has no UFVK, uses
  `zecured-wallet watch-claims --destinations <the bot's file>`. The bots keep every commitment's
  salt (RT-2). For each `SpendCommitted` naming one of its claims, the claimant finds X among the
  Zcash transactions by its reference and decrypts the committed output.

Each finding is written to an evidence file (0600) and prints the `build-violation uncommitted |
withheld-change | misdirected | wrong-claim-ref` command that builds its witness.

Proofs are made on the prover's own machine, because the witness holds a viewing key.

## 7. Bonds: `ISignerBond` (S5, S6)

- **Deposit.** Each of the five signers deposits its bond. On testnet this is 0.01 Base Sepolia ETH,
  which shows the mechanism, not economic security.
- **Slash.** A slash takes the whole bond of every signer the case names. It credits the prover with
  a bounty of 10 %, capped at half a bond (pulled, so a reverting beneficiary cannot block the slash).
  The rest goes to a compensation pool: nothing is burned, and paying victims from the pool is
  roadmap.
- **Halt.** The first slash **halts** minting for good.
- **Withdrawal.** A signer can withdraw only when all of these hold (after a halt too, so that the
  signers a slash did not name get their bonds back):
  - no case is open;
  - no vault is open;
  - the challenge window has passed since the last vault settled. The same five signers hold every
    vault, so "its vaults" are all vaults.
- **Consortium bond.** FROST hides who signed, so an uncommitted spend slashes all five, while a false
  record slashes exactly its signers. Deterrence would need each bond to exceed a third of the value
  at stake; the demo amounts do not.

**Watcher assumption.** For each vault, at least one holder of its UFVK is online within the window
(the borrower, or an honest signer running the audit), and so is each claimant for its own claims.

## 8. Parameters (S4, T3-6)

| Parameter | Judge deployment | Theft-demo deployment |
|---|---|---|
| `slashConfirmationWork` | ≥ 100 normal Testnet blocks of work (~3.8 × 10^7), the anchor rechecked at the end of the window | same |
| Challenge window | 24 hours | 2 minutes (labelled DEMO) |
| Bond per signer | 0.01 ETH | 0.01 ETH |
| Build | the judge build: no compromise mode beyond D18 | `demo-tier3` feature, its own Registry, bond and slasher, five demo keys, same relay |

**The theft demo (T3-6)** never runs on the judge deployment. It has its own deployment
(`THEFT_DEMO=true`, a `DEPLOYMENT_NAME` containing `theft-demo`, a 2-minute window), its own five
signer keys and coordinator (`THEFT_DEMO=1 scripts/signer-network.sh`), and its own build
(`demo-tier3`, in `target/theft-demo/`). Only that build lets a third node (node 3) be compromised
and adds `POST /admin/vaults/:id/steal`: three compromised nodes sweep the vault with no commitment
on Base, which is an F1 violation, and the demo then proves it and slashes. The guards make a
misconfiguration end in an error message, never in a theft on the judge network:
- the deploy script refuses a theft-demo signer set that shares an address with the judge signers;
- that build's node and backend refuse to start if any of these holds
  (`zecured_base::theft_demo_guard`):
  - the configured Registry is a judge Registry (a compiled-in list);
  - a signer of the network config, or of the Registry on chain, is a judge signer;
  - the node's own key is a judge signer.
  A test fails if the compiled-in lists stop matching `deployments/base-sepolia.json`;
- outside that build, at most two nodes (4 and 5) can ever be compromised, a rule enforced by both
  the node and the backend.

## 9. Measurements on real Testnet data

| What | Result |
|---|---|
| A real vault spend | the first payout of the disposable vault 9 (local-fork runs of 2026-10-11, Testnet height 4,488,561) spends the vault's deposit note: `build-violation uncommitted` builds the F1 witness (anchor 4,488,600), the statement proves it with the deposit tag the Registry activated the vault with (`TEST-ONLY_vault9_f1_witness.json`), and `ZecuredSlasher` slashes with exactly that journal (`test_F1_theRustStatementsJournalOfARealSpend`) |
| Statement tests | 19 (`tier2/statement/tests/violations.rs`): each kind on real transactions (F2 against the real payout's internal-scope change, F3 with the vault as claimant), every field of the F1 opening and the viewing key bound, the inclusion checks, the journal ABI; for F3 (E1) only the claim's own output counts, another address of the same wallet is not the receiver, and a note that does not match its `cmx` pays nothing; F4 gives the reference an honest commitment names a claim by; F2 does not count change sent to the external scope |
| F3 through the tools | `build-violation misdirected` on an evidence file for the TEST-ONLY vault as claimant of its own deposit (a stand-in payout): the right output index gives the note's 1,000,000 zat, another index 0; the witness runs in the zkVM in **18.9 M cycles** (20 segments), journal identical |
| zkVM | violation guest image id, reproducible build (E4, 2026-10-14, after the red-team fixes): `0x7d8c9dd23e813e0e44ffea2b18ec9ec451b52a8a7b634ae998583d49b73cc526`; F1 on the real spend: **26.2 M cycles** (26,225,609, 28 segments), journal identical to the native computation; 11 changes (opening, action, kind, inclusion, an F2 and an F4 on the same payout) give the same outcome natively and in the executor, locally and in CI on the Docker-built guest (`zecured-tier2-host violation parity`) |

## 10. Out of scope, said plainly (T3-7)

- **Non-conforming notes** (over- or late payments) are the depositor's refund, not zZEC backing.
  Their theft is not provable yet (roadmap).
- **All five signers colluding** against a third party's claim. A liquidator's or market maker's
  claim committed under a wrong reference is provable (F4) only with the vault's UFVK and the claim's
  salt, which only the signers hold together. With three colluders two honest signers remain; with
  five, nobody can prove that one. The borrower's own claims stay provable either way.
- **Liveness.** Signers can refuse to pay. The ZEC stays in the vault, visible with the UFVK, so
  refusal is not theft. A liveness penalty is roadmap. Three related cases:
  - **A payout confirmed but never mined.** Three colluders can have `confirmPayout` accept a
    committed payout that never landed. That is no profit: the claimant loses its claim, and its value
    stays frozen in the vault. The unspent input remains a lineage note: spending it without a
    commitment is F1, and any later commitment from it must carry its full value (conservation), so
    the unpaid amount stays in the committed change, which F2 protects.
  - **One signer can stop new mints.** Minting needs all five bonds (`fullyBonded()`). A signer may
    withdraw its bond only when no vault is open, and then no new vault can be activated until it
    bonds again. Open vaults are unaffected.
  - **Dust claims.** A claim of at most 10,000 zat can never bear a payout's fee. Once its vault is
    closed anyone can write it off (`writeOffDust`, RT-8): the vault settles and the bonds are no
    longer held by it; the dust stays in the vault.
- **Testnet proof of work.** A single Equihash ASIC out-mines the whole Testnet about 40 times over,
  so every proof-of-work-based guarantee here, Tier 2 minting and Tier 3 slashing, is demonstrative on
  Testnet. A forged block can carry a forged "theft". The guards against that are:
  - a slash needs far more work than a mint, and the anchor is rechecked after the window;
  - the bounty is capped below a bond.
- **A theft between proof and finalization of a deposit** is F1 (the note is lineage while its
  activation is pending). E2 holds an activation while a case about the vault is open, but with the
  deployed parameters it rarely gets the chance (RT-7). An F1 case needs an anchor with about 100
  blocks of work on top of the theft, while finalization waits 6 blocks and 150 s. A deposit stolen
  while pending is therefore minted first and slashed after the fact, like a theft after
  activation.

## 11. Privacy

A commitment shows only:
- PRF outputs under the UFVK's key (tags, `payoutRef`);
- salted hashes (`claimRefs`);
- `changeValue`, which Base can already compute (outstanding plus unpaid claims).

A violation journal shows:
- the anchor header;
- a tag or a reference already on Base;
- for F3, the value the claimant received; for F4, the claim reference an honest commitment gives.

The beneficiary of a proof (and its sender) is linked on Base to the case's vault: provers should
use a fresh address (RT-14).

**New leak (L23).** A commitment lands on Base before its transaction is broadcast, which points at
the next Zcash blocks. The mitigation is a random delay of 1 to 8 blocks between the signed
commitment and the broadcast (the network config's maximum; at least 40 of the 120 expiry blocks
stay free, RT-15). What remains, said plainly (RT-13):
- commitments are not batched across vaults: each is its own Base transaction;
- the number of outputs is public on both chains (the claim count and change on Base; the action
  count and the fee on Zcash), and `claimOutputs` adds the output positions. The anonymity set is the
  Ironwood transactions of the same shape in the next 8 blocks.
