/**
 * What v3 adds to a loan, in plain words, as pure functions the pages render: the opening fee
 * (T-06), the minting gates (T-01 cap, T-03 pause, the halt and the bonds), a pending activation
 * (K3), refunds (T-02), payout disputes (T-05 (a)), the bonds paying an overdue claim (T-05), and
 * the compensation of a compromised vault (T-04). docs/DECISIONS.md D40 to D54 records the rules;
 * the contracts enforce them.
 */
import type { Address, Hex } from "viem";
import type { MintBlock, MintingStatus } from "./api";
import type { LedgerClaim, PendingActivationView, RefundReason, VaultLedger } from "./chain";
import { formatUtc } from "./calls";
import type { V3Params } from "./config";
import { formatTaz } from "./zcash";

/** The project's two standing claims about the pause and about compensation (docs/DECISIONS.md D45, D46). */
export const PAUSE_SENTENCE =
  "Three of five signers can pause new minting for up to 30 days at a time; withdrawals, repayments, liquidations and payouts never stop.";
export const COMPENSATION_SENTENCE =
  "Taking a vault's ZEC without a commitment is provable and slashes the signers' bonds; the bonds compensate the victims: liquidators and lenders before the borrower's own equity.";
/** What none of the minting gates ever stops. */
export const EXITS_NEVER_STOP = "None of this stops an exit: withdrawals, repayments, liquidations and payouts go on.";

const BPS = 10_000n;
const ZERO32 = `0x${"0".repeat(64)}`;

/** "about 24 h", "about 30 days" (seconds). */
export function formatPeriod(seconds: number): string {
  if (seconds < 90 * 60) return `${Math.max(1, Math.round(seconds / 60))} min`;
  if (seconds < 48 * 3600) return `${Math.round(seconds / 3600)} h`;
  return `${Math.round(seconds / 86_400)} days`;
}

// ------------------------------------------------------------------ T-06: the opening fee

/**
 * The opening fee the Registry charges when a deposit of `amount` zatoshi is activated, as
 * `ZecuredRegistry.activate` computes it: `amount * feeBps / 10,000`, rounded down, waived (0)
 * below `minFeeZat`. The vault's collateral is the deposit less the fee.
 */
export function openingFee(amount: bigint, p: Pick<V3Params, "feeBps" | "minFeeZat">): bigint {
  const fee = (amount * BigInt(p.feeBps)) / BPS;
  return fee < p.minFeeZat ? 0n : fee;
}

/** What to say about the fee before a deposit of `amount` (null when the deployment charges none). */
export function feeNotice(amount: bigint, p: V3Params | null): string | null {
  if (!p || p.feeBps === 0) return null;
  const fee = openingFee(amount, p);
  const rate = `${(p.feeBps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })} %`;
  if (fee === 0n) {
    return `No opening fee for ${formatTaz(amount)}: the ${rate} fee would be under ${formatTaz(p.minFeeZat)}, so it is waived.`;
  }
  return `Opening fee: ${formatTaz(fee)} (${rate} of the deposit), owed to the signers' treasury and paid out of the vault with its first payout. The vault's collateral is ${formatTaz(amount - fee)}.`;
}

// ------------------------------------------------------------------ the minting gates (banner)

export interface MintingNotice {
  tone: "warn" | "danger" | "info";
  title: string;
  body: string[];
}

/** The denominations whose proofs the cap holds back now (R-11: they wait), and whether that is all of them. */
function capFull(s: MintingStatus, denominations: readonly bigint[]): { all: boolean; some: bigint[] } {
  const exceeded = denominations.filter((d) => s.mintBlock.get(d) === "CapExceeded");
  return { all: denominations.length > 0 && exceeded.length === denominations.length, some: exceeded };
}

/**
 * The minting banner: what holds new deposits now (halted, paused, bonds short, the coverage cap
 * full, its price stale), with the standing sentences where they fit. Empty when nothing does.
 */
