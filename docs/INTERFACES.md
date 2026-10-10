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
| Registry | `ZecuredRegistry` | `1` | 84532 | ZecuredRegistry | `DepositAttestation`, `PayoutConfirmation`, and all off-chain owner requests (`OpenVault`, `Destination`, `RefundRequest`, `ViewKeyRequest`) |
| Adapter | `ZecuredSettlementAdapter` | `1` | 84532 | SettlementAdapter | `CloseRequest` |

Every request kind has its own type, so a signature for one kind never verifies as another.

### 2.2 Signed by signer nodes (ECDSA, verified on-chain by the Registry)

```
DepositAttestation(bytes32 vaultId,address account,uint64 amount,bytes32 depositTag)
PayoutConfirmation(bytes32 vaultId,bytes32 claimIdsHash)
```

- `depositTag`: keyed tag of the conforming note (`BLAKE2b-256(key = vault tag key, personal =
  "Zecured_DepTag", note nullifier)`); unlinkable to any Zcash data without the vault secret.
  No expiry; the Registry rejects a reused `vaultId` or `depositTag` (I5).
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

## 3. Contracts (Base Sepolia)

### 3.1 `ZZEC` (ERC-20, 8 decimals)

- `mint`/`burn`: Registry only.
- Transfers allowed only: Registry → Morpho (collateral supply, pulled by Morpho), and Morpho →
  Registry or Morpho → SettlementAdapter **while that receiver has armed a one-shot transient
  expectation** with `expectFromMorpho(uint256 maxAmount)` (callable by Registry and Adapter
  only; the expected recipient is `msg.sender`; EIP-1153 transient storage; consumed by the
  transfer; armed right before the Morpho call that moves zZEC). The Registry burns zZEC held
  by the Adapter directly. Everything else reverts (Morpho surfaces it as "transfer reverted").
  No account ever holds zZEC (I2).

### 3.2 `ZecuredRegistry`

```solidity
enum VaultStatus { None, Active, Closed }
enum ClaimKind   { Release, Liquidation, CloseUser, CloseMarketMaker }
enum ClaimStatus { None, Pending, Paid }

function attestDeposit(DepositAttestation calldata att, bytes[] calldata sigs) external;
function requestRelease(bytes32 vaultId, bytes32 destCommitment) external returns (bytes32 claimId);
function recordLiquidation(bytes32 vaultId, uint64 seized, bytes32 liquidatorCommitment) external returns (bytes32 claimId);   // adapter only
function recordClose(bytes32 vaultId, uint64 mmAmount, uint64 userAmount, bytes32 mmCommitment, bytes32 userCommitment)
    external returns (bytes32 userClaimId, bytes32 mmClaimId);                                                             // adapter only
function confirmPayout(bytes32 vaultId, bytes32[] calldata claimIds, bytes[] calldata sigs) external;

function vaultOf(address account) external view returns (bytes32 vaultId);
function vault(bytes32 vaultId) external view returns (address account, uint64 outstanding, VaultStatus status);
function claim(bytes32 claimId) external view returns (bytes32 vaultId, ClaimKind kind, uint64 amount, bytes32 commitment, ClaimStatus status);
function marketParams() external view returns (MarketParams memory);
function signers() external view returns (address[5] memory);

event VaultActivated(bytes32 indexed vaultId, address indexed account, uint64 amount);
event ReleaseRequested(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 destCommitment);
event LiquidationRelease(bytes32 indexed vaultId, bytes32 indexed claimId, uint64 amount, bytes32 recipientCommitment);
event CloseRelease(bytes32 indexed vaultId, bytes32 userClaimId, bytes32 mmClaimId, uint64 userAmount, uint64 mmAmount,
                   bytes32 userCommitment, bytes32 mmCommitment);
event PayoutConfirmed(bytes32 indexed vaultId, bytes32[] claimIds);
```

- `attestDeposit` registers the vault (`outstanding = amount`, `Active`), mints zZEC to itself
  and calls `Morpho.supplyCollateral(market, amount, onBehalf = account, "")` (I1, I4, I5).
- `requestRelease`: caller must be the vault's account; withdraws `outstanding` from Morpho on
  behalf of the account to the Registry (reverts while debt remains), burns it,
  `outstanding = 0`, `Closed`.
- `recordLiquidation`: `seized <= outstanding` (I9), burns, `outstanding -= seized`; the vault
  stays `Active` while `outstanding > 0`.
- `recordClose`: `mmAmount + userAmount == outstanding` (I7), burns, `Closed`, two claims.
- `claimId = keccak256(abi.encode(vaultId, claimSequence))`, sequence per vault from 0.

### 3.3 `SettlementAdapter`

```solidity
function liquidate(address borrower, uint256 seizedAssets, uint256 repaidShares, bytes32 liquidatorCommitment)
    external returns (uint256 seized, uint256 repaid);
function closeWithCollateral(CloseRequest calldata req, bytes calldata userSig, bytes32 mmCommitment) external;
function invalidateNonce(uint256 nonce) external;                 // msg.sender = account
function nonceUsed(address account, uint256 nonce) external view returns (bool);

function onMorphoLiquidate(uint256 repaidAssets, bytes calldata data) external;   // Morpho only
function onMorphoRepay(uint256 repaidAssets, bytes calldata data) external;       // Morpho only

event NonceInvalidated(address indexed account, uint256 nonce);
```

