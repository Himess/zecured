/**
 * R-5: liveness with a receipt. A signer node that holds and validated a claim's destination (the
 * receiver and the salt behind its commitment) signs
 *
 *   DestinationReceived(bytes32 claimId,bytes32 commitment,uint64 receivedAt)
 *
 * under SignerBondV2's EIP-712 domain ("ZecuredSignerBond", "1", chain id, the bond). The account
 * sends it to `SignerBondV2.armLiveness`, and only then does the claim's liveness clock run: an
 * unpaid claim becomes payable by the bonds `livenessPeriod` after `max(createdAt, receivedAt)`.
 * Before sending it, the app checks the receipt here, so a wrong one is never a sponsored UserOp:
 * the claim and its current commitment, a time not in the future, and a signature by one of the
 * five signers pinned in this build.
 */
import { recoverTypedDataAddress, type Address, type Hex } from "viem";
import type { LivenessReceipt } from "./api";
import { CHAIN_ID } from "./config";

export const BOND_DOMAIN_NAME = "ZecuredSignerBond";
export const BOND_DOMAIN_VERSION = "1";

/** The receipt's type (V4 freeze section 3). */
export const DESTINATION_RECEIVED_TYPES = {
  DestinationReceived: [
    { name: "claimId", type: "bytes32" },
    { name: "commitment", type: "bytes32" },
    { name: "receivedAt", type: "uint64" },
  ],
} as const;

export function bondDomain(bond: Address, chainId: number = CHAIN_ID) {
  return { name: BOND_DOMAIN_NAME, version: BOND_DOMAIN_VERSION, chainId, verifyingContract: bond } as const;
}

export function destinationReceivedTypedData(
  bond: Address,
  message: { claimId: Hex; commitment: Hex; receivedAt: bigint },
  chainId: number = CHAIN_ID,
) {
  return {
    domain: bondDomain(bond, chainId),
    types: DESTINATION_RECEIVED_TYPES,
    primaryType: "DestinationReceived",
    message,
  } as const;
}

export type ReceiptCheck = { ok: true } | { ok: false; reason: string };

/**
 * Whether `r` arms `claim` now: the same claim, its current (non-zero) commitment, `receivedAt` not
 * after the chain's time, and signed by `r.signer`, one of `signers` (the build's pinned set).
 */
export async function checkReceipt(
  r: LivenessReceipt,
  claim: { claimId: Hex; commitment: Hex },
  ctx: { bond: Address; signers: readonly Address[] | null; chainTime: number; chainId?: number },
): Promise<ReceiptCheck> {
  if (r.claimId.toLowerCase() !== claim.claimId.toLowerCase()) return { ok: false, reason: "the receipt is for another claim" };
  if (/^0x0{64}$/i.test(claim.commitment)) return { ok: false, reason: "the claim has no destination yet" };
  if (r.commitment.toLowerCase() !== claim.commitment.toLowerCase()) {
    return { ok: false, reason: "the receipt is for an earlier destination of this claim" };
  }
  if (r.receivedAt > BigInt(ctx.chainTime)) return { ok: false, reason: "the receipt is dated in the future" };
  if (!ctx.signers || ctx.signers.length === 0) return { ok: false, reason: "this build pins no signer set to check the receipt against" };
  if (!ctx.signers.some((s) => s.toLowerCase() === r.signer.toLowerCase())) {
    return { ok: false, reason: "the receipt is not from one of the five signers" };
  }
  let recovered: Address;
  try {
    recovered = await recoverTypedDataAddress({
      ...destinationReceivedTypedData(ctx.bond, { claimId: r.claimId, commitment: r.commitment, receivedAt: r.receivedAt }, ctx.chainId),
      signature: r.signature,
    });
  } catch {
    return { ok: false, reason: "the receipt's signature is unreadable" };
  }
  if (recovered.toLowerCase() !== r.signer.toLowerCase()) return { ok: false, reason: "the receipt's signature is not its signer's" };
  return { ok: true };
}

// ------------------------------------------------------------------ which claims to arm

/** The claim kinds whose claimant is the vault's own account and that the bonds pay (R-5, T-05). */
const OWN_LIVENESS_KINDS = new Set(["Release", "CloseUser", "Refund"]);

