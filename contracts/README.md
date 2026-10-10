# Zecured contracts

Base Sepolia side of Zecured: the zZEC receipt token, the vault and claim registry, and the settlement adapter, on
top of a real Morpho Blue market. The cross-module spec is [docs/INTERFACES.md](../docs/INTERFACES.md) (§ 2 EIP-712,
§ 3 contracts). The design rationale is in [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md) and
[docs/DECISIONS.md](../docs/DECISIONS.md), and the Morpho / Safe findings in
[docs/spikes/SPIKE_C.md](../docs/spikes/SPIKE_C.md).

## Layout

```
src/
  ZZEC.sol                     zZEC (8 decimals): non-transferable receipt, transient "expected transfer from Morpho" guard
  ZecuredRegistry.sol          vaults, 3-of-5 signer attestations, burns + claims, payout confirmation
  SettlementAdapter.sol        liquidation (D11 floor) and close from collateral (ERC-1271 CloseRequest)
  interfaces/                  IZZEC, IZecuredRegistry (+ VaultStatus, ClaimKind, ClaimStatus), ISettlementAdapter
  libraries/ZecuredEIP712.sol  EIP-712 domain names, type hashes, DepositAttestation / CloseRequest structs
  mocks/                       MockUSDC, MockZecOracle (MOCK)
script/
  DeployZecured.s.sol          full deployment, writes deployments/base-sepolia.json
  export-abis.sh               writes abi/*.json
test/
  unit/                        zZEC transfer guard (no fork); EIP-712 vectors checked against an independent implementation
  fork/                        Registry, Adapter, transfer guard, end-to-end flows, real Safe v1.4.1 ERC-1271
  invariant/                   handler-based invariants (I1, I2, I3 Base side, I4/I5, I6, I7, I8, I9)
  utils/                       fork fixture, OneTx (several pranked calls in one transaction), Safe helpers
  mocks/, spike/               earlier tests (Spike C, throwaway)
abi/                           bare ABI arrays for the app and the off-chain services (committed)
deployments/                   base-sepolia.json after a real deployment (dry runs: *.dry-run.json, gitignored)
```

`ZecuredEIP712` is the only place where type strings live: the Registry and the Adapter both hash through it.
`test/unit/ZecuredEIP712.t.sol` pins golden digests for fixed inputs. Each digest is produced by the library, by
Foundry's independent EIP-712 implementation from wallet-style JSON (`vm.eip712HashTypedData`), and for
DepositAttestation also with `cast`. The Rust signer and the app can test against the same vectors.

## Testing

```bash
cd contracts
forge build
forge test --isolate          # 166 tests (125 for the production contracts), about 2 s once the fork is cached
```

- **Use `--isolate`.** It makes every top-level call of a test its own transaction, so EIP-1153 transient storage is
  cleared between calls as it is on chain. Without it the suite still passes, but
  `test_expectationDoesNotSurviveTheTransaction` is skipped.
- **Fork.** Tests that need Morpho fork Base Sepolia from `$BASE_SEPOLIA_RPC_URL` (default `https://sepolia.base.org`)
  at block 47,948,000 (`ZECURED_FORK_BLOCK=<n>`, `0` = latest). The first cold run fetches state from the RPC. After
  that, Foundry serves the pinned block from its disk cache.
- **Offline:** `forge test --isolate --match-path 'test/unit/*'`.
- **Invariants:** by default 32 runs × 50 calls per invariant (`[profile.default.invariant]`), with
  `fail_on_revert = true`. For a deeper run:
  `FOUNDRY_INVARIANT_RUNS=256 FOUNDRY_INVARIANT_DEPTH=100 forge test --isolate --match-path 'test/invariant/*'`.
  The handler pins `targetSender`: on a fork, each new random sender costs one RPC round trip (about 0.1 s per call).

## Deployment

`script/DeployZecured.s.sol` deploys or reuses MockUSDC and MockZecOracle, then deploys ZZEC, ZecuredRegistry and
SettlementAdapter. The Registry creates the market. The script can also seed a demo lender and writes the deployment
JSON.

forge loads `.env` only from `contracts/`, so export the two variables it needs from the repo-root `.env`. The script
never prints the key.

```bash
cd contracts
set -a; source <(grep -E '^(DEPLOYER_PRIVATE_KEY|SIGNER_ADDRESSES)=' ../.env); set +a

# Dry run: simulation only, nothing is sent. Writes deployments/base-sepolia.dry-run.json.
forge script script/DeployZecured.s.sol --rpc-url https://sepolia.base.org

# Real deployment, once the deployer holds Base Sepolia ETH (about 8.5 M gas; 0.0001 ETH at 0.011 gwei).
# Writes deployments/base-sepolia.json.
forge script script/DeployZecured.s.sol --rpc-url https://sepolia.base.org --broadcast
```

