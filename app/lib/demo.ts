/**
 * P-1, the demo path (DEMO, judge deployment only, `NEXT_PUBLIC_DEMO_PATH=1`; never on the mainnet
 * beta): the backend's demo wallet pays an opened vault its full amount in demo TAZ, once per vault,
 * within a daily cap. Each request carries a hashcash stamp
 *
 *   sha256("zecured-demo-pay-v1" || vaultId (32 bytes) || nonce (u64, big-endian))
 *
 * with the zero bits `GET /demo/status` names (a second or two of work in a browser), so a script
 * cannot drain the demo wallet by asking for many vaults at once. Same function as
 * backend/src/demo.rs (shared test vector in demo.test.ts). Exits of a demo vault go back to the
 * demo wallet when the user gives no Ironwood address of their own.
 */
import { sha256 } from "@noble/hashes/sha2.js";
import { hexToBytes, isHex, type Hex } from "viem";
import { leadingZeroBits, STAMP_MAX_BITS } from "./stamp";

export const DEMO_STAMP_DOMAIN = "zecured-demo-pay-v1";
const DOMAIN_BYTES = new TextEncoder().encode(DEMO_STAMP_DOMAIN);
/** Nonces tried between two yields to the event loop. */
const CHUNK = 20_000;

function preimage(vaultId: Hex): Uint8Array {
  if (!isHex(vaultId) || vaultId.length !== 66) throw new Error("The demo stamp needs a 32-byte vault id.");
  const out = new Uint8Array(DOMAIN_BYTES.length + 32 + 8);
  out.set(DOMAIN_BYTES, 0);
  out.set(hexToBytes(vaultId), DOMAIN_BYTES.length);
  return out;
}

/** The demo stamp of `nonce` for `vaultId`. */
export function demoStampDigest(vaultId: Hex, nonce: bigint): Uint8Array {
  if (nonce < 0n || nonce >= 1n << 64n) throw new Error("A stamp nonce is a 64-bit unsigned integer.");
  const p = preimage(vaultId);
  new DataView(p.buffer).setBigUint64(DOMAIN_BYTES.length + 32, nonce, false);
  return sha256(p);
}

export function isValidDemoStamp(vaultId: Hex, nonce: bigint, bits: number): boolean {
  if (!Number.isInteger(bits) || bits < 0 || bits > STAMP_MAX_BITS) throw new Error(`A stamp needs between 0 and ${STAMP_MAX_BITS} zero bits.`);
  return leadingZeroBits(demoStampDigest(vaultId, nonce)) >= bits;
}

/** The first nonce from 0 that stamps `vaultId` with `bits` zero bits, yielding to the page between chunks. */
export async function solveDemoStamp(vaultId: Hex, bits: number, signal?: AbortSignal): Promise<bigint> {
  if (!Number.isInteger(bits) || bits < 0 || bits > STAMP_MAX_BITS) throw new Error(`A stamp needs between 0 and ${STAMP_MAX_BITS} zero bits.`);
  const p = preimage(vaultId);
  const view = new DataView(p.buffer);
  const at = DOMAIN_BYTES.length + 32;
  for (let n = 0; ; ) {
    for (let i = 0; i < CHUNK; i++, n++) {
      view.setUint32(at, Math.floor(n / 2 ** 32), false);
      view.setUint32(at + 4, n >>> 0, false);
      if (leadingZeroBits(sha256(p)) >= bits) return BigInt(n);
    }
    if (!Number.isSafeInteger(n + CHUNK)) throw new Error("No stamp found below 2^53.");
    await new Promise((resolve) => setTimeout(resolve, 0));
    signal?.throwIfAborted();
  }
}

/** Why the demo wallet refused (its fixed codes), in plain words; null for any other error. */
export function demoRefusal(code: string | null): string | null {
  switch (code) {
    case "demo_off":
      return "The demo wallet is off on this deployment.";
    case "demo_already_paid":
      return "The demo wallet already paid this vault: each vault gets one demo payment.";
    case "demo_daily_cap":
      return "Today's demo budget is spent. Pay from your own Ironwood wallet, or try again tomorrow (UTC).";
    case "demo_busy":
      return "The demo wallet's funds are on their way in other payments. Try again in a few minutes.";
    case "demo_low_balance":
      return "The demo wallet is nearly empty; the operators were alerted. Pay from your own Ironwood wallet.";
    case "demo_not_awaiting":
      return "This vault no longer waits for its deposit.";
    case "demo_unvouched":
      return "The signers' signatures on this vault do not check out, so the demo wallet does not pay it.";
    case "bad_stamp":
      return "The request's proof of work did not check out. Try again.";
    default:
      return null;
  }
}
