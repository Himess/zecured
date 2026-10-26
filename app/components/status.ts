import type { Vault } from "@/lib/api";

/** Display label and tone of each vault status (INTERFACES § 4). */
export const STATUS_LABEL: Record<Vault["status"], { text: string; tone: "info" | "ok" | "warn" | "danger" | "neutral" }> = {
  awaiting_deposit: { text: "Awaiting deposit", tone: "info" },
  deposit_seen: { text: "Deposit seen", tone: "info" },
  active: { text: "Active", tone: "ok" },
  closing: { text: "Paying out", tone: "info" },
  closed: { text: "Closed", tone: "neutral" },
  expired: { text: "Expired", tone: "warn" },
  cancelled: { text: "Cancelled", tone: "neutral" },
  unknown: { text: "Unknown", tone: "neutral" },
};

/**
 * A vault's status as shown: a deposit the signers saw, its proof on the way, is no longer "Awaiting deposit" (E-01;
 * the coordinator keeps `awaiting_deposit` until the proof is on Base).
 */
export function statusOf(v: Pick<Vault, "status" | "depositSeen">): { text: string; tone: "info" | "ok" | "warn" | "danger" | "neutral" } {
  return v.status === "awaiting_deposit" && v.depositSeen ? { text: "Proving the deposit", tone: "info" } : STATUS_LABEL[v.status];
}
