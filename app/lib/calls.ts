/**
 * The Base calls a loan account makes, as batches for one sponsored UserOp each, plus a
 * client-side mirror of the paymaster allowlist (docs/INTERFACES.md § 4.1, made precise in
 * docs/spikes/SPIKE_C.md). The proxy enforces the allowlist; the app refuses to even ask for
 * anything else, so a bug here fails loudly in the browser instead of at the paymaster.
 */
import { decodeErrorResult, decodeFunctionData, encodeFunctionData, getAddress, type Address, type Hex } from "viem";
import { adapterAbi, bondAbi, disputesAbi, erc20Abi, intakeAbi, morphoAbi, registryAbi, revertErrorsAbi } from "./abi";
import { config as defaultConfig, marketParams, ZERO_BYTES32, type AppConfig, type MarketParams } from "./config";
import { formatUsdc, MIN_RELEASE } from "./morpho";
import { formatTaz } from "./zcash";

/** The Registry's own functions and the dispute path its fallback serves (IZecuredDisputes). */
const registryCallsAbi = [...registryAbi, ...disputesAbi] as const;

/** "2026-10-17 14:05 UTC" (Unix seconds). */
export function formatUtc(seconds: bigint | number): string {
  return `${new Date(Number(seconds) * 1000).toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

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

/**
 * T-02: points the account's Refund claim at a destination the signers pre-validated
 * (`Registry.setRefundDestination`). Also revives a refund written off for want of a destination.
 */
export function refundDestinationCalls(p: { claimId: Hex; commitment: Hex; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  if (p.commitment.toLowerCase() === ZERO_BYTES32) throw new Error("a refund destination cannot be zero");
  return [
    {
      to: c.registry,
      data: encodeFunctionData({ abi: registryAbi, functionName: "setRefundDestination", args: [p.claimId, p.commitment] }),
    },
  ];
}

/**
 * T-03: converts the account's own pending activation into a Refund claim
 * (`Intake.refundPending`): once a pause by signatures has lasted `pendingRefundDelay`, or after
 * a halt.
 */
export function refundPendingCalls(p: { vaultId: Hex; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  return [{ to: c.intake, data: encodeFunctionData({ abi: intakeAbi, functionName: "refundPending", args: [p.vaultId] }) }];
}

/**
 * T-05 (a): disputes the signers' confirmation that the account's own claim was paid
 * (`disputePayout`, at the Registry's address: its fallback serves IZecuredDisputes).
 */
export function disputePayoutCalls(p: { claimId: Hex; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  return [{ to: c.registry, data: encodeFunctionData({ abi: disputesAbi, functionName: "disputePayout", args: [p.claimId] }) }];
}

/** T-05 (a): reopens a disputed claim whose payout nobody proved in time (`reopenDisputedClaim`). */
export function reopenDisputedClaimCalls(p: { claimId: Hex; config?: AppConfig }): Call[] {
  const c = p.config ?? defaultConfig;
  return [
    { to: c.registry, data: encodeFunctionData({ abi: disputesAbi, functionName: "reopenDisputedClaim", args: [p.claimId] }) },
  ];
}

/**
 * T-05: the signers' bonds pay the account's overdue claim in USDC (`SignerBond.slashLiveness`),
 * and the claim's ZEC then goes to the bonds instead. `bond` is the one the pinned Registry
 * names (`Links.bond`).
 */
export function slashLivenessCalls(p: { claimId: Hex; bond: Address }): Call[] {
  return [{ to: p.bond, data: encodeFunctionData({ abi: bondAbi, functionName: "slashLiveness", args: [p.claimId] }) }];
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

/** The contracts the pinned Registry names that the allowlist has calls for (lib/chain.ts `Links`). */
export interface NamedTargets {
  /** The signer bond: `slashLiveness` (null: unknown, so nothing to it is sponsorable). */
  bond: Address | null;
}

/** The pinned contracts the allowlist has calls for: a batch to none other needs no `NamedTargets`. */
export function pinnedTargets(c: AppConfig = defaultConfig): Address[] {
  return [c.registry, c.intake, c.adapter, c.morpho, c.usdc].map((a) => getAddress(a));
}

/**
 * Throws `NotAllowlistedError` unless every call is on the paymaster allowlist:
 * Registry.requestRelease / requestPartialRelease, setRefundDestination (non-zero commitment),
 * disputePayout / reopenDisputedClaim (served by the Registry's fallback); Intake.refundPending;
 * the bond's slashLiveness (each claim once); Adapter.invalidateNonce; Morpho.setAuthorization
 * (Registry or Adapter only), borrow / repay (Zecured market, onBehalf = account, no callback
 * data); the loan token's approve (spender = Morpho) and transfer. No ETH value anywhere. Mirrors
 * backend/src/allowlist.rs; what needs the chain (the claim is the account's own, the vault is
 * its own, the bonds can pay the claim now) only the backend checks.
 */
export function assertAllowlisted(
  calls: readonly Call[],
  account: Address,
  c: AppConfig = defaultConfig,
  named: NamedTargets = { bond: null },
): void {
  if (calls.length === 0) throw new NotAllowlistedError("empty batch");
  const me = getAddress(account);
  // A "bond" that is one of the pinned contracts never widens that contract's rules.
  const bond = named.bond !== null && !pinnedTargets(c).includes(getAddress(named.bond)) ? getAddress(named.bond) : null;
  const bondPaid = new Set<string>();
  for (const call of calls) {
    if (call.value !== undefined && call.value !== 0n) throw new NotAllowlistedError("ETH value");
    const to = getAddress(call.to);
    if (to === bond) {
      const d = decode(bondAbi, call.data, "SignerBond");
      if (d.functionName !== "slashLiveness") throw new NotAllowlistedError(`SignerBond.${d.functionName}`);
      const claimId = d.args[0].toLowerCase();
      if (bondPaid.has(claimId)) throw new NotAllowlistedError("the same claim paid from the bonds twice");
      bondPaid.add(claimId);
    } else if (to === getAddress(c.registry)) {
      const d = decode(registryCallsAbi, call.data, "Registry");
      if (d.functionName === "setRefundDestination") {
        if (d.args[1].toLowerCase() === ZERO_BYTES32) throw new NotAllowlistedError("a zero refund destination");
      } else if (
        d.functionName !== "requestRelease" &&
        d.functionName !== "requestPartialRelease" &&
        d.functionName !== "disputePayout" &&
        d.functionName !== "reopenDisputedClaim"
      ) {
        throw new NotAllowlistedError(`Registry.${d.functionName}`);
      }
    } else if (to === getAddress(c.intake)) {
      const d = decode(intakeAbi, call.data, "Intake");
      if (d.functionName !== "refundPending") throw new NotAllowlistedError(`Intake.${d.functionName}`);
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
 * A user-facing explanation of a Registry, dispute, Intake or bond revert (the D14 limits, the
 * vault's state, refunds, disputes, payments by the bonds) or of Morpho's own checks, found
 * anywhere in `e` (e.g. the bundler's simulation error); null if there is none. viem cannot
 * decode these: calls go out without their ABI.
 */
export function describeRevert(e: unknown): string | null {
  for (const data of hexCandidates(e)) {
    let decoded: { errorName: string; args?: readonly unknown[] };
    try {
      decoded = decodeErrorResult({ abi: revertErrorsAbi, data });
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
      case "NotARefund":
        return "The Registry refused: this claim is not a refund.";
      case "RefundBound":
        return "The Registry refused: this refund is already being paid (or the signers' bonds paid it), so its destination can no longer change.";
      case "VaultFrozen":
        return "The Registry refused: a theft case is open on this vault, so its exits wait until the case is dismissed or executed.";
      case "VaultCompromised":
      case "DisputeVaultCompromised":
        return "The Registry refused: this vault was compromised; the compensator settles its claims in USDC.";
      case "NotDisputable":
        return "The Registry refused the dispute: this claim is not a confirmed payout that can be disputed (its confirmation was upheld, or the signers' bonds paid it).";
      case "NotClaimant":
        return "The Registry refused: only the claim's own claimant can dispute it.";
      case "DisputeWindowClosed":
        return `The Registry refused the dispute: it could be disputed until ${formatUtc(n(1))}.`;
      case "NotDisputed":
        return "The Registry refused: this claim is not disputed (any more).";
      case "InclusionWindowOpen":
        return `The signers can still prove this payout until ${formatUtc(n(1))}: it can be reopened after that.`;
      case "NoPendingActivation":
        return "The Intake refused: this vault has no pending activation (any more).";
      case "NotRefundableYet":
        return "The Intake refused: the pending activation cannot be refunded yet (the pause has not lasted long enough, or the deposit's block left the relay's best chain).";
      case "NotPendingAccount":
        return "The Intake refused: while minting is only paused, only the vault's own account can refund its pending activation.";
      case "NotTheClaimant":
        return "The bonds refused: only the claim's own claimant can have them pay it.";
      case "NotLivenessEligible":
        return "The bonds refused: they cannot pay this claim (it was paid, written off or already paid by the bonds, or a theft case froze its vault).";
      case "LivenessNotDue":
        return `The bonds can pay this claim only from ${formatUtc(n(1))}.`;
      case "InsufficientBond":
        return `The bonds hold ${formatUsdc(n(1))} USDC right now, less than the ${formatUsdc(n(0))} USDC this claim needs. The claim stays payable on Zcash; try again later.`;
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
