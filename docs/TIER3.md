# Tier 3: theft of collateral is provable on Base and slashes bonds

Status (2026-10-27): **implemented, and deployed on Base Sepolia for the live run of 27 October** (Deploy 2). Contracts: Registry v2
(`commitSpend`), `SignerBond`, `ZecuredSlasher`. Signers: the commitment in FROST round 1 and the
round-2 checks (T3-4, T3-5). The violation statement, its zkVM guest, `build-violation` (F1, F2, F3)
and the watcher (`zecured-wallet watch --tier3`) are done, and a real Testnet payout went through the
whole protocol (`docs/TESTING.md` § 2); the review's E1 to E3 are in (D34), and so are the fixes of
the red-team review of 2026-10-14 (D38), including the fourth violation kind, F4. The theft
demonstration (T3-6) runs on its own deployment, on a fork. The guest round (2026-10-15, design notes T-09) made the Zcash network a
build parameter (one violation guest per network), gave F4 the vault's tag key K instead of its
viewing key, and added a fifth kind, `Included`, for a later contract (§ 6). This document fixes what
the contracts, the signers and the violation guest agree on:
- the message the signers sign;
- the state the Registry keeps;
- the four violation statements;
- the bond rules.

It follows the review's decisions T3-1 to T3-7 and S4 to S6 (D31). The interfaces are:
- `contracts/src/libraries/ZecuredEIP712.sol` (`SpendCommit`);
- `contracts/src/interfaces/v3/IZecuredSpendCommits.sol`;
- `contracts/src/interfaces/v3/ISignerBond.sol`, `IZecuredSlasher.sol`, `IZecuredCompensator.sol` and
  `IZecuredDisputes.sol` (v3).

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
| The reference each claim is committed under | the borrower (its own claims) and every signer, with the vault's tag key K (from its viewing key) and the claim's salt | F4 |

Conservation on Base (`input = Σ claims + change`) leaves no value unaccounted for, so a theft
is always one of these. F4 exists because the Registry cannot check a claim's salted reference: a
commitment under a wrong one would hide the payout from its claimant, so F3 could never be proven
(red-team finding RT-1). Each proof is a zero-knowledge proof that reveals no Zcash transaction,
note or address. It slashes the signers' bonds and halts minting.

> **Verified in:** Base verifies every shielded ZEC deposit with a zero-knowledge proof that never
> reveals which Zcash block it was in.
>
> **Accountable out:** Signers must commit every payout on Base before they can sign it. Taking ZEC
> that backs loans (without a commitment, or under a false one), or paying any claim to anyone but
> its owner is provable on Base with a zero-knowledge proof that reveals no Zcash transaction, note or
> address, and it slashes the signers' bonds; the bonds compensate the victims: liquidators and
> lenders before the borrower's own equity. If the signers stop paying a claim, its claimant is
> paid from the bonds; a payout they confirm but never send can be disputed.

## 2. References: what Base sees is keyed or salted

`K = BLAKE2b-256(personal "Zecured_TagKey__", UFVK string)`, the vault's tag key (D24). A txid is
always in internal byte order (the ZIP 244 / ZIP 229 digest itself, not the reversed display form).

| Value | Definition | Code |
|---|---|---|
| `noteTag(cmx)` | `BLAKE2b-256(key K, personal "Zecured_DepTag__", cmx)`: the D24 deposit tag, used for every lineage note (deposit and change) | `derive::deposit_tag` |
| `payoutRef(txid)` | `BLAKE2b-256(key K, personal "Zecured_PayRef__", txid)` | `derive::payout_ref` |
| `claimRef_i` | `keccak256(salt_i \|\| txid)`, `salt_i` the salt of claim *i*'s recipient commitment `keccak256(salt_i \|\| receiver_i)` | `ledger::claim_ref` |

Without K (derived from the UFVK; `VaultOpened` v2 binds it to the vault), a tag or a `payoutRef` links to
nothing; without `salt_i`, a `claimRef` links to nothing. A claimant knows its salt but not `K`: `claimRef` is how it finds and names the payout.

## 3. What the signers sign: `SpendCommit` (EIP-712, Registry domain)

```
SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)
```

- `inputTag`: the `noteTag` of the lineage note the payout spends. A payout spends the whole
  conforming lineage (`zecured_policy::ledger`), and the lineage is always a single unspent note
  (the activated deposit or the latest change), so one tag; a node refuses a plan with more inputs.
