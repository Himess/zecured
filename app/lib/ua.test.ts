import { describe, expect, it } from "vitest";
import { bech32m } from "@scure/base";
import { bytesToHex, hexToBytes } from "viem";
import { f4jumble } from "./f4jumble";
import {
  decodeUnifiedAddress,
  encodeUnifiedAddress,
  isIronwoodOnly,
  TYPECODE_ORCHARD,
  TYPECODE_P2PKH,
  TYPECODE_P2SH,
  TYPECODE_SAPLING,
  UnifiedAddressError,
  type UaItem,
} from "./ua";
// zcash-test-vectors unified_address.json (identical to librustzcash's
// zcash_address/src/kind/unified/address/test_vectors.rs; checked when the fixture was added).
import vectors from "./__fixtures__/zcash-test-vectors/unified_address.json";

type Row = [string | null, string | null, string | null, string | null, number | null, string | null, string, string, number, number];
const rows = (vectors as unknown[][]).slice(2) as Row[];

const hex = (b: Uint8Array | undefined) => (b ? bytesToHex(b).slice(2) : null);

/** Raw (unsorted, unchecked) encoding, to build malformed addresses. */
function rawEncode(hrp: string, items: UaItem[]): string {
  const parts: number[] = [];
  for (const { typecode, data } of items) {
    if (typecode >= 0xfd) throw new Error("test helper: small typecodes only");
    parts.push(typecode, data.length, ...data);
  }
  const padding = new Uint8Array(16);
  padding.set(new TextEncoder().encode(hrp));
  const raw = Uint8Array.from([...parts, ...padding]);
  return bech32m.encode(hrp, bech32m.toWords(f4jumble(raw)), false);
}

const orchard = hexToBytes(`0x${rows.find((r) => r[3])![3]!}`);
const sapling = hexToBytes(`0x${rows.find((r) => r[2])![2]!}`);
const p2pkh = hexToBytes(`0x${rows.find((r) => r[0])![0]!}`);

describe("unified address decoder (zcash-test-vectors)", () => {
  it("has 60 vectors, 48 of them with an Orchard receiver", () => {
    expect(rows).toHaveLength(60);
    expect(rows.filter((r) => r[3]).length).toBe(48);
  });

  it.each(rows.map((r, i) => [i, r] as const))("vector %i decodes to the published receivers", (_i, r) => {
    const [p2pkhHex, p2shHex, saplingHex, orchardHex, unknownTc, unknownBytes, ua] = r;
    const d = decodeUnifiedAddress(ua);
    expect(d.network).toBe("main");
    expect(d.revision).toBe(0);
    expect(hex(d.p2pkh)).toBe(p2pkhHex);
    expect(hex(d.p2sh)).toBe(p2shHex);
    expect(hex(d.sapling)).toBe(saplingHex);
    expect(hex(d.orchard)).toBe(orchardHex);
    if (unknownTc !== null) {
      expect(d.unknownReceivers).toEqual([unknownTc]);
      expect(hex(d.items.find((x) => x.typecode === unknownTc)?.data)).toBe(unknownBytes);
    } else {
      expect(d.unknownReceivers).toEqual([]);
    }
  });

  it("re-encodes every vector to the same string", () => {
    for (const r of rows) {
      const d = decodeUnifiedAddress(r[6]);
      expect(encodeUnifiedAddress("main", d.items)).toBe(r[6]);
    }
  });

  it("accepts an all-uppercase address and surrounding whitespace", () => {
    const ua = rows[0]![6];
    expect(decodeUnifiedAddress(`  ${ua.toUpperCase()}\n`).sapling).toBeDefined();
  });

  it("marks only single-Orchard-receiver addresses as Ironwood-only", () => {
    for (const r of rows) {
      const onlyOrchard = r[3] !== null && r[0] === null && r[1] === null && r[2] === null && r[4] === null;
      expect(isIronwoodOnly(decodeUnifiedAddress(r[6]))).toBe(onlyOrchard);
    }
  });
});

