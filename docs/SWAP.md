# Zecured Swap

Swap USDC on Base for shielded ZEC with no custodian, no signer, no bond and no wrapped asset. The
USDC waits in escrow in `ZecuredSwap` and is released only by a zero-knowledge proof, made by
whoever paid, that a shielded payment for that very order is in the Zcash chain. It runs on the
same Zcash light client as the loans: `ZcashHeaderRelay` (Equihash verified on Base), ZIP 221
history commitments, RISC Zero guests and `ZecuredVersions` (docs/TIER2.md).

The trust assumptions are the relay's proof of work (the work an anchor needs grows with the
order's size), RISC Zero's verifier, and the payment guest's correctness. The signers' only part
is the version registry: a new payment guest takes all five and a public delay, and three can
remove a broken one at once (D63).

## Directions

- **Buy ZEC** (the on-ramp). The taker locks USDC; the market maker pays ZEC to the taker's
  Ironwood address and proves it. Any Ironwood address can receive: the taker needs no Zecured
  tool beyond the app's "Buy shielded ZEC" page.
- **Sell ZEC** (the off-ramp). The maker locks USDC; the taker pays ZEC to the maker and proves
  it, from the CLI wallet (`zecured-wallet swap sell`).

## A Buy, step by step

1. **Quote.** The app asks the maker for a quote (`GET /swap/quote`, through our backend's
   proxy, docs/INTERFACES.md § 4.3): a `SwapQuote` signed under EIP-712 (`ZecuredSwap` `1`) that
   names the taker, so nobody else can take it.
2. **Open.** The app draws a salt and computes `C = keccak256(salt ‖ receiver)` locally, approves
   the USDC and calls `open(quote, signature, C)` from the user's own wallet. The order id is
   `keccak256(abi.encode(chainid, swap, maker, nonce))`, and the maker's nonce is spent.
3. **Destination.** The app hands the maker the salt and the receiver off-chain
   (`POST /swap/orders/:id/destination`); the maker checks them against `C`. Base only ever sees
   `C`, and no authentication is needed: a receiver that does not match `C` is refused.
4. **Payment.** After a random delay, the maker pays the receiver in one Ironwood-only
   transaction, with `"ZSWP" ‖ orderId` at the start of the note's memo and no outgoing viewing
   key.
5. **Proof.** Once the payment's block has the work the order's size requires, plus a random
   delay, the maker proves the payment with the payment guest and calls `proveSettlement` before
   the order's deadline: the settlement is pending.
6. **Finalize.** After `finalizeDepth` headers and `finalizeDelay` seconds, with the anchor still
   on the relay's best chain and the version still active, `finalizeSettlement` pays the USDC to
   the maker, less the fee (none on the judge deployment or the beta).
7. **Refund.** If no valid proof stands when the deadline passes, `refund` returns the USDC to
   the taker. A proof only needs to arrive by the deadline; finalizing may come later.

A Sell runs the same way with the roles swapped: the maker locks the USDC at `open` and names its
own receiver commitment in the quote; the CLI checks the maker's receiver against it before it
opens, pays, proves and finalizes, and keeps an order file (0600) marked paid before the broadcast,
so a resumed run never pays twice.

## What a payment proof shows

The payment statement (`tier2/payment-statement`, guests payment-test and payment-main) proves:

1. a v6 transaction of an epoch the build allows that is Ironwood-only: no transparent input or
   output and no Sapling or Orchard-pool bundle, even an empty one (D5), so no pool turnstile
   publishes a value that could tie the payment to the order's public amount;
2. one of its Ironwood actions creates the V3 note the payer opens (receiver, value, `rseed`):
   the action's ciphertext decrypts, under the ephemeral secret that note derives, to exactly that
   note, so its receiver finds it and can spend it;
3. the note's 512-byte memo starts with `"ZSWP" ‖ orderId`, so one note pays one order, even
   when two orders share a receiver;
4. `C = keccak256(salt ‖ receiver)`;
5. the transaction is in a block committed, through the epoch's ZIP 221 history tree, by a later
   header (the anchor), with the deposit statement's epoch rules.

Its journal is `abi.encode(uint8 kind = 1, uint32 branchId, bytes32 anchor, bytes32 orderId,
bytes32 receiverCommitment, uint64 zatoshi)`, 192 bytes: no transaction id, note commitment,
nullifier or address. The contract checks the seal against an active Swap version and its branch
ids, the order id and the commitment against the order, a value at least the order's, and the
anchor's work for the order's size.

## Parameters

| | Judge (Base Sepolia) | Mainnet beta |
|---|---|---|
| Pay window a quote may set | 2 to 6 hours | 6 to 24 hours |
| Finalization | 150 s and 6 headers | 1,800 s and 6 headers |
| Anchor work | the Intake's judge tier | the Intake's mainnet tiers (by amount) |
| Branch ids | NU7 | NU6.3 and NU7 |
| Largest order | 200 USDC, 10 TAZ | 250 USDC, 5 ZEC |
| Protocol fee | 0 (at most 1 % by construction) | 0 |
| A new payment guest | 10 minutes after five signatures | 14 days after five signatures |

