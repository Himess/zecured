# Interfaces (shared contract between modules)

`contracts/`, `signer/`, `backend/`, `bots/` and `app/` are built against this file. Change it
first, then the code. Design rationale lives in docs/ARCHITECTURE.md; decisions in docs/DECISIONS.md.

Units: ZEC amounts are **zatoshi** everywhere (zZEC has 8 decimals, so 1 zZEC wei = 1 zatoshi).
USDC amounts are 6-decimal units. Heights are Zcash block heights unless stated otherwise.

## 1. Commitments to Zcash recipients

A Zcash recipient never appears on Base. Only

```
commitment = keccak256(salt32 || receiver43)
```

does, where `receiver43` is the raw 43-byte Orchard-protocol receiver (the only receiver of
an Ironwood-only UA, see D4) and `salt32` is 32 random bytes known only to the requester and
the signers. Salts are never logged.

## 2. EIP-712

### 2.1 Domains

| Domain | name | version | chainId | verifyingContract | Used for |
|---|---|---|---|---|---|
| Registry | `ZecuredRegistry` | `1` | 84532 | ZecuredRegistry | `PayoutConfirmation`, `VaultOpened` (checked off-chain by the owner and on-chain by the Intake's `attestDepositWithProof`, under this domain), `SpendCommit` (Tier 3), `ReimburseDestination` (T-05, D47), and all off-chain owner requests (`OpenVault`, `Destination`, `RefundRequest`, `ViewKeyRequest`) |
| Adapter | `ZecuredSettlementAdapter` | `1` | 84532 | SettlementAdapter | `CloseRequest`, `DeleverageRequest` |
| Intake | `ZecuredIntake` | `1` | 84532 | ZecuredIntake | `PauseMinting`, `UnpauseMinting` (T-03) |
| Versions | `ZecuredVersions` | `1` | 84532 | ZecuredVersions | `ScheduleVersion`, `RemoveVersion` (T-07a, § 3.6) |
| Bond | `ZecuredSignerBond` | `1` | 84532 | SignerBondV2 | `DestinationReceived` (v4, R-5: § 2.7) |
| Swap | `ZecuredSwap` | `1` | 84532 | ZecuredSwap | `SwapQuote` (v4: a market maker's quote) |

`chainId` is 8453 on Base mainnet. Every request kind has its own type, so a signature for one kind
never verifies as another.

Registry v3 dropped `DepositAttestation` and `VaultCancellation` (D41): a deposit reaches Base only
by proof, and one that cannot back its vault lands there as a Refund claim, paid from its own note
like any claim (docs/TIER2.md § 5). No node signs either any more.

### 2.2 Signed by signer nodes (ECDSA, verified on-chain by the Registry)

```
PayoutConfirmation(bytes32 vaultId,bytes32 payoutRef,bytes32 claimIdsHash)
```

- v4 (R-1, D76): `claimIdsHash = keccak256(abi.encodePacked(claimIds))`, and the confirmation names
  the committed payout (`payoutRef`) it confirms, so it cannot be replayed for another payout or after
  a reopening. A signed confirmation is a fast first step that each claimant may dispute for
  `disputeWindow` (D53); a payout is final on Base only once its inclusion in a Zcash block is proven
  (proven delivery, § 3.5, docs/TIER3.md § 5).
- `depositTag` (D24), the tag every Base record names a vault note by: `BLAKE2b-256(key =
  BLAKE2b-256(personal = "Zecured_TagKey__", UFVK string), personal = "Zecured_DepTag__", cmx)`,
  what a deposit proof computes; unlinkable to any Zcash data without the vault's tag key. The
  Registry rejects a reused `vaultId` or `depositTag` (I5).
- Signatures are passed as `bytes[]`, ordered by **strictly increasing signer address**; at
  least 3 distinct signers of the 5-member set (I6).

### 2.3 Signed by the vault owner key (EOA generated in the browser per loan)

Verified off-chain by every signer node against the `owner` recorded at vault opening (works
before the smart account is deployed).

```
OpenVault(address owner,address account,uint64 amount,bytes32 requestId,uint64 deadline)
Destination(bytes32 vaultId,bytes32 commitment,uint64 nonce,uint64 deadline)
RefundRequest(bytes32 vaultId,bytes32 noteId,bytes32 commitment,uint64 nonce,uint64 deadline)
ViewKeyRequest(bytes32 vaultId,uint64 nonce,uint64 deadline)
ProveRefund(bytes32 vaultId,uint64 nonce,uint64 deadline)
```

- `deadline` is a Unix timestamp, checked **only when the request is accepted** (P0).
- `nonce`: per-vault, each value usable once.
- `Destination` pre-validates every destination the vault's account later puts on Base: a release's
  (`requestRelease`), a close's, and a Refund claim's (`setRefundDestination(claimId, commitment)`,
  T-02). The signers pay a claim only to a commitment whose salt and receiver they hold.
- `RefundRequest` (P5) asks for the off-chain refund of a deposit that **no guest of the deployment
  can land on Base** (I30, D42): under journal v1, a note of another amount, a late payment or one
  from a transaction with other pools; under v2, a note below the Intake's `minRefundZat`; under
  either, a note to another address of the vault. Every node checks that on its own scan, with the
  Intake's journal version read on each of its Base providers, and refuses any other deposit: one a
  guest can land is a lineage note the day someone proves it, so it is refunded only as a Refund
  claim. `noteId`: the opaque id `GET /vaults/:id` lists it under (`offChain: true`).
- `ProveRefund` (v4, R-11): the owner asks the coordinator to prove the vault's deposit in Refund
  mode when its activation cannot land (the coverage cap full). Checked by the coordinator only;
  it never reaches Base.

### 2.4 Signed by the vault owner, verified on-chain through the smart account (ERC-1271)

```
CloseRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint64 minUserAmount,bytes32 userCommitment,uint256 nonce,uint64 deadline)
```

`marginBps` is the market maker's quoted margin (adapter caps it); `minUserAmount` protects
the user against price moves; `deadline` is a short Unix-time expiry (≈ 10 min).

```
DeleverageRequest(bytes32 vaultId,address account,address executor,uint16 marginBps,uint256 repayAssets,uint64 maxCollateral,uint256 nonce,uint64 deadline)
```

Self-deleverage (S18, D33): the executor repays exactly `repayAssets` of the debt and is paid in
collateral, at most `maxCollateral` (price-move protection); the nonce space is the CloseRequest's.

### 2.5 Signed by signer nodes, verified off-chain by the vault owner (app, CLI)

```
VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 tagKeyHash)
```

- Version 2 (the guest round, design notes T-09): `tagKeyHash` replaces v1's `ufvkHash`
  (keccak256 of the UFVK's text), so the type, and every digest, differ from v1's.
- Each node signs it with its attestation key when it completes the vault's DKG (round 3),
  from values it derived or checked itself: the `OpenVault` it verified (`owner`, `account`,
  `amount`), the expiry height it checked against its own Zcash tip, and the vault's address and
  UFVK as it derived them.
- Never submitted to Base as such: a Tier 2 deposit proof computes its EIP-712 struct hash
  inside the proof, and `attestDepositWithProof` checks all five signatures against that hash
  (D22), so `addressHash` and `tagKeyHash` never appear on Base.
- `addressHash = keccak256(UTF-8 bytes of the vault's unified address)`, the string exactly as
  the owner receives it (`POST /vaults`).
- `tagKeyHash = keccak256(K)`, the 32 bytes of the vault's tag key `K = BLAKE2b-256(personal =
  "Zecured_TagKey__", UTF-8 bytes of its UFVK)` (the key of its deposit tags, § 2.2), the UFVK
  exactly as the owner receives it (`POST /vaults/:id/viewing-key`). A deposit proof's witness
  carries K, not the UFVK, and the proof derives its deposit tag with the K whose hash it puts in
  the struct: a proof under any other K names a `VaultOpened` nobody signed, so every tag that
  lands is the vault's own and one note cannot land twice under two tags (design review).
- 65-byte `r || s || v` signatures (v = 27/28, low s), as in § 2.2.
- The app and the CLI accept a vault only if all five signers of the set pinned in their own
  build signed it (app: `NEXT_PUBLIC_SIGNERS`, compared as a set; CLI: the node `eth` addresses
  of its network config, by index), never a set obtained from the backend. Only then do they show
  the address or the payment request. They keep `tagKeyHash` and show a viewing key only if its
  tag key hashes to it.

### 2.6 Tier 3: signed by signer nodes before they sign a payout (Registry v2, Deploy 2)

```
SpendCommit(bytes32 vaultId,bytes32 inputTag,bytes32[] claimIds,bytes32[] claimRefs,uint8[] claimOutputs,bytes32 payoutRef,bytes32 changeTag,uint64 changeValue)
```

- Signed in FROST round 1 and recorded by `commitSpend` before any node releases a round-2
  share; the full protocol, the Registry state and the violation proofs are in
  [TIER3.md](TIER3.md). Interfaces: `IZecuredSpendCommits`, `ISignerBond`, `IZecuredSlasher`.
- `inputTag` and `changeTag` are note tags (`depositTag`'s function, § 2.2) of the lineage note
  spent and of the change returned; `payoutRef = BLAKE2b-256(key = the same tag key, personal =
  "Zecured_PayRef__", txid)`; `claimRefs[i] = keccak256(salt_i || txid)` for `claimIds[i]`
  (`salt_i` of its recipient commitment, § 1). Txids in internal byte order.
- `claimOutputs[i]` is the index of the payout's action whose output pays `claimIds[i]`; the indexes
  are distinct, so two claims never share one output (E1, D34).
- `claimIds[0]` bears the payout's ZIP 317 fee. A payout settles 1 to 8 claims (review H-06), in
  the queue's order, with distinct receivers and salts (E1). Arrays are hashed as EIP-712 requires
  (`keccak256` of the concatenated elements); signatures as in § 2.2.

### 2.7 Liveness receipts: signed by signer nodes, verified on-chain by the bond (v4, R-5)

```
DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)
```

- A node signs it with its attestation key once it holds a claim's destination (salt and
  receiver) and has checked that `keccak256(salt || receiver)` is the claim's current commitment
  and that the receiver is a valid Ironwood receiver; `receivedAt` is when it received them.
- Anyone relays a receipt to `SignerBondV2.armLiveness` (§ 3.5): one honest signer's receipt
  suffices, so signers who stall a payout cannot withhold it. The app and the bots fetch it
  through the backend (`POST /claims/:claimId/receipt`, § 4). A new destination needs a new
  receipt.

### 2.8 Swap quotes: signed by a market maker, verified on-chain by `ZecuredSwap` (v4)

```
SwapQuote(address maker,address taker,uint8 side,uint64 zatoshi,uint256 usdc,bytes32 makerReceiverCommitment,uint32 payWindow,uint64 quoteDeadline,uint256 nonce)
```

- Signed by the maker, an EOA or an ERC-1271 contract, under the Swap domain (§ 2.1). `side` 0 =
  Buy (the taker locks USDC, the maker pays ZEC), 1 = Sell (the maker locks USDC, the taker pays
  ZEC). `taker` zero lets anyone open the quote, otherwise only that address. On a Sell,
  `makerReceiverCommitment` is `keccak256(salt || receiver)` of the maker's receiver; on a Buy it is
  zero and the taker gives its own commitment to `open`. `payWindow` must be within the contract's
  bounds; `nonce` is the maker's and is spent by `open`. docs/SWAP.md describes a whole trade.

## 3. Contracts (v3)

The authoritative interfaces, with their NatSpec, are in `contracts/src/interfaces/v3/` (and
`ISettlementAdapter`, `IZZEC`, `IRiscZeroVerifier`, `IRiscZeroVerifierRouter`,
`src/relay/IZcashHeaderRelay.sol`); this section lists what each contract offers and its main rules.
The decisions behind them are D40 to D55 in [DECISIONS.md](DECISIONS.md); the invariants each one
keeps are in [INVARIANTS.md](INVARIANTS.md). Addresses: `contracts/deployments/<network>.json`.

### 3.1 `ZZEC` (ERC-20, 8 decimals)

- `mint`/`burn`: Registry only.
- Transfers allowed only: Registry → Morpho (collateral supply, pulled by Morpho), and Morpho →
  Registry or Morpho → SettlementAdapter **while that receiver has armed a one-shot transient
  expectation** with `expectFromMorpho(uint256 maxAmount)` (callable by Registry and Adapter
  only; the expected recipient is `msg.sender`; EIP-1153 transient storage; consumed by the
  transfer; armed right before the Morpho call that moves zZEC) and closed right after it with
  `settleExpectation() returns (uint256 received)` (Y5: reverts unless a transfer consumed it,
  clears the expectation in every case, returns the amount the guard let through; no
  `balanceOf` accounting). The Registry burns zZEC held by the Adapter directly. Everything
  else reverts (Morpho surfaces it as "transfer reverted"). No account ever holds zZEC (I2).

### 3.2 `ZecuredIntake`: deposits and the gates of minting

```solidity
enum MintBlock    { None, Halted, NotBonded, Paused, CapExceeded }
enum RefundReason { ModeRefund, VaultNotNew, PendingExists, AccountHasVault, Halted, CapExceeded, PendingConverted }

function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes[] calldata openedSigs)
    external;                                                            // anyone: a pending activation, or a refund
