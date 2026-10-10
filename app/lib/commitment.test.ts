import { describe, expect, it } from "vitest";
import { bytesToHex, concatBytes, hexToBytes, keccak256 } from "viem";
import { commitment, prepareDestination, randomSalt, UnifiedAddressError } from "./commitment";
import { encodeUnifiedAddress, TYPECODE_ORCHARD, TYPECODE_P2PKH, TYPECODE_SAPLING } from "./ua";
import vectors from "./__fixtures__/zcash-test-vectors/unified_address.json";

type Row = (string | number | null)[];
const rows = (vectors as unknown[][]).slice(2) as Row[];
const orchardHex = rows.find((r) => r[3])![3] as string;
const orchard = hexToBytes(`0x${orchardHex}`);
const sapling = hexToBytes(`0x${rows.find((r) => r[2])![2] as string}`);
const p2pkh = hexToBytes(`0x${rows.find((r) => r[0])![0] as string}`);

describe("commitment = keccak256(salt32 || receiver43)", () => {
  it("matches an independent keccak (cast keccak) of the Rust ledger test inputs", () => {
    // Same inputs as signer/crates/policy ledger test: salt = 0x01 × 32, receiver = 0xab × 43.
    // Expected value computed with `cast keccak 0x0101…01abab…ab`.
    const c = commitment(new Uint8Array(32).fill(0x01), new Uint8Array(43).fill(0xab));
    expect(c).toBe("0xfcc6b8e394092373a433eede26ced5ebe700a837297d4988fda5f0b39fe6c1f2");
  });

  it("is salt first, then receiver", () => {
    const salt = randomSalt();
    expect(commitment(salt, orchard)).toBe(keccak256(concatBytes([salt, orchard])));
    expect(commitment(salt, orchard)).not.toBe(keccak256(concatBytes([orchard, salt])));
  });

  it("rejects wrong lengths", () => {
    expect(() => commitment(new Uint8Array(31), orchard)).toThrow(RangeError);
    expect(() => commitment(new Uint8Array(32), orchard.slice(1))).toThrow(RangeError);
  });

  it("uses fresh 32-byte salts", () => {
    const a = randomSalt();
    const b = randomSalt();
    expect(a).toHaveLength(32);
    expect(bytesToHex(a)).not.toBe(bytesToHex(b));
  });
});

describe("prepareDestination", () => {
  const ironwoodOnly = encodeUnifiedAddress("test", [{ typecode: TYPECODE_ORCHARD, data: orchard }]);

  it("extracts the Orchard-protocol receiver of a testnet UA and commits to it", () => {
    const salt = new Uint8Array(32).fill(7);
    const d = prepareDestination(`  ${ironwoodOnly} `, "test", salt);
    expect(d.ua).toBe(ironwoodOnly);
    expect(bytesToHex(d.receiver)).toBe(`0x${orchardHex}`);
    expect(d.saltHex).toBe(bytesToHex(salt));
    expect(d.commitment).toBe(keccak256(concatBytes([salt, orchard])));
    expect(d.hasOtherReceivers).toBe(false);
  });

  it("accepts a UA with other receivers but pays only the Orchard one", () => {
    const multi = encodeUnifiedAddress("test", [
      { typecode: TYPECODE_P2PKH, data: p2pkh },
      { typecode: TYPECODE_SAPLING, data: sapling },
      { typecode: TYPECODE_ORCHARD, data: orchard },
    ]);
    const d = prepareDestination(multi, "test");
    expect(bytesToHex(d.receiver)).toBe(`0x${orchardHex}`);
    expect(d.hasOtherReceivers).toBe(true);
  });

  it("rejects mainnet addresses, addresses without an Orchard receiver and garbage", () => {
    const mainnet = rows.find((r) => r[3])![6] as string;
    expect(() => prepareDestination(mainnet, "test")).toThrow(/not a testnet address/);
    const saplingOnly = encodeUnifiedAddress("test", [{ typecode: TYPECODE_SAPLING, data: sapling }]);
    expect(() => prepareDestination(saplingOnly, "test")).toThrow(/no Ironwood/);
    expect(() => prepareDestination("ztestsapling1xyz", "test")).toThrow(UnifiedAddressError);
    expect(() => prepareDestination("tmXYZ", "test")).toThrow(UnifiedAddressError);
  });

  it("explains why transparent, Sapling and TEX addresses are refused", () => {
    expect(() => prepareDestination("tmBsTi2xWTjUdEXnuTceL7fecEQKeWaPDJd", "test")).toThrow(/transparent address/);
    expect(() => prepareDestination("ztestsapling1qqqqqqqqqqqqqqqqqqqqqqqqqq", "test")).toThrow(/Sapling address/);
    expect(() => prepareDestination("textest1qqqqqqqqqqqqqqqqqqqq", "test")).toThrow(/TEX/);
  });
});
