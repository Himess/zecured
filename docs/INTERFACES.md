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
| Registry | `ZecuredRegistry` | `1` | 84532 | ZecuredRegistry | `DepositAttestation`, `PayoutConfirmation`, `VaultCancellation`, `VaultOpened` (checked off-chain by the owner and on-chain by `attestDepositWithProof`), `SpendCommit` (Tier 3, Registry v2), and all off-chain owner requests (`OpenVault`, `Destination`, `RefundRequest`, `ViewKeyRequest`) |
| Adapter | `ZecuredSettlementAdapter` | `1` | 84532 | SettlementAdapter | `CloseRequest` |

Every request kind has its own type, so a signature for one kind never verifies as another.

### 2.2 Signed by signer nodes (ECDSA, verified on-chain by the Registry)

```
DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)
PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)
VaultCancellation(bytes32 vaultId)
```

- `depositTag` (D24): keyed tag of the conforming note, `BLAKE2b-256(key = BLAKE2b-256(personal
  = "Zecured_TagKey__", UFVK string), personal = "Zecured_DepTag__", cmx)`, the same value a
  Tier 2 deposit proof computes; unlinkable to any Zcash data without the vault's viewing key.
  No expiry; the Registry rejects a reused `vaultId` or `depositTag` (I5).
- `VaultCancellation` (D23, L-8): signed before the signers refund the conforming deposit of a
  vault that was never activated; `cancelVault` makes the vault `Cancelled` for good.
- `claimIdsHash = keccak256(abi.encodePacked(claimIds))`.
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
```

- `deadline` is a Unix timestamp, checked **only when the request is accepted** (P0).
- `nonce`: per-vault, each value usable once.
- `noteId`: opaque id of a non-conforming note as returned by `GET /vaults/:id`.

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
VaultOpened(bytes32 vaultId,address owner,address account,uint64 amount,uint32 expiryHeight,bytes32 addressHash,bytes32 ufvkHash)
```

- Each node signs it with its attestation key when it completes the vault's DKG (round 3),
  from values it derived or checked itself: the `OpenVault` it verified (`owner`, `account`,
  `amount`), the expiry height it checked against its own Zcash tip, and the vault's address and
  UFVK as it derived them.
- Never submitted to Base as such: a Tier 2 deposit proof computes its EIP-712 struct hash
  inside the proof, and `attestDepositWithProof` checks all five signatures against that hash
  (D22), so `addressHash` and `ufvkHash` never appear on Base.
- `addressHash = keccak256(UTF-8 bytes of the vault's unified address)` and `ufvkHash =
  keccak256(UTF-8 bytes of its UFVK)`, each string exactly as the owner receives it (`POST
  /vaults`, `POST /vaults/:id/viewing-key`).
- 65-byte `r || s || v` signatures (v = 27/28, low s), as in § 2.2.
- The app and the CLI accept a vault only if all five signers of the set pinned in their own
  build signed it (app: `NEXT_PUBLIC_SIGNERS`, compared as a set; CLI: the node `eth` addresses
  of its network config, by index), never a set obtained from the backend. Only then do they show
  the address or the payment request. They keep `ufvkHash` and show a viewing key only if it
  hashes to it.

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
- `claimIds[0]` bears the payout's ZIP 317 fee. Arrays are hashed as EIP-712 requires
  (`keccak256` of the concatenated elements); signatures as in § 2.2.

## 3. Contracts (Base Sepolia)

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

### 3.2 `ZecuredRegistry`

