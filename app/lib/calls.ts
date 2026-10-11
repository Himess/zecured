/**
 * The Base calls a loan account makes, as batches for one sponsored UserOp each, plus a
 * client-side mirror of the paymaster allowlist (docs/INTERFACES.md § 4.1, made precise in
 * docs/spikes/SPIKE_C.md). The proxy enforces the allowlist; the app refuses to even ask for
 * anything else, so a bug here fails loudly in the browser instead of at the paymaster.
 */
import { decodeErrorResult, decodeFunctionData, encodeFunctionData, getAddress, type Address, type Hex } from "viem";
import { adapterAbi, erc20Abi, morphoAbi, registryAbi, registryErrorsAbi } from "./abi";
import { config as defaultConfig, marketParams, type AppConfig, type MarketParams } from "./config";
import { formatUsdc, MIN_RELEASE } from "./morpho";
import { formatTaz } from "./zcash";

export interface Call {
  to: Address;
  data: Hex;
  /** Always 0: the account never holds or sends ETH. */
  value?: 0n;
}

const EMPTY: Hex = "0x";

function setAuthorization(c: AppConfig, authorized: Address): Call {
  return {
    to: c.morpho,
    data: encodeFunctionData({ abi: morphoAbi, functionName: "setAuthorization", args: [authorized, true] }),
  };
}

/**
 * First borrow (or a later one): authorize the Registry and the Adapter on Morpho unless
 * already authorized (Morpho reverts on re-setting), then borrow `assets` USDC units to
 * `receiver` (the account itself by default).
 */
export function borrowCalls(p: {
  account: Address;
  assets: bigint;
  receiver?: Address;
  registryAuthorized: boolean;
  adapterAuthorized: boolean;
  config?: AppConfig;
}): Call[] {
  const c = p.config ?? defaultConfig;
  if (p.assets <= 0n) throw new Error("borrow amount must be positive");
  const calls: Call[] = [];
  if (!p.registryAuthorized) calls.push(setAuthorization(c, c.registry));
  if (!p.adapterAuthorized) calls.push(setAuthorization(c, c.adapter));
  calls.push({
    to: c.morpho,
    data: encodeFunctionData({
      abi: morphoAbi,
      functionName: "borrow",
      args: [marketParams(c), p.assets, 0n, p.account, p.receiver ?? p.account],
    }),
  });
  return calls;
}

/** Releases the remaining collateral (no debt left) to the pre-validated destination. */
export function releaseCalls(p: {
  vaultId: Hex;
  commitment: Hex;
  registryAuthorized: boolean;
  config?: AppConfig;
}): Call[] {
  const c = p.config ?? defaultConfig;
  const calls: Call[] = [];
  if (!p.registryAuthorized) calls.push(setAuthorization(c, c.registry));
  calls.push({
    to: c.registry,
    data: encodeFunctionData({ abi: registryAbi, functionName: "requestRelease", args: [p.vaultId, p.commitment] }),
  });
  return calls;
}

const UINT64_MAX = 2n ** 64n - 1n;

/**
 * D14: releases `amount` zatoshi of the collateral to the pre-validated destination, debt or
 * not; the vault stays active. Morpho's `withdrawCollateral` inside needs the Registry's
 * authorization, set first when missing. The Registry also requires at least MIN_RELEASE to
 * stay in the vault and the LTV after to be <= 50 % (lib/morpho.ts, `maxPartialRelease`).
 */
export function partialReleaseCalls(p: {
  vaultId: Hex;
  amount: bigint;
  commitment: Hex;
  registryAuthorized: boolean;
  config?: AppConfig;
}): Call[] {
  const c = p.config ?? defaultConfig;
  if (p.amount < MIN_RELEASE) throw new Error(`a partial release is at least ${formatTaz(MIN_RELEASE)}`);
  if (p.amount > UINT64_MAX) throw new Error("amount out of range");
  const calls: Call[] = [];
  if (!p.registryAuthorized) calls.push(setAuthorization(c, c.registry));
  calls.push({
    to: c.registry,
    data: encodeFunctionData({
      abi: registryAbi,
      functionName: "requestPartialRelease",
      args: [p.vaultId, p.amount, p.commitment],
    }),
  });
  return calls;
}

/**
 * Repays all debt by shares (never by assets: no dust shares) with USDC held by the account,
 * then releases the collateral. `approve` covers the expected assets plus a small buffer for
 * interest accrued before inclusion.
 */
