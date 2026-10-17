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
 * R-11.1: the signers' scan found the deposit, but its proof cannot land while the coverage cap is
 * full (the Intake reverts it rather than cancel the vault), so Base has nothing yet: no pending
 * activation, the vault unknown there. The deposit waits and is proven once there is room; or its
 * owner has it proven as a refund instead (`POST /vaults/:id/prove-refund`).
 */
export function capBlocksActivation(p: {
  /** GET /vaults/:id `status` (the coordinator's view). */
  vaultStatus: string | undefined;
  /** The Registry's view of the vault, when read. */
  baseStatus: OnchainVaultStatus | undefined;
  /** Base has a pending activation for the vault. */
  pending: boolean;
  /** GET /status `minting.mintBlock[amount]`. */
  mintBlock: MintBlock | null | undefined;
}): boolean {
  return (
    p.vaultStatus === "deposit_seen" &&
    !p.pending &&
    (p.baseStatus === undefined || p.baseStatus === "None") &&
    p.mintBlock === "CapExceeded"
  );
}

export interface Refusal {
  title: string;
  body: string;
  /** Opening a vault is closed for everyone for now: say that exits and payouts are not affected. */
  openingsClosed: boolean;
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
