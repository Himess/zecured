import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { MARKET_LLTV } from "./config";
import {
  borrowAssets,
  collateralValue,
  expectedMarket,
  formatBps,
  formatUsdc,
  formatUsdPrice,
  formatWad,
  healthFactor,
  liquidationIncentiveFactor,
  liquidationPrice,
  ltvAfterRelease,
  ltvBps,
  MAX_WITHDRAW_LTV_BPS,
  maxBorrow,
  maxBorrowAtTargetLtv,
  maxPartialRelease,
  MIN_RELEASE,
  mulDivUp,
  oraclePriceFromCents,
  PARTIAL_RELEASE_DEBT_MARGIN_BPS,
  parseUsdc,
  parseUsdCents,
  partialReleaseRejection,
  toAssetsDown,
  toAssetsUp,
  toSharesDown,
  WAD,
  wTaylorCompounded,
  type MarketState,
  type PartialReleasePosition,
} from "./morpho";

const PRICE_40 = 40n * 10n ** 34n; // MockZecOracle: $40.00 = 40e34 (contracts DeployZecured default)
const ZAT_001 = 1_000_000n; // 0.01 ZEC
const ZAT_005 = 5_000_000n; // 0.05 ZEC

describe("shares math (SharesMathLib)", () => {
  it("first borrower: 1 USDC unit = 1e6 shares, and back", () => {
    const shares = toSharesDown(160_000n, 0n, 0n);
    expect(shares).toBe(160_000n * 10n ** 6n);
    expect(toAssetsUp(shares, 160_000n, shares)).toBe(160_000n);
    expect(toAssetsDown(shares, 160_000n, shares)).toBe(160_000n);
    // Inexact conversions: up rounds against the borrower, down in its favour.
    expect(toAssetsUp(1n, 160_000n, shares)).toBe(1n);
    expect(toAssetsDown(1n, 160_000n, shares)).toBe(0n);
  });

  it("rounds repay amounts up", () => {
    const market: MarketState = {
      totalSupplyAssets: 1_000_000_000n,
      totalSupplyShares: 10n ** 15n,
      totalBorrowAssets: 160_001n,
      totalBorrowShares: 160_000n * 10n ** 6n,
      lastUpdate: 0n,
      fee: 0n,
    };
    const debt = borrowAssets({ supplyShares: 0n, borrowShares: 160_000n * 10n ** 6n, collateral: ZAT_001 }, market);
    // The borrower holds every share: its debt is the whole total (160,000.99… rounded up).
    expect(debt).toBe(160_001n);
  });
});

describe("interest accrual (MorphoBalancesLib)", () => {
  it("wTaylorCompounded approximates e^(rt) − 1 to the third order", () => {
    const year = 365n * 86_400n;
    const ratePerSecond = WAD / year; // 100 % APR
    const g = wTaylorCompounded(ratePerSecond, year);
    // 1 + 1/2 + 1/6 = 1.6666…
    expect(Number(g) / 1e18).toBeCloseTo(1.6667, 3);
    expect(wTaylorCompounded(ratePerSecond, 0n)).toBe(0n);
  });

  it("accrues interest to borrow and supply totals, and fee shares", () => {
    const market: MarketState = {
      totalSupplyAssets: 250_000_000_000n,
      totalSupplyShares: 250_000_000_000n * 10n ** 6n,
      totalBorrowAssets: 160_000n,
      totalBorrowShares: 160_000n * 10n ** 6n,
      lastUpdate: 1_000n,
      fee: 0n,
    };
    const rate = 1_000_000_000n; // ≈ 3.2 % APR per second, WAD
    const later = expectedMarket(market, rate, 1_000n + 30n * 86_400n);
    const interest = later.totalBorrowAssets - market.totalBorrowAssets;
    expect(interest).toBeGreaterThan(0n);
    expect(later.totalSupplyAssets - market.totalSupplyAssets).toBe(interest);
    expect(later.totalSupplyShares).toBe(market.totalSupplyShares);
    expect(expectedMarket(market, rate, 1_000n)).toBe(market);
    const withFee = expectedMarket({ ...market, fee: WAD / 10n }, rate, 1_000n + 30n * 86_400n);
    expect(withFee.totalSupplyShares).toBeGreaterThan(market.totalSupplyShares);
  });

  it("parity vector with the bots and the Adapter (track L-PRE, R-24): one year at 85,220,420 per second", () => {
    const stored: MarketState = {
      totalSupplyAssets: 1_000_000_000_000n,
      totalSupplyShares: 10n ** 18n,
      totalBorrowAssets: 1_249_000n,
      totalBorrowShares: 1_249_000_000_000n,
      lastUpdate: 1_791_664_288n,
      fee: 0n,
    };
    const now = expectedMarket(stored, 85_220_420n, 1_823_200_288n);
    expect(now.totalBorrowAssets).toBe(1_252_361n);
    expect(now.totalSupplyAssets).toBe(1_000_000_003_361n);
    expect(now.totalBorrowShares).toBe(stored.totalBorrowShares);
    expect(now.totalSupplyShares).toBe(stored.totalSupplyShares);
  });
});