function finalizeActivation(bytes32 vaultId) external;                   // anyone: mints (K3)
function dropPendingActivation(bytes32 vaultId) external;                // anyone, once the anchor left the best chain
function refundPending(bytes32 vaultId) external returns (bytes32 claimId);   // after a halt (anyone), or the account later (R-13)
function finalizeRefund(bytes32 depositTag) external returns (bytes32 claimId); // v4: anyone, once ready (R-12)
function dropPendingRefund(bytes32 depositTag) external;                 // v4: anyone, once the anchor left the best chain
function pauseMinting(uint64 until, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;   // 3 of 5 (T-03)
function unpauseMinting(uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;              // 3 of 5

function mintingPaused() external view returns (bool);
function mintBlock(uint64 amount) external view returns (MintBlock);    // what would stop a deposit of `amount` now
function capRoom() external view returns (uint256 zatoshi);             // T-01
function minConfirmationWorkFor(uint64 amount) external view returns (uint256);   // work tiers (T-08)
function pendingActivation(bytes32 vaultId) external view returns (PendingActivation memory);
    // (account, amount, depositTag, anchor, readyHeight, readyTime, versionId)
function pendingRefund(bytes32 depositTag) external view returns (PendingRefund memory);
    // v4: (vaultId, account, amount, reason, anchor, readyHeight, readyTime, versionId)
function pendingTotal() external view returns (uint64);                  // zatoshi of the pending activations
function pendingRefundTotal() external view returns (uint64);            // v4: zatoshi of the pending refunds
function versions() external view returns (address);                    // ZecuredVersions (T-07a)

event ActivationPending(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag,
                        uint256 readyHeight, uint256 readyTime);
event PendingActivationDropped(bytes32 indexed vaultId);
event RefundPending(bytes32 indexed vaultId, bytes32 indexed depositTag, uint8 reason, uint256 readyHeight,
                    uint256 readyTime);                                  // v4
event PendingRefundDropped(bytes32 indexed vaultId, bytes32 indexed depositTag);   // v4
event DepositRefunded(bytes32 indexed vaultId, bytes32 indexed claimId, RefundReason reason);   // when the claim opens
event MintingPaused(uint64 until, uint8 signerBitmap, uint256 nonce);
event MintingUnpaused(uint8 signerBitmap, uint256 nonce);
```

- `attestDepositWithProof` (Tier 2, D22, D41): the seal's router selector and `imageId` must be an
  active Deposit version of `ZecuredVersions`, and the seal must verify under `imageId`; the anchor
  (`tipHash`) must be on the relay's best chain with `minConfirmationWorkFor(amount)` on top; all
  five signers must have signed the journal's `vaultOpenedHash` (Registry domain, sorted); the tag
  must be unused. The deposit then lands, never twice: as a **pending activation** (the vault, its
  tag and its account reserved; `readyHeight = relay tip + finalizeDepth`, `readyTime = now +
  finalizeDelay`), or, when the proof cannot activate the vault (Refund mode, a vault that is not
  new, an account bound elsewhere, a halt), as a **pending refund** (v4, R-12: the tag and the note
  reserved, `RefundPending`), whose Refund claim opens at `finalizeRefund` under the same readiness
  rules. A new vault's proof that finds the coverage cap full reverts `CapExceeded` and is sent
  again once there is room, or proven in Refund mode on its owner's request (R-11); it no longer
  cancels the vault. A refund never reads the price; a stale cap price makes an activatable proof
  revert (`CapPriceStale`), to be sent again.
- Journal v1 (`journalV2() == false`): `abi.encode(bytes32 tipHash, bytes32 vaultId, address
  account, uint64 amount, bytes32 depositTag, bytes32 vaultOpenedHash)`, exactly 192 bytes, Activate
  mode. Journal v2 (the guest round, T-09): `abi.encode(uint8 mode, uint32 branchId, bytes32
  tipHash, bytes32 vaultId, address account, uint64 amount, bytes32 depositTag, bytes32
  vaultOpenedHash)`, exactly 256 bytes. `mode` 0 = Activate (the D5 deposit rule, the vault's
  amount, mined at or below its expiry height), 1 = Refund (any v6 transaction with an Ironwood
  bundle, a note of any value to the vault's published receiver, at any height). `branchId` is the
  consensus branch id of the epoch the proof walked, and must be one the deposit version allows:
  the network is a build parameter of the guest, so each network has its own image id; the
  Testnet guest proves NU7 (`0x77190AD9`) only, the Mainnet guest NU6.3 (`0x37A5165B`) or NU7.
  `vaultOpenedHash` is `VaultOpened` v2's struct hash (§ 2.5). The deposit tag is the same in both
  modes, so a note lands once, as collateral or as a refund.
- `finalizeActivation` (anyone): the relay's tip `>= readyHeight`, `now >= readyTime`, the anchor
  still on the best chain under the same work, its deposit version still active (R-14), no open
  violation case on the vault (E2), not halted, all five bonded, not paused; mints zZEC straight
  into the account's Morpho position (less the opening fee, § 3.3). `dropPendingActivation`
  (anyone) once the anchor left the best chain or the version is no longer active (R-14), never
  while a violation case is open on the vault (R-17).
- `refundPending`: the activation ready and its anchor on the best chain (R-12); after a halt,
  anyone; otherwise the vault's account, once `readyTime + pendingRefundDelay` has passed, whatever
  gate holds the activation (R-13). The pending activation becomes a Refund claim.
- **Pending refunds** (v4, R-12): `finalizeRefund` (anyone) needs the relay's tip `>= readyHeight`,
  `now >= readyTime`, the anchor on the best chain under the same work, the version active, and the
  vault no longer Pending (`VaultPending`: no claim ever exists on a pending vault, R-8); the
  Registry then opens the Refund claim (`RefundOpened`) and the Intake emits `DepositRefunded`.
  `dropPendingRefund` (anyone) once the anchor left the best chain or the version is no longer
  active, never while a case is open. A pending refund's note is a lineage note that no payout may
  spend (`RefundNotOpen`). A refund reserved before its vault was compromised still opens on it, for the
  compensator only (V-4, D80): the signers cannot pay it, and the vault's tally waits while
  `reservedRefundCount` is not zero; a proof landing after the compromise is refused.
- `pauseMinting`: three signers, `now < until <= now + maxPause` (30 days), renewable;
  `autoPauseOnCase` also pauses while an F1/F2 case is open. Withdrawals, repayments, liquidations
  and payouts never stop (I22).
- The coverage cap (T-01): a deposit is reserved only while locked plus pending zatoshi (pending
  activations and pending refunds) stay within `maxTotalZat` and, at the cap price's high side
  (`IZecuredCapPrice.priceHigh`), within `coverageBps` of the bond's effective bond (I18); a
  dormant refund does not count (R-11, § 3.3).
- Constructor checks (R-18): `finalizeDelay > 0`, `pendingRefundDelay > 0`, and work tiers that
  never ask less work for a larger amount.

### 3.3 `ZecuredRegistry` and `ZecuredSpendLogic`

```solidity
enum VaultStatus { None, Active, Closed, Cancelled, Pending }
enum ClaimKind   { Release, Liquidation, CloseUser, CloseMarketMaker, Refund, Fee, Retired }
enum ClaimStatus { None, Pending, Paid, WrittenOff, Compensated, Disputed }

// exits
function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);
function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment) external returns (bytes32 claimId);
function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment, address claimant)
    external returns (bytes32 claimId);                                                      // adapter only
