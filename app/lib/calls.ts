/**
 * The Base calls a loan account makes, as batches for one sponsored UserOp each, plus a
 * client-side mirror of the paymaster allowlist (docs/INTERFACES.md § 4.1, made precise in
 * docs/spikes/SPIKE_C.md). The proxy enforces the allowlist; the app refuses to even ask for
 * anything else, so a bug here fails loudly in the browser instead of at the paymaster.
 */
import { decodeFunctionData, encodeFunctionData, getAddress, type Address, type Hex } from "viem";
import { adapterAbi, erc20Abi, morphoAbi, registryAbi } from "./abi";
import { config as defaultConfig, marketParams, type AppConfig, type MarketParams } from "./config";

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
 * Registry.requestRelease; Adapter.invalidateNonce; Morpho.setAuthorization (Registry or
 * Adapter only), borrow / repay (Zecured market, onBehalf = account, no callback data);
 * MockUSDC.approve (spender = Morpho) and transfer. No ETH value anywhere.
 */
export function assertAllowlisted(calls: readonly Call[], account: Address, c: AppConfig = defaultConfig): void {
  if (calls.length === 0) throw new NotAllowlistedError("empty batch");
  const me = getAddress(account);
  for (const call of calls) {
    if (call.value !== undefined && call.value !== 0n) throw new NotAllowlistedError("ETH value");
    const to = getAddress(call.to);
    if (to === getAddress(c.registry)) {
      const d = decode(registryAbi, call.data, "Registry");
      if (d.functionName !== "requestRelease") throw new NotAllowlistedError(`Registry.${d.functionName}`);
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