describe("risk", () => {
  it("values collateral with the 1e36-scaled oracle price", () => {
    expect(collateralValue(ZAT_001, PRICE_40)).toBe(400_000n); // 0.01 ZEC × $40 = 0.40 USDC
    expect(collateralValue(100_000_000n, PRICE_40)).toBe(40_000_000n); // 1 ZEC = 40 USDC (MockZecOracle doc)
    expect(maxBorrow(ZAT_001, PRICE_40, MARKET_LLTV)).toBe(250_000n);
  });

  it("caps app borrowing at 40 % LTV", () => {
    expect(maxBorrowAtTargetLtv(ZAT_001, PRICE_40)).toBe(160_000n);
    expect(maxBorrowAtTargetLtv(ZAT_005, PRICE_40)).toBe(800_000n);
    expect(ltvBps(ZAT_001, PRICE_40, 160_000n)).toBe(4_000n);
  });

  it("health factor and liquidation price", () => {
    const hf = healthFactor(ZAT_001, PRICE_40, 160_000n, MARKET_LLTV)!;
    expect(hf).toBe(1_562_500_000_000_000_000n); // 0.25 / 0.16
    expect(formatWad(hf)).toBe("1.56");
    expect(healthFactor(ZAT_001, PRICE_40, 0n, MARKET_LLTV)).toBeNull();

    const lp = liquidationPrice(ZAT_001, 160_000n, MARKET_LLTV)!;
    expect(formatUsdPrice(lp)).toBe("$25.60");
    // At the liquidation price the position is exactly at its borrowing limit.
    expect(maxBorrow(ZAT_001, lp, MARKET_LLTV)).toBeGreaterThanOrEqual(160_000n);
    expect(maxBorrow(ZAT_001, lp - 10n ** 30n, MARKET_LLTV)).toBeLessThan(160_000n);
    expect(liquidationPrice(ZAT_001, 0n, MARKET_LLTV)).toBeNull();
  });

  it("liquidation incentive factor for LLTV 62.5 % (SPIKE_C: LIF = 1.12676)", () => {
    expect(liquidationIncentiveFactor(MARKET_LLTV)).toBe(1_126_760_563_380_281_690n);
    // At LTV 62.75 % a full-debt liquidation seizes ≈ 70.70 % of the collateral (LTV × LIF).
    const seizedShare = (6_275n * liquidationIncentiveFactor(MARKET_LLTV)) / 10_000n;
    expect(Number(seizedShare) / 1e18).toBeCloseTo(0.707, 3);
  });
});