export function mintingNotices(s: MintingStatus, denominations: readonly bigint[], nowSeconds: number): MintingNotice[] {
  const out: MintingNotice[] = [];
  if (s.halted) {
    out.push({
      tone: "danger",
      title: "New minting has stopped for good",
      body: [
        "A theft was proven on Base: the signers' bonds were slashed and minting halted. A deposit proven from now on lands as a refund, whose destination you give, and a deposit waiting to be activated can be refunded.",
        COMPENSATION_SENTENCE,
        EXITS_NEVER_STOP,
      ],
    });
    return out;
  }
  if (s.paused) {
    const bySignatures = s.pausedUntil > nowSeconds;
    out.push({
      tone: "warn",
      title: bySignatures ? `New minting is paused until ${formatUtc(s.pausedUntil)}` : "New minting is paused while a theft case is open",
      body: [
        bySignatures
          ? "Three signers paused it. A deposit proven meanwhile waits as a pending activation and is activated once the pause ends; if the pause lasts long enough, its account can turn it into a refund instead."
          : "A theft case is open on Base, which pauses new minting on this deployment until it is decided. A deposit proven meanwhile waits as a pending activation.",
        PAUSE_SENTENCE,
      ],
    });
  }
  if (!s.fullyBonded) {
    out.push({
      tone: "warn",
      title: "Not every signer holds its full bond",
      body: [
        "New deposits wait as pending activations until all five signers are bonded again: the bonds are what pays for a theft.",
        EXITS_NEVER_STOP,
      ],
    });
  }
  const cap = capFull(s, denominations);
  if (cap.all) {
    out.push({
      tone: "warn",
      title: "The coverage cap is full: new deposits wait for room",
      body: [
        "New collateral is accepted only while the signers' bonds cover its value. A deposit made now is not lost: its proof waits until there is room, and zZEC is minted then; or you have it proven as a refund instead. Wait until there is room again before you open a vault.",
        EXITS_NEVER_STOP,
      ],
    });
  } else if (cap.some.length > 0) {
    out.push({
      tone: "info",
      title: `Room left under the coverage cap: ${formatTaz(s.capRoom)}`,
      body: [
        `A deposit of ${cap.some.map(formatTaz).join(" or ")} would wait for room now (or come back as a refund if you ask); the smaller sizes still fit.`,
      ],
    });
  }
  if (denominations.length > 0 && denominations.every((d) => s.mintBlock.get(d) === null)) {
    out.push({
      tone: "info",
      title: "The coverage cap's price is stale",
      body: [
        "Deposit proofs wait until the price is fresh again; nothing is lost, the proof is sent again. " + EXITS_NEVER_STOP,
      ],
    });
  }
  return out;
}

/** Whether to open a vault of `amount` now (the app opens none whose deposit would land as a refund, or wait for room). */
export interface OpenGate {
  allowed: boolean;
  /** What happens to a deposit proven now, or null when it would be activated as usual. */
  message: string | null;
}

export function openGate(s: MintingStatus | null, amount: bigint): OpenGate {
  if (!s) return { allowed: true, message: null };
  const block: MintBlock | null | undefined = s.mintBlock.get(amount);
  switch (block) {
    case "Halted":
      return { allowed: false, message: "Minting has stopped for good: a deposit proven now would only come back as a refund." };
    case "CapExceeded":
      // R-11: the proof of a new vault's deposit reverts while the cap has no room, and is sent again later.
      return {
        allowed: false,
        message: `The coverage cap has room for ${formatTaz(s.capRoom)} only: a deposit of ${formatTaz(amount)} made now would wait, unminted, until there is room (or come back as a refund if you ask). Try a smaller size, or later.`,
      };
    case "Paused":
      // A pause by signatures runs until `pausedUntil`; past it (as the backend read it), an open theft case pauses.
      return {
        allowed: true,
        message:
          s.pausedUntil > (s.readAt ?? 0)
            ? `New minting is paused until ${formatUtc(s.pausedUntil)}: your deposit would wait as a pending activation until then.`
            : "New minting is paused while a theft case is open: your deposit would wait as a pending activation.",
      };
    case "NotBonded":
      return { allowed: true, message: "Not every signer holds its full bond: your deposit would wait as a pending activation until they do." };
    case null:
      return { allowed: true, message: "The coverage cap's price is stale: your deposit proof would wait until it is fresh again." };
    default:
      return { allowed: true, message: null };
  }
}

// ------------------------------------------------------------------ K3 / T-03: a pending activation

export interface PendingState {
  /** The relay reached `readyHeight` and `readyTime` passed. */
  ready: boolean;
  /** Why it waits beyond readiness (empty when anyone can finalize it now). */
  waits: string[];
  /** The account can refund it now (T-03), or after a halt anyone can. */
  refundable: boolean;
  /** When the account can refund it, if a pause by signatures runs (Unix seconds), else null. */
  refundableAt: number | null;
  /** The deposit's anchor left the relay's best chain: it is dropped and proven again. */
  dropped: boolean;
}