```solidity
enum VaultStatus { None, Active, Closed, Cancelled }
enum ClaimKind   { Release, Liquidation, CloseUser, CloseMarketMaker }
enum ClaimStatus { None, Pending, Paid }
enum MintMode    { SignerAttested, ProofOnly }                                                                     // K1, immutable

function attestDeposit(DepositAttestation calldata att, bytes[] calldata sigs) external;                            // not in ProofOnly
function attestDepositWithProof(bytes calldata journal, bytes calldata seal, bytes[] calldata openedSigs) external;  // Tier 2: pends
function finalizeActivation(bytes32 vaultId) external;                                                              // K3: mints
function dropPendingActivation(bytes32 vaultId) external;                                                           // K3
function cancelVault(bytes32 vaultId, bytes[] calldata sigs) external;                                              // L-8
function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);
function requestPartialRelease(bytes32 vaultId, uint64 amount, bytes32 destCommitment) external returns (bytes32 claimId);
function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment) external returns (bytes32 claimId);   // adapter only
function recordClose(bytes32 vaultId, uint64 mmAmount, uint64 userAmount, bytes32 mmCommitment, bytes32 userCommitment)
    external returns (bytes32 userClaimId, bytes32 mmClaimId);                                                             // adapter only
function confirmPayout(bytes32 vaultId, bytes32[] calldata claimIds, bytes[] calldata sigs) external;

function vaultOf(address account) external view returns (bytes32 vaultId);
function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status);
function claim(bytes32 claimId) external view returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status);
function marketParams() external view returns (MarketParams memory);
function signers() external view returns (address[5] memory);
function pendingActivation(bytes32 vaultId) external view returns (address account, uint64 amount, bytes32 depositTag,
    bytes32 anchor, uint256 readyHeight, uint256 readyTime);

event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag);
event ActivationPending(bytes32 indexed vaultId, address indexed account, uint64 amount, bytes32 depositTag,
                        uint256 readyHeight, uint256 readyTime);
event PendingActivationDropped(bytes32 indexed vaultId);
event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
event LiquidationRelease(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment);
event CloseRelease(bytes32 indexed vaultId, bytes32 userClaimId, bytes32 mmClaimId, uint64 userAmount, uint64 mmAmount,
                   bytes32 userCommitment, bytes32 mmCommitment);
event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);
event VaultCancelled(bytes32 indexed vaultId);

// Tier 3 (IZecuredSpendCommits, docs/TIER3.md § 4)
function commitSpend(SpendCommit calldata c, bytes[] calldata sigs) external;
function lineageOf(bytes32 tag) external view returns (LineageNote memory);             // (vaultId, value)
function spendRecord(bytes32 payoutRef) external view returns (SpendRecord memory);    // (recordHash, vaultId, inputTag,
                                                                                        //  changeTag, changeValue, claimCount, signerBitmap)
function claimPayment(bytes32 claimRef) external view returns (ClaimPayment memory);   // (claimId, payoutRef, expected,
                                                                                        //  outputIndex)
function claimInput(bytes32 claimId) external view returns (bytes32 inputTag);
function hashSpendCommit(SpendCommit calldata c) external view returns (bytes32);
function openVaultCount() external view returns (uint256);
function lastVaultSettledAt() external view returns (uint64);
function signerBond() external view returns (ISignerBond);
event SpendCommitted(bytes32 indexed vaultId, bytes32 indexed payoutRef, bytes32 inputTag, bytes32[] claimIds,
                     bytes32[] claimRefs, bytes32 changeTag, uint64 changeValue, uint8 signerBitmap);
```

- `attestDeposit` registers the vault (`outstanding = amount`, `Active`), mints zZEC to itself
  and calls `Morpho.supplyCollateral(market, amount, onBehalf = account, "")` (I1, I4, I5).
  Reverts with `MintRequiresProof` in `ProofOnly` mode (K1, D25; the Base Sepolia deployment).
- `attestDepositWithProof` (Tier 2, D22) accepts a proof instead of an attestation.
  `journal = abi.encode(bytes32 tipHash, bytes32 vaultId, address account, uint64 amount,
  bytes32 depositTag, bytes32 vaultOpenedHash)` (exactly 192 bytes); the seal must start with
  `depositSealSelector` (Y1); it requires `relay.isInBestChain(tipHash)` and
  `relay.confirmationWork(tipHash) >= minConfirmationWork`, the five signers' signatures
  (sorted) over the `VaultOpened` digest of `vaultOpenedHash`, and `verifier.verify(seal,
  depositImageId, sha256(journal))`. It then **reserves** the vault id, the tag and the account
  and emits `ActivationPending` with `readyHeight = relay tip height + finalizeDepth` and
  `readyTime = block.timestamp + finalizeDelay` (K3, D26). Disabled (reverts) when the
  deployment has no verifier. Anyone may relay.
