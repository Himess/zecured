/**
 * What v4 adds to the loan pages, as pure functions the components render: a deposit that waits for
 * room under the coverage cap (R-11.1), the backend's refusals of a vault opening (the mainnet
 * beta's invites, openings closed while payouts go on: P-8), and the app's fixed sentences.
 */
import type { MintBlock } from "./api";
import type { OnchainVaultStatus } from "./chain";

/** The privacy table's swap line (section 5.7 of the v4 design). */
export const SWAP_PRIVACY_SENTENCE = "On a swap your Base account is visible; where the ZEC goes is not.";

/** How minting works, said without any signer's word (section 9.3 of the v4 design). */
export const PROOF_MINTING_SENTENCE =
  "Once a zero-knowledge proof that your deposit is in a Zcash block reaches Base, zZEC is minted straight into your loan account's Morpho position: no signer vouches for it.";

/**
 * R-11.1: while the coverage cap has no room for a new vault's activation, the Intake reverts its
 * deposit proof rather than cancel the vault, so Base has nothing yet: no pending activation, the
 * vault unknown there. A deposit already made waits and is proven once there is room; or its owner
 * has it proven as a refund instead (`POST /vaults/:id/prove-refund`, which the coordinator accepts
 * only once it saw a confirmed deposit of the vault's amount). The coordinator's view shows the
 * vault still awaiting its deposit, so the app cannot tell whether one was made:
 * - `possible`: the cap blocks this vault's amount now and nothing is on Base;
 * - `confirmed`: the same, and the coordinator says a deposit was seen (`deposit_seen`, or a
 *   `depositSeen` flag a newer backend may add);
 * - `none`: nothing waits for the cap (also once the deposit was proven as a refund, R-12: it is a
 *   pending refund, then a Refund claim).
 */
export function capWait(p: {
  /** GET /vaults/:id `status` (the coordinator's view). */
  vaultStatus: string | undefined;
  /** A newer backend's explicit word that the deposit was seen. */
  depositSeen?: boolean;
  /** The Registry's view of the vault, when read. */
  baseStatus: OnchainVaultStatus | undefined;
  /** Base has a pending activation for the vault. */
  pending: boolean;
  /** Base has a pending refund or a Refund claim for the vault (R-12). */
  refunding?: boolean;
  /** GET /status `minting.mintBlock[amount]`. */
  mintBlock: MintBlock | null | undefined;
}): "none" | "possible" | "confirmed" {
  if (p.pending || p.refunding || (p.baseStatus !== undefined && p.baseStatus !== "None") || p.mintBlock !== "CapExceeded") return "none";
  if (p.vaultStatus === "deposit_seen" || p.depositSeen === true) return "confirmed";
  return p.vaultStatus === "awaiting_deposit" ? "possible" : "none";
}

/**
 * Whether the deposit step still asks for a payment (the address, its QR code, "Pay … from Ironwood"): the signers
 * have not seen the vault's conforming deposit (`depositSeen`, mined with its confirmations), and Base has neither its
 * pending activation nor a refund of it. Asking again once the deposit was seen invites a second payment, which only
 * comes back as a refund.
 */
export function awaitingDeposit(p: {
  /** GET /vaults/:id `status`; undefined while unread. */
  vaultStatus: string | undefined;
  depositSeen?: boolean;
  /** Base has a pending activation for the vault. */
  pending: boolean;
  /** Base has a pending refund or a Refund claim for the vault (R-12). */
  refunding: boolean;
}): boolean {
  const waiting = p.vaultStatus === undefined || p.vaultStatus === "awaiting_deposit" || p.vaultStatus === "unknown";
  return waiting && p.depositSeen !== true && !p.pending && !p.refunding;
}

/** P-8: what the operator's switch on new vaults means (C01: separate from existing vaults' payouts). */
export function openingsNotice(o: { open: boolean; inviteOnly: boolean } | null): { title: string; body: string } | null {
  if (!o) return null;
  if (!o.open) {
    return {
      title: "New vault openings are temporarily closed",
      body: "Existing loans are not affected: repayments, exits, liquidations and payouts go on as usual.",
    };
  }
  return o.inviteOnly
    ? { title: "Opening a vault needs an invite code", body: "New vaults open by invitation only; each code opens one vault." }
    : null;
}

export interface Refusal {
  title: string;
  body: string;
  /** Opening a vault is closed for everyone for now: say that exits and payouts are not affected. */
  openingsClosed: boolean;
}

/**
 * R-11.1: why the coordinator did not prove the deposit as a refund (`POST /vaults/:id/prove-refund`)
 * or relay a deposit proof (`cap_full`), track OFF's fixed codes, in plain words; null for others.
 */
export function refundProofRefusal(code: string | null): string | null {
  switch (code) {
    case "not_blocked":
      return "The coverage cap has room again: your deposit can still back the loan, so there is nothing to refund. It is proven and activated as usual.";
    case "no_deposit":
      return "The signers have not seen a confirmed deposit of the vault's amount yet. Wait for its confirmations, then try again.";
    case "already_landed":
      return "Your deposit is already on Base (proven, or waiting to be activated): it is no longer refunded this way.";
    case "refund_unavailable":
      return "This deployment cannot prove a deposit as a refund. Your deposit waits until there is room under the coverage cap.";
    case "cap_full":
      return "The coverage cap is full: the proof waits until there is room, or you ask for your deposit back instead.";
    case "not_configured":
      return "This backend does not make deposit proofs, so it cannot prove a refund either.";
    default:
      return null;
  }
}

/** The backend's fixed refusal codes of `POST /vaults` (track OFF's), in plain words; null for others. */
export function openRefusal(code: string | null): Refusal | null {
  switch (code) {
    case "invite_required":
      return {
        title: "An invite code is needed",
        body: "The mainnet beta opens vaults by invitation only. Enter the invite code you were given, then try again.",
        openingsClosed: false,
      };
    case "invite_used":
      return {
        title: "This invite code was already used",
        body: "Each invite code opens one vault. Ask for a new one if you need another vault.",
        openingsClosed: false,
      };
    case "openings_closed":
      return {
        title: "New vault openings are closed for now",
        body: "Nothing changes for existing loans: repayments, exits, liquidations and payouts go on as usual. Try again later.",
        openingsClosed: true,
      };
    case "busy":
      return {
        title: "Too many vaults are waiting for their deposit",
        body: "The signers keep a limited number of new vaults open at once. Try again in a few minutes.",
        openingsClosed: false,
      };
    default:
      return null;
  }
}