| Variable | |
|---|---|
| `DEPLOYER_PRIVATE_KEY` | deployer key; it also owns the mocks |
| `SIGNER_ADDRESSES` | the 5 signer addresses, comma-separated, strictly increasing |
| `ZEC_USD_PRICE_CENTS` | initial oracle price when the oracle is deployed (default `4000` = $40.00) |
| `MOCK_USDC`, `MOCK_ZEC_ORACLE` | reuse already deployed mocks (set both) |
| `LENDER_SEED_USDC` | whole MockUSDC the deployer mints and supplies as demo lender (default `0`) |

Deployment JSON keys: `network`, `chainId`, `dryRun`, `block`, `timestamp`, `deployer`, `contracts.{ZZEC,
ZecuredRegistry, SettlementAdapter, MockUSDC, MockZecOracle}`, `market.{morphoBlue, marketId, loanToken,
collateralToken, oracle, irm, lltv}`, `eip712.{registryDomain, adapterDomain, version, registryDomainSeparator,
adapterDomainSeparator}`, `signers`, `threshold`, `minSeize`, `maxMarginBps`, `oraclePrice`, `mocksReused`,
`lenderSeedUsdcUnits`, `mocks`. Integers above 2^53 (`lltv`, `oraclePrice`) are decimal strings.

## Design notes

### Deployment ordering

ZZEC needs the Registry's and the Adapter's addresses, the Registry needs ZZEC and the Adapter, and the Adapter needs
the Registry. All of them are immutables, so there is no initializer and no admin. The deployer creates the three
contracts back to back, and their addresses are predicted from its nonces:

```
nonce n     ZZEC(registry = addr(n+1), adapter = addr(n+2), morpho)
nonce n+1   ZecuredRegistry(morpho, zzec, adapter = addr(n+2), market params, signers)   creates the market
nonce n+2   SettlementAdapter(registry)                                                  reads the rest from the Registry
```

Both constructors verify the wiring: `zzec.registry() == this`, `zzec.adapter()` and `zzec.morpho()` in the Registry;
`registry.adapter() == this` and `zzec.adapter() == this` in the Adapter. If anything shifts the nonces, deployment
reverts; it never silently mis-wires. Market creation is permissionless, so someone may create the zZEC market between
the ZZEC and Registry deployments. The Registry then reuses the existing market instead of reverting.

### The zZEC transfer guard (EIP-1153)

A Morpho position owner can always withdraw its own collateral (`msg.sender == onBehalf`). The token therefore refuses
every transfer except:

- Registry → Morpho, pulled by Morpho (`supplyCollateral`);
- Morpho → Registry or Adapter, while that receiver has armed `expectFromMorpho(max)`.

The expectation is one `(recipient = msg.sender, max)` pair in transient storage. It is armed right before the Morpho
call and consumed by the transfer. For liquidation it is armed before `Morpho.liquidate`, with `max = outstanding`,
because Morpho transfers the seized collateral before the callback. After each call, the received amount is checked
against what Morpho reported. Account self-withdrawals, a direct `Morpho.liquidate` and a zZEC `flashLoan` all fail;
Morpho reports them as `"transfer reverted"`. Mint goes only to the Registry, and burns come only from the Registry or
the Adapter.

### Morpho authorization is all-or-nothing

Accounts authorize the Registry and the Adapter, which lets those contracts do anything on the account's behalf. They
therefore call only:

- Registry: `supplyCollateral` and `withdrawCollateral` (the latter only inside the account's own `requestRelease`, to
  the Registry);
- Adapter: `liquidate`, `repay` and `withdrawCollateral` (the latter only for a valid signed CloseRequest whose
  executor is the caller, I8).

Neither has a code path that borrows or withdraws loan assets. The flow tests assert that Morpho's `borrow` and
`withdraw` are never called after the account's own borrow.

### Rounding and amounts

- **Close.** The close repays by shares (`repay(0, allShares)`, read in the same call), so no dust shares remain; Morpho
  rounds the assets up. The executor's USDC is pulled inside `onMorphoRepay` for exactly the reported amount.
- **Market maker amount.** `mmAmount = ceil(repaidAssets × (10,000 + marginBps) × 1e36 / (10,000 × price))`, computed
  with a 512-bit `Math.mulDiv`. The borrower gets `outstanding − mmAmount`, which must be at least `minUserAmount`.
- **Liquidation.** `repaidShares` is capped at the borrower's current shares, so a front-running 1-share repay cannot
  make Morpho underflow. D11: seized ≥ 100,000 zat, or seized = the vault's whole remaining outstanding. The
  remainder has no floor: a sub-floor remainder stays with the borrower (dust in the vault if below the payout fee).