/** What the app knows about a claim to decide whether it arms it (lib/chain.ts `LedgerClaim`). */
export interface ArmableClaim {
  claimId: Hex;
  kind: string;
  status: string;
  commitment: Hex;
  bondPaid: boolean;
  claimant: Address;
  /** `livenessDue(claimId)`: null while the bonds would not pay it now (unarmed, not eligible, a stale price). */
  liveness: { dueAt: number } | null;
  /** The bond's `livenessArming(claimId)`; null for a bond without armings (v3). */
  arming: { commitment: Hex; payableAt: number } | null;
}

export type ArmingState =
  /** Nothing to arm: not the account's, settled, no destination, paid by the bonds, a frozen vault, a v3 bond. */
  | { kind: "none" }
  /**
   * Armed for the claim's current destination since `payableAt`: the bonds pay it if it is still
   * unpaid at `dueAt` (null while they would not pay now, e.g. a stale price).
   */
  | { kind: "armed"; payableAt: number; dueAt: number | null }
  /** Not armed (or armed for an earlier destination): a signer's receipt is fetched and sent (one sponsored UserOp). */
  | { kind: "unarmed" };

export function armingState(c: ArmableClaim, ctx: { account: Address; frozen: boolean; compromised: boolean }): ArmingState {
  const own = c.claimant.toLowerCase() === ctx.account.toLowerCase() && !/^0x0{40}$/i.test(c.claimant);
  if (!own || ctx.frozen || ctx.compromised || !OWN_LIVENESS_KINDS.has(c.kind)) return { kind: "none" };
  if (c.status !== "Pending" || c.bondPaid || /^0x0{64}$/i.test(c.commitment)) return { kind: "none" };
  // A v3 bond has no receipts: its liveness runs from the claim's creation.
  if (c.arming === null) return { kind: "none" };
  const current = c.arming.payableAt > 0 && c.arming.commitment.toLowerCase() === c.commitment.toLowerCase();
  if (!current) return { kind: "unarmed" };
  return { kind: "armed", payableAt: c.arming.payableAt, dueAt: c.liveness?.dueAt ?? null };
}

/**
 * What to say about an armed claim: when the bonds pay it if it is still unpaid then, or, while
 * `livenessDue` is (0, 0) for it (track L-INTAKE: the price feed is stale, or the compensator owes
 * the claim; a frozen vault shows no arming at all), that they would pay nothing right now.
 */
export function armedNote(state: { payableAt: number; dueAt: number | null }, formatTime: (unixSeconds: number) => string): string {
  const head = `Bonds' guarantee armed (a signer confirmed the destination at ${formatTime(state.payableAt)})`;
  return state.dueAt !== null
    ? `${head}: if it is still unpaid at ${formatTime(state.dueAt)}, the signers' bonds pay you instead.`
    : `${head}. Right now the bonds would pay nothing (their price feed is stale, or the compensator owes this claim); it stays payable on Zcash.`;
}

// ------------------------------------------------------------------ what this browser already sent

const ARMED_KEY = "zecured:armed:v1";
const MAX_REMEMBERED = 256;

/** `claimId:commitment` pairs this browser armed (so it arms each destination once on its own). */
export function rememberedArmings(storage: Pick<Storage, "getItem"> | null): Set<string> {
  try {
    const raw = storage?.getItem(ARMED_KEY);
    const list = raw ? (JSON.parse(raw) as unknown) : [];
    return new Set(Array.isArray(list) ? list.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function rememberArming(storage: Pick<Storage, "getItem" | "setItem"> | null, claimId: Hex, commitment: Hex): void {
  try {
    const seen = [...rememberedArmings(storage), `${claimId.toLowerCase()}:${commitment.toLowerCase()}`];
    storage?.setItem(ARMED_KEY, JSON.stringify([...new Set(seen)].slice(-MAX_REMEMBERED)));
  } catch {
    // Storage unavailable: the app offers the arming again, which is harmless.
  }
}

export function wasArmedHere(seen: Set<string>, claimId: Hex, commitment: Hex): boolean {
  return seen.has(`${claimId.toLowerCase()}:${commitment.toLowerCase()}`);
}
