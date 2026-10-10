# Spike C — Morpho Blue + gasless smart account on Base Sepolia (result: ✅ GO, on a fork)

Date: 2026-10-11. Everything ran on a **fork** of Base Sepolia (84532) at block **47,948,000**
against the real deployed contracts. Nothing was broadcast to a live network.

Toolchain: forge 1.4.2, solc 0.8.28, `evm_version = "cancun"`; forge-std v1.17.0,
OpenZeppelin Contracts v5.7.0, morpho-blue `8e26ca6` (the docs/ref commit; its `src/` is
byte-identical to `docs/ref/vendor/morpho-blue/src`).

## Success criterion

> A paymaster-sponsored account can borrow and repay without ever holding zZEC; an authorized
> contract can withdraw the collateral; the debt can be closed from another address.

**Met on a fork.** The only stand-in is the paymaster (`SpikeAllowlistPaymaster`, MOCK),
because there is no Pimlico/CDP key yet. Morpho Blue, AdaptiveCurveIRM, Safe 1.4.1,
Safe4337Module 0.3.0 and EntryPoint v0.7 are the real Base Sepolia deployments.

```
$ forge test
Ran 5 test suites in 1.61s (2.77s CPU time): 41 tests passed, 0 failed, 0 skipped (41 total tests)
$ forge test --isolate
Ran 5 test suites in 1.31s (2.02s CPU time): 41 tests passed, 0 failed, 0 skipped (41 total tests)
```

(The first cold run fetches fork state from the public RPC and takes about 10–20 s. After that
Foundry serves the pinned block from its disk cache.)

## What is proven

| # | Claim | Evidence |
|---|---|---|
| 1 | A zZEC stand-in (8 dec) / MockUSDC (6 dec) / MockZecOracle / AdaptiveCurveIRM / LLTV 62.5% market can be created on the real Morpho. IRM and LLTV are enabled | `SpikeC.t.sol` test_01, `DeployMocks` dry run |
| 2 | Registry mints to itself and calls `supplyCollateral(onBehalf = account)`. No authorization is needed, and it works **before the account exists** (counterfactual Safe address) | test_03, `Safe4337UserOp.t.sol` (deposit before UserOp 1) |
| 3 | The account does `setAuthorization(registry)` + `setAuthorization(adapter)` + `borrow(receiver = another address)`. The USDC never touches the account | test_04; as one Safe batch (`SafeErc1271.t.sol`); as one sponsored UserOp (`test_userOp1…`) |
| 4 | `repay(shares)` pulls exactly `MorphoBalancesLib.expectedBorrowAssets` (160 → 160.042384 USDC after 30 days of real AdaptiveCurveIRM interest). Then the registry withdraws the collateral to itself using the account's authorization, and burns it | test_04, `test_userOp2…` (approve + repay + requestRelease in one sponsored UserOp) |
| 5 | Third-party close: the adapter calls `repay(0, allShares, onBehalf = account, data)`. In `onMorphoRepay(assets)` it pulls **exactly** `assets` from the market maker (exact allowance, zero dust) and approves Morpho, then withdraws the collateral to itself | test_05 |
| 6 | Without the account's authorization of the adapter, the close reverts atomically, including the repay that already ran | test_06 |
| 7 | Partial liquidation through the adapter at $25.50 (LTV 62.75%): repaying all debt seized 706,987,019 of 1,000,000,000 zat = **70.70 %** (= LTV × LIF, LIF = 1/(1−0.3·0.375) = 1.12676). The seized collateral was **already in the adapter when `onMorphoLiquidate` ran**. The registry then released the remaining 29.30 % | test_07; exact-seize path (`seizedAssets`) in test_08 |
| 8 | A direct `withdrawCollateral` / `borrow` on behalf of the account by a stranger reverts `unauthorized`, and so does the registry before the account authorizes it | test_09 |
| 9 | The account holds **0** collateral tokens after every step of every flow (asserted throughout) | all flows |
| 10 | The zZEC transfer guard (prototype, EIP-1153) blocks: the account withdrawing its own collateral (to itself, the registry or the adapter), a direct `Morpho.liquidate`, and a Morpho `flashLoan` of the collateral. **Without the guard the account can take its collateral** (test_13), so the guard is required | test_11, test_12, test_13 |
| 11 | ERC-1271 with a real Safe 1.4.1 (3 configurations, see below) | `SafeErc1271.t.sol` |
| 12 | ERC-4337: real `EntryPoint.handleOps` deploys the Safe from `initCode` and runs the batched authorize + borrow. The paymaster deposit pays; the account has 0 ETH and no EntryPoint deposit throughout. Non-allowlisted calls are rejected with `AA33` | `Safe4337UserOp.t.sol` (3 tests) |
| 13 | EIP-1153 is live on Base Sepolia (real node, not only the fork EVM): `cast rpc eth_call '{"data":"0x602a5f5d5f5c5f5260205ff3"}' latest --rpc-url https://sepolia.base.org` returns `0x…2a` (TSTORE/TLOAD round-trip); unknown opcodes error | manual, 2026-10-10 |
| 14 | `DeployMocks` dry run prints a market id and sends nothing. On a **local anvil fork**, `--broadcast` without `COLLATERAL_TOKEN` is refused, and with it the market is created (the local-fork broadcast artifacts were deleted afterwards) | manual |
| 15 | With D7 vault sizes, a **0.01 TAZ** vault cannot satisfy INTERFACES.md 3.3's 0.01 ZEC minimum seize near LLTV (see Recommendations); 0.05 TAZ vaults can | test_15, test_16, test_17 |