- `finalizeActivation` (anyone): the relay's tip height `>= readyHeight`, `block.timestamp >=
  readyTime`, and the anchor still on the best chain under `minConfirmationWork`; then the same
  effects as `attestDeposit`, emitting `VaultActivated`.
- `dropPendingActivation` (anyone): only once the anchor is off the relay's best chain; frees the
  vault id, the tag and the account (nothing was minted).
- `VaultActivated.depositTag` names the note that backs the vault: the signers root its lineage
  there and never refund it (K2-a, D27).
- `cancelVault` (D23): 3 signer signatures over `VaultCancellation(vaultId)`; only a vault that
  is still `None` with no pending proof; it becomes `Cancelled` and can never be activated.
- `requestRelease`: caller must be the vault's account; withdraws `outstanding` from Morpho on
  behalf of the account to the Registry (reverts while debt remains), burns it,
  `outstanding = 0`, `Closed`.
- `requestPartialRelease` (D14): caller must be the vault's account; `MIN_RELEASE = 100,000 zat
  <= amount <= outstanding - MIN_RELEASE` (the rest must stay payable on its own); withdraws `amount` from Morpho to the Registry (Morpho enforces
  LLTV) and additionally requires the position's LTV after the withdrawal to be <= 50%
  (`MAX_WITHDRAW_LTV_BPS = 5_000`, oracle price, debt rounded up); burns; `outstanding -= amount`;
  emits `ReleaseRequested` (a `Release` claim); the vault stays `Active`.
- `recordLiquidation`: `seized <= outstanding` (I9), burns, `outstanding -= seized`; the vault
  stays `Active` while `outstanding > 0`.
- `recordClose`: `mmAmount + userAmount == outstanding` (I7), burns, `Closed`, two claims.
- `claimId = keccak256(abi.encode(vaultId, claimSequence))`, sequence per vault from 0.
- Tier 3 (D31): `commitSpend` records a payout before the signers sign it (rules in
  `IZecuredSpendCommits` and docs/TIER3.md § 4: 3 sorted signer signatures, one lineage input of the
  vault, 1 to 8 distinct pending claims bound to that input on first commitment, one unused claim ref
  and one distinct output index each, exact conservation, the ZIP 317 fee fixed by the committed
  shape). `confirmPayout` requires every claim to have been committed. A deposit note is a lineage
  note from its proof (or attestation) on, and stops being one if its pending activation is dropped.
  New proofs and activations need the bond contract `fullyBonded()` and not `halted()`;
  `finalizeActivation` also reverts with `ViolationCaseOpen(vaultId)` while the slasher has a case open
  on the vault (E2, D34).

### 3.3 `SettlementAdapter`

```solidity
function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment)
    external returns (uint256 seized, uint256 repaid);
function closeWithCollateral(CloseRequest calldata req, bytes calldata userSig, bytes32 mmCommitment, uint256 maxRepayAssets,
    uint64 minMmAmount) external;
function deleverageWithCollateral(DeleverageRequest calldata req, bytes calldata userSig, bytes32 mmCommitment,
    uint64 minMmAmount) external returns (uint64 mmAmount);       // S18
function invalidateNonce(uint256 nonce) external;                 // msg.sender = account
function nonceUsed(address account, uint256 nonce) external view returns (bool);

function onMorphoLiquidate(uint256 repaidAssets, bytes calldata data) external;   // Morpho only
function onMorphoRepay(uint256 repaidAssets, bytes calldata data) external;       // Morpho only

event NonceInvalidated(address indexed account, uint256 nonce);
```

- `liquidate`: exactly one of `seizedAssets`/`repaidShares` non-zero (`repaidShares` capped at
  the borrower's shares read in the same call); seized ≥ `MIN_SEIZE` = 100,000 zat (0.001 ZEC),
  or seized = the vault's entire remaining `outstanding`, or the liquidation clears all the borrower's debt (dust
  guard, D11); USDC pulled from `msg.sender` inside
  `onMorphoLiquidate`; seized zZEC → `Registry.recordLiquidation`.
- `closeWithCollateral`: `msg.sender == req.executor`, `block.timestamp <= req.deadline`, nonce
  unused and not invalidated, `marginBps <= MAX_MARGIN_BPS`, ERC-1271 valid on `req.account`;
  repays all borrow shares (USDC pulled from the executor in `onMorphoRepay`), at most `maxRepayAssets` (chosen
  by the executor: the borrower may borrow more after signing; D21),
  `mmAmount = ceil(repaidAssets * (10_000 + marginBps) * 1e36 / (10_000 * price))`,
  `mmAmount >= minMmAmount` (Y7: chosen by the executor, the price may rise after it quoted),
  `userAmount = outstanding - mmAmount >= minUserAmount`, withdraws `outstanding` to itself,
  `Registry.recordClose` (I8).
- `deleverageWithCollateral` (S18): the same checks as a close (executor, deadline, nonce, margin cap,
  ERC-1271, the account's own Active vault) plus `repayAssets > 0`; repays exactly `repayAssets`
  (USDC pulled from the executor in `onMorphoRepay`); `mmAmount` by the close formula, with
  `mmAmount <= maxCollateral`, `mmAmount >= MIN_SEIZE`, `outstanding - mmAmount >= MIN_SEIZE` (taking the rest is
  a close) and `mmAmount >= minMmAmount`; withdraws `mmAmount` (Morpho checks the health of the reduced position)
  and calls `Registry.recordLiquidation`, so the executor's payment is an ordinary third-party claim
  (`LiquidationRelease` on Base). The vault stays Active; its LTV falls whenever `LTV * (1 + margin) < 1`.

### 3.4 Tier 3: `SignerBond` and `ZecuredSlasher`

```solidity
// SignerBond (ISignerBond): BOND 0.01 ether, BOUNTY_BPS 1,000, BOUNTY_CAP 0.005 ether
function deposit() external payable;                                       // a Registry signer, exactly BOND, once
function slash(bytes32 caseId, uint8 signerBitmap, address beneficiary) external returns (uint256);  // slasher only
function claimBounty() external;
function withdraw(address payable to) external;   // no open vault, no open case, window passed since the last settled
function fullyBonded() external view returns (bool);
function halted() external view returns (bool);