describe("partial release (D14)", () => {
  const REGISTRY_SOURCE = readFileSync(
    fileURLToPath(new URL("../../contracts/src/ZecuredRegistry.sol", import.meta.url)),
    "utf8",
  );
  /** 0.01 ZEC at the app's 40 % LTV limit, $40. */
  const TYPICAL: PartialReleasePosition = { collateral: ZAT_001, debt: 160_000n, price: PRICE_40, lltv: MARKET_LLTV };
  const at = (p: PartialReleasePosition, amount: bigint) => partialReleaseRejection({ ...p, amount });

  it("uses the Registry's constants", () => {
    expect(REGISTRY_SOURCE).toMatch(/uint64 public constant MIN_RELEASE = 100_000;/);
    expect(REGISTRY_SOURCE).toMatch(/uint256 public constant MAX_WITHDRAW_LTV_BPS = 5_000;/);
    expect(MIN_RELEASE).toBe(100_000n);
    expect(MAX_WITHDRAW_LTV_BPS).toBe(5_000n);
  });

  it("rejects like the Registry, in its order", () => {
    expect(at(TYPICAL, MIN_RELEASE - 1n)).toBe("ReleaseAmountTooSmall");
    expect(at({ ...TYPICAL, collateral: 50_000n }, 60_000n)).toBe("ReleaseAmountTooSmall"); // size first
    expect(at(TYPICAL, ZAT_001)).toBe("ReleaseAmountNotPartial");
    expect(at(TYPICAL, ZAT_001 + 1n)).toBe("ReleaseAmountNotPartial");
    expect(at({ ...TYPICAL, debt: 0n }, ZAT_001 - MIN_RELEASE + 1n)).toBe("ReleaseRemainderTooSmall");
    expect(at({ ...TYPICAL, debt: 0n }, ZAT_001 - MIN_RELEASE)).toBeNull();
    // Morpho's own health check (62.5 %) fails first when the remainder cannot even carry the debt.
    expect(at(TYPICAL, 800_000n)).toBe("InsufficientCollateral");
    expect(at(TYPICAL, 300_000n)).toBe("ReleaseLtvTooHigh");
  });

  it("the 50 % limit is exact: debt × 10,000 <= value × 5,000 with the value rounded down", () => {
    // 0.008 ZEC × $40 = 0.32 USDC; 0.16 USDC of debt is exactly 50 %.
    expect(at(TYPICAL, 200_000n)).toBeNull();
    expect(at(TYPICAL, 200_001n)).toBe("ReleaseLtvTooHigh");
    expect(ltvAfterRelease({ ...TYPICAL, amount: 200_000n })).toBe(5_000n);
    // 799,999 zat are worth 319,999.6 units, rounded down: the preview rounds the LTV up.
    expect(ltvAfterRelease({ ...TYPICAL, amount: 200_001n })).toBe(5_001n);
    expect(ltvAfterRelease({ ...TYPICAL, amount: ZAT_001 })).toBeNull();
    expect(ltvAfterRelease({ ...TYPICAL, debt: 0n, amount: 1n })).toBe(0n);
  });

  it("max: everything above MIN_RELEASE without debt; the exact LTV bound with debt", () => {
    expect(maxPartialRelease({ ...TYPICAL, debt: 0n })).toBe(900_000n);
    expect(maxPartialRelease({ ...TYPICAL, marginBps: 0n })).toBe(200_000n);
    expect(maxPartialRelease({ ...TYPICAL, collateral: ZAT_005, debt: 800_000n, marginBps: 0n })).toBe(1_000_000n);
    // Default margin: 0.1 % more debt (160,160) needs a remainder worth 320,320, i.e. 800,800 zat.
    expect(PARTIAL_RELEASE_DEBT_MARGIN_BPS).toBe(10n);
    expect(maxPartialRelease(TYPICAL)).toBe(199_200n);
  });

  it("max: nothing when the vault is too small or the loan already at the limit", () => {
    expect(maxPartialRelease({ ...TYPICAL, collateral: 2n * MIN_RELEASE, debt: 0n })).toBe(MIN_RELEASE);
    expect(maxPartialRelease({ ...TYPICAL, collateral: 2n * MIN_RELEASE - 1n, debt: 0n })).toBe(0n);
    expect(maxPartialRelease({ ...TYPICAL, debt: 200_000n, marginBps: 0n })).toBe(0n); // already at 50 %
    expect(maxPartialRelease({ ...TYPICAL, price: 0n })).toBe(0n);
    // A sliver above the floor is not enough either: what is left would be below MIN_RELEASE to take.
    expect(maxPartialRelease({ ...TYPICAL, debt: 199_000n, marginBps: 0n })).toBe(0n);
  });

  it("max is the largest accepted amount, also with awkward prices and debts (window search)", () => {
    const positions: PartialReleasePosition[] = [
      TYPICAL,
      { collateral: ZAT_005, debt: 123_457n, price: 37n * 10n ** 34n + 123_456_789_012_345_678_901_234_567n, lltv: MARKET_LLTV },
      { collateral: 5_000_001n, debt: 1n, price: PRICE_40, lltv: MARKET_LLTV },
      { collateral: 1_234_567n, debt: 99_999n, price: 25n * 10n ** 34n + 7n, lltv: MARKET_LLTV },
      // A market whose LLTV is below 50 %: Morpho's health check is the binding one.
      { collateral: ZAT_001, debt: 100_000n, price: PRICE_40, lltv: 400_000_000_000_000_000n },
    ];
    for (const p of positions) {
      const max = maxPartialRelease({ ...p, marginBps: 0n });
      expect(max, `${p.debt}`).toBeGreaterThanOrEqual(MIN_RELEASE);
      for (let a = max - 40n; a <= max + 40n; a++) {
        if (a < MIN_RELEASE) continue;
        expect(at(p, a) === null, `amount ${a}, max ${max}`).toBe(a <= max);
      }
      // With the default margin the result still passes against a debt 0.1 % higher.
      const safe = maxPartialRelease(p);
      expect(safe).toBeLessThanOrEqual(max);
      expect(at({ ...p, debt: p.debt + mulDivUp(p.debt, PARTIAL_RELEASE_DEBT_MARGIN_BPS, 10_000n) }, safe)).toBeNull();
    }
    const lowLltv = positions[4]!;
    expect(at(lowLltv, maxPartialRelease({ ...lowLltv, marginBps: 0n }) + 1n)).toBe("InsufficientCollateral");
  });
});

