import { describe, expect, it } from "vitest";
import { bytesToHex, hexToBytes } from "viem";
import { f4jumble, f4jumbleInv } from "./f4jumble";
// zcash-test-vectors f4jumble.json (identical to librustzcash's f4jumble/src/test_vectors.rs).
import vectors from "./__fixtures__/zcash-test-vectors/f4jumble.json";

const rows = (vectors as unknown[][]).slice(2) as [string, string][];

describe("F4Jumble (zcash-test-vectors)", () => {
  it("has the 8 published vectors, including multi-block G rounds", () => {
    expect(rows).toHaveLength(8);
    expect(rows.map(([n]) => n.length / 2)).toEqual([48, 64, 128, 129, 192, 193, 16448, 16449]);
  });

  it.each(rows.map(([normal, jumbled], i) => [i, normal, jumbled] as const))(
    "vector %i: jumble and unjumble",
    (_i, normal, jumbled) => {
      const n = hexToBytes(`0x${normal}`);
      const j = hexToBytes(`0x${jumbled}`);
      expect(bytesToHex(f4jumble(n))).toBe(`0x${jumbled}`);
      expect(bytesToHex(f4jumbleInv(j))).toBe(`0x${normal}`);
    },
  );

  it("does not modify its input", () => {
    const n = hexToBytes(`0x${rows[0]![0]}`);
    const copy = Uint8Array.from(n);
    f4jumble(n);
    f4jumbleInv(n);
    expect(n).toEqual(copy);
  });

  it("rejects lengths outside 38..=4194368", () => {
    expect(() => f4jumble(new Uint8Array(37))).toThrow(RangeError);
    expect(() => f4jumbleInv(new Uint8Array(37))).toThrow(RangeError);
    expect(() => f4jumble(new Uint8Array(38))).not.toThrow();
  });
});