Gas: UserOp 1 (Safe deploy + 2 × setAuthorization + borrow) was charged 663,004 gas,
including the 100k preVerificationGas we set. That is negligible at the fork block's 0.005 gwei
base fee.

## Files (all new)

```
contracts/foundry.toml
contracts/src/mocks/MockUSDC.sol                 MOCK: 6 dec, owner-only mint
contracts/src/mocks/MockZecOracle.sol            MOCK: IOracle, price = USD/ZEC * 1e34, owner-only setPrice, PriceSet event
contracts/script/DeployMocks.s.sol               mocks + market, prints market id
contracts/script/DryRunPlaceholderCollateral.sol MOCK: dry-run-only placeholder for zZEC
contracts/test/mocks/MockUSDC.t.sol              5 tests (incl. fuzz)
contracts/test/mocks/MockZecOracle.t.sol         10 tests (incl. fuzz of the 1e34 scaling)
contracts/test/spike/  (throwaway, MOCK/SPIKE-ONLY)
  SpikeCollateral.sol  SpikeRegistry.sol  SpikeAdapter.sol  SpikeCBase.sol  SpikeC.t.sol (17 tests)
  SafeInterfaces.sol  SpikeCloseRequestVerifier.sol  SafeErc1271.t.sol (6 tests)
  AccountAbstractionV07.sol  SpikeAllowlistPaymaster.sol  Safe4337UserOp.t.sol (3 tests)
contracts/lib/{forge-std, openzeppelin-contracts, morpho-blue}   (plain clones, see below)
```

## Commands

```bash
cd contracts
forge build
forge test                                    # 41 tests; fork tests use $BASE_SEPOLIA_RPC_URL or https://sepolia.base.org
forge test --isolate                          # each top-level call is its own tx (real transient-storage semantics)
forge test --no-match-path 'test/spike/*'     # offline: mock unit tests only (15)
SPIKE_FORK_BLOCK=0 forge test --match-path 'test/spike/*'    # latest block instead of 47,948,000
forge script script/DeployMocks.s.sol --rpc-url https://sepolia.base.org      # dry run, sends nothing

# later, when zZEC is deployed and a deployer key is funded (do not run before):
COLLATERAL_TOKEN=<zZEC> ZEC_USD_PRICE_CENTS=4000 \
  forge script script/DeployMocks.s.sol --rpc-url base_sepolia --broadcast --account <keystore>
```

Dependencies were installed with `forge install --root contracts --no-git [--shallow]`
(forge-std@v1.17.0, OpenZeppelin/openzeppelin-contracts@v5.7.0,
morpho-org/morpho-blue@8e26ca6a8dbc5089edcd67fb576248810fd2870a). forge 1.4.2 never commits by
default, and `--no-git` also avoids writing a root `.gitmodules` or staging anything. No commit
was made by this spike. (The signer commits in `git log` come from the parallel Spike A
session.) `--no-git` left five dangling `.git` pointer files inside OZ's and Morpho's own test
dependencies; they were removed so that `git add` works.

