/**
 * F4Jumble and its inverse (ZIP 316 § Jumbling), used by unified addresses.
 *
 * An unkeyed 4-round Feistel construction over BLAKE2b-512 with personalizations
 * "UA_F4Jumble_H" || i || 0 || 0 and "UA_F4Jumble_G" || i || j (j as 2 bytes, little-endian).
 * Mirrors librustzcash's `f4jumble` crate (docs/ref/vendor/librustzcash/components/f4jumble).
 */
import { blake2b } from "@noble/hashes/blake2.js";

/** BLAKE2b output size in bytes (the left half is at most this long). */
const OUTBYTES = 64;
/** Valid message lengths: 38 ..= 4194368 bytes. */
export const F4JUMBLE_MIN_LENGTH = 38;
export const F4JUMBLE_MAX_LENGTH = 4_194_368;

const PREFIX = new TextEncoder().encode("UA_F4Jumble_"); // 12 bytes

function hPers(i: number): Uint8Array {
  const p = new Uint8Array(16);
  p.set(PREFIX, 0);
  p[12] = 0x48; // 'H'
  p[13] = i;
  return p;
}

function gPers(i: number, j: number): Uint8Array {
  const p = new Uint8Array(16);
  p.set(PREFIX, 0);
  p[12] = 0x47; // 'G'
  p[13] = i;
  p[14] = j & 0xff;
  p[15] = (j >> 8) & 0xff;
  return p;
}

function xorInto(target: Uint8Array, source: Uint8Array): void {
  const n = Math.min(target.length, source.length);
  for (let k = 0; k < n; k++) target[k] = (target[k] ?? 0) ^ (source[k] ?? 0);
}

function checkLength(len: number): void {
  if (len < F4JUMBLE_MIN_LENGTH || len > F4JUMBLE_MAX_LENGTH) {
    throw new RangeError(`F4Jumble: message length must be in ${F4JUMBLE_MIN_LENGTH}..=${F4JUMBLE_MAX_LENGTH}`);
  }
}

function split(message: Uint8Array): { left: Uint8Array; right: Uint8Array } {
  const leftLength = Math.min(OUTBYTES, Math.floor(message.length / 2));
  // Subarrays share the buffer: rounds update the message in place.
  return { left: message.subarray(0, leftLength), right: message.subarray(leftLength) };
}

function hRound(left: Uint8Array, right: Uint8Array, i: number): void {
  xorInto(left, blake2b(right, { dkLen: left.length, personalization: hPers(i) }));
}

function gRound(left: Uint8Array, right: Uint8Array, i: number): void {
  const blocks = Math.ceil(right.length / OUTBYTES);
  for (let j = 0; j < blocks; j++) {
    const hash = blake2b(left, { dkLen: OUTBYTES, personalization: gPers(i, j) });
    xorInto(right.subarray(j * OUTBYTES), hash);
  }
}

/** F4Jumble(message); returns a new array. */
export function f4jumble(message: Uint8Array): Uint8Array {
  checkLength(message.length);
  const out = Uint8Array.from(message);
  const { left, right } = split(out);
  gRound(left, right, 0);
  hRound(left, right, 0);
  gRound(left, right, 1);
  hRound(left, right, 1);
  return out;
}

/** F4Jumble⁻¹(jumbled); returns a new array. */
export function f4jumbleInv(jumbled: Uint8Array): Uint8Array {
  checkLength(jumbled.length);
  const out = Uint8Array.from(jumbled);
  const { left, right } = split(out);
  hRound(left, right, 1);
  gRound(left, right, 1);
  hRound(left, right, 0);
  gRound(left, right, 0);
  return out;
}
