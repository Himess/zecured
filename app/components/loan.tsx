/**
 * A loan's figures as the app draws them everywhere: an amount with its token's mark, the health runway, and the
 * health of a position read from Base (lib/chain.ts `PositionSnapshot`). Server-safe: no hooks.
 */
import type { ReactNode } from "react";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { loanTokenName } from "@/lib/labels";
import { formatBps, formatUsdc, formatUsdPrice, formatWad, healthFactor, liquidationPrice, ltvBps, TARGET_LTV_BPS, WAD } from "@/lib/morpho";
import { formatZec, TICKER } from "@/lib/zcash";
import { TokenMark, type Token } from "./marks";
import { cx } from "./ui";

/**
 * An amount with its token's mark: "◉ 0.05 TAZ", "◉ 12.50 MockUSDC". `size` sets the figure's scale; on the large
 * sizes the unit is set smaller than the figure, so the number leads.
 */
export function TokenAmount({
  token,
  value,
  unit,
  size = "md",
  paper = false,
  className,
}: {
  token: Token;
  value: ReactNode;
  /** The unit after the figure; the token's own name by default ("TAZ" for testnet ZEC, the loan token's name). */
  unit?: ReactNode;
  size?: "sm" | "md" | "lg" | "xl" | "hero";
  /** On Base's light ground: the unit in ink. */
  paper?: boolean;
  className?: string;
}) {
  const mark = { sm: 16, md: 20, lg: 26, xl: 34, hero: 44 }[size];
  const text = {
    sm: "text-[14px]",
    md: "text-[16px]",
    lg: "text-[22px]",
    xl: "text-[clamp(26px,3vw,34px)]",
    hero: "text-[clamp(38px,4vw,54px)] tracking-[-0.02em]",
  }[size];
  const big = size === "lg" || size === "xl" || size === "hero";
  return (
    <span className={cx("inline-flex min-w-0 items-center gap-2 leading-none font-bold tabular-nums font-stretch-110%", size === "hero" && "gap-3", text, className)}>
      <TokenMark token={token} size={mark} />
      <span className="truncate">
        {value}
        {unit !== null ? (
          <span className={cx("font-semibold", paper ? "text-ink-muted" : "text-muted", big ? "ml-[0.3em] text-[0.55em] tracking-normal" : "ml-1.5")}>
            {unit ?? (token === "ZEC" || token === "TAZ" ? TICKER : token === "USDC" ? loanTokenName() : token)}
          </span>
        ) : null}
      </span>
    </span>
  );
}

/** Zatoshi as "0.05" (the unit is drawn by TokenAmount). */
export const zec = (zat: bigint) => formatZec(zat);
/** Rounded to 5 decimals, for figures the eye adds up (the exact zatoshi stay in the terms and the payouts). */
export const round5 = (zat: bigint) => ((zat + 500n) / 1_000n) * 1_000n;
export const zec5 = (zat: bigint) => formatZec(round5(zat));
/** USDC units as "12.50", thousands grouped for display ("100,000.00"). */
export const usdc = (units: bigint) => {
  const [whole, frac] = formatUsdc(units).split(".");
  return `${whole!.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${frac !== undefined ? `.${frac}` : ""}`;
};
/** USDC to the cent, rounded down: what can still be borrowed ("25.17", never more than there is). */
export const usdcFloor = (units: bigint) => usdc((units / 10_000n) * 10_000n);
/** A dollar value (USDC units) to the cent, as people read prices: "$2.49", "$100,000.00". */
export const usd = (units: bigint) => {
  const cents = (units + 5_000n) / 10_000n;
  const whole = (cents / 100n).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `$${whole}.${(cents % 100n).toString().padStart(2, "0")}`;
};

/** A yearly rate (WAD) to the hundredth, as markets quote it: "1.00 %", "4.37 %". */
export const rate = (wad: bigint) => {
  const hundredths = (wad * 10_000n + WAD / 2n) / WAD;
  return `${hundredths / 100n}.${(hundredths % 100n).toString().padStart(2, "0")} %`;
};

export type HealthTone = "ok" | "warn" | "danger" | "muted";

export interface Health {
  /** Morpho's health factor (WAD); null without debt (∞). */
  hf: bigint | null;
  /** Loan-to-value in basis points; null without collateral. */
  ltv: bigint | null;
  /** How far towards liquidation, 0 (no debt) to 1 (LTV at the LLTV). */
  used: number;
  tone: HealthTone;
  /** "Healthy", "Watch it", "At risk", "Liquidatable", "No debt". */
  label: string;
  /** Liquidation price (oracle scale), null without debt. */
  liquidationPrice: bigint | null;
  /** The oracle's price the figures were read at. */
  price: bigint;
}