/** See `PendingState`, as `ZecuredIntake.finalizeActivation` and `refundPending` decide. */
export function pendingState(p: PendingActivationView, pendingRefundDelay: number, nowSeconds: number): PendingState {
  const ready = p.relayTip >= p.readyHeight && nowSeconds >= p.readyTime;
  const waits: string[] = [];
  if (!p.anchorOnBestChain) {
    return { ready, waits: ["The Zcash block the proof is anchored to left the relay's best chain."], refundable: false, refundableAt: null, dropped: true };
  }
  if (p.openCases > 0n) waits.push("A violation case is open on this vault: it waits until the case is decided.");
  if (p.halted) waits.push("Minting has stopped for good: this activation can only be refunded.");
  if (!p.fullyBonded) waits.push("Not every signer holds its full bond.");
  if (p.paused) {
    waits.push(
      p.pausedUntil > nowSeconds ? `New minting is paused until ${formatUtc(p.pausedUntil)}.` : "New minting is paused while a theft case is open.",
    );
  }
  let refundable = p.halted;
  let refundableAt: number | null = null;
  if (!p.halted && p.pausedUntil > nowSeconds) {
    const at = p.pausedSince + pendingRefundDelay;
    if (nowSeconds >= at) refundable = true;
    else if (at < p.pausedUntil) refundableAt = at;
  }
  return { ready, waits, refundable, refundableAt, dropped: false };
}

// ------------------------------------------------------------------ T-02: refunds

/** Why a deposit landed as a refund (the Intake's `RefundReason`), in plain words. */
export const REFUND_REASON_TEXT: Record<RefundReason, string> = {
  ModeRefund: "It did not match the vault's deposit rule (a different amount, after the deposit deadline, or not one Ironwood payment), so it was proven as a refund.",
  VaultNotNew: "The vault already had its deposit: this was a second payment.",
  PendingExists: "Another deposit of this vault was already waiting to be activated.",
  AccountHasVault: "The loan account already had another vault.",
  Halted: "New minting had stopped for good (a theft was proven and the signers' bonds were slashed).",
  CapExceeded: "The coverage cap was full: the signers' bonds did not cover more collateral at that moment.",
  PendingConverted: "It waited as a pending activation and was turned into a refund (after a long pause, or a halt).",
};

/** A Refund claim that needs the account's destination (none given yet, or abandoned). */
export function needsRefundDestination(c: LedgerClaim): boolean {
  if (c.kind !== "Refund" || c.bondPaid) return false;
  if (c.status === "Pending") return c.commitment.toLowerCase() === ZERO32;
  return c.status === "WrittenOff";
}

/** When a Refund claim without a destination can be written off (Unix seconds); null otherwise. */
export function abandonableAt(c: LedgerClaim, refundAbandonPeriod: number): number | null {
  if (c.kind !== "Refund" || c.status !== "Pending" || c.bondPaid || c.commitment.toLowerCase() !== ZERO32) return null;
  return c.createdAt + refundAbandonPeriod;
}

// ------------------------------------------------------------------ claims

export const CLAIM_KIND_LABEL: Record<LedgerClaim["kind"], string> = {
  Release: "To you: your collateral",
  Liquidation: "Repaid debt (paid to a liquidator or market maker)",
  CloseUser: "To you: the rest of your ZEC",
  CloseMarketMaker: "To the market maker, for repaying your debt",
  Refund: "Refund of a deposit",
  Fee: "Opening fee, to the signers",
  Retired: "Retired by the compensator",
  Unknown: "Payout",
};

export interface ClaimView {
  label: string;
  badge: { tone: "info" | "ok" | "warn" | "danger" | "neutral" | "proof"; text: string };
  /** One plain sentence on what it means and what happens next. */
  detail: string;
}

/**
 * A claim of the vault, in plain words. Section 3 of the v4 design: a pending claim whose payout
 * the signers committed on Base is "Sent"; one they confirmed paid is "Signers confirmed:
 * disputable until …" (`disputeWindow` after the confirmation, when known); a final one (`paidAt`
 * 0: its delivery proven on Base, or its confirmation undisputed through the window) is "Final on
 * Base ✓". R-11: a Refund claim without a destination may be set aside as dormant.
 */