function recordClose(bytes32 vaultId, uint64 mmAmount, uint64 userAmount, bytes32 mmCommitment, bytes32 userCommitment,
    address mmClaimant) external returns (bytes32 userClaimId, bytes32 mmClaimId);           // adapter only
// payouts
function confirmPayout(bytes32 vaultId, bytes32 payoutRef, bytes32[] calldata claimIds, bytes[] calldata sigs)
    external;                                           // 3 of 5; v4: IZecuredPayouts, run by the spend logic
function payoutConfirmed(bytes32 payoutRef) external view returns (bool);                  // v4: each payout once
function expireConfirmation(bytes32 claimId) external;                                     // v4: anyone (R-3)
function unprovenConfirmations(bytes32 vaultId) external view returns (uint256);           // v4 (R-3)
function writeOffDust(bytes32 claimId) external;                                           // anyone (RT-8)
// refunds (T-02) and the bonds' claims (T-05)
function setRefundDestination(bytes32 claimId, bytes32 destCommitment) external;           // the vault's account
function abandonRefund(bytes32 claimId) external;                                          // anyone, after refundAbandonPeriod
function markRefundDormant(bytes32 claimId) external;                                      // v4: anyone, after refundDormancy
function reservedRefundCount(bytes32 vaultId) external view returns (uint256);             // v4: the vault's reserved refunds
function isRefundDormant(bytes32 claimId) external view returns (bool);                    // v4
function setReimburseDestination(bytes32 claimId, bytes32 commitment, uint64 deadline, bytes[] calldata sigs) external;  // 3 of 5
// hooks
function reserveDeposit / releaseDeposit / activate / reserveRefund / convertToRefund(...)  // Intake only
function openReservedRefund(bytes32 vaultId, bytes32 depositTag) external returns (bytes32 claimId);   // v4, Intake only
function releaseRefund(bytes32 vaultId, bytes32 depositTag) external;                      // v4, Intake only
function retireVault(bytes32 vaultId, uint64 amount) external returns (uint64 retired);   // compensator only
function settleCompensatedClaim(bytes32 claimId) external;                                // compensator only (spend logic)
function closeMisdirectedClaim(bytes32 claimId) external;                                 // v4: compensator only, at F3 (R-7)
function markVaultCompensated(bytes32 vaultId) external;                                  // v4: compensator only (R-20)
function markBondPaid(bytes32 claimId) external;                                          // bond only
// views
function vaultOf(address account) external view returns (bytes32 vaultId);
function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status);
function vaultDetails(bytes32 vaultId) external view returns (VaultDetails memory);   // + claimCount, unpaidClaims
function claim(bytes32 claimId) external view returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment,
    ClaimStatus status);
function claimDetails(bytes32 claimId) external view returns (ClaimDetails memory);   // + createdAt, bondPaid, claimant
function claimIdOf(bytes32 vaultId, uint64 sequence) external pure returns (bytes32);  // keccak256(abi.encode(vaultId, sequence))
function lockedZatoshi() external view returns (uint256);   // zZEC supply + pending and disputed claims, less dormant refunds

// Tier 3 (IZecuredSpendCommits, docs/TIER3.md § 4); commitSpend runs ZecuredSpendLogic's code
function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external;          // 3 of 5
function lineageOf(bytes32 tag) external view returns (LineageNote memory);            // (vaultId, value)
function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);
function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);   // (claimId, payoutRef, expected, outputIndex)
function claimInput(bytes32 claimId) external view returns (bytes32 inputTag);
function inputClaimSet(bytes32 inputTag) external view returns (bytes32);              // RT-4
function claimRefOf(bytes32 payoutRef, bytes32 commitment) external view returns (bytes32);   // F4 (RT-1)
function lastCommitAt(bytes32 inputTag) external view returns (uint40);               // T-05
function openVaultCount() external view returns (uint256);
function lastVaultSettledAt() external view returns (uint64);

