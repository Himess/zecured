/**
 * Salted commitments to Zcash recipients (docs/INTERFACES.md § 1):
 *
 *     commitment = keccak256(salt32 || receiver43)
 *
 * where `receiver43` is the raw Orchard-protocol receiver of an Ironwood-only UA (D4) and
 * `salt32` is 32 random bytes known only to the borrower and the signers. The browser decodes
 * the UA itself (lib/ua.ts) so that it can compute and sign the commitment without trusting
 * the backend's decoding. Salts and addresses are never logged or stored by the app.
 */
import { bytesToHex, concatBytes, keccak256, type Hex } from "viem";
import { decodeUnifiedAddress, UnifiedAddressError, type DecodedUa, type ZcashNetwork } from "./ua";

export { decodeUnifiedAddress, isIronwoodOnly, UnifiedAddressError } from "./ua";
export type { DecodedUa } from "./ua";

export const SALT_BYTES = 32;
export const RECEIVER_BYTES = 43;

/** 32 bytes from the platform CSPRNG. */
export function randomSalt(): Uint8Array {
  const salt = new Uint8Array(SALT_BYTES);
  globalThis.crypto.getRandomValues(salt);
  return salt;
}

/** keccak256(salt32 || receiver43). */
export function commitment(salt: Uint8Array, receiver: Uint8Array): Hex {
  if (salt.length !== SALT_BYTES) throw new RangeError("salt must be 32 bytes");
  if (receiver.length !== RECEIVER_BYTES) throw new RangeError("receiver must be 43 bytes");
  return keccak256(concatBytes([salt, receiver]));
}

/** Recognizes the other Zcash address kinds by prefix, to explain why they are refused. */
function nonUnifiedKind(s: string): string | null {
  const lower = s.toLowerCase();
  if (/^(zs|ztestsapling|zregtestsapling)1/.test(lower)) return "Sapling";
  if (/^(tex|textest|texregtest)1/.test(lower)) return "transparent-source-only (TEX)";
  const base58 = "[1-9A-HJ-NP-Za-km-z]";
  if (new RegExp(`^(t1|t3|tm|t2)${base58}{20,}$`).test(s)) return "transparent";
  if (new RegExp(`^(zc|zt)${base58}{20,}$`).test(s)) return "Sprout";
  return null;
}

/** A validated Ironwood destination, ready to be signed (Destination / RefundRequest). */
export interface PreparedDestination {
  /** The UA exactly as the user typed it (sent to the signers, never logged). */
  ua: string;
  receiver: Uint8Array;
  salt: Uint8Array;
  saltHex: Hex;
  commitment: Hex;
  /** True when the UA also has transparent / Sapling / unknown receivers (only Orchard is paid). */
  hasOtherReceivers: boolean;
}

/**
 * Validates `ua` as a destination on `network` and commits to its Orchard-protocol receiver
 * with a fresh salt. Throws `UnifiedAddressError` with a message suitable for the UI.
 */
export function prepareDestination(
  ua: string,
  network: ZcashNetwork = "test",
  salt: Uint8Array = randomSalt(),
): PreparedDestination {
  const trimmed = ua.trim();
  const kind = nonUnifiedKind(trimmed);
  if (kind) {
    throw new UnifiedAddressError(
      `this is a ${kind} address. Zecured pays only the Ironwood (Orchard-protocol) receiver of a unified address (utest1…)`,
    );
  }
  let decoded: DecodedUa;
  try {
    decoded = decodeUnifiedAddress(trimmed);
  } catch (e) {
    if (e instanceof UnifiedAddressError) throw e;
    throw new UnifiedAddressError("not a valid unified address");
  }
  if (decoded.network !== network) {
    throw new UnifiedAddressError(
      network === "test"
        ? "this is not a testnet address (testnet unified addresses start with utest1)"
        : `this address is for ${decoded.network}, not ${network}`,
    );
  }
  if (!decoded.orchard) {
    throw new UnifiedAddressError(
      "this address has no Ironwood (Orchard-protocol) receiver. Use a shielded unified address from an Ironwood-capable wallet.",
    );
  }
  if (decoded.expiryHeight !== undefined || decoded.expiryTime !== undefined) {
    throw new UnifiedAddressError(
      "this address expires; payouts can happen much later. Use an address without an expiry.",
    );
  }
  const hasOtherReceivers =
    Boolean(decoded.p2pkh || decoded.p2sh || decoded.sapling) || decoded.unknownReceivers.length > 0;
  return {
    ua: trimmed,
    receiver: decoded.orchard,
    salt,
    saltHex: bytesToHex(salt),
    commitment: commitment(salt, decoded.orchard),
    hasOtherReceivers,
  };
}