- `claimIds`: the claims it pays, at least one (T3-1: no "change only" commitment), in plan order;
  `claimIds[0]` bears the fee, less ZIP 317's marginal 5,000 zatoshi for each Fee claim behind it, which
  bears exactly that (T-06, D48). `claimRefs[i]` belongs to `claimIds[i]`.
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
- 1 to 8 distinct claims, each of the vault, `Pending` and bound to no other input; a new commitment
  of an input also names its claims once confirmed or disputed (D57): every commitment of an input
  carries the same claims, so while that input is unspent none of its payouts landed;
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
- records `expected = amount − fee` for `claimIds[0]` (its share of the fee), `amount − 5,000` for a
  Fee claim behind it, and `amount` for the others;
- binds every claim to `inputTag`;
- emits `SpendCommitted`.

`confirmPayout` (v4, R-1) names the committed payout it confirms (`payoutRef`), once per payout,
for pending claims of the vault bound to that payout's input; never a reopened claim, never on a
frozen vault. A confirmation is a fast first step; the payout is final once proven (§ 5).

`finalizeActivation` reverts with `ViolationCaseOpen` while the slasher has a case open on the vault
(`openCasesOf(vaultId)`, E2). A deposit note stolen while its activation is pending (it is a lineage
note from the proof on, so the theft is F1) therefore never backs zZEC. The hold is per vault, so a
false case cannot stop the system; if the case is dismissed the activation goes on, and the global
halt still needs an executed case. A pending deposit is also not refundable off-chain (its tag counts
as used for the nodes' P5 guard); on Base it becomes a refund only through `refundPending` (after a halt,
or a long pause), and once a pending activation is dropped its note can be proven again (E3).

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
- **A pending deposit frozen by a halt** (RT-6): once minting has halted, `refundPending` (anyone;
  the coordinator does it as a keeper) turns any pending activation into a Refund claim of its
  deposit, which the signers pay like any claim.
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
as deep as the nodes check (below), then run round 2, handing every node the signatures it used.

**Round 2 (each node, T3-4).** Release the share only when both hold:
- the node holds **every** signature the `commitSpend` used;
- it sees `spendRecord(payoutRef).recordHash` equal to the struct hash it signed, on **two independent
  RPC providers**, at Base's `safe` head or, while that lags, in a block `commit_depth_secs` old
  (default 120 s), and at least `commit_depth_blocks` deep (review H-11: a depth in seconds or at
  the safe head keeps its meaning with 200 ms blocks).

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
- E1: a payout never pays one receiver twice, nor names one salt twice (two claims with one salt
  would share a claim reference, which the Registry refuses). A payout settles up to 8 claims (review
  H-06): a claim to a receiver, or with a salt, the payout already has waits for a later payout, and
  if a close's borrower and market maker share one, the borrower is paid first and the market maker
  in a later payout.
- H-1: a node never signs a second payout while one it signed can still be mined, which it assumes
  for 10 blocks past the payout's expiry height (a reorg margin, RT-4). A claim is therefore never
  committed to two payouts that could both land.
- v3 (D41 to D53): nothing is paid from a vault whose activation is pending (K3) or that the slasher
  marked compromised (T-04: the compensator settles its claims). A claim is payable once it is pending
  on Base with a destination: a Refund claim once its account gave one, from its own note alone (its
  own lineage; the main lineage backs only the main claims, I10); a Fee claim rides with the vault's
  first exit, behind it, and heads a payout only when nothing else waits; a claim the bonds paid goes
  to its own treasury destination, which three signers gave after each derived it from the claim id
  (W-6), without the vault's ovk. A disputed
  claim counts unpaid but is not payable until it is reopened, and then only by a payout of its input
  with the same claim list (RT-4), which may name the list's confirmed claims again (D57): while the
  input is unspent, no payout of it landed. A node that confirmed the payout itself keeps it paid,
  since it saw its value leave. The coordinator plans with the same functions
  (`ledger::next_vault_payout`).
