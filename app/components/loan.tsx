/**
 * A loan's figures as the app draws them everywhere: an amount with its token's mark, the health dial, and the
 * health of a position read from Base (lib/chain.ts `PositionSnapshot`). Server-safe: no hooks.
 */
import type { ReactNode } from "react";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { loanTokenName } from "@/lib/labels";
import { formatBps, formatUsdc, formatWad, healthFactor, liquidationPrice, ltvBps, WAD } from "@/lib/morpho";
import { formatZec, TICKER } from "@/lib/zcash";
import { TokenMark, type Token } from "./marks";
import { cx, Ring } from "./ui";

/** An amount with its token's mark: "◉ 0.05 TAZ", "◉ 12.50 USDC". `size` sets the figure's scale. */
export function TokenAmount({
  token,
  value,
  unit,
  size = "md",
  className,
}: {
  token: Token;
  value: ReactNode;
  /** The unit after the figure; the token's own name by default ("TAZ" for testnet ZEC). */
  unit?: ReactNode;
  size?: "sm" | "md" | "lg" | "xl";
  className?: string;
}) {
  const mark = { sm: 16, md: 20, lg: 26, xl: 34 }[size];
  const text = { sm: "text-[14px]", md: "text-[16px]", lg: "text-[22px]", xl: "text-[clamp(26px,3vw,34px)]" }[size];
  return (
    <span className={cx("inline-flex min-w-0 items-center gap-2 font-bold tabular-nums font-stretch-112%", text, className)}>
      <TokenMark token={token} size={mark} />
      <span className="truncate">
        {value}
        {unit !== null ? (
          <span className="ml-1.5 font-semibold text-muted">
            {unit ?? (token === "ZEC" || token === "TAZ" ? TICKER : token === "USDC" ? loanTokenName() : token)}
          </span>
        ) : null}
      </span>
    </span>
  );
}

/** Zatoshi as "0.05" (the unit is drawn by TokenAmount). */
export const zec = (zat: bigint) => formatZec(zat);
/** USDC units as "12.50", thousands grouped for display ("100,000.00"). */
export const usdc = (units: bigint) => {
  const [whole, frac] = formatUsdc(units).split(".");
  return `${whole!.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}${frac !== undefined ? `.${frac}` : ""}`;
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
  /** "Healthy", "Watch it", "At risk", "Liquidatable". */
  label: string;
  /** Liquidation price (oracle scale), null without debt. */
  liquidationPrice: bigint | null;
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
  return { hf, ltv, used, tone, label, liquidationPrice: p.debt > 0n ? liquidationPrice(collateral, p.debt, config.lltv) : null };
}

/** The health dial: how far the position is from liquidation, its health factor in the middle. */
export function HealthRing({ health, size = 132 }: { health: Health | null; size?: number }) {
  if (!health) {
    return (
      <Ring value={0} size={size} tone="muted">
        <span className="text-[13px] text-muted">Health</span>
        <span className="text-[22px] font-bold text-faint">—</span>
      </Ring>
    );
  }
  const text = { ok: "text-ok", warn: "text-warn", danger: "text-danger", muted: "text-muted" }[health.tone];
  return (
    <Ring value={health.used} size={size} tone={health.tone}>
      <span className="text-[12px] font-semibold text-muted">Health factor</span>
      <span className={cx("font-bold tabular-nums font-stretch-112%", size >= 120 ? "text-[28px] leading-tight" : "text-[18px]", text)}>
        {health.hf === null ? "∞" : formatWad(health.hf)}
      </span>
      {size >= 120 ? <span className={cx("text-[12px] font-semibold", text)}>{health.label}</span> : null}
    </Ring>
  );
}

/** "40.0 %" or "—". */
export function ltvText(h: Health | null): string {
  return h?.ltv !== null && h?.ltv !== undefined ? formatBps(h.ltv) : "—";
}