export function repayAndReleaseCalls(p: {
  account: Address;
  borrowShares: bigint;
  approveAssets: bigint;
  vaultId: Hex;
  commitment: Hex;
  registryAuthorized: boolean;
  config?: AppConfig;
}): Call[] {
  const c = p.config ?? defaultConfig;
  if (p.borrowShares <= 0n) throw new Error("nothing to repay");
  return [
    { to: c.usdc, data: encodeFunctionData({ abi: erc20Abi, functionName: "approve", args: [c.morpho, p.approveAssets] }) },
    {
      to: c.morpho,
      data: encodeFunctionData({
        abi: morphoAbi,
        functionName: "repay",
        args: [marketParams(c), 0n, p.borrowShares, p.account, EMPTY],
      }),
    },
    ...releaseCalls({ vaultId: p.vaultId, commitment: p.commitment, registryAuthorized: p.registryAuthorized, config: c }),
  ];
}

/** Sends USDC held by the account (allowlisted: MockUSDC.transfer to anyone). */
export function transferUsdcCalls(p: { to: Address; amount: bigint; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  if (p.amount <= 0n) throw new Error("amount must be positive");
  return [{ to: c.usdc, data: encodeFunctionData({ abi: erc20Abi, functionName: "transfer", args: [p.to, p.amount] }) }];
}

/** Cancels a signed CloseRequest (SettlementAdapter.invalidateNonce). */
export function invalidateNonceCalls(p: { nonce: bigint; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  return [{ to: c.adapter, data: encodeFunctionData({ abi: adapterAbi, functionName: "invalidateNonce", args: [p.nonce] }) }];
}

/** Authorizes the Adapter alone (needed before a close if the first UserOp did not). */
export function authorizeAdapterCalls(p: { config?: AppConfig } = {}): Call[] {
  const c = p.config ?? defaultConfig;
  return [setAuthorization(c, c.adapter)];
}

// ------------------------------------------------------------------ allowlist mirror

export class NotAllowlistedError extends Error {
  constructor(message: string) {
    super(`Not sponsorable: ${message}`);
    this.name = "NotAllowlistedError";
  }
}

/** decodeFunctionData that reports unknown selectors as not allowlisted. */
function decode<const abi extends readonly unknown[]>(abi: abi, data: Hex, contract: string) {
  try {
    return decodeFunctionData({ abi: abi as never, data }) as ReturnType<typeof decodeFunctionData<abi>>;
  } catch {
    throw new NotAllowlistedError(`unknown ${contract} function`);
  }
}

function sameMarket(a: MarketParams, b: MarketParams): boolean {
  return (
    getAddress(a.loanToken) === getAddress(b.loanToken) &&
    getAddress(a.collateralToken) === getAddress(b.collateralToken) &&
    getAddress(a.oracle) === getAddress(b.oracle) &&
    getAddress(a.irm) === getAddress(b.irm) &&
    a.lltv === b.lltv
  );
}

/**
 * Throws `NotAllowlistedError` unless every call is on the paymaster allowlist:
 * Registry.requestRelease / requestPartialRelease; Adapter.invalidateNonce;
 * Morpho.setAuthorization (Registry or Adapter only), borrow / repay (Zecured market,
 * onBehalf = account, no callback data); MockUSDC.approve (spender = Morpho) and transfer.
 * No ETH value anywhere. Mirrors backend/src/allowlist.rs.
 */
export function assertAllowlisted(calls: readonly Call[], account: Address, c: AppConfig = defaultConfig): void {
  if (calls.length === 0) throw new NotAllowlistedError("empty batch");
  const me = getAddress(account);
  for (const call of calls) {
    if (call.value !== undefined && call.value !== 0n) throw new NotAllowlistedError("ETH value");
    const to = getAddress(call.to);
    if (to === getAddress(c.registry)) {
      const d = decode(registryAbi, call.data, "Registry");
      if (d.functionName !== "requestRelease" && d.functionName !== "requestPartialRelease") {
        throw new NotAllowlistedError(`Registry.${d.functionName}`);
      }
    } else if (to === getAddress(c.adapter)) {
      const d = decode(adapterAbi, call.data, "Adapter");
      if (d.functionName !== "invalidateNonce") throw new NotAllowlistedError(`Adapter.${d.functionName}`);
    } else if (to === getAddress(c.morpho)) {
      const d = decode(morphoAbi, call.data, "Morpho");
      if (d.functionName === "setAuthorization") {
        const target = getAddress(d.args[0]);
        if (target !== getAddress(c.registry) && target !== getAddress(c.adapter)) {
          throw new NotAllowlistedError("setAuthorization for a contract other than the Registry or the Adapter");
        }
      } else if (d.functionName === "borrow" || d.functionName === "repay") {
        if (!sameMarket(d.args[0], marketParams(c))) throw new NotAllowlistedError(`${d.functionName} on another market`);
        if (getAddress(d.args[3]) !== me) throw new NotAllowlistedError(`${d.functionName} on behalf of another account`);
        if (d.functionName === "repay" && d.args[4] !== EMPTY) throw new NotAllowlistedError("repay with callback data");
      } else {
        throw new NotAllowlistedError(`Morpho.${d.functionName}`);
      }
    } else if (to === getAddress(c.usdc)) {
      const d = decode(erc20Abi, call.data, "USDC");
      if (d.functionName === "approve") {
        if (getAddress(d.args[0]) !== getAddress(c.morpho)) throw new NotAllowlistedError("USDC approve to a spender other than Morpho");
      } else if (d.functionName !== "transfer") {
        throw new NotAllowlistedError(`USDC.${d.functionName}`);
      }
    } else {
      throw new NotAllowlistedError(`call to ${to}`);
    }
  }
}

// ------------------------------------------------------------------ reverts

/** Hex strings in an error and its causes: viem nests the bundler's revert data in them. */
function hexCandidates(e: unknown): Hex[] {
  const found: Hex[] = [];
  const seen = new Set<unknown>();
  for (let cur = e; cur && typeof cur === "object" && !seen.has(cur); cur = (cur as { cause?: unknown }).cause) {
    seen.add(cur);
    const o = cur as Record<string, unknown>;
    const data = o.data && typeof o.data === "object" ? (o.data as Record<string, unknown>) : null;
    for (const field of [o.data, data?.revertData, data?.data, o.details, o.shortMessage, o.message]) {
      if (typeof field === "string") for (const m of field.matchAll(/0x[0-9a-fA-F]{8,}/g)) found.push(m[0] as Hex);
    }
  }
  return found;
}

/**
 * A user-facing explanation of a Registry revert (the D14 limits, the vault's state) or of
 * Morpho's own checks, found anywhere in `e` (e.g. the bundler's simulation error); null if
 * there is none. viem cannot decode these: calls go out without their ABI.
 */
export function describeRevert(e: unknown): string | null {
  for (const data of hexCandidates(e)) {
    let decoded: { errorName: string; args?: readonly unknown[] };
    try {
      decoded = decodeErrorResult({ abi: registryErrorsAbi, data });
    } catch {
      continue;
    }
    const a = decoded.args ?? [];
    const n = (i: number) => (typeof a[i] === "bigint" ? (a[i] as bigint) : 0n);
    switch (decoded.errorName) {
      case "ReleaseAmountTooSmall":
        return `The Registry refused the withdrawal: it must be at least ${formatTaz(n(1))}.`;
      case "ReleaseAmountNotPartial":
        return `The Registry refused the withdrawal: it must be smaller than the collateral (${formatTaz(n(1))}). To take everything, repay and release instead.`;
      case "ReleaseRemainderTooSmall":
        return `The Registry refused the withdrawal: at least ${formatTaz(n(1))} must stay in the vault (this would leave ${formatTaz(n(0))}).`;
      case "ReleaseLtvTooHigh":
        return `The Registry refused the withdrawal: it would leave ${formatUsdc(n(0))} USDC of debt against ${formatUsdc(n(1))} USDC of collateral, above the 50 % limit. The price or the interest moved: try a smaller amount.`;
      case "VaultNotActive":
        return "The Registry refused: this vault is not active on Base.";
      case "NotVaultAccount":
        return "The Registry refused: only the vault's own account can do this.";
      case "ZeroCommitment":
        return "The Registry refused: the destination commitment is missing.";
      case "DebtOutstanding":
        return "The Registry refused: debt remains, so the whole collateral cannot be released yet.";
      case "CollateralMismatch":
        return "The Registry refused: the Morpho position and the vault disagree. Nothing was changed.";
      case "Error":
        if (a[0] === "insufficient collateral") {
          return "Morpho refused the withdrawal: the position would no longer be healthy. Try a smaller amount.";
        }
        if (a[0] === "unauthorized") {
          return "Morpho refused the withdrawal: the Registry is not authorized for this account yet. Refresh and try again.";
        }
        break;
    }
  }
  return null;
}
