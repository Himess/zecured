/**
 * DEMO keys of the replay, derived from public labels: they are worthless and public by design,
 * never used on any chain. The placeholder recording's five signers, owner, market maker, swap
 * maker and wallet are these keys' addresses; at run time the replay only uses the swap maker's,
 * to sign each fresh quote of the placeholder (lib/replay/rest.ts), and the dummy owner key the
 * replay's loan carries instead of the recorded loan's (lib/replay/loans.ts).
 */
import { keccak256, toBytes, type Hex } from "viem";

/** `keccak256("zecured-replay/1 DEMO " + label)`: a valid secp256k1 key for any label (with overwhelming probability). */
export function demoKey(label: string): Hex {
  return keccak256(toBytes(`zecured-replay/1 DEMO ${label}`));
}

/** A deterministic 32-byte value for the placeholder's ids, tags and commitments. */
export function demoBytes32(label: string): Hex {
  return keccak256(toBytes(`zecured-replay/1 placeholder ${label}`));
}

/**
 * The owner key the replay's loan carries: NOT the recorded loan's, which is never published. It
 * signs the replay's owner requests, which nothing checks; the loan's owner and account are the
 * recorded ones (lib/replay/loans.ts).
 */
export const REPLAY_OWNER_KEY: Hex = demoKey("owner key of the replay's loan (not the recorded loan's)");
