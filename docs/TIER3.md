# Tier 3: theft of collateral is provable on Base and slashes bonds

Status (2026-10-12): **interface locked, implementation in progress** (Deploy 2). This document fixes
what the contracts, the signers and the violation guest must agree on before Deploy 2, whose
Registry is immutable: the message the signers sign, the state the Registry keeps, the three
violation statements and the bond rules. It follows the review's decisions T3-1 to T3-7 and S4 to
S6. The locked interfaces are `contracts/src/libraries/ZecuredEIP712.sol` (`SpendCommit`),
`contracts/src/interfaces/IZecuredSpendCommits.sol`, `ISignerBond.sol` and `IZecuredSlasher.sol`;
the derivations are `zecured_policy::derive::payout_ref` and `zecured_policy::ledger::claim_ref`.

## 1. The idea

A 3-of-5 FROST signature on a Zcash spend says nothing about who signed: three colluding signers
can spend a vault and nobody can point at them. Tier 3 changes the order of signing. In a fully
shielded v6 transaction the signature digest *is* the txid (ZIP 244: with no transparent inputs, the
shielded sighash is the txid digest; pinned by zcash-ops' `frost_spend` test), so every signer knows
the payout's txid in FROST round 1, before any share leaves it. The signers first **commit** the
payout on Base (which lineage note it spends, which claims it pays, which change note it returns),
and only then release their round-2 shares. Every value then has an owner who can check it:

| Where the value of a payout goes | Who checks it | How a lie is proven |
|---|---|---|
| A spend nobody committed | anyone with the vault's viewing key | F1 |
| The change back to the vault | anyone with the vault's viewing key | F2 |
| Each claim's payment | the claimant, with its own wallet's viewing key | F3 |

Conservation on Base (`input = Σ claims + change`) leaves no value unaccounted for, so a theft
is always one of the three. Each proof is a zero-knowledge proof that reveals no Zcash transaction,
note or address. It slashes the signers' bonds and halts minting.

> **Verified in:** Base verifies every shielded ZEC deposit with a zero-knowledge proof that never
> reveals which Zcash block it was in.
>
> **Accountable out:** Signers must commit every payout on Base before they can sign it. Taking
> collateral any other way is provable on Base and slashes their bonds.

## 2. References: what Base sees is keyed or salted

`K = BLAKE2b-256(personal "Zecured_TagKey__", UFVK string)`, the vault's tag key (D24). A txid is
always in internal byte order (the ZIP 244 digest itself, not the reversed display form).

| Value | Definition | Code |
|---|---|---|
| `noteTag(cmx)` | `BLAKE2b-256(key K, personal "Zecured_DepTag__", cmx)`: the D24 deposit tag, used for every lineage note (deposit and change) | `derive::deposit_tag` |
| `payoutRef(txid)` | `BLAKE2b-256(key K, personal "Zecured_PayRef__", txid)` | `derive::payout_ref` |
| `claimRef_i` | `keccak256(salt_i ‖ txid)`, `salt_i` the salt of claim *i*'s recipient commitment `keccak256(salt_i ‖ receiver_i)` | `ledger::claim_ref` |

Without the UFVK, a tag or a `payoutRef` links to nothing; without `salt_i`, a `claimRef` links to
nothing. A claimant knows its salt but not `K`: `claimRef` is how it finds and names the payout.

## 3. What the signers sign: `SpendCommit` (EIP-712, Registry domain)

```
SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)
```

- `inputTag`: the `noteTag` of the lineage note the payout spends. A payout spends the whole
  conforming lineage (`zecured_policy::ledger`), and the lineage is always a single unspent note
  (the activated deposit or the latest change), so one tag; a node refuses a plan with more inputs.
- `claimIds`: the claims it pays, at least one (T3-1: no "change only" commitment), in plan order;
  `claimIds[0]` bears the fee. `claimRefs[i]` belongs to `claimIds[i]`.
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
| `claimPayment(claimRef) = (claimId, payoutRef, expected)` | `commitSpend` | what the claimant must receive in one output |
| `claimInput(claimId)` | the first `commitSpend` naming the claim | the lineage note the claim is bound to |

`commitSpend(c, sigs)`, sent by anyone, requires:
- at least 3 sorted signatures of distinct signers (I6);
- `payoutRef` never committed;
- `inputTag` a lineage note of the vault;
- 1 to 8 distinct claims, each of the vault, `Pending` and bound to no other input;
- unused claim refs, one per claim;
- **conservation**: `lineageOf(inputTag).value == Σ claim.amount + changeValue`, exact because the fee
  is paid out of a claim;
- a change tag exactly when there is change, not yet a lineage note;
- a fee-bearing claim larger than the fee. The Registry computes the fee as the signers' plan does:
  ZIP 317 with one input, `5,000 × max(2, claims + (change > 0))` zatoshi.

`commitSpend` then:
- records the change as a lineage note;
- records `expected = amount − fee` for `claimIds[0]` and `amount` for the others;
- binds every claim to `inputTag`;
- emits `SpendCommitted`.

`confirmPayout` additionally requires each claim to have been committed.

Each of the following is prevented, and each has a test:

- **Replaying a claim** on successive change notes (commit and mine C from t0, never confirm it,
  commit C again from t1): the claim stays bound to t0.
- **A commitment with no claim** written after an uncommitted theft, to have the F1 case dismissed
  while no claim is pending (T3-1).
- **Minting after a slash**: activation reverts once the bond contract has halted.

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

Holding the signatures lets any honest node resubmit the commitment after a Base reorg or a
coordinator crash, since `commitSpend` needs no permission.

**Node policy.**
- T3-5 (liveness): a rebuilt payout always carries the claims already committed against its input.
- H-1, unchanged: a node never signs a second payout while one it signed can still be mined. A claim
  is therefore never committed to two payouts that could both land.

**Refunds of non-conforming notes (P5)** need no commitment. Those notes are never lineage notes, so
no zZEC rests on them (§ 9).

## 6. The violation statements: one guest, one entry point (T3-3)

There is one RISC Zero guest with one image id, and `kind ∈ {Uncommitted, WithheldChange, Misdirected}`.

**Common to every kind.** The guest first proves, with the Tier 2 inclusion code (`check_inclusion`),
that a transaction X is in a block committed by an anchor header on the relay; it computes X's txid
from its bytes. The journal is `abi.encode(ViolationJournal{kind, anchor, beneficiary, ref, noteTag,
value, destCommitment})`. The reward goes to `beneficiary`, which is bound inside the proof, so nobody
can copy a seal from the mempool and redirect the reward.

**F1 Uncommitted.**
- Private input: the vault's UFVK, a lineage note's opening, and the index of the action of X that spends it.
- The guest shows: the note's nullifier (`Note::nullifier` under the UFVK's full viewing key) is
  revealed by X; `noteTag = noteTag(cmx)` and `ref = payoutRef(txid_X)`, both under the UFVK's K.
