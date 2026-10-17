import { describe, expect, it } from "vitest";
import { MARKET_LLTV } from "./config";
import { formatUsdc, liquidationIncentiveFactor, WAD } from "./morpho";
import {
  bandAt,
  formatIncentive,
  formatWadPercent,
  PRE_LIQUIDATION_PRESETS,
  PRE_LIQUIDATION_TERM_SECONDS,
  previewSentence,
  termsActive,
  termsOf,
  termsProblem,
  type PreLiquidationTerms,
} from "./preliq";

const PCT = WAD / 100n;
const NOW = 1_800_000_000n;
const LLTV = MARKET_LLTV;
const standard = PRE_LIQUIDATION_PRESETS.find((p) => p.id === "standard")!;

describe("pre-liquidation presets (section 4 of the v4 design)", () => {
  it("the design's example is the standard preset: from 55 %, 2 % to 5 % incentive, 10 % to 50 % of the debt", () => {
    expect(standard).toMatchObject({ preLltv: 55n * PCT, preLif1: 102n * PCT, preLif2: 105n * PCT, preLcf1: 10n * PCT, preLcf2: 50n * PCT });
    // Every preset starts above the app's 40 % borrowing limit and is within the Adapter's bounds.
    for (const p of PRE_LIQUIDATION_PRESETS) {
      expect(p.preLltv > 40n * PCT).toBe(true);
      expect(termsProblem(termsOf(p, NOW), LLTV)).toBeNull();
    }
  });

  it("terms last 180 days from now (the Adapter wants an expiry in the future)", () => {
    expect(termsOf(standard, NOW).expiry).toBe(NOW + PRE_LIQUIDATION_TERM_SECONDS);
    expect(PRE_LIQUIDATION_TERM_SECONDS).toBe(180n * 86_400n);
  });
});

describe("the Adapter's bounds (mirrored from SettlementAdapter.setPreLiquidation and the paymaster)", () => {
  const ok = termsOf(standard, NOW);
  const lif = liquidationIncentiveFactor(LLTV);

  it("accepts the edges: the market's own incentive, the whole debt, just below the LLTV", () => {
    expect(termsProblem({ ...ok, preLif2: lif, preLcf2: WAD, preLltv: LLTV - 1n }, LLTV)).toBeNull();
    expect(termsProblem({ ...ok, preLif1: WAD, preLif2: WAD }, LLTV)).toBeNull();
  });

  it("refuses everything outside them", () => {
    const cases: [Partial<PreLiquidationTerms>, RegExp][] = [
      [{ preLltv: LLTV }, /below the market's liquidation LTV/],
      [{ preLif1: WAD - 1n }, /cannot be negative/],
      [{ preLif1: 106n * PCT }, /must not fall/],
      [{ preLif2: lif + 1n }, /cannot exceed a full liquidation/],
      [{ preLcf1: 0n }, /must be positive/],
      [{ preLcf1: 60n * PCT }, /close factor must not fall/],
      [{ preLcf2: WAD + 1n }, /whole debt/],
      [{ expiry: 0n }, /expiry/],
      [{ expiry: 1n << 64n }, /expiry/],
    ];
    for (const [patch, pattern] of cases) expect(termsProblem({ ...ok, ...patch }, LLTV)).toMatch(pattern);
  });

  it("terms apply while set and unexpired", () => {
    expect(termsActive(ok, NOW)).toBe(true);
    expect(termsActive({ ...ok, expiry: NOW }, NOW)).toBe(false);
    expect(termsActive({ ...ok, preLltv: 0n, expiry: 0n }, NOW)).toBe(false);
    expect(termsActive(null, NOW)).toBe(false);
  });
});

describe("the band, as the Adapter interpolates it (Morpho's PreLiquidation arithmetic)", () => {
  const t = termsOf(standard, NOW);

  it("is empty at or below preLltv and above the LLTV (there, it is a full liquidation)", () => {
    expect(bandAt(t, LLTV, 55n * PCT)).toBeNull();
    expect(bandAt(t, LLTV, LLTV + 1n)).toBeNull();
  });

  it("rises linearly from (preLcf1, preLif1) just above preLltv to (preLcf2, preLif2) at the LLTV", () => {
    expect(bandAt(t, LLTV, LLTV)).toEqual({ closeFactor: 50n * PCT, incentive: 105n * PCT });
    // Halfway (58.75 %): 30 % of the debt, 3.5 % incentive.
    expect(bandAt(t, LLTV, 5875n * (WAD / 10_000n))).toEqual({ closeFactor: 30n * PCT, incentive: 1035n * (WAD / 1000n) });
    const near = bandAt(t, LLTV, 55n * PCT + 1n)!;
    expect(near.closeFactor >= 10n * PCT && near.closeFactor < 10n * PCT + 10n ** 6n).toBe(true);
  });
});

describe("the preview sentence", () => {
  it("says the design's sentence, with today's debt", () => {
    const s = previewSentence(standard, LLTV, 100_000_000n, formatUsdc);
    expect(s).toBe(
      "If your loan-to-value passes 55 %, anyone may repay 10 % to at most 50 % of your debt (about 50.00 USDC of today's debt) for a 2 % to 5 % incentive, paid in your vault's shielded ZEC; at 62.5 % a full liquidation means a 12.7 % incentive.",
    );
    expect(previewSentence(standard, LLTV, null, formatUsdc)).not.toMatch(/today's debt/);
    const flat = { ...standard, preLcf1: 25n * PCT, preLcf2: 25n * PCT, preLif1: 103n * PCT, preLif2: 103n * PCT };
    expect(previewSentence(flat, LLTV, 0n, formatUsdc)).toMatch(/repay 25 % of your debt for a 3 % incentive/);
  });

  it("formats WAD percentages to a tenth", () => {
    expect(formatWadPercent(55n * PCT)).toBe("55 %");
    expect(formatWadPercent(LLTV)).toBe("62.5 %");
    expect(formatIncentive(liquidationIncentiveFactor(LLTV))).toBe("12.7 %");
    expect(formatIncentive(WAD - 1n)).toBe("0 %");
  });
});
