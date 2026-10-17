/**
 * P-7, "Reduce debt with collateral" (S18, `SettlementAdapter.deleverageWithCollateral`): a market
 * maker repays exactly the amount asked for and takes collateral worth it plus its margin; the vault
 * stays active. Before anything is signed, the app shows the whole effect: the debt repaid, the
 * collateral spent, and the loan after it.
 */
import type { DeleverageQuote } from "./api";
import { BPS, healthFactor, liquidationPrice, mulDivUp } from "./morpho";

/** The account accepts up to 1 % more collateral than quoted (the price moves until execution). */
export const DELEVERAGE_SLIPPAGE_BPS = 100n;

export interface DeleveragePreview {
  /** Loan-token units repaid (exactly the request's). */
  repaid: bigint;
  /** zatoshi the market maker takes, as quoted, and the most the request allows. */
  collateralSpent: bigint;
  maxCollateral: bigint;
  debtAfter: bigint;
  collateralAfter: bigint;
  /** Basis points, rounded up (never looks safer than it is); null without collateral left. */
  ltvAfterBps: bigint | null;
  /** WAD; null without debt left. */
  healthAfter: bigint | null;
  liquidationPriceAfter: bigint | null;
}

/** The position after `q`, from today's collateral (zatoshi), debt (loan-token units) and price. */
export function deleveragePreview(p: { collateral: bigint; debt: bigint; price: bigint; lltv: bigint; quote: DeleverageQuote }): DeleveragePreview {
  const { quote: q } = p;
  const debtAfter = p.debt > q.repayAssets ? p.debt - q.repayAssets : 0n;
  const collateralAfter = p.collateral > q.mmAmount ? p.collateral - q.mmAmount : 0n;
  const value = (collateralAfter * p.price) / 10n ** 36n;
  const slippage = mulDivUp(q.mmAmount, DELEVERAGE_SLIPPAGE_BPS, BPS);
  return {
    repaid: q.repayAssets,
    collateralSpent: q.mmAmount,
    maxCollateral: q.mmAmount + slippage > p.collateral ? p.collateral : q.mmAmount + slippage,
    debtAfter,
    collateralAfter,
    ltvAfterBps: value === 0n ? null : mulDivUp(debtAfter, BPS, value),
    healthAfter: healthFactor(collateralAfter, p.price, debtAfter, p.lltv),
    liquidationPriceAfter: liquidationPrice(collateralAfter, debtAfter, p.lltv),
  };
}
