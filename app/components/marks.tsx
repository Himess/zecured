/**
 * Other projects' marks, drawn from their owners' brand kits and used only to name the asset, the chain or the
 * protocol (as on the site, docs/SITE.md): the Zcash brandmark (the Zcash brand folder ZecHub keeps), the USDC
 * token logo (Circle's press kit) and Morpho's logo (brand.morpho.org). They are their owners' trademarks. Base is
 * named by a plain blue square, the shape our design gives Base everywhere. Server-safe: no hooks.
 */
import type { ReactNode } from "react";

const cx = (...c: (string | false | undefined)[]) => c.filter(Boolean).join(" ");

type MarkProps = { size?: number; className?: string; title?: string };

function svgProps(size: number, title: string | undefined, className: string | undefined) {
  return title
    ? { width: size, height: size, role: "img" as const, "aria-label": title, className: cx("flex-none", className) }
    : { width: size, height: size, "aria-hidden": true as const, className: cx("flex-none", className) };
}

/** The Zcash brandmark in its yellow: a gold disc with the Z cut through it (fill-rule evenodd). */
export function ZecMark({ size = 20, className, title }: MarkProps) {
  return (
    <svg viewBox="270 270 540 540" {...svgProps(size, title, className)}>
      <path
        fill="#F4B728"
        fillRule="evenodd"
        d="m270,540c0-148.9,121.1-270,270-270s270,121.1,270,270-121.1,270-270,270-270-121.1-270-270Zm366.31-125.3v41.09l-114.28,155h114.28v54.5h-73.67v45.16h-45.28v-45.16h-73.67v-41.09l114.16-155h-114.16v-54.5h73.67v-45.28h45.28v45.28h73.67Z"
      />
    </svg>
  );
}

/** Circle's USDC token logo, as its press kit draws it. */
export function UsdcMark({ size = 20, className, title }: MarkProps) {
  return (
    <svg viewBox="1 1 94 94" {...svgProps(size, title, className)}>
      <path d="M48 95C73.9574 95 95 73.9574 95 48C95 22.0426 73.9574 1 48 1C22.0426 1 1 22.0426 1 48C1 73.9574 22.0426 95 48 95Z" fill="#0B53BF" />
      <path
        d="M56.4609 13.7778V19.8291C68.5341 23.4716 77.3759 34.6928 77.3759 47.9997C77.3759 61.3066 68.5341 72.5278 56.4609 76.1703V82.2216C71.8534 78.4616 83.2509 64.5672 83.2509 47.9997C83.2509 31.4322 71.8534 17.5378 56.4609 13.7778Z"
        fill="#fff"
      />
      <path
        d="M18.625 47.9997C18.625 34.6928 27.4669 23.4716 39.54 19.8291V13.7778C24.1475 17.5378 12.75 31.4322 12.75 47.9997C12.75 64.5672 24.1475 78.4616 39.54 82.2216V76.1703C27.4669 72.5572 18.625 61.3066 18.625 47.9997Z"
        fill="#fff"
      />
      <path
        d="M60.6319 54.5506C60.6319 42.5362 41.8025 47.4713 41.8025 40.8325C41.8025 38.4531 43.7119 36.9256 47.3544 36.9256C51.7019 36.9256 53.2 39.0406 53.67 41.89H59.6625C59.1279 36.5426 56.0588 33.1662 50.9382 32.1604V27.4375H45.0632V31.9918C39.4534 32.7062 35.9275 35.973 35.9275 40.8325C35.9275 52.9056 54.7863 48.3819 54.7863 54.9031C54.7863 57.3706 52.4069 59.0156 48.3825 59.0156C43.1244 59.0156 41.3913 56.695 40.745 53.4931H34.8994C35.2781 59.3502 38.8897 63.0159 45.0632 63.9307V68.5625H50.9382V63.9923C56.9633 63.2139 60.6319 59.7089 60.6319 54.5506Z"
        fill="#fff"
      />
    </svg>
  );
}