// Disputes (IZecuredDisputes, T-05 (a)): served by the Registry's fallback, which runs ZecuredSpendLogic
function disputePayout(bytes32 claimId) external;                                      // the claim's claimant
function upholdPayout(bytes32 claimId, bytes32 payoutRef) external;                    // slasher only
function reopenDisputedClaim(bytes32 claimId) external;                                // anyone, after the inclusion window (still upholdable, D57)
function payoutDispute(bytes32 claimId) external view returns (uint64 paidAt, uint64 dueAt);
```

- **Activation** (from the Intake): `outstanding = amount − fee`, `Active`; zZEC is minted to the
  Registry and supplied on behalf of the account (I1, I4, I5). The opening fee (T-06, `feeBps`,
  waived below `minFeeZat`) is a Fee claim to `feeCommitment`.
- **Exits** refuse a vault the slasher reports frozen (an open F1/F2 case, T-04).
  `requestRelease`: the vault's account; withdraws the outstanding to the Registry (reverts while
  debt remains), burns it, creates a Release claim, then `Closed`. `requestPartialRelease` (D14):
  `MIN_RELEASE (100,000 zat) <= amount <= outstanding - MIN_RELEASE`, the position's LTV after it at
  most 50 % (oracle price, debt rounded up); the vault stays `Active`. `recordLiquidation`:
  `seized <= outstanding` (I9); the claimant is the liquidator or the deleverage's executor.
  `recordClose`: `mmAmount + userAmount == outstanding` (I7), two claims, `Closed`.
- **Claims** are paid only by a committed payout (`confirmPayout` requires each claim's input,
  I16) and only with a destination (I21). A Refund claim's destination comes from its account
  (`setRefundDestination`, which restarts its liveness clock, wakes a dormant refund and revives an
  abandoned one). A refund still without a destination `refundDormancy` after it opened (48 hours
  on the judge deployment, 7 days on mainnet) may be marked dormant by anyone (`RefundDormant`): it
  leaves `lockedZatoshi` and the coverage cap until its account gives one (R-11). A refund nobody
  claims is written off after `refundAbandonPeriod`. `setRefundDestination`, `abandonRefund` and
  the v4 refund functions run in `ZecuredSpendLogic` (`IZecuredRefunds`), the first two through
  stubs that keep their selectors on the Registry.
- **The bonds' claims** (T-05): when the bond pays a claim's claimant (`slashLiveness`),
  `markBondPaid` zeroes its commitment; three signers then give the destination that reimburses
  the bonds (`setReimburseDestination`). W-6: it is the claim's own treasury destination, derived
  from the claim id under the signers' treasury keys (`zecured_policy::treasury`: a diversified
  receiver of the treasury's incoming viewing key at an index drawn from the claim id, and a salt
  keyed by the treasury's salt key), and each signer signs it only after deriving the same
  commitment (`zecured-ops sign --treasury`). Two claims never share it (one payout may reimburse
  several, and the Registry refuses one commitment for two claims of a payout).
- **Confirmations** (v4, R-1, R-3): `confirmPayout` needs a payout committed for the vault, confirms
  each payout once (`payoutConfirmed`), and every claim must be pending, of the vault and bound to
  that payout's input; never a reopened claim (proof only), never on a frozen vault. A confirmation
  made within `disputeWindow` before a freeze counts as unpaid unless proven (the compensator tallies
  it after the proof window); `expireConfirmation` makes an older one final; the compensator's
  `tallyVault` waits until `unprovenConfirmations` is zero. Events `PayoutConfirmed(vaultId,
  payoutRef, claimIds)`, `ConfirmationExpired(vaultId, claimId)`.
- **Proven delivery** (v4, D76): `upholdPayout` (slasher only) takes every proof transition: a
  committed pending claim (also on a frozen or compromised vault, before the compensator tallied
  it), a confirmed one (its `paidAt` becomes 0), a disputed one and a reopened one; a final claim
  (Paid with `paidAt == 0`) reverts `AlreadyFinal`, and the payout must be a committed payout of the
  claim's own input and vault. Event `PayoutProven(vaultId, claimId, payoutRef)` (`PayoutUpheld`
  stays for disputed and reopened claims).
- **Change tags** (v4, R-16): a proven deposit overrides a change record under its tag
  (`ChangeTagOverridden(tag, oldVaultId)`): another vault's record loses its claim set and commit
  time, and its claims may be bound again; the vault's own record (a refund to the same vault) keeps
  its claim set (RT-4); F1 and `upholdPayout` count only commitments of the note's own vault.
- **Compromised vaults** (an executed F1/F2 case): no new claim, no payout; the compensator
  settles their claims (`settleCompensatedClaim`) and retires their collateral (`retireVault`: all
  of it once the debt is repaid, else down to the LLTV). v4: the collateral is burned down to the LLTV
  already at the slash (R-19), an executed F3 closes its claim (`closeMisdirectedClaim`, R-7), and a
  settled victim left active leaves `openVaultCount` and `lockedZatoshi` (`markVaultCompensated`,
  `VaultCompensated`, R-20) while anyone may burn it again or retire it once its debt is gone.
- **Disputes** (D53): within `disputeWindow` of its confirmation, a claim's claimant may dispute it;
  the claim is unpaid again until the slasher upholds it with a proof of its payout's inclusion, or
  anyone reopens it after `inclusionWindow` (then payable again, and from the bonds on its original
  liveness schedule). Once per confirmation; never a bond-paid claim, an upheld one, or one of a
  compromised vault. A reopened claim is still upheld by a later proof (D57), and a new commitment of
  its input names the input's whole claim list again, its confirmed claims included.
- Tier 3 (D31): `commitSpend` records a payout before the signers sign it (3 sorted signer
  signatures, one lineage input of the vault, 1 to 8 distinct claims bound to that input on first
  commitment, pending (or, in a later commitment of the same input, confirmed or disputed: D57), one
  unused claim reference, one distinct output index and one distinct
  recipient commitment each, the claim list of the note's first commitment if any (RT-4), exact
  conservation, the ZIP 317 fee fixed by the committed shape, each Fee claim after the first
  bearing only its own output). Not for a Pending or compromised vault, nor from a pending refund's
  note (`RefundNotOpen`, v4).

### 3.4 `SettlementAdapter`

```solidity
function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment)
    external returns (uint256 seized, uint256 repaid);
function closeWithCollateral(CloseRequest calldata req, bytes calldata userSig, bytes32 mmCommitment, uint256 maxRepayAssets,
    uint64 minMmAmount) external;
function deleverageWithCollateral(DeleverageRequest calldata req, bytes calldata userSig, bytes32 mmCommitment,
    uint64 minMmAmount) external returns (uint64 mmAmount);       // S18
function invalidateNonce(uint256 nonce) external;                 // msg.sender = account
function nonceUsed(address account, uint256 nonce) external view returns (bool);
function minSeize(uint64 outstanding) external view returns (uint256);   // max(MIN_SEIZE, outstanding * minSeizeBps / 10,000)

// v4, opt-in pre-liquidation (values in WAD)
struct PreLiquidationTerms { uint256 preLltv; uint256 preLcf1; uint256 preLcf2; uint256 preLif1; uint256 preLif2; uint64 expiry; }
function setPreLiquidation(PreLiquidationTerms calldata t) external;     // msg.sender = the account of an Active vault
function clearPreLiquidation() external;                                 // msg.sender = account; never reverts
function preLiquidate(address account, uint256 repaidShares, bytes32 mmCommitment, uint64 minSeizedZat)
    external returns (uint256 repaidAssets, uint64 seized);                // v4.1 (W-17): minSeizedZat
function preLiquidationTerms(address account) external view returns (PreLiquidationTerms memory);

function onMorphoLiquidate(uint256 repaidAssets, bytes calldata data) external;   // Morpho only
function onMorphoRepay(uint256 repaidAssets, bytes calldata data) external;       // Morpho only

event NonceInvalidated(address indexed account, uint256 nonce);
event PreLiquidationSet(address indexed account, PreLiquidationTerms terms);
event PreLiquidationCleared(address indexed account);
event PreLiquidated(bytes32 indexed vaultId, address indexed account, address indexed liquidator, uint256 repaidAssets,
    uint64 seized);