**Before committing**, choose one:
- commit `contracts/lib` as vendored files (about 21 MB), or
- convert to submodules: `rm -rf contracts/lib` and rerun the same `forge install` commands
  without `--no-git`. This creates the root `.gitmodules`.

## Gotchas

**Morpho call ordering** (read from `Morpho.sol`, confirmed by the callback probes in `SpikeAdapter`):

| Call | Order |
|---|---|
| `supplyCollateral` | position += → event → `onMorphoSupplyCollateral` → `transferFrom(caller)`; no interest accrual |
| `withdrawCollateral` | auth → accrue → position −= → health check → `transfer(receiver)`; no callback |
| `repay` | accrue → shares burned → event → `onMorphoRepay(assets)` → `transferFrom(caller, assets)` |
| `liquidate` | accrue → unhealthy check → state updated (bad debt realized if collateral hits 0) → **`transfer(seized → caller)`** → `onMorphoLiquidate(repaid)` → `transferFrom(caller, repaid)` |
| `flashLoan` | `transfer(token → caller)` → callback → `transferFrom(caller)`, for **any** token Morpho holds, collateral included |

Callbacks fire only when `data.length > 0`, and only on the `msg.sender` of the Morpho call.

**Rounding**
- Always close positions with `repay(0, shares)`, never by assets, to avoid dust shares. The
  assets pulled (`toAssetsUp`) equal `MorphoBalancesLib.expectedBorrowAssets` within the same
  block. Exact quotes need that library because `market()` is stale until accrual.
- `liquidate(repaidShares)`: seized is computed from `toAssetsDown`, repaid is `toAssetsUp`.
  `liquidate(seizedAssets)`: repaidShares is `toSharesUp(...)` and **underflows** (bare
  `Panic(0x11)`) when it exceeds the position. This is why seizing the whole vault fails while
  its value / LIF > debt (test_15).
- First borrower in an empty market: 1 USDC unit = 1e6 shares (`VIRTUAL_SHARES`). Morpho warns
  about markets with < 1e4 units borrowed; D7-size loans are ≥ 1.6e5 units.

**Authorization**
- `setAuthorization` is global: all markets and all actions. An authorized contract can also
  `borrow` on behalf of the account to any receiver (test_10).
- Setting a value that is already set reverts `already set`. A UserOp batch must leave out
  authorizations that are already true, or the whole batch fails.
- `repay` / `supply` / `supplyCollateral` on behalf need no authorization: anybody can repay
  anybody's debt.
- `msg.sender == onBehalf` is always authorized, so without the token guard the account can
  withdraw its own collateral anywhere (test_13).
- `setAuthorizationWithSig` uses plain `ecrecover`, so a smart account cannot use it. The
  account must send `setAuthorization` itself (UserOp).

**Errors**
- Morpho's `SafeTransferLib` replaces any token revert with `"transfer reverted"` /
  `"transferFrom reverted"`. zZEC's custom errors are invisible at the Morpho boundary (only in
  traces), so guard tests must expect those strings.
- Front-run griefing: a 1-share third-party repay makes a liquidation with a precomputed
  `repaidShares` underflow (test_14).

**Interest and oracle**
- The real AdaptiveCurveIRM at 0.16 % utilization gave about 0.3 % APR over 30 days in the test.
- `MockZecOracle` price = USD/ZEC × 1e34 (for example $25.50 = `2550e32`). A demo price crash
  can break Morpho's "no drop below LLTV × LIF in one step" assumption and leave bad debt in
  the demo market (test_16).

**Foundry**
- Transient storage **persists across top-level calls inside one test** unless `--isolate` is
  used. Measured: TSTORE 42 in one call, TLOAD in a later call returns 42 normally and 0 with
  `--isolate`. Run the zZEC / Registry / Adapter suites with `--isolate` as well.
- `--fork-retries` / `--fork-retry-backoff` require `--fork-url`; they do nothing for
  `vm.createSelectFork`. A pinned fork block is cached on disk (`~/.foundry/cache/rpc/…`). The
  public RPC served state at least 5M blocks back, so pinning is safe.
- `abi.encodeCall(IMorpho.setAuthorization, …)` does not compile. Inherited interface members
  must be referenced through `IMorphoBase`.
- forge-lint style notes are limited to `src/` (`[lint] ignore = ["test/**/*.sol"]`).

## ERC-1271 with Safe (task 5)