- Base checks: `lineageOf(noteTag)` exists, and `spendRecord(ref).inputTag ≠ noteTag`.
- Slashes all five bonds: FROST hides which three signed.

**F2 WithheldChange.**
- Private input: the vault's UFVK.
- The guest shows: `ref = payoutRef(txid_X)`, and no action of X carries a note whose tag is `noteTag`
  and which decrypts, under either of the vault's IVKs, to a note of value `value`.
- Base checks: `spendRecord(ref)` has change, with `changeTag == noteTag` and `changeValue == value`.
- Slashes the record's signers (`signerBitmap`): they signed a false record.

**F3 Misdirected (any claimant).**
- Private input: the claimant's salt, receiver and wallet IVK.
- The guest shows:
  - `ref = keccak256(salt ‖ txid_X)` and `destCommitment = keccak256(salt ‖ receiver)`;
  - the IVK owns the receiver (`pk_d = [ivk] g_d`);
  - `value` is the largest note of X that decrypts under that IVK. Trying the IVK covers every
    diversified address of the wallet, so a payment to another address of the same wallet raises no
    false alarm.
- Base checks: `claimPayment(ref)` exists, its claim's commitment is `destCommitment`, and
  `value < expected`.
- Slashes the payout's signers.

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

Proofs are made on the prover's own machine, because the witness holds a viewing key.