- Planner rules (v4, O-4), applied by every node and the coordinator alike:
  1. A reopened claim whose liveness is overdue is not committed again: its claimant takes the bonds'
     payment, and the claim's ZEC then reimburses the bonds.
  2. No claim is committed within one payout lifetime of its `livenessDue`, read on every provider,
     which must agree (an unarmed claim, or an unreadable due time, reads 0, so payouts never stop).
     The lifetime is `(120 + 10 + confirmations) x 25 s + 300 s`, about an hour; the deploy script
     requires `commitGrace` of at least twice a payout's 136 blocks (R-10).
  3. When a vault's books do not add up (I10), a confirmed claim that is not final and whose input is
     still an unspent lineage note counts as unpaid again: a new commitment of that input names it
     (D57), it is paid, and the payout is proven. A final claim (proven, or a confirmation that
     expired undisputed, R-3) is never planned again.
- Liveness (T-05): every node and the coordinator warn when a payable claim is older than half the
  bonds' `livenessPeriod`, at most every half hour per vault, naming the vault by its id prefix only.

**Proven delivery (v4, D76).** A payout is final on Base once its inclusion in a Zcash block
is proven in zero knowledge, or once its signed confirmation's dispute window closes undisputed (R-3).
The coordinator proves every payout it committed: once the payout's block
has `slashConfirmationWork` on top of it, it builds the `Included` witness (journal `(5, anchor, 0,
payoutRef, 0, 0, 0, 0)`, the witness holding only K) and calls `proveInclusionBatch` for the payout's
claims that are not final yet. Each becomes final (`Paid`, `paidAt == 0`) from whatever state it is in:
committed and pending (also on a frozen or compromised vault, until the compensator tallies it),
confirmed, disputed or reopened. Its queue proves deposits first, then disputes, swap settlements and
routine inclusions. Anyone can prove a payout the same way, with only the vault's K. The signed
confirmation stays as a fast first step: at most one per payout, never a reopened claim, never on a
frozen vault; a claimant who sees it before the proof may dispute it (D53), and a dispute is answered
by the same proof. A claimant disputes with `zecured-wallet loan dispute` (TEST ONLY, the owner EOA as
the account; the app sends the same call by UserOp).

**Off-chain refunds (P5)** need no commitment, and exist only for deposits that no guest of the
deployment can land on Base (I30, docs/TIER2.md § 5): those never become lineage notes, so no zZEC
rests on them (§ 10). Every other non-conforming deposit lands as a Refund claim and is paid as above.

**Share refresh and repair (C04).** Both keep a vault's group key, hence its address and viewing
keys. A refresh gives every signer a new share, so that a share stolen before it is useless after it;
a repair rebuilds one signer's lost share from three others, none of whom learns it.

| Level | State |
|---|---|
| Library (`zecured-threshold`) | Built and tested: `refresh_part1`, `refresh_part2` and `refresh_finish` (refused unless the group key is unchanged and the new share matches the public key package), `repair_deltas`, `repair_sigma` and `repair_finish`. Tests refresh all five shares, sign with the new ones and show that an old share no longer combines with new ones, and rebuild a lost share from three helpers (two are not enough) |
| Node API | Not built: no node endpoint runs a refresh or a repair round, and nothing carries their sealed messages between nodes |
| Operations | Not built: no runbook, never run on a live vault. A share lost today leaves four, still above the threshold of three; with three lost, the vault's claims can only be paid by the bonds (§ 7) |

## 6. The violation statements: one guest, one entry point (T3-3)

There is one RISC Zero guest per Zcash network (image ids violation-test and violation-main: the
network is a build parameter, docs/TIER2.md § 2.2), and `kind ∈ {Uncommitted, WithheldChange,
Misdirected, WrongClaimRef}`, plus `Included`, which proves no violation (below).

