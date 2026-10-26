/**
 * Morpho Blue math for the position page, ported from morpho-blue's SharesMathLib, MathLib,
 * MorphoBalancesLib and the `_isHealthy` check (docs/ref/vendor/morpho-blue). All values are
 * bigint in on-chain units: collateral in zatoshi (zZEC, 8 decimals), debt in USDC units
 * (6 decimals), oracle price = USD-per-ZEC × 1e34 (MockZecOracle), WAD = 1e18.
 */

export const WAD = 10n ** 18n;
export const ORACLE_PRICE_SCALE = 10n ** 36n;
export const VIRTUAL_SHARES = 10n ** 6n;
export const VIRTUAL_ASSETS = 1n;
/** MockZecOracle: price = USD-per-ZEC × 1e34. */
export const USD_PRICE_SCALE = 10n ** 34n;
export const USDC_DECIMALS = 6;
/** The app lets users borrow up to 40 % LTV (the market liquidates above 62.5 %). */
export const TARGET_LTV_BPS = 4_000n;
export const BPS = 10_000n;
/** Liquidation cursor and max incentive factor (ConstantsLib). */
const LIQUIDATION_CURSOR = 300_000_000_000_000_000n;
const MAX_LIQUIDATION_INCENTIVE_FACTOR = 1_150_000_000_000_000_000n;

export interface MarketState {
  totalSupplyAssets: bigint;
  totalSupplyShares: bigint;
  totalBorrowAssets: bigint;
  totalBorrowShares: bigint;
  lastUpdate: bigint;
  fee: bigint;
}

export interface PositionState {
  supplyShares: bigint;
  borrowShares: bigint;
  /** zatoshi */
  collateral: bigint;
}

// ------------------------------------------------------------------ MathLib / SharesMathLib

export function mulDivDown(x: bigint, y: bigint, d: bigint): bigint {
  return (x * y) / d;
}

export function mulDivUp(x: bigint, y: bigint, d: bigint): bigint {
  return (x * y + (d - 1n)) / d;
}

export function wMulDown(x: bigint, y: bigint): bigint {
  return mulDivDown(x, y, WAD);
}

export function wDivDown(x: bigint, y: bigint): bigint {
  return mulDivDown(x, WAD, y);
}

/** e^(x·n) − 1 approximated by its third-order Taylor expansion (MathLib.wTaylorCompounded). */
export function wTaylorCompounded(x: bigint, n: bigint): bigint {
  const first = x * n;
  const second = mulDivDown(first, first, 2n * WAD);
  const third = mulDivDown(second, first, 3n * WAD);
  return first + second + third;
}

export function toSharesDown(assets: bigint, totalAssets: bigint, totalShares: bigint): bigint {
  return mulDivDown(assets, totalShares + VIRTUAL_SHARES, totalAssets + VIRTUAL_ASSETS);
}

export function toAssetsDown(shares: bigint, totalAssets: bigint, totalShares: bigint): bigint {
  return mulDivDown(shares, totalAssets + VIRTUAL_ASSETS, totalShares + VIRTUAL_SHARES);
}

export function toAssetsUp(shares: bigint, totalAssets: bigint, totalShares: bigint): bigint {
  return mulDivUp(shares, totalAssets + VIRTUAL_ASSETS, totalShares + VIRTUAL_SHARES);
}

// ------------------------------------------------------------------ MorphoBalancesLib

/**
 * Market totals after accruing interest up to `nowSeconds` at `borrowRate` (per second,
 * WAD; from the IRM's `borrowRateView`). `market()` is stale until someone accrues.
 */
export function expectedMarket(market: MarketState, borrowRate: bigint, nowSeconds: bigint): MarketState {
  const elapsed = nowSeconds > market.lastUpdate ? nowSeconds - market.lastUpdate : 0n;
  if (elapsed === 0n || market.totalBorrowAssets === 0n) return market;
  const interest = wMulDown(market.totalBorrowAssets, wTaylorCompounded(borrowRate, elapsed));
  const next = {
    ...market,
    totalBorrowAssets: market.totalBorrowAssets + interest,
    totalSupplyAssets: market.totalSupplyAssets + interest,
  };
  if (market.fee !== 0n) {
    const feeAmount = wMulDown(interest, market.fee);
    const feeShares = toSharesDown(feeAmount, next.totalSupplyAssets - feeAmount, next.totalSupplyShares);
    next.totalSupplyShares += feeShares;
  }
  return next;
}

/** The USDC a `repay(0, borrowShares)` pulls (rounded up, as Morpho does). */
export function borrowAssets(position: PositionState, market: MarketState): bigint {
  return toAssetsUp(position.borrowShares, market.totalBorrowAssets, market.totalBorrowShares);
}

// ------------------------------------------------------------------ risk

/** Collateral value in USDC units at `price` (USD-per-ZEC × 1e34). */
export function collateralValue(collateral: bigint, price: bigint): bigint {
  return mulDivDown(collateral, price, ORACLE_PRICE_SCALE);
}