- **Claims.** `claimId = keccak256(abi.encode(vaultId, sequence))`, with a per-vault sequence starting at 0. A close
  creates the user claim, then the market maker claim.

### Signatures

- **Signer signatures.** Signatures are 65-byte ECDSA, checked with OpenZeppelin ECDSA, which rejects high-s and bad v.
  Every signature must recover to a signer, and the signers must be strictly increasing, so a signer can never count
  twice (I6). Three to five signatures are accepted. Any invalid extra signature fails the call, so the coordinator
  must submit only valid ones.
- **CloseRequest.** The CloseRequest is checked with `SignatureChecker`: ERC-1271 for a deployed smart account, ECDSA
  for an address without code. ERC-6492 is not unwrapped, so requests must be signed after the account is deployed.
  Safe accounts sign with the SafeMessage recipe of SPIKE_C (tested against a real Safe v1.4.1 on the fork).

### Selectors and topics (for the paymaster proxy and the signers)

| | |
|---|---|
| `requestRelease(bytes32,bytes32)` | `0xf55cc22b` |
| `invalidateNonce(uint256)` | `0xb70e36f0` |
| `attestDeposit((bytes32,address,uint64,bytes32),bytes[])` | `0x23f77140` |
| `confirmPayout(bytes32,bytes32[],bytes[])` | `0x0722ba58` |
| `liquidate(address,uint256,uint256,bytes32)` | `0x7d9d7046` |
| `closeWithCollateral((bytes32,address,address,uint16,uint64,bytes32,uint256,uint64),bytes,bytes32)` | `0x0581902b` |
| `VaultActivated` | `0x714d0c4b5fa075b8dc145a5380ca53b724512932500b23033f093977089817fe` |
| `ReleaseRequested` | `0xb545a19ffe4f361b085b12413c2adebdf1cf42c1e908e27d2174a0736d01f8c3` |
| `LiquidationRelease` | `0x597af40951130e256f7211bb6b00dd3ce4ba748ccf6d0815c98d4655ab9dec90` |
| `CloseRelease` | `0x34f19c7b5dc2ce9f03bddb38a2a8b8114849f6a9b79f45a9d2f827c22e28c67d` |
| `PayoutConfirmed` | `0xa0cc9d518f5e0d84deafe314d887ba12dbc82c2e8c18cbd08439944b2b1ca97d` |
| `NonceInvalidated` | `0x1800cd2301fbc20790ed94f3d55a28ef2306a9c31cd3c72b5b71b6e4cf5c6241` |

### Beyond docs/INTERFACES.md

Every function, event and type string of INTERFACES.md § 2–3 is implemented exactly as specified. The additions are
views (`THRESHOLD`, `morpho`, `zzec`, `adapter`, `marketId`, `depositTagUsed`, `claimCount`, `claimIdOf`,
`hashDepositAttestation`, `hashPayoutConfirmation`, `hashCloseRequest`, `domainSeparator`, ERC-5267 `eip712Domain`,
`MIN_SEIZE`, `MAX_MARGIN_BPS`, `pendingExpectation`) and custom errors. The rules below are stricter than the spec
text:

- Zero vault ids, accounts, amounts, deposit tags and commitments are rejected.
- An account can never get a second vault, even after its vault is closed (I4).
- `closeWithCollateral` requires `req.vaultId` to be the account's vault, an active vault and existing debt.

Where the spec is silent: `invalidateNonce` is idempotent (it never reverts, also for a nonce that is already used or
invalidated, and emits `NonceInvalidated` on every call). The D11 rule follows docs/DECISIONS.md (2026-10-11): seized ≥
`MIN_SEIZE` or the whole remaining outstanding, with no floor on the remainder.

## ABIs

`abi/*.json` hold the bare ABI arrays of ZZEC, ZecuredRegistry, SettlementAdapter, MockUSDC and MockZecOracle for the
app and the off-chain services. After any change to an external interface, regenerate them with
`script/export-abis.sh` (it builds first) and commit them.

## Known gaps

- The Adapter's `nonReentrant` guard is not exercised by any test. Reaching it needs a loan token that calls back during
  transfers, i.e. another market. With MockUSDC and the real Morpho, no untrusted code with state access runs inside
  `liquidate` or `closeWithCollateral` (the ERC-1271 check is a staticcall).

## Mocks

`MockUSDC` and `MockZecOracle` are MOCKS: owner-controlled testnet stand-ins, also listed in the root README under
"What is real and what is a mock". Everything under `test/utils` is test-only tooling.