**Common to every kind.** The guest first proves, with the Tier 2 inclusion code (`check_inclusion`),
that a transaction X is in a block committed by an anchor header on the relay; it computes X's txid
from its bytes. The witness names the epoch's consensus branch id: the build must allow it, X must
carry it, and X's block and the anchor must be of it. The journal is `abi.encode(ViolationJournal{kind, anchor, beneficiary, ref, noteTag,
value, destCommitment, outputIndex})`, eight words; the fields a kind does not use are zero. The reward
goes to `beneficiary`, which is bound inside the proof, so nobody can copy a seal from the mempool and
redirect the reward (`test_prove_revertsOnABadJournalOrSeal`: a seal for another beneficiary fails).

**F1 Uncommitted.**
- Private input: the vault's UFVK, a lineage note's opening, and the index of the action of X that spends it.
  The full UFVK, not K and a nullifier key (design review): on a forged block nothing
  ties a nullifier key to the vault, so anyone who knows K, the deposit prover included, could pair it
  with a made-up one; and F1 is proven by the owner or our watcher anyway.
- The guest shows: the note's nullifier (`Note::nullifier` under the UFVK's full viewing key) is
  revealed by X; `noteTag = noteTag(cmx)` and `ref = payoutRef(txid_X)`, both under the UFVK's K.
- Base checks: `lineageOf(noteTag)` exists, and `spendRecord(ref).inputTag ≠ noteTag`.
- Slashes all five bonds: FROST hides which three signed.

**F2 WithheldChange.**
- Private input: the vault's UFVK. F2 proves an absence, so its incoming viewing key must be the
  vault's: the UFVK is tied to the vault's tags through `K = H(UFVK)`; an unbound key would "prove"
  every absence.
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
  - `ref = keccak256(salt || txid_X)` and `destCommitment = keccak256(salt || receiver)`;
  - the IVK owns the receiver (`pk_d = [ivk] g_d`);
  - `value` is what action `outputIndex` of X pays **exactly that receiver** (E1): its note,
    decrypted under the IVK with librustzcash's checked decryption, which rebuilds the note and
    requires its commitment to equal the action's `cmx`. A note to another address of the same
    wallet, a ciphertext that decrypts to more than its `cmx` commits, or an index X does not have,
    all count as 0. Honest nodes always pay the committed receiver in the committed output, so this
    raises no false alarm.
- Base checks: `claimPayment(ref)` exists for `outputIndex`, the commitment of its payout recorded the
  claim under `destCommitment` (`claimRefOf(payoutRef, destCommitment) == ref`: what the payout was
  committed to, so that a liveness payment that cleared the claim's destination since cannot hide
  the misdirection, W-5), and `value < expected`.
- Slashes the payout's signers; the compensator owes the claimant the shortfall.

**F4 WrongClaimRef (the borrower, and every signer).**
- Private input: the vault's tag key K, and a claim's salt and receiver. A wrong K gives a `payoutRef`
  with no record on Base.
- The guest shows: `ref = payoutRef(txid_X)`, `noteTag = keccak256(salt || txid_X)` (the reference an
  honest commitment of X gives the claim) and `destCommitment = keccak256(salt || receiver)`.
- Base checks: `spendRecord(ref)` exists, and `claimRefOf(ref, destCommitment)` is set and differs
  from `noteTag`.
- Slashes the record's signers: they signed a reference that hides X from the claimant. W-1: the
  hidden claim (`claimPayment` of the committed reference) is compensated in full, its `expected`,
  pending or already final; a claim the bonds paid is not (its claimant has its USDC).
- W-5: while an F3 or F4 case about a claim is open (`claimCaseOpen`), the bonds pay no liveness for
  it.

**Included (kind 5): not a violation.**
- Private input: the vault's tag key K.
- The guest shows: `ref = payoutRef(txid_X)` for an X in a block under the anchor. Journal
  `{5, anchor, 0, ref, 0, 0, 0, 0}` (no beneficiary).
- It makes a payout final (proven delivery, § 5): `ZecuredSlasher.proveInclusionBatch(journal, seal,
  imageId, claimIds)` (or `proveInclusion` for one claim) checks it like a violation's seal and anchor,
  then the Registry checks that `ref` is a committed payout of each claim's own input and vault; it
  upholds a disputed confirmation (D53) or a reopened one (D57) the same way. `proveViolation` refuses it (`ViolationKind` stops at
  `WrongClaimRef`).
- What it does not show (W-11): that X spent the committed input, nor that X was mined after the
  commitment. Three colluding signers can commit an input a second time naming a transaction mined
  months before, and make its claims final with an `Included` proof of that old transaction. Finality
  stays safe because every claimant still has its remedy against the named X: F3 when X's committed
  output did not pay it, F4 when the commitment named its claim under a reference it cannot find. For
  that, the watchers search the whole Ironwood history for X, not the blocks after the claim (below);
  a payout the scan does not show is resolved the same way by the borrower and the coordinator. A
  version of `Included` and F4 that also proves X spent the committed input is a mainnet design
  (MAINNET.md, W-22).

**Cases.** `proveViolation(kind, journal, seal, imageId)`:
- checks the seal (an active Violation version of `ZecuredVersions`: its router selector and image id,
  D54) and the anchor (best chain, at least `slashConfirmationWork`);
- checks the kind's condition and a beneficiary that is not a vault's smart account (smart accounts
  never receive ETH);
- then opens case `keccak256(kind, ref, noteTag)`, for F1 `keccak256(kind, ref, noteTag, vaultId)`:
  one case per violation, whoever proves it first.
- W-2: an F1 case belongs to the vault that owns its note's tag. When a deposit takes the tag over
  from a (false) change record, the case moves to the deposit's vault (`followNote`, anyone, open or
  executed; also at execution): a new case from the same verified fields, a new challenge window, that
  vault frozen; the old case, if open, is dismissed. While an F1 case is about a note, or one was
  executed (`openF1CasesOf`, `noteSpendProven`), the Intake activates no deposit of that note and drops
  it only once its anchor leaves the best chain.

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
  `keccak256(salt || txid)` against its committed `claimRef` for each new Zcash transaction.
- F4: whoever holds the vault's K (from its UFVK) and the claim's salt: the borrower for its own
  claims, and the signers for every claim. With three colluders, two honest signers remain to prove it.

**Tools.**
- **Any UFVK holder:** `zecured-wallet watch --ufvk-file <vault UFVK> --tier3` checks every spend of
  the vault's lineage (F1, F2).
- **The borrower** adds `--claims-of <vault id>` to check its own claims:
  - its loan file keeps the salt of every destination it gave;
  - for each committed payout X, the watcher checks that the commitment names the claim by
    `keccak256(salt || txid_X)` (F4);
  - it then decrypts the named output with the wallet's own viewing key (F3).
- **A signer's operator** checks every claim of a vault the same way, with
  `zecured-node export-audit` (the vault's UFVK and every claim's salt and receiver, in operator-only
  files) and `watch --tier3 --destinations`.
- **A third-party claimant** (the liquidator, the market maker), which has no UFVK, uses
  `zecured-wallet watch-claims --destinations <the bot's file>`. The bots keep every commitment's
  salt (RT-2). For each `SpendCommitted` naming one of its claims, the claimant finds X among the
  Zcash transactions by its reference and decrypts the committed output.