```

- `liquidate`: exactly one of `seizedAssets`/`repaidShares` non-zero (`repaidShares` capped at
  the borrower's shares read in the same call); seized ≥ `minSeize(outstanding)`, or seized = the
  vault's entire remaining `outstanding`, or the liquidation clears all the borrower's debt (dust
  guard, D11); USDC pulled from `msg.sender` inside `onMorphoLiquidate`; seized zZEC →
  `Registry.recordLiquidation` with the liquidator as the claimant.
- `closeWithCollateral`: `msg.sender == req.executor`, `block.timestamp <= req.deadline`, nonce
  unused and not invalidated, `marginBps <= MAX_MARGIN_BPS`, ERC-1271 valid on `req.account`;
  repays all borrow shares (USDC pulled from the executor in `onMorphoRepay`), at most
  `maxRepayAssets` (chosen by the executor: the borrower may borrow more after signing; D21),
  `mmAmount = ceil(repaidAssets * (10_000 + marginBps) * 1e36 / (10_000 * price))`,
  `mmAmount >= minMmAmount` (Y7), `userAmount = outstanding - mmAmount >= minUserAmount`, withdraws
  `outstanding` to itself, `Registry.recordClose` with the executor as the market maker's claimant (I8).
- `deleverageWithCollateral` (S18): the same checks as a close plus `repayAssets > 0`; repays exactly
  `repayAssets`; `mmAmount` by the close formula, with `mmAmount <= maxCollateral`, at least
  `minSeize` taken and left (taking the rest is a close) and `mmAmount >= minMmAmount`; withdraws
  `mmAmount` and calls `Registry.recordLiquidation` with the executor as the claimant. The vault
  stays Active.
- `setPreLiquidation` (v4): `preLltv < LLTV`, `0 < preLcf1 <= preLcf2 <= 1`, `1 <= preLif1 <= preLif2 <=
  LIF(LLTV)` (Morpho's liquidation incentive factor at the market's LLTV, held as an immutable),
  `expiry > now`. `preLiquidate`, by anyone: the terms in force (`now < expiry`), the vault Active
  and not frozen; Morpho accrues interest; `value * preLltv < debt <= value * LLTV` with Morpho's
  rounding (debt rounded up; the upper bound is Morpho's health check, so a liquidatable position
  is never pre-liquidated); `q = (ltv - preLltv) / (LLTV - preLltv)` with `ltv` rounded up and `q`
  down; the close factor and the incentive linear in `q`; `repaidShares <= borrowShares * preLcf`;
  `seized = assets(repaidShares, rounded down) * preLif / price`, rounded down, at least
  `minSeize(outstanding)` taken and left, and at least the caller's `minSeizedZat` (v4.1, W-17:
  `PreLiquidationSlippage` otherwise, so that a price or debt move before the transaction lands
  never makes the caller repay for less); the caller's USDC repays through `onMorphoRepay`, the
  collateral comes to the Adapter and `Registry.recordLiquidation` names the caller as claimant.

### 3.5 Tier 3: `SignerBondV2`, `ZecuredSlasher`, `ZecuredCompensator`

```solidity
// SignerBondV2 (ISignerBond): USDC bonds (T-01, T-05)
function deposit(uint256 amount) external;                         // a Registry signer
function requestUnbond(uint256 amount) external;                   // slashable until withdrawn
function cancelUnbond() external;
function withdrawUnbonded(address to) external;                    // after unbondingPeriod, no open case
function slashLiveness(bytes32 claimId) external returns (uint256 paid);   // the claim's claimant, once overdue
function armLiveness(bytes32 claimId, uint64 receivedAt, bytes calldata signature) external;   // v4: anyone (R-5)
function hashDestinationReceived(bytes32 claimId, bytes32 commitment, uint64 receivedAt) external view returns (bytes32);
function livenessArming(bytes32 claimId) external view returns (bytes32 commitment, uint64 payableAt);
function livenessDue(bytes32 claimId) external view returns (uint64 dueAt, uint256 amount);   // (0, 0): not payable now
function priceStateChecked() external view returns (bool);         // v4: the oracle reports a feed state (R-9)
function effectiveBond() external view returns (uint256);          // min(sum, 5/3 x the three smallest)
function fullyBonded() external view returns (bool);
function halted() external view returns (bool);
function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary) external returns (uint256 slashed);   // slasher only
function claimBounty() external;
event LivenessArmed(bytes32 indexed claimId, bytes32 commitment, uint64 payableAt);   // v4

// ZecuredSlasher (IZecuredSlasher)
enum ViolationKind { None, Uncommitted, WithheldChange, Misdirected, WrongClaimRef }  // F1..F4
struct ViolationJournal { ViolationKind kind; bytes32 anchor; address beneficiary; bytes32 ref; bytes32 noteTag;
                          uint64 value; bytes32 destCommitment; uint8 outputIndex; }  // abi.encode: 8 words
function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal, bytes32 imageId)
    external returns (bytes32 caseId);
function executeSlash(bytes32 caseId) external;                               // after challengeWindow
function proveInclusionBatch(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes32[] calldata claimIds)
    external returns (uint256 upheld);                  // v4: one verification, claims already final skipped
function proveInclusionByCase(bytes32 caseId, bytes32[] calldata claimIds) external returns (uint256 upheld);   // v4: executed F2/F4
function compromisedAt(bytes32 vaultId) external view returns (uint64);   // v4 (R-2)
function frozenSince(bytes32 vaultId) external view returns (uint64);     // v4 (R-3)
function proveInclusion(bytes calldata journal, bytes calldata seal, bytes32 imageId, bytes32 claimId) external;   // D53
function vaultFrozen(bytes32 vaultId) external view returns (bool);          // an open F1/F2 case, or compromised
function isCompromised(bytes32 vaultId) external view returns (bool);

// ZecuredCompensator (IZecuredCompensator): pulled by anyone, one vault or claim per call
function tallyClaim(bytes32 claimId) external;
function tallyVault(bytes32 vaultId) external;
function finalizeIncident(uint256 incident) external;
function settleVault(bytes32 vaultId) external;
function withdrawEntitlement(bytes32 claimId) external returns (uint256 amount);
function withdrawEquity(bytes32 vaultId) external returns (uint256 amount);
function sweepLeftover() external returns (uint256 amount);                  // to leftoverSink, once nothing is open
```

- **Bonds** (D43): a full exit needs no open vault, no open case and the challenge window since the
  last vault settled; a partial exit keeps `minBond` and the coverage cap;
  `unbondingPeriod >= challengeWindow + livenessPeriod + commitGrace` and
  `unbondingPeriod >= disputeWindow + inclusionWindow` (R-18).
- **Liveness** (D47; v4: R-5, R-7, R-9): a Release, Liquidation, CloseUser, CloseMarketMaker or
  Refund claim with a destination, not of a frozen vault, armed by a signer's receipt for its
  current commitment (`armLiveness`: the signer one of the Registry's, `receivedAt <= now`,
  `payableAt = max(createdAt, receivedAt)`, the earliest receipt stands, a new commitment voids
  it), is due at `max(payableAt + livenessPeriod, min(lastCommitAt, payableAt + livenessPeriod) +
  commitGrace)`; the claimant then takes its value at the oracle price plus `livenessPremiumBps`,
  pro rata from the bonds (I26). Never a claim the compensator owes (R-7), nor while an oracle that
  reports a feed state reports it stale (`PriceStale`, R-9; required on mainnet).
- **Violations**: the journal's kinds, the Registry records each is checked against, and the
  dismissal rules are in docs/TIER3.md § 6; the seal must come from an active Violation version.
  The beneficiary is bound in the proof and is never a vault's smart account. An executed F1/F2
  case slashes, halts minting, marks the vault compromised and registers it with the compensator;
  F3 registers the claim's shortfall.
- **`proveInclusion`, `proveInclusionBatch`**: the violation guest's kind 5, `Included`: journal
  `{5, anchor, 0, payoutRef(txid_X), 0, 0, 0, 0}` under the slash's confirmation work; it makes final
  every claim of the payout `payoutRef` names (§ 3.3, proven delivery). The batch verifies once and
  skips claims already final, so nobody can make it revert by front-running one.
  `proveInclusionByCase`: an executed F2 or F4 case's journal already proves its payout's block, so it
  makes that payout's claims final (R-2).
- **Compensation** (D46; v4: D77, D79): layers paid in order, each in full before the next: the
  prover's bounty; third parties; the lenders (the victim vault's debt repaid on Morpho with a 1 %
  interest allowance, its unbacked zZEC retired); the owners (their claims and equity). Cases within
  `incidentWindow` share one price. v4: `executeSlash` burns a compromised vault's collateral down
  to the LLTV at once (`burnCollateral`, `CollateralBurned`; a call starved of gas reverts and anyone
  retries; equity is still that of the outstanding at the compromise, R-19); `tallyClaim` waits
  `compromisedAt + inclusionWindow` for a committed claim (`ProofWindowOpen`, R-2) and takes an
  abandoned refund until the incident is finalized (R-21); `tallyVault` waits for the vault's open
  confirmations (`ConfirmationsUnsettled`, R-3) and for its reserved refunds to open or fall
  (`RefundsReserved`, V-4).

### 3.6 `ZecuredVersions` (T-07a)

```solidity
enum VersionKind { Deposit, Violation, Swap }                       // Swap: v4
function scheduleVersion(VersionKind kind, bytes4 selector, bytes32 imageId, uint32[4] calldata branches, uint256 nonce,
    uint64 deadline, bytes[] calldata sigs) external returns (bytes32 versionId);   // all 5, after a delay
function removeVersion(bytes32 versionId, uint256 nonce, uint64 deadline, bytes[] calldata sigs) external;   // 3 of 5, at once
function versionIdOf(VersionKind kind, bytes4 selector, bytes32 imageId) external pure returns (bytes32);
    // keccak256(abi.encode(kind, selector, imageId))
