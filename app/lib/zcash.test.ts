import { describe, expect, it } from "vitest";
import { encodeUnifiedAddress, TYPECODE_ORCHARD } from "./ua";
import {
  blocksToDuration,
  DENOMINATIONS,
  formatDenominations,
  formatTaz,
  formatZec,
  looksLikeTestnetUnifiedAddress,
  parseZec,
  zip321Uri,
} from "./zcash";

const testnetUa = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: new Uint8Array(43).fill(9) }]);

describe("zatoshi ↔ ZEC", () => {
  it("formats without trailing zeros", () => {
    expect(formatZec(1_000_000n)).toBe("0.01");
    expect(formatZec(5_000_000n)).toBe("0.05");
    expect(formatZec(100_000_000n)).toBe("1");
    expect(formatZec(123_456_789n)).toBe("1.23456789");
    expect(formatZec(1n)).toBe("0.00000001");
    expect(formatZec(0n)).toBe("0");
    expect(formatZec(-1_500_000n)).toBe("-0.015");
    expect(formatTaz(1_000_000n)).toBe("0.01 TAZ");
  });

  it("parses decimal strings", () => {
    expect(parseZec("0.01")).toBe(1_000_000n);
    expect(parseZec(" 1 ")).toBe(100_000_000n);
    expect(parseZec(".5")).toBe(50_000_000n);
    expect(parseZec("1.23456789")).toBe(123_456_789n);
    expect(() => parseZec("1.234567891")).toThrow();
    expect(() => parseZec("-1")).toThrow();
    expect(() => parseZec("1e3")).toThrow();
    expect(() => parseZec("")).toThrow();
    expect(() => parseZec("21000001")).toThrow(/supply/);
  });

  it("round-trips", () => {
    for (const z of [1n, 99n, 1_000_000n, 123_456_789n, 2_100_000_000_000_000n]) expect(parseZec(formatZec(z))).toBe(z);
  });

  it("uses the D7 testnet denominations", () => {
    expect(DENOMINATIONS).toEqual([1_000_000n, 5_000_000n, 12_500_000n]);
  });

  it("lists denominations for prose", () => {
    expect(formatDenominations(DENOMINATIONS)).toBe("0.01, 0.05 or 0.125 TAZ");
    expect(formatDenominations([1_000_000n, 5_000_000n, 10_000_000n])).toBe("0.01, 0.05 or 0.1 TAZ");
    expect(formatDenominations([5_000_000n])).toBe("0.05 TAZ");
  });
});

describe("ZIP 321", () => {
  it("matches the coordinator's zip321_uri test cases", () => {
    // signer/crates/coordinator/src/lib.rs, zip321 tests.
    expect(zip321Uri("utest1x", 1_000_000n)).toBe("zcash:utest1x?amount=0.01");
    expect(zip321Uri("utest1x", 5_000_000n)).toBe("zcash:utest1x?amount=0.05");
    expect(zip321Uri("utest1x", 100_000_000n)).toBe("zcash:utest1x?amount=1");
    expect(zip321Uri("utest1x", 123_456_789n)).toBe("zcash:utest1x?amount=1.23456789");
  });

  it("builds a URI for a real testnet UA and carries no label, message or memo", () => {
    const uri = zip321Uri(testnetUa, 1_000_000n);
    expect(uri).toBe(`zcash:${testnetUa}?amount=0.01`);
    expect(uri).not.toMatch(/label|message|memo/);
  });

  it("rejects non-positive amounts and non-Bech32 addresses", () => {
    expect(() => zip321Uri(testnetUa, 0n)).toThrow();
    expect(() => zip321Uri("utest1x?amount=9", 1n)).toThrow();
    expect(() => zip321Uri("UTEST1X", 1n)).toThrow();
  });
});

describe("unified address sanity check", () => {
  it("accepts testnet UAs (utest1, and Revision 2 zutest1 / tutest1 prefixes)", () => {
    expect(looksLikeTestnetUnifiedAddress(testnetUa)).toBe(true);
    expect(looksLikeTestnetUnifiedAddress(` ${testnetUa}\n`)).toBe(true);
    expect(looksLikeTestnetUnifiedAddress(`z${testnetUa}`)).toBe(true);
  });

  it("rejects mainnet, transparent, Sapling and short strings", () => {
    expect(looksLikeTestnetUnifiedAddress(testnetUa.replace(/^utest/, "u"))).toBe(false);
    expect(looksLikeTestnetUnifiedAddress("tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd")).toBe(false);
    expect(looksLikeTestnetUnifiedAddress("ztestsapling1qqqq")).toBe(false);
    expect(looksLikeTestnetUnifiedAddress("utest1abc")).toBe(false);
    expect(looksLikeTestnetUnifiedAddress(testnetUa.replace(/.$/, "q"))).toBe(true); // checksum not checked here
    expect(looksLikeTestnetUnifiedAddress(testnetUa.replace(/.$/, "b"))).toBe(false); // not a Bech32 character
  });
});

describe("block time estimates", () => {
  it("uses 25 s blocks", () => {
    expect(blocksToDuration(0)).toBe("now");
    expect(blocksToDuration(1)).toBe("less than a minute");
    expect(blocksToDuration(12)).toBe("about 5 min");
    expect(blocksToDuration(288)).toBe("about 2 h");
  });
});