- `liquidate`: exactly one of `seizedAssets`/`repaidShares` non-zero (`repaidShares` capped at
  the borrower's shares read in the same call); seized ≥ `MIN_SEIZE` = 100,000 zat (0.001 ZEC)
  or seized = the vault's entire remaining `outstanding` (dust guard, D11); USDC pulled from `msg.sender` inside
  `onMorphoLiquidate`; seized zZEC → `Registry.recordLiquidation`.
- `closeWithCollateral`: `msg.sender == req.executor`, `block.timestamp <= req.deadline`, nonce
  unused and not invalidated, `marginBps <= MAX_MARGIN_BPS`, ERC-1271 valid on `req.account`;
  repays all borrow shares (USDC pulled from the executor in `onMorphoRepay`),
  `mmAmount = ceil(repaidAssets * (10_000 + marginBps) * 1e36 / (10_000 * price))`,
  `userAmount = outstanding - mmAmount >= minUserAmount`, withdraws `outstanding` to itself,
  `Registry.recordClose` (I8).

### 3.4 Mocks (labelled MOCK)

- `MockUSDC` (6 decimals, owner mint).
- `MockZecOracle`: `price() = ZEC/USD * 1e34`; owner-only `setPrice`.

### 3.5 Market

zZEC / MockUSDC / MockZecOracle / AdaptiveCurveIRM `0x46415998764C29aB2a25CbeA6254146D50D22687`
/ LLTV `0.625e18`, on Morpho Blue `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb`.
Deployed addresses: `contracts/deployments/base-sepolia.json`.

## 4. Coordinator API (backend, JSON over HTTPS)

All bodies are JSON; hex is `0x`-prefixed; amounts are decimal strings. No endpoint logs
addresses, salts, viewing keys or IPs.

| Method & path | Body | Response |
|---|---|---|
| `POST /vaults` | `{ owner, account, amount, requestId, deadline, signature }` (`OpenVault`) | `{ vaultId, address (Ironwood-only UA), zip321, amount, expiryHeight }` |
| `GET /vaults/:id` | — | `{ vaultId, status, amount, expiryHeight, confirmations, deposit?: { attested, txHash? }, nonConforming: [{ noteId, amount }], claims: [{ claimId, kind, amount, status }] }` |
| `POST /vaults/:id/destination` | `{ receiver (UA), salt, nonce, deadline, signature }` (`Destination`) | `{ commitment }` — validated **before** any Base tx that burns zZEC |
| `POST /vaults/:id/refund` | `{ noteId, receiver, salt, nonce, deadline, signature }` (`RefundRequest`) | `{ accepted: true }` |
| `POST /vaults/:id/viewing-key` | `{ nonce, deadline, signature }` (`ViewKeyRequest`) | `{ ufvk }` |
| `POST /recipients` | `{ receiver (UA), salt }` (liquidator / market maker) | `{ commitment }` |
| `GET /quotes/close/:vaultId` | — | `{ executor, marginBps, repayAssets, mmAmount, userAmount, expiresAt }` (relayed to the market maker) |
| `POST /closes` | `{ request: CloseRequest (camelCase), signature }` | `{ txHash }` (relayed; the market maker executes `closeWithCollateral`) |
| `POST /proxy/rpc` | JSON-RPC | Base RPC passthrough (read methods + `eth_sendRawTransaction` disallowed) |
| `POST /proxy/bundler` | JSON-RPC | ERC-4337 bundler passthrough |
| `POST /proxy/paymaster` | JSON-RPC | Sponsors only the allowlist below; rejects anything else |

Vault status values: `awaiting_deposit`, `deposit_seen`, `active`, `closed`, `expired`.

### 4.2 Demo control (header `x-admin-token`)

| Method & path | Body | Response |
|---|---|---|
| `GET /admin/nodes` | — | `{ nodes: [{ index, mode, vaults }] }` |
| `POST /admin/nodes/:index/mode` | `{ mode: "normal" \| "compromised" \| "offline" }` | `{ mode }` |
| `POST /admin/vaults/:id/drain` | `{ to (UA) }` | `{ vault_id, amount, votes: [{ index, signed, reason }], threshold_reached }` — never broadcasts |
| `GET /admin/oracle` | — | `{ priceUsdCents, raw }` |
| `POST /admin/oracle` | `{ priceUsdCents }` | `{ priceUsdCents, txHash }` (MOCK oracle, owner key) |

### 4.1 Paymaster allowlist (enforced by the proxy)

A UserOp is sponsored only if every call it makes is one of:

| Target | Calls |
|---|---|
| Smart account factory | deployment in the account's first UserOp (`initCode` / `factory`) |
| `ZecuredRegistry` | `requestRelease` |
| `SettlementAdapter` | `invalidateNonce` |
| Morpho Blue | `setAuthorization` (only for the Registry and the Adapter), `borrow` and `repay` (only the Zecured market) |
| MockUSDC | `approve` (spender = Morpho) and `transfer` |

Anything else is rejected. The account never receives ETH. The first UserOp must skip
authorizations that are already set (Morpho reverts on re-setting).