function isActive(bytes32 versionId) external view returns (bool);
function isWorking(bytes32 versionId) external view returns (bool);  // v4: active, routed and not stopped
function activeVersion(VersionKind kind, bytes4 selector, bytes32 imageId) external view returns (Version memory);
function versionIds(VersionKind kind) external view returns (bytes32[] memory);
function violationStopped() external view returns (bool);   // every active violation verifier stopped by the router
```

- A version takes effect `depositDelay` (deposit and swap versions) or `violationDelay` after it is
  scheduled, `estopDelay` while `violationStopped()`. Removal takes three signers and is immediate,
  except that a working violation version goes only while another one works (R-22, I28; not even
  five signers override it) and the last working deposit version takes all five (R-23). A version
  only scheduled, or whose verifier the router stopped, goes freely; swap versions have no
  last-version rule. "Working" means active, its selector routed and its emergency-stop wrapper not
  paused; those reads have a fixed gas budget and revert when starved (`InsufficientGasForRouter`),
  so nobody picks the answer by picking the gas. EIP-712
  domain `ZecuredVersions` `1`: `ScheduleVersion(uint8 kind,bytes4 selector,bytes32 imageId,uint32[4]
  branches,uint256 nonce,uint64 deadline)`, `RemoveVersion(bytes32 versionId,uint256 nonce,uint64
  deadline)`.

### 3.7 `ZecuredPriceFeed` (T-10, mainnet only)

`IOracle.price()` for the market and `IZecuredCapPrice.priceHigh()` for the cap, from Chainlink's
ZEC/USD (primary) and API3's ZEC/USD (secondary) on Base; `poke()`, `state()`, `lastGoodPrice()`,
`readSources()`. Rules in docs/DECISIONS.md D55 and the contract's NatSpec. The judge deployment
does not use it.

### 3.8 Mocks and demo controls (labelled MOCK or DEMO)

- `MockUSDC` (6 decimals, owner mint): the loan token on a local fork only.
- `MockZecOracle`: `price() = ZEC/USD * 1e34`; owner-only `setPrice`.
- `DemoShockOracle` (DEMO): the testnet market's oracle, an owner-set shock on top of the base price.
- `DemoCapPrice` (MOCK / DEMO): the testnet cap's price, the market oracle's, always fresh.
- `MockRiscZeroVerifier` (MOCK): accepts unproven journals on a local fork; also plays the router and
  its emergency stop.

### 3.9 Market

zZEC / USDC (Circle's testnet USDC on Base Sepolia) / `DemoShockOracle` / AdaptiveCurveIRM
`0x46415998764C29aB2a25CbeA6254146D50D22687` / LLTV `0.625e18`, on Morpho Blue
`0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb`.

### 3.10 `ZecuredSwap` (v4, docs/SWAP.md)

```solidity
function open(SwapQuote calldata q, bytes calldata makerSig, bytes32 takerReceiverCommitment)
    external returns (bytes32 orderId);                 // the taker; orderId = keccak256(abi.encode(chainid, this, maker, nonce))
function proveSettlement(bytes32 orderId, bytes calldata journal, bytes calldata seal, bytes32 imageId) external;   // anyone
function finalizeSettlement(bytes32 orderId) external;  // anyone, once ready
function dropSettlement(bytes32 orderId) external;      // anyone, once the record is stale
function refund(bytes32 orderId) external;              // anyone, after payDeadline: to whoever locked the USDC
function order(bytes32 orderId) external view returns (Order memory);
    // (maker, zatoshi, side, status: None | Open | Settled | Refunded, taker, payDeadline, usdc, receiverCommitment)
function pendingSettlement(bytes32 orderId) external view returns (PendingSettlement memory);
    // (anchor, versionId, readyHeight, readyTime)
function settlementStale(bytes32 orderId) external view returns (bool);
function lockedUsdc() external view returns (uint256);

event OrderOpened(bytes32 indexed orderId, address indexed maker, address indexed taker, uint8 side, uint64 zatoshi,
    uint256 usdc, bytes32 receiverCommitment, uint64 payDeadline);
