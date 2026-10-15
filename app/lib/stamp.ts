/**
 * Review H-01: the hashcash stamp of a vault opening (docs/INTERFACES.md § 4, `POST /vaults`).
 *
 * Opening a vault costs the five signers a DKG and a slot among the vaults waiting for a
 * deposit, so every request carries a nonce whose stamp
 *
 *   sha256("zecured-open-vault-v1" || requestId (32 bytes) || owner (20 bytes) || nonce (u64, big-endian))
 *
 * starts with the network's number of zero bits (`GET /config` `openVaultStampBits`): about a
 * second of work in a browser at 20 bits, one hash for the backend to check. The stamp binds the
 * request id and the owner key, which the `OpenVault` signature binds to the rest of the request.
 * Same function as signer/crates/base-client/src/stamp.rs (shared test vectors in stamp.test.ts).
 */
import { sha256 } from "@noble/hashes/sha2.js";
import { hexToBytes, isAddress, isHex, type Address, type Hex } from "viem";

/** Domain separation of the stamp's preimage. */
export const STAMP_DOMAIN = "zecured-open-vault-v1";
/** Most zero bits a network may require (the backend refuses a configuration above it). */
export const STAMP_MAX_BITS = 32;
/** The network's default, used while `GET /config` has not said otherwise. */
export const STAMP_DEFAULT_BITS = 20;

const DOMAIN_BYTES = new TextEncoder().encode(STAMP_DOMAIN);
/** domain || requestId || owner: the part of the preimage that does not change with the nonce. */
const PREFIX_LEN = DOMAIN_BYTES.length + 32 + 20;
/** Nonces tried between two yields to the event loop (a few tens of milliseconds of work). */
const CHUNK = 20_000;
const TWO_32 = 2 ** 32;

function prefix(requestId: Hex, owner: Address): Uint8Array {
  if (!isHex(requestId) || requestId.length !== 66) throw new Error("The stamp needs a 32-byte request id.");
  if (!isAddress(owner, { strict: false })) throw new Error("The stamp needs the owner's address.");
  const out = new Uint8Array(PREFIX_LEN);
  out.set(DOMAIN_BYTES, 0);
  out.set(hexToBytes(requestId), DOMAIN_BYTES.length);
  out.set(hexToBytes(owner), DOMAIN_BYTES.length + 32);
  return out;
}

function checkNonce(nonce: bigint): void {
  if (nonce < 0n || nonce >= 1n << 64n) throw new Error("A stamp nonce is a 64-bit unsigned integer.");
}

function checkBits(bits: number): void {
  if (!Number.isInteger(bits) || bits < 0 || bits > STAMP_MAX_BITS) {
    throw new Error(`A stamp needs between 0 and ${STAMP_MAX_BITS} zero bits.`);
  }
}

/** The stamp of `nonce` for the request `requestId` of `owner` (the whole preimage, hashed at once). */
export function stampDigest(requestId: Hex, owner: Address, nonce: bigint): Uint8Array {
  checkNonce(nonce);
  const preimage = new Uint8Array(PREFIX_LEN + 8);
  preimage.set(prefix(requestId, owner));
  new DataView(preimage.buffer).setBigUint64(PREFIX_LEN, nonce, false);
  return sha256(preimage);
}

/** Zero bits at the start of `d`, most significant bit first. */
export function leadingZeroBits(d: Uint8Array): number {
  let bits = 0;
  for (const byte of d) {
    if (byte !== 0) return bits + Math.clz32(byte) - 24;
    bits += 8;
  }
  return bits;
}

/** Whether `nonce` stamps the request with at least `bits` zero bits. */
export function isValidStamp(requestId: Hex, owner: Address, nonce: bigint, bits: number): boolean {
  checkBits(bits);
  return leadingZeroBits(stampDigest(requestId, owner, nonce)) >= bits;
}

/** Lets the page paint and handle input between chunks of work. */
function yieldToEventLoop(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

export interface SolveOptions {
  /** The first nonce tried (default 0). */
  start?: bigint;
  /** Stops the search (it then rejects with the signal's reason). */
  signal?: AbortSignal;
}

/**
 * The first nonce from `start` that stamps the request with `bits` zero bits. The 81-byte
 * preimage spans two SHA-256 blocks and only the second depends on the nonce, so the state
 * after the first block is computed once and copied for each nonce. The search yields to the
 * event loop every few thousand nonces, so the page stays responsive while it runs (about a
 * million nonces on average at 20 bits).
 */
export async function solveStamp(requestId: Hex, owner: Address, bits: number, opts: SolveOptions = {}): Promise<bigint> {
  checkBits(bits);
  const start = opts.start ?? 0n;
  checkNonce(start);
  const pre = prefix(requestId, owner);
  const block = sha256.blockLen;
  // The first block (domain, request id, 11 bytes of the owner) is hashed once.
  const head = sha256.create().update(pre.subarray(0, block));
  // The second block's input: the rest of the owner, then the nonce.
  const tail = new Uint8Array(PREFIX_LEN - block + 8);
  tail.set(pre.subarray(block));
  const view = new DataView(tail.buffer);
  const at = PREFIX_LEN - block;
  const scratch = sha256.create();
  const out = new Uint8Array(sha256.outputLen);
  // Nonces below 2^53 are exact as numbers; at 32 bits a search ends far below that.
  let n = Number(start);
  if (!Number.isSafeInteger(n)) throw new Error("The stamp search must start below 2^53.");
  for (;;) {
    for (let i = 0; i < CHUNK; i++, n++) {
      view.setUint32(at, Math.floor(n / TWO_32), false);
      view.setUint32(at + 4, n >>> 0, false);
      head._cloneInto(scratch).update(tail).digestInto(out);
      if (leadingZeroBits(out) >= bits) return BigInt(n);
    }
    if (!Number.isSafeInteger(n + CHUNK)) throw new Error("No stamp found below 2^53.");
    await yieldToEventLoop();
    opts.signal?.throwIfAborted();
  }
}