/** Morpho's logo with its background (brand.morpho.org), its square's corners rounded like an app icon. */
export function MorphoMark({ size = 20, className, title }: MarkProps) {
  return (
    <svg viewBox="0 0 200 200" {...svgProps(size, title, className)}>
      <rect width="200" height="200" rx="46" fill="#2973FF" />
      <path
        opacity="0.8"
        d="M58.6604 117.953V147.059C58.6604 148.851 60.1768 149.595 60.6486 149.764C61.1204 149.967 62.7042 150.406 64.1195 149.088L86.1389 127.927C88.0141 126.126 89.8238 124.231 91.1838 122.015C91.8239 120.973 92.0893 120.387 92.0893 120.387C93.4373 117.648 93.4373 115.011 92.1226 112.375C90.1689 108.453 85.1811 104.464 77.666 100.678L64.8272 107.845C61.0194 110.008 58.6604 113.828 58.6604 117.953Z"
        fill="#fff"
      />
      <path
        d="M46 53.2138V83.7407C46 87.5607 48.5611 90.9413 52.2006 92.0231C64.6016 95.6065 86.2021 103.314 91.4255 114.639C92.099 116.127 92.5035 117.58 92.6392 119.102C96.1099 112.78 97.6936 105.512 96.9523 98.1419C95.941 87.6959 90.4142 78.1964 81.7878 72.1451L51.0211 50.6107C50.4819 50.205 49.8416 50.0022 49.2014 50.0022C48.6621 50.0022 48.1904 50.1036 47.6849 50.3741C46.6739 50.9488 46 51.9968 46 53.2138Z"
        fill="#fff"
      />
      <path
        opacity="0.8"
        d="M140.559 117.953V147.059C140.559 148.851 139.043 149.595 138.57 149.764C138.099 149.967 136.514 150.406 135.1 149.088L112.568 127.435C111.034 125.961 109.569 124.402 108.414 122.615C107.499 121.201 107.13 120.387 107.13 120.387C105.782 117.648 105.782 115.011 107.096 112.375C109.051 108.453 114.038 104.464 121.552 100.678L134.392 107.845C138.234 110.008 140.559 113.828 140.559 117.953Z"
        fill="#fff"
      />
      <path
        d="M153.24 53.2116V83.7384C153.24 87.5585 150.678 90.9391 147.038 92.0209C134.638 95.6043 113.037 103.312 107.814 114.637C107.139 116.125 106.735 117.578 106.6 119.1C103.13 112.778 101.546 105.51 102.287 98.1398C103.297 87.6937 108.824 78.1943 117.452 72.143L148.219 50.6085C148.758 50.2028 149.398 50 150.038 50C150.577 50 151.049 50.1014 151.554 50.3719C152.565 50.9466 153.24 51.9946 153.24 53.2116Z"
        fill="#fff"
      />
    </svg>
  );
}

/** Base, named by the shape our design gives it: a blue square. */
export function BaseMark({ size = 20, className, title }: MarkProps) {
  return (
    <svg viewBox="0 0 20 20" {...svgProps(size, title, className)}>
      <rect x="1" y="1" width="18" height="18" rx="1.5" fill="#0052FF" />
    </svg>
  );
}

/**
 * zZEC, Zecured's collateral receipt: the ZEC it stands for inside a ring, because it is ZEC held, not ZEC moved.
 * It never leaves Morpho, so it has no market and no logo of its own.
 */
export function ZzecMark({ size = 20, className, title }: MarkProps) {
  return (
    <span
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      className={cx("relative inline-flex flex-none items-center justify-center rounded-full", className)}
      style={{ width: size, height: size, boxShadow: "inset 0 0 0 1.5px #F4B728", background: "#0d1611" }}
    >
      <ZecMark size={Math.round(size * 0.66)} />
    </span>
  );
}

export type Token = "ZEC" | "TAZ" | "USDC" | "zZEC";

/** A token's mark: ZEC (TAZ on testnet is the same coin's test network), USDC, zZEC. */
export function TokenMark({ token, size = 20, className }: { token: Token; size?: number; className?: string }) {
  if (token === "USDC") return <UsdcMark size={size} className={className} />;
  if (token === "zZEC") return <ZzecMark size={size} className={className} />;
  return <ZecMark size={size} className={className} />;
}

/** Two marks overlapping, the first in front: a market's pair (zZEC / USDC). */
export function PairMarks({ a, b, size = 28 }: { a: ReactNode; b: ReactNode; size?: number }) {
  return (
    <span className="relative inline-flex flex-none items-center" style={{ width: size * 1.62, height: size }} aria-hidden="true">
      <span className="absolute left-0 z-[1] rounded-full ring-2 ring-surface">{a}</span>
      <span className="absolute right-0">{b}</span>
    </span>
  );
}

/**
 * The two chains the app works across, as one pill: "Zcash testnet | Base Sepolia", the hairline between them a
 * seam (not a bridge), names hidden below 1600 px; `status` ends it (the backend's dot, or the replay's mark).
 */
export function NetworkPill({
  zcash,
  base,
  short,
  status,
  className,
}: {
  zcash: string;
  base: string;
  /** What the pill says while the names are hidden ("Testnet"). */
  short?: string;
  status?: ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cx(
        "inline-flex min-h-8 items-center gap-2 rounded-full bg-surface px-3 text-[13px] font-semibold whitespace-nowrap text-soft shadow-[inset_0_0_0_1px_var(--color-border)]",
        className,
      )}
    >
      <ZecMark size={16} />
      <span className="max-[1600px]:sr-only">{zcash}</span>
      <span aria-hidden="true" className="h-3.5 w-px bg-line" />
      <BaseMark size={14} />
      <span className="max-[1600px]:sr-only">{base}</span>
      {short ? (
        <span aria-hidden="true" className="text-muted min-[1601px]:hidden">
          {short}
        </span>
      ) : null}
      {status ? (
        <>
          <span aria-hidden="true" className="h-3.5 w-px bg-line" />
          {status}
        </>
      ) : null}
    </span>
  );
}

/** A chain's chip: its mark and its name ("Zcash testnet", "Base Sepolia"). */
export function ChainPill({ chain, label, className }: { chain: "zcash" | "base"; label: string; className?: string }) {
  return (
    <span
      className={cx(
        "inline-flex min-h-8 items-center gap-2 rounded-full bg-surface px-3 text-[13px] font-semibold whitespace-nowrap text-soft shadow-[inset_0_0_0_1px_var(--color-border)]",
        className,
      )}
    >
      {chain === "zcash" ? <ZecMark size={16} /> : <BaseMark size={15} />}
      {label}
    </span>
  );
}