event SettlementPending(bytes32 indexed orderId, bytes32 anchor, uint256 readyHeight, uint256 readyTime);
event OrderSettled(bytes32 indexed orderId);
event SettlementDropped(bytes32 indexed orderId);
event OrderRefunded(bytes32 indexed orderId);
```

- `open`: the maker's signature (EOA or ERC-1271), `now <= quoteDeadline`, the taker named or none,
  the nonce unused, `payWindow` within `[minPayWindow, maxPayWindow]`, the order within
  `maxOrderUsdc` and `maxOrderZat`, exactly one receiver commitment (the taker's on a Buy, the
  maker's on a Sell); the USDC is pulled from the taker (Buy) or the maker (Sell), exact amounts
  only; `payDeadline = now + payWindow`.
- `proveSettlement`: the order Open and `now <= payDeadline`; the seal from an active Swap version of
  `ZecuredVersions` (its router selector and image id) and the journal's branch id one that version
  allows; the journal (192 bytes: kind 1, branch id, anchor, order id, receiver commitment, zatoshi)
  names this order and its commitment, with at least its zatoshi; the anchor on the relay's best
  chain with the work the order's size requires. The result is a pending settlement that records
  its version (R-14); a stale one is replaced, a valid one never.
- `finalizeSettlement`: `readyHeight` and `readyTime` reached (also after the deadline), the anchor
  still on the best chain with its work and the version still active; the USDC goes to the maker
  (Buy) or the taker (Sell), less `feeBps` (at most 1 %), rounded down.
- `dropSettlement`: once the anchor left the best chain or the version is no longer active; the order
  is Open again and may be proven again before its deadline.
- `refund`: the order Open, `now > payDeadline`, and no valid pending settlement (a stale one is
  dropped first); the USDC goes back to whoever locked it.

## 4. Coordinator API (backend, JSON over HTTPS)

All bodies are JSON; hex is `0x`-prefixed; amounts are decimal strings. No endpoint logs
addresses, salts, viewing keys or IPs.

| Method & path | Body | Response |
|---|---|---|
| `POST /vaults` | `{ owner, account, amount, requestId, deadline, signature, stamp, invite? }` (`OpenVault`; `stamp`: the nonce of the request's hashcash stamp, below; `invite`: a code, when openings are invite-only) | `{ vaultId, address (Ironwood-only UA), zip321, amount, expiryHeight, tagKeyHash, signatures: [{ index, signature }] }`; `signatures`: every signer's `VaultOpened` (§ 2.5), ordered by node index; the client checks all five before showing `address` and builds the payment URI itself |
| `GET /vaults/:id` | — | `{ vaultId, status, amount, address, expiryHeight, birthdayHeight, tipHeight, outstanding, payoutInFlight, nonConforming: [{ noteId, amount, offChain }], depositSeen, claims: [{ claimId, kind, amount, paid, status, destinationSet }] }`; `kind` ∈ `Release`, `Liquidation`, `CloseUser`, `CloseMarketMaker`, `Refund` (T-02), `Fee` (T-06), `Retired` (T-04); `status` ∈ `pending`, `paid`, `written_off`, `compensated`, `disputed` (D53), `paid` being `status == "paid"`; `destinationSet`: the claim has a destination on Base (a Refund claim waits for its account's, `setRefundDestination`); v4: `proven` (the claim is final: its payout proven, or its confirmation expired undisputed), `disputableUntil` (Unix s, while a signed confirmation can be disputed), `livenessArmed` (a signer's receipt armed its liveness clock). `nonConforming`: the payments the coordinator's scan found that cannot back the loan (P1 classified them non-conforming; the vault's own deposit, while it waits for its confirmations or its proof, is not listed) and that have not landed on Base, each refunded once a proof lands it as a Refund claim (the coordinator proves those a guest of the deployment can land, when it proves deposits), or, with `offChain: true` (no guest can land it, I30), off-chain at its owner's request (`POST /vaults/:id/refund`). `depositSeen` (v4, R-11.1): the vault's conforming deposit is mined with its confirmations; its proof may still be on its way, or waiting for room under the coverage cap |
| `POST /vaults/:id/destination` | `{ receiver (UA), salt, nonce, deadline, signature }` (`Destination`) | `{ commitment }` — validated **before** any Base tx that burns zZEC |
| `POST /vaults/:id/refund` | `{ noteId, receiver, salt, nonce, deadline, signature }` (`RefundRequest`, § 2.3) | `{ accepted: true }`; only for a deposit listed with `offChain: true`, else `409 refund_on_base` (I30); the signers then refund it, net of the fee, to `receiver` |
| `POST /vaults/:id/viewing-key` | `{ nonce, deadline, signature }` (`ViewKeyRequest`) | `{ ufvk }` |
| `POST /vaults/:id/deposit-proof` | `{ imageId, journal, seal }` (a deposit proof made elsewhere, e.g. `zecured-wallet loan prove`; a journal of the Intake's version, in Activate mode for v2, § 3) | `{ accepted: true }` (v4: `409 cap_full` while the coverage cap has no room for a new vault) once the Intake's `attestDepositWithProof(journal, seal, imageId, openedSigs)` is mined with the vault's `VaultOpened` signatures and the coordinator's gas (Y10: proving one's own deposit needs no ETH); the coordinator then finalizes the activation (K3). Only while the vault waits for its deposit, and only for a guest image id and router selector that are an active deposit version (T-07a, D54) |
| `POST /vaults/:id/prove-refund` | `{ nonce, deadline, signature }` (`ProveRefund`, § 2.3; v4, R-11) | `{ queued: true }`: the coordinator proves the vault's deposit in Refund mode; `409 not_blocked` (its activation can land), `already_landed`, `refund_unavailable`, `nonce_used`; `422 no_deposit`, `expired`; `403 not_owner`; `404 unknown_vault`; `503 not_configured`, `chain_unavailable` |
| `POST /claims/:claimId/receipt` | — (public, 20 a minute) | `{ claimId, commitment, receivedAt, signer, signature }` (v4, R-5, § 2.7): the valid receipt with the earliest `receivedAt` among the nodes', for the claim's current commitment; the last three go to `SignerBondV2.armLiveness`. `404 no_receipt`, `400 bad_request`, `429 rate_limited`, `503 not_configured`, `chain_unavailable` |
| `GET /demo/status` | — | DEMO (P-1, the judge deployment only): `{ enabled: false }`, or `{ enabled: true, returnAddress, dailyCapZat, paidTodayZat, lowBalance, stampBits }` |
| `POST /demo/vaults/:id/pay` | `{ owner, account, amount, expiryHeight, address, tagKeyHash, signatures: [{ index, signature }], stamp }` | DEMO: `{ txid, amount }`. The operators' demo wallet pays `address` the vault's amount (Ironwood, `ovk = None`) only when all five `VaultOpened` signatures check out and the coordinator still waits for this deposit with at least 24 blocks before its expiry; once per vault, within `DEMO_DAILY_CAP_ZAT` per UTC day; the stamp is `sha256("zecured-demo-pay-v1" \|\| vaultId \|\| stamp u64 BE)` with `stampBits` zero bits. Refusals `demo_off` (404), `bad_stamp` (422), `demo_unvouched` (403), `demo_not_awaiting`, `demo_already_paid` (409), `demo_daily_cap` (429), `demo_busy`, `demo_low_balance` (503), `demo_wallet_unavailable` (502); 6 per 10 minutes. Never on Base mainnet |
| `GET /evidence` | — | the evidence file (`zecured-evidence/1`, `app/lib/evidence.ts`), read again at most once a minute; `404 not_configured`, `503 evidence_invalid` |
| `GET /stats/vaults` | — | `{ byAmount: { <zat>: open vaults }, total, complete, readAt }` (the anonymity sets, from `VaultActivated` events and each vault's status, cached two minutes); `503 chain_unavailable` |
| `GET /swap/quote`, `POST /swap/orders/:orderId/destination`, `GET /swap/orders/:orderId` | as the market maker's (§ 4.3; the header `x-zecured-invite` on the beta) | relayed to the maker; its refusals become fixed messages; `422 invalid_receiver` is checked before relaying; `503 not_configured`, `502 swap_maker_unavailable`, `swap_maker_refused`; 20 per minute |
| `POST /recipients` | `{ receiver (UA), salt }` (liquidator / market maker) | `{ commitment }` |
| `GET /quotes/close/:vaultId` | — | `{ executor, marginBps (number), repayAssets, mmAmount, userAmount, expiresAt (number, Unix s) }` (relayed to the market maker) |
| `POST /closes` | `{ request: CloseRequest (camelCase), signature }` | `{ txHash }` once `closeWithCollateral` is mined (relayed; executed by the market maker) |
| `GET /quotes/deleverage/:vaultId?repay=<units>` | — | `{ executor, marginBps (number), repayAssets, mmAmount, remaining, expiresAt (number, Unix s) }` for repaying `repay` loan-token units of the debt (S18; relayed); refused with `repay_above_debt`, `deleverage_too_small` or `deleverage_too_large` |
| `POST /deleverages` | `{ request: DeleverageRequest (camelCase), signature }` | `{ txHash }` once `deleverageWithCollateral` is mined (relayed; the market maker repays exactly the quoted amount) |
| `GET /status` | — | `{ commit, treeClean, startedAt, deployment: { chainId, block, registry, intake, slasher, signerBond, compensator, versions, mintMode, verifierIsMock, depositImageId, violationImageId, challengeWindow, theftDemo, v3 }, zcashTip, nodes: [{ index, mode }], depositProofs: { queued, running, concurrency }, minting: { paused, pausedUntil, pausedSince, halted, fullyBonded, capRoom, mintBlock: { <zatoshi>: MintBlock \| null }, readAt }, versions: { deposit: [Version], violation: [Version] } }`: the commit the backend was built from and the deployed image ids, so anyone can check that the running system is the submitted code (review G5): `deployment.depositImageId` and `violationImageId` are the guests the deployment started with, `versions` (design notes T-07a) every verifier version `ZecuredVersions` lists for each kind, in order (the newest 16), each `{ versionId, selector, imageId, branches, activeAt, active, removed }` (`active`: `isActive(versionId)` now), read every 60 s at most, `null` while unreadable or without the registry; `depositProofs`: the deposit proof queue's counts (review H-12; `null` without Tier 2 deposit proofs); `deployment.v3`: the public v3 parameters, as in `GET /config`; `minting` (design notes T-01, T-03): the Intake's `mintingPaused()`, `pausedUntil()` and `pausedSince()` (Unix s; `pausedUntil` 0: not paused by signatures), the bond's `halted()` and `fullyBonded()`, `capRoom()` (zatoshi, decimal string; 0 while the cap's price is stale, 2^256 - 1 without a cap) and, per vault denomination, `mintBlock(amount)` (`None`, `Halted`, `NotBonded`, `Paused`, `CapExceeded`; `null` while the cap's price is stale), read every 15 s at most, `null` while the chain cannot be read or without a v3 deployment (none of these gates ever blocks an exit); nothing about any vault or user; node modes cached 15 s |
| `GET /config` | — | `{ chainId, registry, adapter, intake, v3, contracts, market, signers, denominations, confirmations, vaultExpiryBlocks, openVaultStampBits, sponsorship, openings }` (v4: `openings { open, inviteOnly }`) (`intake`: the deployment's `ZecuredIntake`, or `null`; `v3`: the deployment's public v3 parameters `{ feeBps, minFeeZat, minRefundZat, refundAbandonPeriod, pendingRefundDelay, maxPause, livenessPeriod, commitGrace, livenessPremiumBps, coverageBps, maxTotalZat, incidentWindow }` (zatoshi, basis points, seconds), or `null` before v3; `signers`: the deployment's five signer addresses, ascending, or `null`; `openVaultStampBits`: the zero bits of the open-vault stamp) — the app pins contract addresses and signers at build time and uses this only as a consistency check (any mismatch disables Base actions); it takes `sponsorship` (bundler, paymaster and allowlist all configured), denominations, confirmations, the deposit window, the stamp's bits and the v3 parameters (the opening fee it shows before a deposit, the refund and liveness periods it explains) from it |
| `POST /proxy/rpc` | JSON-RPC | Base RPC passthrough (read methods + `eth_sendRawTransaction` disallowed) |
| `POST /proxy/bundler` | JSON-RPC | ERC-4337 bundler passthrough |
| `POST /proxy/paymaster` | JSON-RPC | Sponsors only the allowlist below; rejects anything else |

Vault status values: `awaiting_deposit`, `deposit_seen` (a proof is on Base, its activation pending,
K3), `active`, `closed`, `expired`, `cancelled` (never activated and never will be: a deposit landed
as a refund while the vault was new, or its pending activation was converted into one, T-02).
Amounts are decimal strings in zatoshis (ZEC) or 6-decimal units (USDC).

**Opening a vault has a cost (review H-01).** Each opening costs the five signers a DKG, so
`POST /vaults` carries a hashcash stamp: `stamp` is a decimal 64-bit nonce such that
`sha256("zecured-open-vault-v1" || requestId (32 bytes) || owner (20 bytes) || stamp (8 bytes, big-endian))`
starts with `openVaultStampBits` zero bits (`network.json` `open_vault.stamp_bits`, 20 by default:
about a million hashes, a second or two in a browser). The backend checks it with one hash before
anything else, and only a request whose stamp checks out takes a token of the open-vault rate limit;
a missing or weak stamp is refused (`422 bad_stamp`). A request id opens at most one vault: while
its opening runs, and once its vault is open, the same request id is refused (`409 nonce_used`); a
failed opening releases it, so the owner can retry the same request. At most
`open_vault.max_pending` vaults (512) wait for their deposit at once (`429 busy` beyond); a vault
whose deposit has not been seen stops counting `open_vault.pending_slot_secs` (30 minutes) after it
opened, and keeps accepting its deposit, as before, until its expiry height. Implementations:
`signer/crates/base-client/src/stamp.rs` (backend, CLI) and `app/lib/stamp.ts` (same test vectors).

**Invites (v4, the mainnet beta).** With an invite file configured (`--invites`, docs/OPERATIONS.md),
`POST /vaults` needs a valid `invite` (`403 invite_required` when missing or unknown, `409 invite_used`
when its uses are spent), and `openings: false` refuses every opening (`403 openings_closed`) while
exits, payouts, refunds and proofs go on. Only the codes' keccak256 hashes are configured, uses are
counted in the coordinator's data directory, and a use is given back when its opening fails. Codes
are never logged. Without an invite file (the judge deployment) openings are open to all.

### 4.2 Demo control (header `x-admin-token`)

| Method & path | Body | Response |
|---|---|---|
| `GET /admin/nodes` | — | `{ nodes: [{ index, mode, vaults, compromisable }] }`; `compromisable`: the panel may switch this node to "compromised" (`DEMO_COMPROMISABLE_NODES`) |
| `POST /admin/nodes/:index/mode` | `{ mode: "normal" \| "compromised" \| "offline" }` | `{ mode }` |
| `POST /admin/vaults/:id/drain` | `{ to (UA) }` | `{ vaultId, amount, votes: [{ index, signed, reason }], thresholdReached }` — never broadcasts |
| `GET /admin/oracle` | — | `{ priceUsdCents, raw, demo, basePriceUsdCents, baseIsMock, shockBps }`: the market oracle's price; `demo`: the market's oracle is the DemoShockOracle (MOCK / DEMO), else `basePriceUsdCents` and `shockBps` are `null` |
| `POST /admin/oracle` | `{ priceUsdCents }` | `{ priceUsdCents, txHash }` (MOCK base oracle, owner key); `409 not_demo_oracle` when the market's oracle is not a DemoShockOracle (Base mainnet: the ZecuredPriceFeed itself), `409 real_price` when the base below it is a real feed |
| `POST /admin/shock` | `{ shockBps }` | `{ shockBps, txHash }` (DEMO, owner key: `DemoShockOracle.setShock`); `409 not_demo_oracle` when the market's oracle is not a DemoShockOracle (Base mainnet), `409 shock_refused` for a step the contract refuses |

### 4.1 Paymaster allowlist (enforced by the proxy)

A UserOp is sponsored only if every call it makes is one of (targets: the deployment file's
`contracts`; no ETH value in any call; a MultiSend batch of at most 8 plain calls):

| Target | Calls |
|---|---|
| Smart account factory | deployment in the account's first UserOp (`factory` / `factoryData`): a Safe v1.4.1 with the app's exact configuration, created at the sender |
| `ZecuredRegistry` | `requestRelease`, `requestPartialRelease`; `setRefundDestination(claimId, commitment)` (T-02: a non-zero commitment, and `claimDetails(claimId)` must be a Refund claim whose claimant, the vault's account, is the sender); `disputePayout(claimId)` and `reopenDisputedClaim(claimId)` (T-05 (a), `IZecuredDisputes`, served by the Registry's fallback: `claimDetails(claimId).claimant` must be the sender) |
| `ZecuredIntake` | `refundPending(vaultId)` (T-03: only the sender's own vault, `vaultOf(sender)`; the Intake lets the account convert its pending activation once a pause by signatures has lasted `pendingRefundDelay`, and anyone after a halt) |
| `SignerBond` (SignerBondV2; the proxy confirms it is the Registry's `signerBond()`) | `slashLiveness(claimId)` only (T-05: the bonds pay an overdue claim): `claimDetails(claimId).claimant` must be the sender, and the call must succeed now: `livenessDue(claimId)` a non-zero `dueAt` no later than the latest block and a non-zero amount, the total of the UserOp's claims within `totalSlashable()`, each claim once; v4 (R-5): `armLiveness(claimId, receivedAt, signature)`, the claim's own claimant only (`claimDetails(claimId).claimant` is the sender), each claim once per UserOp, simulated from the sender first (refusal `not_armable`) |
| `SettlementAdapter` | `invalidateNonce`; v4: `setPreLiquidation(PreLiquidationTerms)` (`0x3b8f1fc8`) and `clearPreLiquidation()` (`0xad90c3b3`), the sender's own terms, from a sender bound to a vault, within the Adapter's bounds (`preLltv < LLTV`, `1 <= preLif1 <= preLif2 <= LIF(LLTV)`, `0 < preLcf1 <= preLcf2 <= 1`, an expiry), simulated first (refusal `pre_liquidation_refused`); `preLiquidate` is never sponsored |
| Morpho Blue | `setAuthorization` (only for the Registry and the Adapter), `borrow` and `repay` (only the Zecured market, on behalf of the sender, no callback data) |
| Loan token (USDC; MockUSDC on a local fork) | `approve` (spender = Morpho) and `transfer` |

The sender must be a Zecured account: bound to a vault (`Registry.vaultOf(sender) != 0`, set by
the deposit proof that reserved the vault, before the account's first UserOp), except for a
UserOp whose only calls are about the sender's own claims (refund destinations, disputes,
reopenings, payments by the bonds, liveness armings): the claim binds the account, and a deposit
that landed only as a refund (a Refund-mode proof, a halt) binds no account to a vault. Once
deployed, the account must still be the canonical Safe proxy with the Safe 4337 module as
fallback handler. Anything else is rejected. The account never receives ETH. The first UserOp
must skip authorizations that are already set (Morpho reverts on re-setting).

### 4.3 Market maker API (v4, Zecured Swap; served by `zecured-swap-maker`)

The app reaches it through the backend's proxy, never directly. In invite-only mode (the mainnet
beta) quotes need the header `X-Zecured-Invite`; the maker stores only the codes' hashes.

| Method and path | Answer |
|---|---|
| `GET /swap/quote?side=buy\|sell&zatoshi=N` (or `usdc=N`) `[&taker=0x…]` | `{ quote, signature, swap, chainId, orderId, expiresAt }` (integers as strings; a Sell adds the maker's receiver and salt); errors `bad_request`, `invite_required`, `side_disabled`, `taker_required`, `too_small`, `too_large`, `no_inventory`, `busy` (also: the taker already holds `maxOpenOrdersPerTaker` open orders), `relay_behind`, `price_stale`, `unavailable` (also: the maker's payment guest is not a working Swap version, so no proof could land) |
| `POST /swap/orders/:orderId/destination` | body `{ receiver, salt }` (a unified address or 43 raw bytes, and 32 bytes); 202 `{ orderId, status: "destination_received" }`, idempotent; errors `unknown_order`, `not_buy`, `not_awaiting`, `too_late`, `bad_destination` (a receiver that does not match the order's commitment), and for a first destination `no_inventory` (the maker could not pay it now), `busy` (the taker's other orders in payment reach `maxOpenOrdersPerTaker`), `relay_behind` and `unavailable` (no working Swap version, or the maker's view of the chains is stale); a refused destination may be given again while the window allows |
| `GET /swap/orders/:orderId` | `{ orderId, side, status, zatoshi, usdc, payDeadline, note }`, status `awaiting_destination`, `destination_received`, `paid`, `proven`, `taker_pays`, `abandoned`, `settled` or `refunded` |
| `GET /swap/status` | what the maker quotes now: `{ maker, swap, chainId, sides, payWindow, minOrderZat, maxOrderZat, maxOrderUsdc, spreadBps, inviteOnly, openOrders, maxOpenOrders, maxOpenOrdersPerTaker }` |

Operators only: with `status_bind` set (a loopback address other than `bind`, never added to the backend's
proxy), the maker serves `GET /inventory` → `{ "zecFreeZat", "usdcFree" }` (decimal strings: the last scan's
spendable ZEC less what open Buy orders with a destination reserved, and the USDC it can lock, capped at
its allowance to `ZecuredSwap`), for the watchdog.
