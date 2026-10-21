/**
 * v4 pre-liquidation ("Protect my loan", the v4 design's section 4): the borrower consents once;
 * when the loan-to-value passes `preLltv`, anyone may repay part of the debt for a small incentive
 * paid in the vault's shielded ZEC, long before Morpho's full liquidation (about 12.7 % at 62.5 %).
 * The Settlement Adapter keeps the terms per account and does the work; this module only builds
 * them, checks them as the Adapter does, and explains them. Values in WAD (1e18 = 100 %).
 */
import {
  liquidationIncentiveFactor,
  mulDivDown,
  mulDivUp,
  ORACLE_PRICE_SCALE,
  toAssetsDown,
  toAssetsUp,
  WAD,
  type MarketState,
  type PositionState,
} from "./morpho";

export interface PreLiquidationTerms {
  /** The LTV above which pre-liquidation may start (WAD). */
  preLltv: bigint;
  /** Close factor at `preLltv` and at the market's LLTV: the share of the debt one call may repay. */
  preLcf1: bigint;
  preLcf2: bigint;
  /** Incentive factor at `preLltv` and at LLTV (1.02e18 = the liquidator gets 2 % more than it repays). */
  preLif1: bigint;
  preLif2: bigint;
  /** Unix seconds after which the terms no longer apply. */
  expiry: bigint;
}

export interface PreLiquidationPreset {
  id: "early" | "standard" | "late";
  label: string;
  preLltv: bigint;
  preLcf1: bigint;
  preLcf2: bigint;
  preLif1: bigint;
  preLif2: bigint;
}

const PCT = WAD / 100n;

/** The presets the app offers: the design's example (55 %, 2 to 5 %, 10 to 50 %) and an earlier and a later band. */
export const PRE_LIQUIDATION_PRESETS: readonly PreLiquidationPreset[] = [
  { id: "early", label: "Early: from 50 % LTV", preLltv: 50n * PCT, preLcf1: 10n * PCT, preLcf2: 50n * PCT, preLif1: 102n * PCT, preLif2: 105n * PCT },
  { id: "standard", label: "Standard: from 55 % LTV", preLltv: 55n * PCT, preLcf1: 10n * PCT, preLcf2: 50n * PCT, preLif1: 102n * PCT, preLif2: 105n * PCT },
  { id: "late", label: "Late: from 58 % LTV", preLltv: 58n * PCT, preLcf1: 10n * PCT, preLcf2: 50n * PCT, preLif1: 102n * PCT, preLif2: 105n * PCT },
];

/** How long the app's terms last; the borrower can cancel them any time. */
export const PRE_LIQUIDATION_TERM_SECONDS = 180n * 86_400n;

/** The terms of `preset`, valid for `PRE_LIQUIDATION_TERM_SECONDS` from `nowSeconds`. */
export function termsOf(preset: PreLiquidationPreset, nowSeconds: bigint): PreLiquidationTerms {
  return {
    preLltv: preset.preLltv,
    preLcf1: preset.preLcf1,
    preLcf2: preset.preLcf2,
    preLif1: preset.preLif1,
    preLif2: preset.preLif2,
    expiry: nowSeconds + PRE_LIQUIDATION_TERM_SECONDS,
  };
}

/**
 * Why the Settlement Adapter would refuse `t` (its bounds: `preLltv < LLTV`,
 * `WAD <= preLif1 <= preLif2 <= LIF(LLTV)`, `0 < preLcf1 <= preLcf2 <= WAD`, an expiry), or null.
 * The same check as backend/src/allowlist.rs; the expiry against the chain's time is the Adapter's.
 */
export function termsProblem(t: PreLiquidationTerms, lltv: bigint): string | null {
  if (t.preLltv >= lltv) return "the threshold must be below the market's liquidation LTV";
  if (t.preLif1 < WAD) return "the incentive cannot be negative";
  if (t.preLif1 > t.preLif2) return "the incentive must not fall as the LTV rises";
  if (t.preLif2 > liquidationIncentiveFactor(lltv)) return "the incentive cannot exceed a full liquidation's";
  if (t.preLcf1 === 0n) return "the close factor must be positive";
  if (t.preLcf1 > t.preLcf2) return "the close factor must not fall as the LTV rises";
  if (t.preLcf2 > WAD) return "the close factor cannot exceed the whole debt";
  if (t.expiry <= 0n || t.expiry >= 1n << 64n) return "the terms need an expiry";
  return null;
}

/** The terms are set (not cleared) and still apply at `nowSeconds`. */
export function termsActive(t: PreLiquidationTerms | null, nowSeconds: bigint): boolean {
  return t !== null && t.preLltv !== 0n && t.expiry > nowSeconds;
}

/**
 * Where an LTV of `ltv` (WAD) sits in the band: the close factor and the incentive factor the
 * Adapter interpolates linearly from `preLltv` to LLTV (rounded down, as Morpho's PreLiquidation
 * does); null outside `preLltv < ltv <= LLTV`.
 */
export function bandAt(t: PreLiquidationTerms, lltv: bigint, ltv: bigint): { closeFactor: bigint; incentive: bigint } | null {
  if (ltv <= t.preLltv || ltv > lltv) return null;
  const q = mulDivDown(ltv - t.preLltv, WAD, lltv - t.preLltv);
  return {
    closeFactor: t.preLcf1 + mulDivDown(q, t.preLcf2 - t.preLcf1, WAD),
    incentive: t.preLif1 + mulDivDown(q, t.preLif2 - t.preLif1, WAD),
  };
}