**Safe v1.4.1 is deployed on Base Sepolia at the canonical addresses.** The safe-deployments
registry lists 84532 as `canonical`, and the on-chain code hashes match the registry's
`codeHash`:

- SafeProxyFactory `0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67`
- Safe `0x41675C099F32341bf84BFc5382aF534df5C7461a` (`VERSION() = "1.4.1"`)
- SafeL2 `0x29fcB43b46531BcA003ddC8FCB67FFE91900C762`
- CompatibilityFallbackHandler `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99`
- MultiSend `0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526`
- MultiSendCallOnly `0x9641d764fc13c8B624c04430C7356C1C7C8102e2`
- Safe4337Module 0.3.0 `0x75cf11467937ce3F2f357CE24ffc3DBF8fD5c226` (`SUPPORTED_ENTRYPOINT` = v0.7)
- SafeModuleSetup `0x2dd68b007B46fBe91B9A7c3EDa5A7a1063cB5b47`

Kernel was not needed.

**How the signature must be produced.** It is checked on a fresh 1-owner Safe, the account is
the `CloseRequest.account`, and the verifier calls
`SignatureChecker.isValidSignatureNow(account, digest, sig)`:

```
digest     = EIP-712 digest of CloseRequest under the adapter domain
             (INTERFACES.md 2.1/2.4: "ZecuredSettlementAdapter", "1", 84532, adapter)
safeDomain = keccak256(abi.encode(keccak256("EIP712Domain(uint256 chainId,address verifyingContract)"), 84532, safe))
structHash = keccak256(abi.encode(keccak256("SafeMessage(bytes message)"), keccak256(abi.encode(digest))))
safeHash   = keccak256(0x1901 || safeDomain || structHash)
sig        = owner ECDSA over safeHash, encoded r || s || v (65 bytes, v = 27/28)
```

In viem terms, the owner key signs EIP-712 typed data:

```ts
const digest = hashTypedData({ domain: adapterDomain, types: { CloseRequest: [...] },
                               primaryType: "CloseRequest", message: req })
const sig = await ownerAccount.signTypedData({
  domain: { chainId: 84532, verifyingContract: safeAddress },
  types: { SafeMessage: [{ name: "message", type: "bytes" }] },
  primaryType: "SafeMessage",
  message: { message: digest },          // the 32 digest bytes as `bytes`
})
```

permissionless.js (main-branch source read on 2026-10-11; latest release 0.4.1) does exactly this in its Safe
account's `signTypedData(typedData)`: it wraps `hashTypedData(typedData)` in `SafeMessage` and
normalizes v to 27/28. For one EOA owner the result is the plain 65 bytes.

The checks (all asserted, `test_erc1271_*`):
- **Accepted:** the format above, both as OZ `SignatureChecker` and as a direct
  `isValidSignature` call returning `0x1626ba7e`.
- **Also accepted:** `eth_sign` over `safeHash` with v + 4.
- **Rejected:** the owner signing the raw `digest` (the likeliest integration bug); EIP-191
  without the +4; a non-owner; an empty signature (would need a SignMessageLib approval); any
  field change; a signature made for another Safe with the same owner.
- **Same result in all three configurations:** CompatibilityFallbackHandler;
  Safe4337Module as fallback handler, which inherits CompatibilityFallbackHandler; and
  permissionless.js' default setup (setup → MultiSend → delegatecall
  `SafeModuleSetup.enableModules`).
- OZ 5.7 `SignatureChecker` does **not** unwrap ERC-6492. CloseRequests must be signed after the
  account is deployed. Our flow guarantees this because the account is deployed by its first
  UserOp, but a client library may 6492-wrap signatures for undeployed accounts.

## Smart account + paymaster: what remains (needs a Pimlico or CDP key)

Done on the fork: Safe 1.4.1 + Safe4337Module 0.3.0 + EntryPoint v0.7. One UserOp deploys
the account from `initCode` and runs [setAuthorization(registry), setAuthorization(adapter),
borrow] through MultiSendCallOnly. A second UserOp runs [approve, repay(shares),
requestRelease]. Both are sponsored. The allowlist (INTERFACES.md § 4.1) was enforced on-chain by
`SpikeAllowlistPaymaster`, which rejects other setAuthorization targets, other markets,
`withdrawCollateral` and other contracts.

