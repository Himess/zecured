import { describe, expect, it } from "vitest";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex, type Address, type Hex } from "viem";
import { isValidStamp, leadingZeroBits, solveStamp, STAMP_DOMAIN, STAMP_MAX_BITS, stampDigest } from "./stamp";

const REQUEST: Hex = `0x${"33".repeat(32)}`;
const OWNER: Address = "0x1111111111111111111111111111111111111111";

describe("open-vault stamp (review H-01)", () => {
  /** The vectors of signer/crates/base-client/src/stamp.rs (`stamp_vectors`). */
  it("matches the backend's vectors", async () => {
    const preimage = new Uint8Array([
      ...new TextEncoder().encode(STAMP_DOMAIN),
      ...new Uint8Array(32).fill(0x33),
      ...new Uint8Array(20).fill(0x11),
      ...[0, 0, 0, 0, 0, 0, 0, 7],
    ]);
    expect(preimage).toHaveLength(81);
    expect(stampDigest(REQUEST, OWNER, 7n)).toEqual(sha256(preimage));
    expect(bytesToHex(stampDigest(REQUEST, OWNER, 0n))).toBe("0xaa84c1fd702563a0fd72199fdac42b0b6b923e9a84ce8d52be66821802369c27");
    // The first nonce with 12 zero bits (its stamp has 14: 0x0003638f...).
    expect(await solveStamp(REQUEST, OWNER, 12)).toBe(8_729n);
    expect(bytesToHex(stampDigest(REQUEST, OWNER, 8_729n).subarray(0, 4))).toBe("0x0003638f");
  });

  it("counts zero bits from the first byte, most significant bit first", () => {
    const d = new Uint8Array(32);
    expect(leadingZeroBits(d)).toBe(256);
    d[0] = 0x80;
    expect(leadingZeroBits(d)).toBe(0);
    d[0] = 0x01;
    expect(leadingZeroBits(d)).toBe(7);
    d[0] = 0;
    d[1] = 0x10;
    expect(leadingZeroBits(d)).toBe(11);
    d[2] = 0xff;
    expect(leadingZeroBits(d)).toBe(11);
  });

  /**
   * The search hashes only the second block per nonce and writes the nonce as two 32-bit words;
   * its answers must check with the whole preimage, above 2^32 too.
   */
  it("finds the first stamp that checks out, with nonces above 2^32 too", async () => {
    for (const [bits, start] of [
      [0, 5n],
      [1, 0n],
      [8, 1_000n],
      [10, 2n ** 32n],
      [6, 2n ** 40n + 3n],
    ] as const) {
      const nonce = await solveStamp(REQUEST, OWNER, bits, { start });
      expect(nonce >= start).toBe(true);
      expect(isValidStamp(REQUEST, OWNER, nonce, bits)).toBe(true);
      // No earlier nonce from `start` has the bits: it is the first.
      for (let n = start; n < nonce; n++) expect(isValidStamp(REQUEST, OWNER, n, bits)).toBe(false);
    }
    expect(await solveStamp(REQUEST, OWNER, 0, { start: 5n })).toBe(5n);
  });

  it("binds the request id and the owner", async () => {
    const nonce = await solveStamp(REQUEST, OWNER, 16);
    const otherRequest: Hex = `0x${"34".repeat(32)}`;
    const otherOwner: Address = "0x1212121212121212121212121212121212121212";
    // With overwhelming probability a stamp is worth nothing for another request or owner.
    expect(isValidStamp(otherRequest, OWNER, nonce, 16) && isValidStamp(REQUEST, otherOwner, nonce, 16)).toBe(false);
    // The owner's address is read as bytes: its checksum case does not matter.
    expect(isValidStamp(REQUEST, "0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD", 3n, 0)).toBe(true);
    expect(bytesToHex(stampDigest(REQUEST, "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd", 3n))).toBe(
      bytesToHex(stampDigest(REQUEST, "0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD", 3n)),
    );
  });

  it("refuses malformed inputs", async () => {
    await expect(solveStamp(REQUEST, OWNER, STAMP_MAX_BITS + 1)).rejects.toThrow(/zero bits/);
    await expect(solveStamp(REQUEST, OWNER, -1)).rejects.toThrow(/zero bits/);
    await expect(solveStamp("0x1234", OWNER, 8)).rejects.toThrow(/request id/);
    await expect(solveStamp(REQUEST, "0x1234" as Address, 8)).rejects.toThrow(/owner/);
    expect(() => stampDigest(REQUEST, OWNER, -1n)).toThrow(/64-bit/);
    expect(() => stampDigest(REQUEST, OWNER, 2n ** 64n)).toThrow(/64-bit/);
  });

  it("stops when its signal is aborted", async () => {
    const ctrl = new AbortController();
    ctrl.abort(new Error("left the page"));
    // 32 bits: no stamp in the first chunk of nonces (with overwhelming probability).
    await expect(solveStamp(REQUEST, OWNER, 32, { signal: ctrl.signal })).rejects.toThrow("left the page");
  });
});
