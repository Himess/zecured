import { describe, expect, it } from "vitest";
import { MARKET_LLTV } from "./config";
import { DELEVERAGE_SLIPPAGE_BPS, deleveragePreview } from "./deleverage";

const PRICE = 40n * 10n ** 34n; // $40 per ZEC, MockZecOracle scale
const quote = {
  executor: "0x4444444444444444444444444444444444444444" as const,
  marginBps: 100,
  repayAssets: 2_000_000n, // 2 USDC
  mmAmount: 5_050_000n, // 0.0505 ZEC: 2 USDC at $40 plus 1 %
  remaining: 4_950_000n,
  expiresAt: 1_800_000_600,
};

describe("P-7: the full preview of reducing debt with collateral", () => {
  it("debt repaid, collateral spent, and the loan after", () => {
    // 0.1 ZEC ($4) against 3 USDC of debt; the market maker repays 2 USDC for 0.0505 ZEC.
    const p = deleveragePreview({ collateral: 10_000_000n, debt: 3_000_000n, price: PRICE, lltv: MARKET_LLTV, quote });
    expect(p.repaid).toBe(2_000_000n);
    expect(p.collateralSpent).toBe(5_050_000n);
    expect(p.debtAfter).toBe(1_000_000n);
    expect(p.collateralAfter).toBe(4_950_000n);
    // 1 USDC against 0.0495 ZEC ($1.98): 50.6 % rounded up.
    expect(p.ltvAfterBps).toBe(5_051n);
    expect(p.healthAfter).not.toBeNull();
    expect(p.liquidationPriceAfter).not.toBeNull();
    // The request allows 1 % more collateral than quoted.
    expect(DELEVERAGE_SLIPPAGE_BPS).toBe(100n);
    expect(p.maxCollateral).toBe(5_100_500n);
  });

  it("never allows more collateral than the vault holds, and repaying everything leaves no LTV risk", () => {
    const all = deleveragePreview({
      collateral: 5_060_000n,
      debt: 2_000_000n,
      price: PRICE,
      lltv: MARKET_LLTV,
      quote: { ...quote, remaining: 10_000n },
    });
    expect(all.maxCollateral).toBe(5_060_000n);
    expect(all.debtAfter).toBe(0n);
    expect(all.healthAfter).toBeNull();
    expect(all.liquidationPriceAfter).toBeNull();
    expect(all.ltvAfterBps).toBe(0n);
  });
});