Remaining:
1. **Provider and key.** Bundler and paymaster endpoints for 84532 behind `POST /proxy/bundler`
   and `/proxy/paymaster`. preVerificationGas on Base includes the L1 data fee; take it from
   the bundler's `eth_estimateUserOperationGas`, never hard-code it.
2. **Allowlist enforcement.** Provider sponsorship policies probably see only the outer
   `executeUserOp(MultiSendCallOnly, …)`. Enforce the allowlist in the backend proxy before
   requesting paymaster data. `SpikeAllowlistPaymaster._checkCallData` is the reference decoder
   to port. Alternatively, ship an allowlist paymaster of our own (it reads only immutables
   and returns an empty context, so it should fit ERC-7562's unstaked-paymaster rules; confirm
   with the chosen bundler), funded with Base Sepolia ETH. Judges could then verify the allowlist on-chain. A bundler endpoint is still needed
   either way.
3. **Pin the account config.** permissionless defaults to `useMultiSendForSetup = true`, which
   produces a different initializer and therefore a different CREATE2 address than
   `useMultiSendForSetup = false`. The app must compute the account address with the exact
   options it will deploy with, because `attestDeposit` supplies collateral to that address
   before the first UserOp. The proxy / paymaster `initCode` check must accept exactly that
   config. (The spike paymaster accepts only `setup.to = SafeModuleSetup`.)
4. In the first UserOp, skip any `setAuthorization` that is already set (`already set` revert).
5. A live smoke test on Base Sepolia once the key exists.

## Recommendations for the real contracts (INTERFACES.md § 3)

### ZZEC
1. Guard in `_update`, as prototyped in `SpikeCollateral`:
   - mint and burn: Registry only;
   - `from == Registry && to == Morpho`: allowed (Morpho pulls it inside `supplyCollateral`);
   - `from == Morpho`: only if `to == expectedRecipient && value <= expectedMax`; the
     expectation is then cleared (single use);
   - everything else reverts.

   The Registry burns zZEC held by the Adapter with a Registry-only `burn(from, amount)`.
   Do not add an Adapter → Registry transfer path.
2. **The expectation must be bound to `msg.sender`.** Use `expectFromMorpho(uint256 maxAmount)`,
   callable by Registry or Adapter, with recipient = caller. INTERFACES.md 3.1 has an explicit
   `recipient` argument; drop it or require `recipient == msg.sender`. Storage is one transient
   (recipient, max) pair, which suffices because Registry and Adapter never have two
   collateral-moving Morpho calls in flight.
3. **Arm immediately before the Morpho call that moves zZEC**, with `max = vault.outstanding`.
   After the call, check `balanceAfter − balanceBefore` against the expected amount and clear
   the expectation (defensive). Never call untrusted code between arming and the Morpho call.
   - `requestRelease`: arm(outstanding) → `withdrawCollateral(outstanding, account, registry)`
     → delta == outstanding → burn.
   - `closeWithCollateral`: repay first (moves no zZEC) → arm(outstanding) →
     `withdrawCollateral(outstanding, account, adapter)` → delta == outstanding → `recordClose`.
   - `liquidate`: **arm before `Morpho.liquidate`**, because Morpho transfers the seized zZEC to
     the adapter before calling `onMorphoLiquidate`; arming in the callback is too late. Use an
     upper bound (outstanding), since the repaidShares path does not know the seized amount in
     advance. Afterwards require `delta == seized <= outstanding` (I9). The transfer consumes
     the flag before the callback runs, so the callback cannot pull in more.
4. With the guard, direct `Morpho.liquidate` by anyone else, `flashLoan(zZEC)`, and an account
   withdrawing its own collateral (to itself, the Registry or the Adapter) all revert with
   `"transfer reverted"` (tests 11–12). Keep those as regression tests and run them with
   `--isolate`.

### ZecuredRegistry
1. `attestDeposit`: mint → `approve(Morpho, amount)` → `supplyCollateral(onBehalf = account, "")`.
   This works for a counterfactual (not yet deployed) account address.
2. `requestRelease`:
   - caller == vault account;
   - check `borrowShares == 0` first for a clear error (Morpho would say
     `insufficient collateral`);
   - assert Morpho `position.collateral == outstanding` (they cannot diverge with the guard);
   - then flow 3 above.

   After a partial liquidation it releases the remainder (test_07).