describe("formatting", () => {
  it("USDC", () => {
    expect(formatUsdc(160_000n)).toBe("0.16");
    expect(formatUsdc(0n)).toBe("0.00");
    expect(formatUsdc(1n)).toBe("0.000001");
    expect(formatUsdc(12_345_678n)).toBe("12.345678");
    expect(formatUsdc(-1_500_000n)).toBe("-1.50");
    expect(parseUsdc("0.16")).toBe(160_000n);
    expect(parseUsdc("1")).toBe(1_000_000n);
    expect(parseUsdc(".5")).toBe(500_000n);
    expect(parseUsdc("2.")).toBe(2_000_000n);
    expect(() => parseUsdc("1.1234567")).toThrow();
    expect(() => parseUsdc("")).toThrow();
    expect(() => parseUsdc(".")).toThrow();
    expect(() => parseUsdc("-1")).toThrow();
  });

  it("USD prices for the MOCK oracle", () => {
    expect(parseUsdCents("40")).toBe(4_000n);
    expect(parseUsdCents("$40.5")).toBe(4_050n);
    expect(parseUsdCents(" 40.50 ")).toBe(4_050n);
    expect(parseUsdCents(".5")).toBe(50n);
    expect(parseUsdCents("20.")).toBe(2_000n);
    for (const bad of ["", ".", "$", "40.123", "-1", "4e1", "forty"]) expect(() => parseUsdCents(bad), bad).toThrow();
    expect(oraclePriceFromCents(4_000n)).toBe(PRICE_40);
    expect(formatUsdPrice(oraclePriceFromCents(2_560n))).toBe("$25.60");
  });

  it("prices, ratios and percentages", () => {
    expect(formatUsdPrice(PRICE_40)).toBe("$40.00");
    expect(formatUsdPrice(2_550n * 10n ** 32n)).toBe("$25.50");
    expect(formatBps(4_000n)).toBe("40.0 %");
    expect(formatBps(6_275n)).toBe("62.7 %");
  });
});