/** The position's health from its snapshot, at the market's LLTV (62.5 %). */
export function healthOf(p: PositionSnapshot): Health {
  const collateral = p.position.collateral;
  const hf = healthFactor(collateral, p.price, p.debt, config.lltv);
  const ltv = ltvBps(collateral, p.price, p.debt);
  const lltvBps = (config.lltv * 10_000n) / WAD;
  const used = ltv === null || lltvBps === 0n ? 0 : Math.min(1.2, Number(ltv) / Number(lltvBps));
  const tone: HealthTone = hf !== null && hf < WAD ? "danger" : used >= 0.9 ? "danger" : used >= 0.75 ? "warn" : "ok";
  const label =
    hf !== null && hf < WAD ? "Liquidatable" : p.debt === 0n ? "No debt" : used >= 0.9 ? "At risk" : used >= 0.75 ? "Watch it" : "Healthy";
  return {
    hf,
    ltv,
    used,
    tone,
    label,
    liquidationPrice: p.debt > 0n ? liquidationPrice(collateral, p.debt, config.lltv) : null,
    price: p.price,
  };
}

/** How far ZEC's price can fall before the position can be liquidated, in basis points (null without debt). */
export function fallBeforeLiquidation(h: Health): bigint | null {
  if (h.liquidationPrice === null || h.price === 0n) return null;
  const left = 10_000n - (h.liquidationPrice * 10_000n) / h.price;
  return left > 0n ? left : 0n;
}

/** "ZEC can fall 44 % before liquidation ($1,265.00 → $707.54)", in words a borrower acts on. */
/** A fall in basis points as a borrower reads it: whole percents from 10 %, one decimal below. */
export const fallPercent = (bps: bigint) => (bps >= 1_000n ? `${bps / 100n}` : `${bps / 100n}.${(bps % 100n) / 10n}`);

export function fallSentence(h: Health): string {
  const fall = fallBeforeLiquidation(h);
  if (fall === null) return "No debt: nothing can be liquidated.";
  if (fall === 0n) return `Liquidatable at today's price (${formatUsdPrice(h.price)}).`;
  return `ZEC can fall ${fallPercent(fall)} % before liquidation (${formatUsdPrice(h.price)} → ${formatUsdPrice(h.liquidationPrice!)}).`;
}

const TONE_TEXT: Record<HealthTone, string> = { ok: "text-ok", warn: "text-warn", danger: "text-danger", muted: "text-muted" };
const TONE_FILL: Record<HealthTone, string> = { ok: "bg-ok", warn: "bg-warn", danger: "bg-danger", muted: "bg-line" };

/** The runway's right end, in % LTV: the liquidation line (62.5 %) sits well inside it. */
const RUNWAY_MAX = 75;

/**
 * The health runway: loan-to-value along a track from 0 to 75 %, filled up to today's, with the 40 % target and the
 * liquidation line at the LLTV (hatched beyond); a borrow's preview slides along it. Above it, the health factor
 * and how far ZEC can fall. `s`: the track and the health factor only (a loan's row). The track is decorative:
 * every number is in its text.
 */