/** Morpho's borrowing capacity: collateral value × LLTV. */
export function maxBorrow(collateral: bigint, price: bigint, lltv: bigint): bigint {
  return wMulDown(collateralValue(collateral, price), lltv);
}

/** What the app lets the user borrow in total (TARGET_LTV_BPS of the collateral value). */
export function maxBorrowAtTargetLtv(collateral: bigint, price: bigint, ltvBps: bigint = TARGET_LTV_BPS): bigint {
  return (collateralValue(collateral, price) * ltvBps) / BPS;
}

/** Health factor (WAD): maxBorrow / debt. `null` when there is no debt (infinitely healthy). */
export function healthFactor(collateral: bigint, price: bigint, debt: bigint, lltv: bigint): bigint | null {
  if (debt === 0n) return null;
  return wDivDown(maxBorrow(collateral, price, lltv), debt);
}

/** Loan-to-value in basis points (debt / collateral value). */
export function ltvBps(collateral: bigint, price: bigint, debt: bigint): bigint | null {
  const value = collateralValue(collateral, price);
  if (value === 0n) return null;
  return (debt * BPS) / value;
}

/**
 * Oracle price (USD-per-ZEC × 1e34) below which the position becomes liquidatable, i.e. where
 * collateral × price / 1e36 × LLTV = debt. `null` without debt or collateral.
 */
export function liquidationPrice(collateral: bigint, debt: bigint, lltv: bigint): bigint | null {
  if (debt === 0n || collateral === 0n) return null;
  return mulDivUp(debt * WAD, ORACLE_PRICE_SCALE, collateral * lltv);
}

/** Liquidation incentive factor: min(1.15, 1 / (1 − 0.3 × (1 − LLTV))) (WAD). */
export function liquidationIncentiveFactor(lltv: bigint): bigint {
  const lif = wDivDown(WAD, WAD - wMulDown(LIQUIDATION_CURSOR, WAD - lltv));
  return lif < MAX_LIQUIDATION_INCENTIVE_FACTOR ? lif : MAX_LIQUIDATION_INCENTIVE_FACTOR;
}

// ------------------------------------------------------------------ partial release (D14)

/** ZecuredRegistry.MIN_RELEASE: the smallest partial release, and the smallest remainder it may leave (zatoshi). */
export const MIN_RELEASE = 100_000n;
/** ZecuredRegistry.MAX_WITHDRAW_LTV_BPS: the highest LTV a partial release may leave. */
export const MAX_WITHDRAW_LTV_BPS = 5_000n;
/**
 * Margin on the debt when computing the largest partial release. Interest accrues between the
 * position read and the UserOp's inclusion, and the Registry checks the debt at inclusion:
 * 0.1 % of the debt covers about 9 hours at 100 % APR (far above this market's rate), so a
 * "max" withdrawal does not revert with ReleaseLtvTooHigh because a few blocks passed.
 */
export const PARTIAL_RELEASE_DEBT_MARGIN_BPS = 10n;

export interface PartialReleasePosition {
  /** zatoshi in the Morpho position (= the vault's outstanding). */
  collateral: bigint;
  /** USDC units, rounded up as Morpho does (`borrowAssets`). */
  debt: bigint;
  /** USD-per-ZEC × 1e34 */
  price: bigint;
  lltv: bigint;
}

/** Why `requestPartialRelease` reverts: the Registry's errors and Morpho's "insufficient collateral". */
export type PartialReleaseRejection =
  | "ReleaseAmountTooSmall"
  | "ReleaseAmountNotPartial"
  | "ReleaseRemainderTooSmall"
  | "InsufficientCollateral"
  | "ReleaseLtvTooHigh";

/**
 * Why `requestPartialRelease(amount)` would revert for this position, or null if it would go
 * through. Mirrors ZecuredRegistry's checks in their order, including Morpho's health check
 * inside `withdrawCollateral` and the 50 % rule (debt × 10,000 ≤ value × 5,000, with the
 * value rounded down). `debt` must be the debt at inclusion.
 */
export function partialReleaseRejection(p: PartialReleasePosition & { amount: bigint }): PartialReleaseRejection | null {
  if (p.amount < MIN_RELEASE) return "ReleaseAmountTooSmall";
  if (p.amount >= p.collateral) return "ReleaseAmountNotPartial";
  const remaining = p.collateral - p.amount;
  if (remaining < MIN_RELEASE) return "ReleaseRemainderTooSmall";
  if (p.debt === 0n) return null;
  if (maxBorrow(remaining, p.price, p.lltv) < p.debt) return "InsufficientCollateral";
  if (p.debt * BPS > collateralValue(remaining, p.price) * MAX_WITHDRAW_LTV_BPS) return "ReleaseLtvTooHigh";
  return null;
}

