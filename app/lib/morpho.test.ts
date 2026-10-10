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
  ltvBps,
  maxBorrow,
  maxBorrowAtTargetLtv,
  parseUsdc,
  toAssetsDown,
  toAssetsUp,
  toSharesDown,
  WAD,
  wTaylorCompounded,
  type MarketState,
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

  it("prices, ratios and percentages", () => {
    expect(formatUsdPrice(PRICE_40)).toBe("$40.00");
    expect(formatUsdPrice(2_550n * 10n ** 32n)).toBe("$25.50");
    expect(formatBps(4_000n)).toBe("40.0 %");
    expect(formatBps(6_275n)).toBe("62.7 %");
  });
});