export function claimView(
  c: LedgerClaim,
  ctx: { refundAbandonPeriod: number; nowSeconds: number; disputeWindow?: number; refundDormancy?: number | null },
): ClaimView {
  const label = CLAIM_KIND_LABEL[c.kind];
  if (c.kind === "Retired") {
    return {
      label,
      badge: { tone: "neutral", text: "Compensated" },
      detail: "Bookkeeping only: the compensator repaid this vault's lenders in USDC and burned this much zZEC. It is never paid on Zcash.",
    };
  }
  if (c.bondPaid) {
    return {
      label,
      badge: { tone: "ok", text: "Paid from the bonds" },
      detail:
        "The payout was overdue, so the signers' bonds paid its claimant its value plus a premium in USDC. Its ZEC now reimburses the bonds.",
    };
  }
  switch (c.status) {
    case "Pending":
      if (c.kind === "Refund" && c.commitment.toLowerCase() === ZERO32) {
        const at = abandonableAt(c, ctx.refundAbandonPeriod);
        const writeOff = at !== null ? ` Without one, it can be written off after ${formatUtc(at)} (you can revive it later).` : "";
        if (c.dormant) {
          return {
            label,
            badge: { tone: "warn", text: "Dormant: needs your destination" },
            detail: `No destination was given in time, so it was set aside: it no longer counts against the coverage cap, and it is still yours. Giving it an Ironwood destination wakes it, and it is paid.${writeOff}`,
          };
        }
        const dormantAt = ctx.refundDormancy ? c.createdAt + ctx.refundDormancy : null;
        return {
          label,
          badge: { tone: "warn", text: "Needs your destination" },
          detail: `Give it an Ironwood destination to be paid.${dormantAt !== null ? ` Without one, it is set aside as dormant after ${formatUtc(dormantAt)} (still yours, outside the coverage cap).` : ""}${writeOff}`,
        };
      }
      if (c.kind === "Fee") {
        return { label, badge: { tone: "neutral", text: "Pending" }, detail: "Paid out of the vault with its next payout, to the signers' treasury." };
      }
      if (c.committed) {
        return {
          label,
          badge: { tone: "info", text: "Sent" },
          detail: "The signers committed its payout on Base and are sending it on Zcash. It is final once its delivery is proven on Base.",
        };
      }
      return { label, badge: { tone: "warn", text: "Pending" }, detail: "The signers pay it in their payout queue." };
    case "Paid":
      // `paidAt` 0: a proof of the payout's inclusion, or `expireConfirmation` once a confirmation
      // stood undisputed through its window (R-3); Base keeps no trace of which in the claim itself.
      if (c.paidAt === 0) {
        return {
          label,
          badge: { tone: "proof", text: "Final on Base" },
          detail: "This claim is final: its delivery is proven on Base in zero knowledge, or its confirmation stood undisputed through the dispute window.",
        };
      }
      if (ctx.disputeWindow !== undefined && ctx.nowSeconds <= c.paidAt + ctx.disputeWindow) {
        return {
          label,
          badge: { tone: "info", text: `Signers confirmed: disputable until ${formatUtc(c.paidAt + ctx.disputeWindow)}` },
          detail:
            "The signers confirmed on Base that they paid it on Zcash. The coordinator proves every payout's delivery on Base; until then, its claimant can dispute the confirmation.",
        };
      }
      return {
        label,
        badge: { tone: "ok", text: "Signers confirmed" },
        detail: "Confirmed paid on Zcash by the signers; its delivery proof has not reached Base yet.",
      };
    case "WrittenOff":
      return c.kind === "Refund"
        ? {
            label,
            badge: { tone: "warn", text: "Abandoned" },
            detail: "No destination was given in time, so it was written off. Its ZEC is still in the vault: give a destination to revive it.",
          }
        : { label, badge: { tone: "neutral", text: "Written off" }, detail: "Too small to bear a payout's network fee (dust)." };
    case "Compensated":
      return {
        label,
        badge: { tone: "info", text: "Compensated" },
        detail: "This vault was compromised: the compensator pays this claim in USDC instead.",
      };
    case "Disputed":
      return {
        label,
        badge: { tone: "warn", text: "Disputed" },
        detail:
          c.disputeDueAt > ctx.nowSeconds
            ? `The confirmation is disputed. The signers have until ${formatUtc(c.disputeDueAt)} to prove the payout is in a Zcash block; otherwise the claim can be reopened and paid again.`
            : "Nobody proved the payout in time: the claim can be reopened, and is then paid again (or from the bonds once overdue).",
      };
    default:
      return { label, badge: { tone: "neutral", text: c.status }, detail: "" };
  }
}

// ------------------------------------------------------------------ T-05 (a): disputes

export interface DisputeState {
  /** "My payout did not arrive": the claim's confirmation can be disputed now, until `until`. */
  canDispute: boolean;
  until: number | null;
  /** The inclusion window passed without a proof: the claim can be reopened. */
  canReopen: boolean;
  /** While disputed: when the signers' time to prove the payout ends. */
  dueAt: number | null;
}