The configurations are `contracts/config/swap-base-sepolia.json` and
`swap-base-mainnet-beta.json`; `script/DeploySwap.s.sol` deploys them, and the lending deployment
can start the first Swap version (`PAYMENT_IMAGE_ID`).

## Attacks and what stops them

| Attack | What stops it | Tests |
|---|---|---|
| One payment for two orders | The memo binds one order id | K4 memo changes; `test_prove_aJournalOfAnotherOrder` |
| A wrong receiver or too little | The commitment and the amount are checked | `test_prove_wrongReceiverOrTooLittle` |
| An Orchard-pool, Sapling or transparent part | D5 in the statement | K4 |
| A fake branch | Work tiers by amount, the readiness wait (K3), drops and stale-record replacement | `test_prove_anchorAndWorkTiers`, `test_finalize_anchorReorgedOut`, `test_refund_afterAFakeProofIsDropped` |
| A journal replayed on another order, contract or chain | The order id binds the chain, the contract, the maker and its nonce | `test_orderId_formulaAndVector` |
| A quote replayed | The nonce is spent at `open`; a quote naming a taker is that taker's | `test_open_replayOfAQuote`, `test_open_takerBinding` |
| A proof at the deadline | A proof must come by the deadline; no refund while one stands | `test_prove_byTheDeadline`, `test_refund_notWhileAProofStands` |
| A broken guest | Three signers remove the Swap version at once; pending settlements under it go stale and are dropped (R-14) | `test_finalize_versionRemoved` |
| RISC Zero's emergency stop | Proofs revert; after the deadline the USDC goes back to whoever locked it (the payer's risk) | `test_refund_afterARouterEstop` |
| Rounding | Exact zatoshi; the fee rounds down | — |
| Reentrancy | `nonReentrant` everywhere, SafeERC20, ERC-1271 checked by a static call | `test_reentrancy`, `test_open_erc1271MakerCannotMeddle` |
| A leak through events | No event or calldata holds a Zcash identifier | `test_events_carryNoZcashIdentifier`; the leak scanner (docs/TESTING.md § 4) |

Four invariants run over a handler that opens, proves, reorganizes, drops, removes versions,
finalizes, refunds and moves time: the contract's balance is what it holds locked, which is exactly
its open orders' USDC; each unit leaves once; pending settlements exist only for open orders; and a
settled or refunded order stays so (docs/INVARIANTS.md I36 to I39).

## The market maker

`swap/maker` (`zecured-swap-maker`) quotes from a price feed plus a spread within its inventory and
the order caps, follows the `OrderOpened` events of its own address, pays, proves, submits and
finalizes, drops stale records and proves again, and after a deadline refunds its own Sell orders
and the Buy orders it did not pay (the USDC goes back to whoever locked it). It keeps
one unmined payment at a time and records each payment's opening before broadcasting it. It refuses
to start when its pay window is shorter than twice its worst-case pipeline, when its guest is not
an active Swap version, or when its chain does not match the RPC; it refuses a quote while the
relay lags or the price is stale, and a destination when too little of the window is left. It never
logs a receiver, salt, transaction id, key or IP, and erases them once an order is done. In the
mainnet beta it quotes only to holders of an invite code (`X-Zecured-Invite`; only the codes'
hashes are configured).

## Privacy

On a swap, the taker's Base account and the order's amount are public, and where the ZEC went is
not: Base holds only the receiver's salted commitment, and the payment's block stays hidden behind
a later anchor header among its epoch's blocks. The maker pays after a random delay of 1 to
`pay_delay_max_blocks` blocks and proves after a further random 0 to `prove_delay_max_blocks`
blocks (threat model L26), and its payment carries no outgoing viewing key.

## Limits, said plainly

- **No maker stake.** A maker who never pays costs its taker only time: the USDC comes back after
  the window. A stake needs a provable delivery of the receiver (docs/MAINNET.md § 4).
- **Payer risk.** A router emergency stop or a version removal during the window keeps the payer's
  proof out; the order then refunds to whoever locked the USDC. The CLI asks for a window of at
  least two hours, and the contract bounds every quote's window.
- **USDC's blocklist.** A blocklisted counterparty cannot be paid: its order's USDC stays in the
  contract.
- **Testnet first.** The payment fixture is synthetic (the disposable test vault's real deposit
  transaction with one action re-encrypted as a payment, re-anchored under a synthetic header)
  until a rehearsal payment gives a real one, and the maker proves Testnet payments only: a Mainnet
  witness builder and payment-main as an active version come before the swap opens on mainnet.
