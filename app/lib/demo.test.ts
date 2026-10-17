import { describe, expect, it } from "vitest";
import { bytesToHex, type Hex } from "viem";
import { DEMO_STAMP_DOMAIN, demoRefusal, demoStampDigest, isValidDemoStamp, solveDemoStamp } from "./demo";

const VAULT: Hex = `0x${"55".repeat(32)}`;

describe("P-1: the demo payment's stamp (DEMO)", () => {
  it("is sha256(domain || vaultId || nonce): vectors shared with backend/src/demo.rs, recomputed with Node's crypto", () => {
    expect(DEMO_STAMP_DOMAIN).toBe("zecured-demo-pay-v1");
    expect(bytesToHex(demoStampDigest(VAULT, 7n))).toBe("0xe5bfd308ea2e37fdf9dce75a209820c1411f1e640760f0f94195861d0337fc33");
    expect(bytesToHex(demoStampDigest(VAULT, 0n))).toBe("0x9e61a4bba0470454cb96a998eb45e91794e816e3c18c38374ad4affd6daa7468");
  });

  it("finds the first nonce with enough zero bits, and checks it", async () => {
    // Node's crypto: the first nonce for this vault with 12 zero bits is 21,216 (digest 0x0003d1cf…).
    expect(await solveDemoStamp(VAULT, 12)).toBe(21_216n);
    expect(isValidDemoStamp(VAULT, 21_216n, 12)).toBe(true);
    expect(isValidDemoStamp(VAULT, 21_215n, 12)).toBe(false);
    expect(await solveDemoStamp(VAULT, 0)).toBe(0n);
    expect(() => isValidDemoStamp(VAULT, 1n, 33)).toThrow(/zero bits/);
    expect(() => demoStampDigest("0x1234", 1n)).toThrow(/32-byte vault id/);
    expect(() => demoStampDigest(VAULT, 1n << 64n)).toThrow(/64-bit/);
  });

  it("says why the demo wallet refused", () => {
    expect(demoRefusal("demo_already_paid")).toMatch(/one demo payment/);
    expect(demoRefusal("demo_daily_cap")).toMatch(/budget is spent/);
    expect(demoRefusal("demo_off")).toMatch(/off/);
    expect(demoRefusal("something_else")).toBeNull();
  });
});