/**
 * The largest partial release (zatoshi) that passes `partialReleaseRejection` with the debt
 * raised by `marginBps` (default PARTIAL_RELEASE_DEBT_MARGIN_BPS); 0 when none is possible.
 * Exact, in closed form: the smallest remainder whose rounded-down value meets both the 50 %
 * rule and Morpho's LLTV, never below MIN_RELEASE.
 */
export function maxPartialRelease(p: PartialReleasePosition & { marginBps?: bigint }): bigint {
  const marginBps = p.marginBps ?? PARTIAL_RELEASE_DEBT_MARGIN_BPS;
  const debt = p.debt === 0n ? 0n : p.debt + mulDivUp(p.debt, marginBps, BPS);
  let minRemaining = MIN_RELEASE;
  if (debt > 0n) {
    if (p.price === 0n || p.lltv === 0n) return 0n;
    // Smallest collateral value passing debt × BPS <= value × 5,000 and value × LLTV / WAD >= debt...
    const ltvValue = mulDivUp(debt, BPS, MAX_WITHDRAW_LTV_BPS);
    const healthValue = mulDivUp(debt, WAD, p.lltv);
    const minValue = ltvValue > healthValue ? ltvValue : healthValue;
    // ...and the smallest remainder reaching it: floor(remainder × price / 1e36) >= minValue.
    const remainder = mulDivUp(minValue, ORACLE_PRICE_SCALE, p.price);
    if (remainder > minRemaining) minRemaining = remainder;
  }
  return p.collateral >= minRemaining + MIN_RELEASE ? p.collateral - minRemaining : 0n;
}

/**
 * The position's LTV (bps) after releasing `amount`, rounded up so a preview never looks
 * safer than the Registry's check. null when nothing (of value) would remain.
 */
export function ltvAfterRelease(p: { collateral: bigint; debt: bigint; price: bigint; amount: bigint }): bigint | null {
  if (p.amount >= p.collateral) return null;
  const value = collateralValue(p.collateral - p.amount, p.price);
  if (value === 0n) return null;
  return mulDivUp(p.debt, BPS, value);
}

// ------------------------------------------------------------------ formatting

function trimDecimals(whole: bigint, frac: bigint, decimals: number, minDecimals: number): string {
  let f = frac.toString().padStart(decimals, "0");
  while (f.length > minDecimals && f.endsWith("0")) f = f.slice(0, -1);
  return f.length ? `${whole}.${f}` : whole.toString();
}

/** USDC units → "12.34" (at least 2, at most 6 decimals). */
export function formatUsdc(units: bigint): string {
  const neg = units < 0n;
  const abs = neg ? -units : units;
  const scale = 10n ** BigInt(USDC_DECIMALS);
  return `${neg ? "-" : ""}${trimDecimals(abs / scale, abs % scale, USDC_DECIMALS, 2)}`;
}

/** "12.34" → USDC units. Throws on malformed input or more than 6 decimals. */
export function parseUsdc(input: string): bigint {
  const m = /^(\d*)(?:\.(\d{0,6}))?$/.exec(input.trim());
  if (!m || (!m[1] && !m[2])) throw new Error("not a USDC amount (use up to 6 decimals)");
  return BigInt(m[1] || "0") * 10n ** BigInt(USDC_DECIMALS) + BigInt((m[2] ?? "").padEnd(USDC_DECIMALS, "0") || "0");
}

/** MockZecOracle price of one US cent per ZEC (USD-per-ZEC × 1e34 / 100). */
const USD_CENT_PRICE = 10n ** 32n;

/** Oracle price → "$40.00" (USD per ZEC, cents precision, rounded down). */
export function formatUsdPrice(price: bigint): string {
  const cents = price / USD_CENT_PRICE;
  const whole = (cents / 100n).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `$${whole}.${(cents % 100n).toString().padStart(2, "0")}`;
}

/** USD cents per ZEC → MockZecOracle price (what `setPrice` stores). */
export function oraclePriceFromCents(cents: bigint): bigint {
  return cents * USD_CENT_PRICE;
}

/** "40", "$40.5", "40.50" → USD cents. Throws on malformed input or more than 2 decimals. */
export function parseUsdCents(input: string): bigint {
  const m = /^\$?(\d*)(?:\.(\d{0,2}))?$/.exec(input.trim());
  if (!m || (!m[1] && !m[2])) throw new Error("not a USD amount (use up to 2 decimals)");
  return BigInt(m[1] || "0") * 100n + BigInt((m[2] ?? "").padEnd(2, "0") || "0");
}

/** WAD → "1.53" (two decimals, rounded down). */
export function formatWad(x: bigint): string {
  const hundredths = (x * 100n) / WAD;
  return trimDecimals(hundredths / 100n, hundredths % 100n, 2, 2);
}

/** Basis points → "40.0 %". */
export function formatBps(bps: bigint): string {
  return `${trimDecimals(bps / 100n, (bps % 100n) / 10n, 1, 1)} %`;
}