// ZecuredSlasher (IZecuredSlasher)
enum ViolationKind { None, Uncommitted, WithheldChange, Misdirected }
struct ViolationJournal { ViolationKind kind; bytes32 anchor; address beneficiary; bytes32 ref; bytes32 noteTag;
                          uint64 value; bytes32 destCommitment; uint8 outputIndex; }  // abi.encode: 8 words
function proveViolation(ViolationKind kind, bytes calldata journal, bytes calldata seal) external returns (bytes32 caseId);
function executeSlash(bytes32 caseId) external;                               // after challengeWindow
function caseIdOf(ViolationKind kind, bytes32 ref, bytes32 noteTag) external pure returns (bytes32);
function openCaseCount() external view returns (uint256);                     // SignerBond.withdraw needs 0
function openCasesOf(bytes32 vaultId) external view returns (uint256);        // finalizeActivation needs 0 (E2)
```

- The journal's kinds, the Registry records each is checked against, and the dismissal rules are in
  docs/TIER3.md § 6; the guest is `tier2/methods/violation` (one image id). The beneficiary is bound in
  the proof and is never a vault's smart account (smart accounts never receive ETH).
- Deployment: `script/DeployZecured.s.sol` with `VIOLATION_IMAGE_ID`; the signers then bond with
  `script/BondSigners.s.sol`. Challenge window: at least a day on the judge deployment, 2 minutes only on
  the separate theft demo (T3-6).

### 3.5 Mocks (labelled MOCK)

- `MockUSDC` (6 decimals, owner mint).
- `MockZecOracle`: `price() = ZEC/USD * 1e34`; owner-only `setPrice`.

### 3.6 Market

zZEC / MockUSDC / MockZecOracle / AdaptiveCurveIRM `0x46415998764C29aB2a25CbeA6254146D50D22687`
/ LLTV `0.625e18`, on Morpho Blue `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb`.
Deployed addresses: `contracts/deployments/base-sepolia.json`.

## 4. Coordinator API (backend, JSON over HTTPS)

All bodies are JSON; hex is `0x`-prefixed; amounts are decimal strings. No endpoint logs
addresses, salts, viewing keys or IPs.

| Method & path | Body | Response |
|---|---|---|
| `POST /vaults` | `{ owner, account, amount, requestId, deadline, signature }` (`OpenVault`) | `{ vaultId, address (Ironwood-only UA), zip321, amount, expiryHeight, ufvkHash, signatures: [{ index, signature }] }`; `signatures`: every signer's `VaultOpened` (§ 2.5), ordered by node index; the client checks all five before showing `address` and builds the payment URI itself |
| `GET /vaults/:id` | — | `{ vaultId, status, amount, address, expiryHeight, birthdayHeight, tipHeight, outstanding, payoutInFlight, nonConforming: [{ noteId, amount }], claims: [{ claimId, kind, amount, paid }] }`; `kind` ∈ `Release`, `Liquidation`, `CloseUser`, `CloseMarketMaker` |
| `POST /vaults/:id/destination` | `{ receiver (UA), salt, nonce, deadline, signature }` (`Destination`) | `{ commitment }` — validated **before** any Base tx that burns zZEC |
| `POST /vaults/:id/refund` | `{ noteId, receiver, salt, nonce, deadline, signature }` (`RefundRequest`) | `{ accepted: true }` |
| `POST /vaults/:id/viewing-key` | `{ nonce, deadline, signature }` (`ViewKeyRequest`) | `{ ufvk }` |
| `POST /vaults/:id/deposit-proof` | `{ imageId, journal, seal }` (a deposit proof made elsewhere, e.g. `zecured-wallet loan prove`) | `{ accepted: true }` once `attestDepositWithProof` is mined with the vault's `VaultOpened` signatures and the coordinator's gas (Y10: proving one's own deposit needs no ETH); the coordinator then finalizes the activation (K3). Only while the vault waits for its deposit |
| `POST /recipients` | `{ receiver (UA), salt }` (liquidator / market maker) | `{ commitment }` |
| `GET /quotes/close/:vaultId` | — | `{ executor, marginBps (number), repayAssets, mmAmount, userAmount, expiresAt (number, Unix s) }` (relayed to the market maker) |
| `POST /closes` | `{ request: CloseRequest (camelCase), signature }` | `{ txHash }` once `closeWithCollateral` is mined (relayed; executed by the market maker) |
| `GET /quotes/deleverage/:vaultId?repay=<units>` | — | `{ executor, marginBps (number), repayAssets, mmAmount, remaining, expiresAt (number, Unix s) }` for repaying `repay` loan-token units of the debt (S18; relayed); refused with `repay_above_debt`, `deleverage_too_small` or `deleverage_too_large` |
| `POST /deleverages` | `{ request: DeleverageRequest (camelCase), signature }` | `{ txHash }` once `deleverageWithCollateral` is mined (relayed; the market maker repays exactly the quoted amount) |
| `GET /status` | — | `{ commit, treeClean, startedAt, deployment: { chainId, block, registry, slasher, signerBond, mintMode, verifierIsMock, depositImageId, violationImageId, challengeWindow, theftDemo }, zcashTip, nodes: [{ index, mode }] }`: the commit the backend was built from and the deployed image ids, so anyone can check that the running system is the submitted code (review G5); nothing about any vault or user; node modes cached 15 s |
| `GET /config` | — | `{ chainId, registry, adapter, contracts, market, signers, denominations, confirmations, vaultExpiryBlocks, sponsorship }` (`signers`: the deployment's five signer addresses, ascending, or `null`) — the app pins contract addresses and signers at build time and uses this only as a consistency check (any mismatch disables Base actions); it takes `sponsorship` (bundler, paymaster and allowlist all configured), denominations, confirmations and the deposit window from it |
| `POST /proxy/rpc` | JSON-RPC | Base RPC passthrough (read methods + `eth_sendRawTransaction` disallowed) |
| `POST /proxy/bundler` | JSON-RPC | ERC-4337 bundler passthrough |
| `POST /proxy/paymaster` | JSON-RPC | Sponsors only the allowlist below; rejects anything else |

Vault status values: `awaiting_deposit`, `deposit_seen`, `active`, `closed`, `expired`.
Amounts are decimal strings in zatoshis (ZEC) or 6-decimal units (USDC).

### 4.2 Demo control (header `x-admin-token`)

| Method & path | Body | Response |
|---|---|---|
| `GET /admin/nodes` | — | `{ nodes: [{ index, mode, vaults, compromisable }] }`; `compromisable`: the panel may switch this node to "compromised" (`DEMO_COMPROMISABLE_NODES`) |
| `POST /admin/nodes/:index/mode` | `{ mode: "normal" \| "compromised" \| "offline" }` | `{ mode }` |
| `POST /admin/vaults/:id/drain` | `{ to (UA) }` | `{ vaultId, amount, votes: [{ index, signed, reason }], thresholdReached }` — never broadcasts |
| `GET /admin/oracle` | — | `{ priceUsdCents, raw }` |
| `POST /admin/oracle` | `{ priceUsdCents }` | `{ priceUsdCents, txHash }` (MOCK oracle, owner key) |

### 4.1 Paymaster allowlist (enforced by the proxy)

A UserOp is sponsored only if every call it makes is one of:

| Target | Calls |
|---|---|
| Smart account factory | deployment in the account's first UserOp (`initCode` / `factory`) |
| `ZecuredRegistry` | `requestRelease`, `requestPartialRelease` |
| `SettlementAdapter` | `invalidateNonce` |
| Morpho Blue | `setAuthorization` (only for the Registry and the Adapter), `borrow` and `repay` (only the Zecured market) |
| MockUSDC | `approve` (spender = Morpho) and `transfer` |

Anything else is rejected. The account never receives ETH. The first UserOp must skip
authorizations that are already set (Morpho reverts on re-setting).