3. The Registry must never call `borrow` / `withdraw` on behalf of accounts. Authorization is
   global (test_10). Keep it non-upgradeable, or document the admin power.

### SettlementAdapter
1. **`closeWithCollateral`.**
   - Read `borrowShares` in the same tx and call `repay(0, shares, account, nonEmptyData)`.
   - In `onMorphoRepay`, require `msg.sender == Morpho` plus a transient in-flight context set
     by this adapter (op, executor, vault).
   - Pull exactly `assets` from the executor (= `msg.sender` of `closeWithCollateral`; never
     from a caller-supplied address) and `forceApprove(Morpho, assets)`.
   - Compute `mmAmount` from that exact `assets` (INTERFACES.md formula, rounded up) and revert
     if `userAmount < minUserAmount`.
2. **ERC-1271.** Use `SignatureChecker.isValidSignatureNow(req.account, digest, userSig)` with
   the Safe-wrapped signature above. The domain and type are already exercised by the spike
   (`SpikeCloseRequestVerifier`, INTERFACES.md 2.4 type).
3. **`liquidate`.**
   - Cap `repaidShares` at the current `borrowShares` (test_14).
   - Pull USDC only from `msg.sender`.
   - Use exactly one of `seizedAssets` / `repaidShares`; arm per ZZEC rule 3.
   - When a whole-vault seize realizes bad debt, Morpho zeroes the borrow shares. The vault
     goes to `outstanding = 0` / `Closed`.
4. **Minimum seize vs D7 (needs an owner decision).**
   - INTERFACES.md 3.3 requires seized ≥ 0.01 ZEC (1,000,000 zat) or the whole outstanding.
   - With D7's **0.01 TAZ** vaults, repaying all debt just past LLTV seizes only ~707,000 zat
     (test_15).
   - Seizing the whole vault reverts until LTV ≥ 1/LIF = 88.75 %, and at that point the
     lenders take bad debt (test_16: 249 units at $18.00).
   - Result: 0.01 TAZ vaults are unliquidatable between 62.5 % and 88.75 % LTV, and the
     "partial liquidation → release the rest" demo is impossible for them. 0.05 TAZ vaults are
     fine (test_17: 3,534,933 zat seized, 1,465,067 left).
   - **Proposal:** a fee-based floor, e.g. 100,000 zat (0.001 ZEC, ~7–10× the 10,000–15,000 zat
     ZIP-317 payout fee), for both the seized amount and a non-zero remainder. Otherwise demo
     partial liquidation with 0.05 TAZ vaults only.
5. Because authorization is global, the Adapter must only ever call `repay`, `liquidate` and
   `withdrawCollateral` (the latter only with a valid request, I8). It must never call
   `borrow`.

### Paymaster / proxy allowlist (INTERFACES.md § 4.1, made precise)
The precise rules (implemented in `SpikeAllowlistPaymaster`):
- no ETH value anywhere;
- `initCode` = the exact Safe config;
- `setAuthorization` only for registry or adapter;
- `borrow` / `repay` only on this market, with `onBehalf == sender`; `repay` with empty
  callback data;
- USDC: `approve` only to Morpho; `transfer` to anyone;
- the real selectors `requestRelease(bytes32,bytes32)` and `invalidateNonce(uint256)`.

## Privacy notes (CLAUDE.md)
- Nothing Zcash-related appears on Base in these flows.
- Morpho events link the Registry to the account (`SupplyCollateral` caller/onBehalf) and to
  amounts. That is by design.
- `Borrow` publishes the **receiver**. Borrowing straight to a main wallet links it to the vault
  account, so the app's USDC-destination warning must cover this.
- Close: the market maker's address appears in the USDC `Transfer` to the adapter (an expected,
  public role).
- `SafeSetup` publishes the per-loan owner EOA, which is fresh per loan.
- We use the non-L2 Safe singleton (the permissionless default for 1.4.1). SafeL2 would also
  emit every transaction's data in events.

## Not covered
The real ZZEC / ZecuredRegistry / SettlementAdapter (signatures, vault bookkeeping, claims,
invariants I1–I9), the provider bundler and paymaster, the backend proxy, live deployment and
`deployments/base-sepolia.json`. Root README must list `MockUSDC`, `MockZecOracle` (and the
spike's MOCK helpers if they are kept) under "What is real and what is a mock". This spike did
not edit README.