/**
 * What the claim's own claimant (`account`) can do about its confirmation, as
 * `IZecuredDisputes.disputePayout` and `reopenDisputedClaim` decide: dispute a claim confirmed
 * paid within `disputeWindow` of its confirmation (not one upheld, paid from the bonds, or of a
 * compromised vault); reopen a disputed one once its inclusion window passed.
 */
export function disputeState(
  c: LedgerClaim,
  ctx: { account: Address; disputeWindow: number; compromised: boolean; nowSeconds: number },
): DisputeState {
  const own = c.claimant.toLowerCase() === ctx.account.toLowerCase() && c.claimant !== `0x${"0".repeat(40)}`;
  if (!own || ctx.compromised) return { canDispute: false, until: null, canReopen: false, dueAt: null };
  if (c.status === "Paid" && c.paidAt > 0 && !c.bondPaid) {
    const until = c.paidAt + ctx.disputeWindow;
    return { canDispute: ctx.nowSeconds <= until, until, canReopen: false, dueAt: null };
  }
  if (c.status === "Disputed") {
    return { canDispute: false, until: null, canReopen: ctx.nowSeconds > c.disputeDueAt, dueAt: c.disputeDueAt };
  }
  return { canDispute: false, until: null, canReopen: false, dueAt: null };
}

// ------------------------------------------------------------------ T-05: the bonds pay an overdue claim

/** What the signers' bonds can do about one of the account's own unpaid claims. */
export type BondPayment =
  /** Nothing: not the account's claim, no destination yet, settled, already paid by the bonds, a frozen vault. */
  | { kind: "none" }
  /** Still within the signers' time: if it is unpaid at `dueAt` (Unix seconds), the bonds pay it instead. */
  | { kind: "later"; dueAt: number }
  /** Overdue: the bonds pay `usdc` (6 decimals) now, if they hold that much (`covered`). */
  | { kind: "now"; usdc: bigint; covered: boolean };

/**
 * See `BondPayment`, as `SignerBondV2.slashLiveness` decides: from the claim's `livenessDue`
 * (read for the account's own unpaid claims only), the chain's time and what the bonds hold
 * (`totalSlashable`, read when a claim is due; a payment beyond it reverts).
 */
export function bondPayment(c: LedgerClaim, ctx: { nowSeconds: number; slashable: bigint | null }): BondPayment {
  const l = c.liveness;
  if (!l || l.dueAt === 0 || c.status !== "Pending" || c.bondPaid) return { kind: "none" };
  if (l.dueAt > ctx.nowSeconds) return { kind: "later", dueAt: l.dueAt };
  if (l.usdc === 0n) return { kind: "none" };
  return { kind: "now", usdc: l.usdc, covered: ctx.slashable === null || ctx.slashable >= l.usdc };
}

/** What having the bonds pay a claim means (`premiumBps`: the deployment's `livenessPremiumBps`, if known). */
export function bondPaymentExplainer(premiumBps: number | null): string {
  const premium =
    premiumBps === null
      ? " plus a premium"
      : premiumBps > 0
        ? ` plus a ${(premiumBps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })} % premium`
        : "";
  return `The signers did not pay this claim in time, so their bonds can pay you instead: its value in USDC at the current price${premium}, sent to this loan account. In exchange the claim's ZEC no longer comes to you: when the signers pay it out, it goes to the bonds instead, to reimburse them.`;
}

// ------------------------------------------------------------------ the ledger as a whole

/** The vault's own claims (its account is their claimant) that need a refund destination. */
export function refundsToClaim(l: VaultLedger): LedgerClaim[] {
  return l.claims.filter(needsRefundDestination);
}

/** The vault's Refund claims, newest first. */
export function refundClaims(l: VaultLedger): LedgerClaim[] {
  return l.claims.filter((c) => c.kind === "Refund").sort((a, b) => b.sequence - a.sequence);
}

/** The vault's Fee claim, if the deployment charged one. */
export function feeClaim(l: VaultLedger): LedgerClaim | undefined {
  return l.claims.find((c) => c.kind === "Fee");
}

/** The claims the compensator may owe something for (status Compensated, not Retired bookkeeping). */
export function compensatedClaimIds(l: VaultLedger): Hex[] {
  return l.claims.filter((c) => c.status === "Compensated" && c.kind !== "Retired").map((c) => c.claimId);
}