## 7. Bonds: `ISignerBond` (S5, S6)

- **Deposit.** Each of the five signers deposits its bond. On testnet this is 0.01 Base Sepolia ETH,
  which shows the mechanism, not economic security.
- **Slash.** A slash takes the whole bond of every signer the case names. It credits the prover with
  a bounty of 10 %, capped at half a bond (pulled, so a reverting beneficiary cannot block the slash).
  The rest goes to a compensation pool: nothing is burned, and paying victims from the pool is
  roadmap.
- **Halt.** The first slash **halts** minting for good.
- **Withdrawal.** A signer can withdraw only when all of these hold:
  - the bond contract has not halted;
  - no case is open;
  - no vault is open;
  - the challenge window has passed since the last vault settled. The same five signers hold every
    vault, so "its vaults" are all vaults.
- **Consortium bond.** FROST hides who signed, so an uncommitted spend slashes all five, while a false
  record slashes exactly its signers. Deterrence would need each bond to exceed a third of the value
  at stake; the demo amounts do not.

**Watcher assumption.** For each vault, at least one holder of its UFVK is online within the window,
and so is each claimant for its own claims.

## 8. Parameters (S4, T3-6)

| Parameter | Judge deployment | Theft-demo deployment |
|---|---|---|
| `slashConfirmationWork` | ≥ 100 normal Testnet blocks of work (~3.8 × 10^7), the anchor rechecked at the end of the window | same |
| Challenge window | 24 hours | 2 minutes (labelled DEMO) |
| Bond per signer | 0.01 ETH | 0.01 ETH |
| Build | the judge build: no compromise mode beyond D18 | `demo-tier3` feature, its own Registry, bond and slasher, five demo keys, same relay |

The theft demo never runs on the judge deployment (T3-6).

## 9. Out of scope, said plainly (T3-7)

- **Non-conforming notes** (over- or late payments) are the depositor's refund, not zZEC backing.
  Their theft is not provable yet (F4, roadmap).
- **Liveness.** Signers can refuse to pay. The ZEC stays in the vault, visible with the UFVK, so
  refusal is not theft. The same holds for a claim confirmed as paid whose payout never landed: its
  value stays in the lineage (F1 and F2 keep it there), but the claimant is denied. A liveness
  penalty is roadmap.
- **Testnet proof of work.** A single Equihash ASIC out-mines the whole Testnet about 40 times over,
  so every proof-of-work-based guarantee here, Tier 2 minting and Tier 3 slashing, is demonstrative on
  Testnet. A forged block can carry a forged "theft". The guards against that are:
  - a slash needs far more work than a mint, and the anchor is rechecked after the window;
  - the bounty is capped below a bond.
- **A theft between proof and finalization of a deposit** is F1 (the note is lineage while its
  activation is pending), but the zZEC may already be minted when the case executes: slashing is
  after the fact.

## 10. Privacy

A commitment shows only:
- PRF outputs under the UFVK's key (tags, `payoutRef`);
- salted hashes (`claimRefs`);
- `changeValue`, which Base can already compute (outstanding plus unpaid claims).

A violation journal shows:
- the anchor header;
- a tag or a reference already on Base;
- for F3, the value the claimant received.

**New leak (L23).** A commitment lands on Base seconds before its transaction is broadcast, which
points at the next one or two Zcash blocks. The mitigations are:
- a random delay of 0 to 8 blocks between commitment and broadcast, inside the 120-block expiry (D29);
- several vaults' commitments in one Base transaction where possible.
