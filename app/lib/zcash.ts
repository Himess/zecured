/**
 * Zcash helpers: zatoshi ↔ ZEC, ZIP 321 payment URIs, a cheap unified-address sanity check.
 * Amounts are bigint zatoshi everywhere (1 ZEC = 1e8 zat; 1 zZEC wei = 1 zat).
 */
import { DEPLOYMENT } from "./deployment";

export const ZATS_PER_ZEC = 100_000_000n;
/** "TAZ" on Zcash testnet (the judge deployment), "ZEC" on the mainnet beta (lib/deployment.ts). */
export const TICKER = DEPLOYMENT.ticker;
/**
 * Collateral denominations before GET /config answers (the backend's list wins once loaded): on
 * testnet (D7) 0.01 and 0.05 TAZ, and 0.125 TAZ, what the Valar testnet faucet pays, so that a vault
 * can be funded straight from the faucet (S13); on the mainnet beta 0.05 and 0.1 ZEC.
 */
export const DENOMINATIONS: readonly bigint[] = DEPLOYMENT.defaultDenominations;
/** NU7 target block spacing (ZIP 218 / docs/ref/FACTS.md), for time estimates only. */
export const ZCASH_BLOCK_SECONDS = 25;
const MAX_MONEY = 21_000_000n * ZATS_PER_ZEC;

/** Zatoshi → decimal ZEC string without trailing zeros ("0.01", "1", "1.23456789"). */
export function formatZec(zat: bigint): string {
  const negative = zat < 0n;
  const abs = negative ? -zat : zat;
  const whole = abs / ZATS_PER_ZEC;
  const frac = abs % ZATS_PER_ZEC;
  const fracStr = frac === 0n ? "" : `.${frac.toString().padStart(8, "0").replace(/0+$/, "")}`;
  return `${negative ? "-" : ""}${whole}${fracStr}`;
}

/** Decimal ZEC string → zatoshi. Throws on malformed input, > 8 decimals or > 21M ZEC. */
export function parseZec(input: string): bigint {
  const s = input.trim();
  const m = /^(\d+)(?:\.(\d{1,8}))?$/.exec(s) ?? /^()\.(\d{1,8})$/.exec(s);
  if (!m) throw new Error("not a ZEC amount (use up to 8 decimals)");
  const whole = BigInt(m[1] || "0");
  const frac = BigInt((m[2] ?? "").padEnd(8, "0") || "0");
  const zat = whole * ZATS_PER_ZEC + frac;
  if (zat > MAX_MONEY) throw new Error("amount exceeds the ZEC supply");
  return zat;
}

/**
 * ZIP 321 payment URI for a single payment: `zcash:<address>?amount=<decimal ZEC>`.
 * Deliberately carries no label, message or memo (those would travel with the payment
 * request and could leak). Matches the coordinator's `zip321_uri` byte for byte.
 */
export function zip321Uri(address: string, zat: bigint): string {
  if (zat <= 0n) throw new RangeError("amount must be positive");
  if (!/^[a-z0-9]+$/.test(address)) throw new Error("address must be a lowercase Bech32 string");
  return `zcash:${address}?amount=${formatZec(zat)}`;
}

const BECH32_CHARS = "[02-9ac-hj-np-z]";
const TESTNET_UA = new RegExp(`^(utest|zutest|tutest)1${BECH32_CHARS}{60,}$`);

/**
 * Cheap sanity check for a testnet unified address (prefix `utest1`, or a Revision 2
 * `zutest1` / `tutest1`). Use `prepareDestination` (lib/commitment.ts) for real validation.
 */
export function looksLikeTestnetUnifiedAddress(input: string): boolean {
  return TESTNET_UA.test(input.trim());
}

/** Rough wall-clock estimate for `blocks` Zcash blocks. */
export function blocksToDuration(blocks: number): string {
  if (blocks <= 0) return "now";
  const minutes = Math.round((blocks * ZCASH_BLOCK_SECONDS) / 60);
  if (minutes < 1) return "less than a minute";
  if (minutes < 90) return `about ${minutes} min`;
  const hours = Math.round((minutes / 60) * 10) / 10;
  return `about ${hours} h`;
}

/** "0.01 TAZ". */
export function formatTaz(zat: bigint): string {
  return `${formatZec(zat)} ${TICKER}`;
}

/** Collateral sizes for prose: "0.01 or 0.05 TAZ", "0.01, 0.05 or 0.1 TAZ". */
export function formatDenominations(zats: readonly bigint[]): string {
  const list = zats.map(formatZec);
  const last = list.pop();
  if (last === undefined) return TICKER;
  return `${list.length ? `${list.join(", ")} or ` : ""}${last} ${TICKER}`;
}