describe("testnet addresses", () => {
  it("round-trips an Ironwood-only testnet UA (utest1…)", () => {
    const ua = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: orchard }]);
    expect(ua.startsWith("utest1")).toBe(true);
    const d = decodeUnifiedAddress(ua);
    expect(d.network).toBe("test");
    expect(d.hrp).toBe("utest");
    expect(bytesToHex(d.orchard!)).toBe(bytesToHex(orchard));
    expect(isIronwoodOnly(d)).toBe(true);
  });

  it("accepts Revision 2 shielded-only (zutest) and transparent-including (tutest) addresses", () => {
    const zu = decodeUnifiedAddress(rawEncode("zutest", [{ typecode: TYPECODE_ORCHARD, data: orchard }]));
    expect(zu.revision).toBe(2);
    expect(zu.network).toBe("test");
    const tu = decodeUnifiedAddress(
      rawEncode("tutest", [
        { typecode: TYPECODE_P2PKH, data: p2pkh },
        { typecode: TYPECODE_ORCHARD, data: orchard },
      ]),
    );
    expect(tu.p2pkh).toBeDefined();
  });

  it("parses Revision 2 expiry metadata", () => {
    const height = Uint8Array.of(0x10, 0x20, 0x30, 0x00); // little-endian 0x302010
    const d = decodeUnifiedAddress(
      rawEncode("zutest", [
        { typecode: TYPECODE_ORCHARD, data: orchard },
        { typecode: 0xe0, data: height },
      ]),
    );
    expect(d.expiryHeight).toBe(0x302010);
  });
});

describe("rejections", () => {
  const ua = rows[0]![6];

  function expectError(s: string, pattern: RegExp) {
    expect(() => decodeUnifiedAddress(s)).toThrow(UnifiedAddressError);
    expect(() => decodeUnifiedAddress(s)).toThrow(pattern);
  }

  it("rejects a bad checksum, truncation and garbage", () => {
    const i = 20;
    const flipped = ua.slice(0, i) + (ua[i] === "q" ? "p" : "q") + ua.slice(i + 1);
    expectError(flipped, /Bech32m/);
    expectError(ua.slice(0, -3), /Bech32m/);
    expectError("not an address", /Bech32m/);
    expectError("", /Bech32m/);
  });

  it("rejects unknown prefixes and padding that does not match the prefix", () => {
    expectError(rawEncode("zs", [{ typecode: TYPECODE_ORCHARD, data: orchard }]), /unknown address prefix/);
    // A mainnet payload re-labelled as testnet: the padding still says "u".
    const words = bech32m.decode(ua as `${string}1${string}`, false).words;
    expectError(bech32m.encode("utest", words, false), /padding/);
  });

  it("enforces ZIP 316 item rules", () => {
    const o = { typecode: TYPECODE_ORCHARD, data: orchard };
    const s = { typecode: TYPECODE_SAPLING, data: sapling };
    expectError(rawEncode("utest", [o, s]), /out of typecode order/);
    expectError(rawEncode("utest", [o, o]), /duplicate typecode/);
    expectError(
      rawEncode("utest", [
        { typecode: TYPECODE_P2PKH, data: p2pkh },
        { typecode: TYPECODE_P2SH, data: p2pkh },
        o,
      ]),
      /both P2PKH and P2SH/,
    );
    expectError(rawEncode("utest", [{ typecode: TYPECODE_P2PKH, data: p2pkh }]), /only transparent/);
    expectError(rawEncode("utest", [{ typecode: TYPECODE_ORCHARD, data: orchard.slice(1) }]), /Orchard receiver length/);
    expectError(rawEncode("utest", [{ typecode: 0xe0, data: new Uint8Array(4) }, o].reverse()), /Revision 0/);
    expectError(rawEncode("zutest", [o, { typecode: 0xe5, data: new Uint8Array(2) }]), /MUST-understand/);
    expectError(
      rawEncode("zutest", [{ typecode: TYPECODE_P2PKH, data: p2pkh }, o]),
      /transparent receiver in a shielded-only/,
    );
    expectError(rawEncode("zutest", [{ typecode: 0xc0, data: new Uint8Array(3) }, { typecode: 0xd0, data: new Uint8Array(30) }]), /no receivers/);
  });
});