export function HealthRunway({ health, preview, size = "l" }: { health: Health | null; preview?: Health | null; size?: "l" | "m" | "s" }) {
  const lltv = Number((config.lltv * 10_000n) / WAD) / 100;
  const target = Number(TARGET_LTV_BPS) / 100;
  const at = (pct: number) => `${(Math.max(0, Math.min(RUNWAY_MAX, pct)) / RUNWAY_MAX) * 100}%`;
  const ltvOf = (h: Health | null | undefined) => (h?.ltv !== null && h?.ltv !== undefined ? Number(h.ltv) / 100 : 0);
  const shown = preview ?? health;
  const now = ltvOf(health);
  const next = preview ? ltvOf(preview) : null;
  const tone = shown?.tone ?? "muted";
  const hf = shown ? (shown.hf === null ? "∞" : formatWad(shown.hf)) : "—";

  const track = (
    <div aria-hidden="true" className={cx("relative", size === "s" ? "h-1.5 w-[120px]" : "h-2.5 w-full")}>
      <div className="absolute inset-0 overflow-hidden rounded-full bg-surface-3">
        {/* Past the liquidation line: hatched. */}
        <div
          className="absolute inset-y-0 right-0"
          style={{
            left: at(lltv),
            backgroundImage: "repeating-linear-gradient(45deg, rgb(242 107 94 / 0.28) 0 4px, transparent 4px 8px)",
          }}
        />
        <div className={cx("absolute inset-y-0 left-0 rounded-full transition-[width] duration-500", TONE_FILL[health?.tone ?? "muted"])} style={{ width: at(now) }} />
      </div>
      {size !== "s" ? (
        <>
          <span className="absolute top-1/2 h-3.5 w-px -translate-y-1/2 bg-soft/80" style={{ left: at(target) }} />
          <span className="absolute top-1/2 h-5 w-[2px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-danger" style={{ left: at(lltv) }} />
          {health && health.ltv !== null ? (
            <span
              className="absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-text shadow-[0_0_0_3px_var(--color-bg)] transition-[left] duration-200"
              style={{ left: at(now) }}
            />
          ) : null}
          {next !== null && Math.abs(next - now) > 0.05 ? (
            <>
              <span
                className="absolute top-1/2 h-0 -translate-y-1/2 border-t border-dotted border-soft transition-[left,width] duration-200"
                style={{ left: at(Math.min(now, next)), width: `calc(${at(Math.max(now, next))} - ${at(Math.min(now, next))})` }}
              />
              <span
                className="absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-dashed border-text bg-bg transition-[left] duration-200"
                style={{ left: at(next) }}
              />
            </>
          ) : null}
        </>
      ) : null}
    </div>
  );

  if (size === "s") {
    return (
      <span className="inline-flex items-center gap-2.5">
        {track}
        <span className={cx("text-[13px] font-bold tabular-nums", TONE_TEXT[tone])}>
          <span className="sr-only">Health factor </span>
          {hf}
        </span>
      </span>
    );
  }
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-baseline justify-between gap-x-5 gap-y-1">
        <p className="flex items-baseline gap-2">
          <span className="text-[13px] font-semibold text-muted">Health</span>
          <span className={cx("font-bold tabular-nums font-stretch-110%", size === "l" ? "text-[26px]" : "text-[22px]", TONE_TEXT[tone])}>{hf}</span>
          {shown ? <span className={cx("text-[13px] font-semibold", TONE_TEXT[tone])}>{shown.label}</span> : null}
        </p>
        {shown ? <p className={cx("text-soft", size === "l" ? "text-[14px]" : "text-[13px]")}>{fallSentence(shown)}</p> : null}
      </div>
      <div className="space-y-1.5">
        {track}
        {/* The two lines' names, each under its own tick. */}
        <div className="relative h-4 text-[11.5px] tabular-nums">
          <span className="absolute top-0 -translate-x-1/2 whitespace-nowrap text-muted" style={{ left: at(target) }}>
            target {target} %
          </span>
          <span className="absolute top-0 -translate-x-1/2 whitespace-nowrap text-danger" style={{ left: at(lltv) }}>
            {size === "m" ? "liq." : "liquidation"} {lltv} %
          </span>
        </div>
      </div>
      <p className="text-[11.5px] text-muted tabular-nums">
        <span>
          {health?.ltv !== null && health?.ltv !== undefined ? (
            <>
              LTV <strong className="font-semibold text-soft">{formatBps(health.ltv)}</strong>
              {preview?.ltv !== null && preview?.ltv !== undefined && preview.ltv !== health.ltv ? (
                <>
                  {" "}→ <strong className="font-semibold text-text">{formatBps(preview.ltv)}</strong> after
                </>
              ) : null}
            </>
          ) : (
            "LTV —"
          )}
        </span>
      </p>
    </div>
  );
}

/**
 * What leaves a vault at a close from collateral, as one bar: the market maker's share (it repaid the debt) and
 * yours (I7: together, all of the collateral). Above it the two amounts; decorative bar, real text.
 */
export function SplitBar({ mm, user, mmFor, userTo }: { mm: bigint; user: bigint; mmFor?: ReactNode; userTo?: ReactNode }) {
  const total = mm + user;
  const mmPct = total > 0n ? Number((mm * 1000n) / total) / 10 : 0;
  // Whole percents that add up to 100, and amounts to the hundred-thousandth that add up too (the exact ones are in
  // the terms and the payouts).
  const mmRound = Math.round(mmPct);
  const mmShown = round5(mm);
  const userShown = round5(total) - mmShown;
  return (
    <div className="space-y-2.5">
      <div aria-hidden="true" className="flex h-3 gap-[2px] overflow-hidden rounded-full">
        <span className="h-full rounded-l-full bg-soft/70" style={{ width: `${mmPct}%` }} />
        <span className="h-full flex-1 rounded-r-full bg-pool" />
      </div>
      <div className="grid gap-3 text-[13.5px] sm:grid-cols-2">
        <p>
          <span className="flex items-center gap-1.5 text-muted">
            <span aria-hidden="true" className="inline-block size-2 rounded-full bg-soft/70" />
            Market maker · {mmRound} %
          </span>
          <TokenAmount token="TAZ" value={formatZec(mmShown)} size="md" className="mt-1" />
          {mmFor ? <span className="mt-0.5 block text-[12.5px] text-muted">{mmFor}</span> : null}
        </p>
        <p className="sm:text-right">
          <span className="flex items-center gap-1.5 text-muted sm:justify-end">
            <span aria-hidden="true" className="inline-block size-2 rounded-full bg-pool" />
            You · {100 - mmRound} %
          </span>
          <TokenAmount token="TAZ" value={formatZec(userShown)} size="md" className="mt-1" />
          {userTo ? <span className="mt-0.5 block text-[12.5px] text-muted">{userTo}</span> : null}
        </p>
      </div>
    </div>
  );
}