/** Where the position stands against the terms now, as `SettlementAdapter.preLiquidate` decides. */
export type PreLiquidationNow =
  /** No debt, or no collateral value. */
  | { state: "none" }
  /** `borrowed <= value * preLltv`: nobody can pre-liquidate. */
  | { state: "below"; ltv: bigint }
  /** `borrowed > value * LLTV`: a full liquidation applies instead. */
  | { state: "above"; ltv: bigint }
  /**
   * In the band: the close and incentive factors at this LTV, the most one pre-liquidation may repay
   * (borrow shares, and the USDC Morpho pulls for them) and the collateral it would take for that.
   */
  | {
      state: "band";
      ltv: bigint;
      closeFactor: bigint;
      incentive: bigint;
      maxRepayShares: bigint;
      maxRepayAssets: bigint;
      /** zatoshi, before the Adapter's floor checks (at least the D11 floor taken, and left in the vault). */
      seizedAtMax: bigint;
    };

/**
 * See `PreLiquidationNow`, with the Adapter's formulas and roundings (track L-PRE): the collateral
 * value `coll * price / 1e36` and the debt as Morpho's health check counts it (rounded up); in the
 * band iff `value * preLltv / 1e18 < debt <= value * LLTV / 1e18`; `ltv = ceil(debt * 1e18 / value)`,
 * `q = (ltv - preLltv) * 1e18 / (LLTV - preLltv)`, the factors linear in `q` (rounded down); at most
 * `shares * preLcf / 1e18` repaid (USDC rounded up), and `seized = (assetsDown(shares) * preLif / 1e18)
 * * 1e36 / price`, every step rounded down. `market` must carry the interest accrued until now.
 */
export function preLiquidationNow(
  t: Pick<PreLiquidationTerms, "preLltv" | "preLcf1" | "preLcf2" | "preLif1" | "preLif2">,
  lltv: bigint,
  position: Pick<PositionState, "collateral" | "borrowShares">,
  market: Pick<MarketState, "totalBorrowAssets" | "totalBorrowShares">,
  price: bigint,
): PreLiquidationNow {
  const value = mulDivDown(position.collateral, price, ORACLE_PRICE_SCALE);
  const borrowed = toAssetsUp(position.borrowShares, market.totalBorrowAssets, market.totalBorrowShares);
  if (borrowed === 0n || value === 0n || price === 0n) return { state: "none" };
  const ltv = mulDivUp(borrowed, WAD, value);
  if (borrowed <= mulDivDown(value, t.preLltv, WAD)) return { state: "below", ltv };
  if (borrowed > mulDivDown(value, lltv, WAD)) return { state: "above", ltv };
  const q = mulDivDown(ltv - t.preLltv, WAD, lltv - t.preLltv);
  const closeFactor = t.preLcf1 + mulDivDown(q, t.preLcf2 - t.preLcf1, WAD);
  const incentive = t.preLif1 + mulDivDown(q, t.preLif2 - t.preLif1, WAD);
  const maxRepayShares = mulDivDown(position.borrowShares, closeFactor, WAD);
  const maxRepayAssets = toAssetsUp(maxRepayShares, market.totalBorrowAssets, market.totalBorrowShares);
  const repaidDown = toAssetsDown(maxRepayShares, market.totalBorrowAssets, market.totalBorrowShares);
  const seizedAtMax = mulDivDown(mulDivDown(repaidDown, incentive, WAD), ORACLE_PRICE_SCALE, price);
  return { state: "band", ltv, closeFactor, incentive, maxRepayShares, maxRepayAssets, seizedAtMax };
}

/** WAD → "55 %", "2.5 %", "12.7 %" (one decimal at most, rounded to the nearest tenth). */
export function formatWadPercent(x: bigint): string {
  const tenths = (x * 1_000n + WAD / 2n) / WAD;
  return `${tenths / 10n}${tenths % 10n === 0n ? "" : `.${tenths % 10n}`} %`;
}

/** An incentive factor (WAD) as the premium it pays: 1.05e18 → "5 %". */
export function formatIncentive(lif: bigint): string {
  return formatWadPercent(lif > WAD ? lif - WAD : 0n);
}

/**
 * The preview sentence of the v4 design's section 4: from where the terms start, how much of the
 * debt one call may repay (and how much that is today, `debtNow` in loan-token units, formatted
 * by `usdc`), for which incentive, against a full liquidation's.
 */
export function previewSentence(
  t: Pick<PreLiquidationTerms, "preLltv" | "preLcf1" | "preLcf2" | "preLif1" | "preLif2">,
  lltv: bigint,
  debtNow: bigint | null,
  usdc: (units: bigint) => string,
): string {
  const most = debtNow !== null && debtNow > 0n ? ` (about ${usdc(mulDivDown(debtNow, t.preLcf2, WAD))} USDC of today's debt)` : "";
  const share =
    t.preLcf1 === t.preLcf2 ? `${formatWadPercent(t.preLcf2)} of your debt` : `${formatWadPercent(t.preLcf1)} to at most ${formatWadPercent(t.preLcf2)} of your debt`;
  const incentive =
    t.preLif1 === t.preLif2 ? `a ${formatIncentive(t.preLif2)} incentive` : `a ${formatIncentive(t.preLif1)} to ${formatIncentive(t.preLif2)} incentive`;
  return `If your loan-to-value passes ${formatWadPercent(t.preLltv)}, anyone may repay ${share}${most} for ${incentive}, paid in your vault's shielded ZEC; at ${formatWadPercent(lltv)} a full liquidation means a ${formatIncentive(liquidationIncentiveFactor(lltv))} incentive.`;
}