- **W-11: the whole Ironwood history.** A commitment may name a transaction mined long before it
  (above), so both watchers search every Ironwood transaction since the network's NU6.3 activation,
  from an index kept next to the wallet (`zecured_zcash_ops::txindex`: the txids of the transactions
  with Ironwood actions, public data; the first run reads the history once, later runs only the new
  blocks, and the last 100 blocks are read again each time). The claimant finds X by each committed
  claim reference wherever it was mined; `watch --tier3` also resolves every `SpendCommitted` of the
  vault by its payout reference, and reports an X that spent no lineage note: F2 if its committed
  change never came, and its claims are checked for F3 and F4 like any payout's. The coordinator does
  the same for commitments its scan does not show, and proves each hidden claim's F4 (W-1).

Each finding is written to an evidence file (0600) and prints the `build-violation uncommitted |
withheld-change | misdirected | wrong-claim-ref` command that builds its witness (`build-violation
included` builds an `Included` witness).

F1, F2 and F3 proofs are made on the prover's own machine, because their witnesses hold a viewing key.
F4 and `Included` witnesses hold only K, which links the vault's notes and payouts but shows no amount
or recipient.

**Per kind, in short (C05).** Every slash pays the prover its bounty and the rest to the victims (§ 7).

| Kind | What it shows | Bonds slashed | Who can prove it | The witness holds |
|---|---|---|---|---|
| F1 Uncommitted | a lineage note spent by a transaction Base has no commitment for | all five (FROST hides which three signed) | any holder of the vault's viewing key: the borrower, a watcher it chose, any signer | the viewing key |
| F2 WithheldChange | a committed payout without the change its record names | the record's signers | any holder of the vault's viewing key | the viewing key |
| F3 Misdirected | a claim's committed output paying less than the claim expects | the record's signers | the claimant: the borrower, a liquidator or a market maker | the claimant's salt, receiver and incoming viewing key |
| F4 WrongClaimRef | a commitment naming a claim under a reference that hides the payout from its claimant | the record's signers | holders of K and the claim's salt: the borrower for its own claims, any signer for every claim | K, the claim's salt and receiver |
| `Included` (not a violation) | a committed payout is in a Zcash block: its claims are final (proven delivery) | none | holders of K: the coordinator, any signer, the borrower | K |

## 7. Bonds: `SignerBondV2` (S5, S6; D43 to D47)

- **Deposit.** Each of the five signers bonds USDC (100 USDC each on testnet, which
  shows the mechanism, not economic security; docs/ECONOMICS.md sizes them for mainnet). What the
  bonds stand for is the effective bond, `min(sum, 5/3 x the three smallest)`, and new collateral is
  accepted only while it covers the collateral's value (the coverage cap, D44).
- **Slash.** A slash takes the whole bond and unbonding of every signer the case names. It credits
  the prover with a bounty of 10 %, capped (50 USDC on testnet; pulled, so a reverting
  beneficiary cannot block the slash). The rest goes to `ZecuredCompensator`, which pays the victims
  in layers: third parties, then the lenders, then the owners (D46).
- **Halt.** The first slash **halts** minting for good.
- **Leaving.** A signer leaves in two steps: an unbonding, slashable until it is withdrawn
  `unbondingPeriod` later, with no case open. A full exit needs no open vault, no open case, and the
  challenge window since the last vault settled (the same five signers hold every vault, so "its
  vaults" are all vaults); a partial exit keeps `minBond` and the coverage cap.
- **Liveness** (T-05). A claim left unpaid `livenessPeriod` after it became payable (deferred by a
  payout committed in flight for `commitGrace` at most) can be paid to its claimant from the bonds;
  its ZEC then reimburses the bonds (D47). It became payable only once a signer acknowledged its
  destination: a node signs a receipt (`DestinationReceived`) when it holds the salt and receiver
  and has checked them against the claim's commitment, and anyone arms the claim with it
  (`armLiveness`, R-5). A claimant that never hands over its destination gets no liveness, and
  one honest signer's receipt is enough. No liveness for a claim the compensator owes (R-7), nor
  at a stale price (R-9).
- **Consortium bond.** FROST hides who signed, so an uncommitted spend slashes all five, while a false
  record slashes exactly its signers. Any three colluders lose at least 3/5 of the effective bond.

**Watcher assumption.** For each vault, at least one holder of its UFVK is online within the window
(the borrower, or an honest signer running the audit), and so is each claimant for its own claims.

## 8. Parameters (S4, T3-6)

| Parameter | Testnet (Base Sepolia) | Theft-demo deployment |
|---|---|---|
| `slashConfirmationWork` | ≥ 100 normal Testnet blocks of work (~3.8 × 10^7), the anchor rechecked at the end of the window | same |
| Challenge window | 24 hours | 2 minutes (labelled DEMO) |
| Bond per signer (`minBond`) | 100 USDC | 10 USDC |
| Liveness period / commit grace | 24 hours / 2 hours | 10 minutes / 2 hours (a payout's lifetime, R-10) |
| Dispute window / inclusion window | 24 hours / 6 hours | 10 minutes / 20 minutes |
| Build | the standard build: no compromise mode beyond D18 | `demo-tier3` feature, its own Registry, bond and slasher, five demo keys, same relay |

**The theft demo (T3-6)** never runs on the lending deployment. It has its own deployment
(`THEFT_DEMO=true`, a `DEPLOYMENT_NAME` containing `theft-demo`, a 2-minute window), its own five
signer keys and coordinator (`THEFT_DEMO=1 scripts/signer-network.sh`), and its own build
(`demo-tier3`, in `target/theft-demo/`). Only that build lets a third node (node 3) be compromised
and adds `POST /admin/vaults/:id/steal`: three compromised nodes sweep the vault with no commitment
on Base, which is an F1 violation, and the demo then proves it and slashes. The guards make a
misconfiguration end in an error message, never in a theft on the lending deployment:
- the deploy script refuses a theft-demo signer set that shares an address with the lending deployment's signers;
- that build's node and backend refuse to start if any of these holds
  (`zecured_base::theft_demo_guard`):
  - the configured Registry is the lending deployment's (a compiled-in list, `JUDGE_REGISTRIES`);
  - a signer of the network config, or of the Registry on chain, is one of the lending deployment's
    signers (`JUDGE_SIGNERS`);
  - the node's own key is one of them.
  A test fails if the compiled-in lists stop matching `deployments/base-sepolia.json`;
- outside that build, at most two nodes (4 and 5) can ever be compromised, a rule enforced by both
  the node and the backend.

## 9. Measurements on real Testnet data

| What | Result |
|---|---|
| A real vault spend | the first payout of the disposable vault 9 (local-fork runs of 2026-10-11, Testnet height 4,488,561) spends the vault's deposit note: `build-violation uncommitted` builds the F1 witness (anchor 4,488,600), the statement proves it with the deposit tag the Registry activated the vault with (`TEST-ONLY_vault9_f1_witness.json`), and `ZecuredSlasher` slashes with exactly that journal (`test_F1_theRustStatementsJournalOfARealSpend`) |
| Statement tests | 24 (`tier2/statement/tests/violations.rs`): each kind on real transactions (F2 against the real payout's internal-scope change, F3 with the vault as claimant), every field of the F1 opening and the viewing key bound, the inclusion checks, the journal ABI; for F3 (E1) only the claim's own output counts, another address of the same wallet is not the receiver, and a note that does not match its `cmx` pays nothing; F4 gives the reference an honest commitment names a claim by, with K only; F2 does not count change sent to the external scope; `Included` names the payout by its reference only; the epoch is one the build allows and X's own; a Mainnet build refuses a Testnet viewing key. The F3 and F4 journals are byte for byte those before the guest round |
| F3 through the tools | `build-violation misdirected` on an evidence file for the TEST-ONLY vault as claimant of its own deposit (a stand-in payout): the right output index gives the note's 1,000,000 zat, another index 0; the witness runs in the zkVM in **18.9 M cycles** (20 segments), journal identical |
| zkVM | violation guest image id, reproducible build (E4): statement v1 (2026-10-14, after the red-team fixes; the guest measured in this row) `0x7d8c9dd23e813e0e44ffea2b18ec9ec451b52a8a7b634ae998583d49b73cc526`, statement v2 (2026-10-16) violation-test `0x65705f75c5482b2b0cf6fee12760b2364d48213bb2933f3882dc2d92f824866e` and violation-main `0x24edd49e52447665146009687b6b85e3f0b6184883cbf635a762048cd1a5ce33` (docs/TIER2.md § 8.1); F1 on the real spend: **26.2 M cycles** (26,225,609, 28 segments), journal identical to the native computation; 11 changes (opening, action, kind, inclusion, an F2 and an F4 on the same payout) give the same outcome natively and in the executor, locally and in CI on the Docker-built guest (`zecured-tier2-host violation parity`). The v2 suite (15 changes: also two epochs, F4 with K and `Included`) runs in the `guests` workflow on each network's guest |

## 10. Out of scope, said plainly (T3-7)

- **Non-conforming notes** (over- or late payments) are the depositor's refund, not zZEC backing.
  With refund proofs (journal v2's Refund mode, which testnet deployments take) they land on Base
  as Refund claims and are lineage notes like any other, so their theft is F1. The notes no guest of
  the deployment can land (without refund proofs: another amount, a late payment, other pools; with
  them: a note below `minRefundZat`; either way a note to another address of the vault) are refunded
  off-chain (P5), and their theft is not provable.
- **All five signers colluding** against a third party's claim. A liquidator's or market maker's
  claim committed under a wrong reference is provable (F4) only with the vault's K and the claim's
  salt, which only the signers hold together. With three colluders two honest signers remain; with
  five, nobody can prove that one. The borrower's own claims stay provable either way.
- **Liveness.** Signers can refuse to pay. The ZEC stays in the vault, visible with the UFVK, so
  refusal is not theft; it is answered by the bonds (T-05, § 7), up to their size. Three related
  cases:
  - **A payout confirmed but never mined.** Three colluders can have `confirmPayout` accept a
    committed payout that never landed, but a confirmation is not final while its dispute window is
    open: only a proof of the payout's inclusion, or the window closing undisputed (R-3), makes the
    claim final (§ 5). Its claimant disputes the confirmation (D53):
    unless someone proves the payout's inclusion in time (the `Included` kind), the claim is payable
    again, and from the bonds on its original liveness schedule. The unspent input
    remains a lineage note: spending it without a commitment is F1, and any later commitment from it
    must carry its full value (conservation) and its whole claim list, the confirmed claims included
    (RT-4, D57), so the payout can be made again. A proof that comes after the window still upholds
    the reopened claim (D57).
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
- PRF outputs under the vault's tag key K (tags, `payoutRef`);
- salted hashes (`claimRefs`);
- `changeValue`, which Base can already compute (outstanding plus unpaid claims).

A violation journal shows:
- the anchor header;
- a tag or a reference already on Base;
- for F3, the value the claimant received; for F4, the claim reference an honest commitment gives;
  for `Included`, only the anchor and a payout reference.

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
