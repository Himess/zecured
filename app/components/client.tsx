"use client";

/**
 * Interactive UI primitives (client components).
 */
import Link from "next/link";
import { usePathname } from "next/navigation";
import qrcode from "qrcode-generator";
import { useEffect, useMemo, useState } from "react";
import { api } from "@/lib/api";
import { config } from "@/lib/config";
import { loanTokenName, realAndMock } from "@/lib/labels";
import { REPLAY } from "@/lib/replay/flag";
import { formatDenominations } from "@/lib/zcash";
import { IconCheck, IconCopy, IconEye, IconHome, IconLoans, IconMarket, IconSwap, IconVault } from "./icons";
import { cx, Notice } from "./ui";

/** Copies `value` to the clipboard; falls back to selecting text when the API is blocked. */
export function CopyButton({
  value,
  label = "Copy",
  size = "sm",
  className,
}: {
  value: string;
  label?: string;
  size?: "sm" | "md";
  className?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 2000);
    return () => clearTimeout(t);
  }, [state]);
  return (
    <button
      type="button"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setState("copied");
        } catch {
          setState("failed");
        }
      }}
      className={cx(
        "inline-flex items-center gap-1.5 rounded-full bg-surface-2 font-semibold text-soft shadow-[inset_0_0_0_1px_var(--color-line)] transition-colors hover:bg-surface-3 hover:text-text",
        size === "md" ? "min-h-11 px-5 text-sm" : "min-h-8 px-3 text-xs",
        state === "copied" && "text-ok",
        className,
      )}
    >
      {state === "copied" ? <IconCheck size={14} strokeWidth={2.4} /> : <IconCopy size={14} />}
      <span aria-live="polite">{state === "copied" ? "Copied" : state === "failed" ? "Copy failed: select and copy" : label}</span>
    </button>
  );
}

/** QR code rendered locally as SVG (no image service, no network). */
export function Qr({ value, label, size = 240 }: { value: string; label: string; size?: number }) {
  const { path, count } = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(value, "Byte");
    qr.make();
    const n = qr.getModuleCount();
    let d = "";
    for (let r = 0; r < n; r++) {
      for (let c = 0; c < n; c++) if (qr.isDark(r, c)) d += `M${c + 4} ${r + 4}h1v1h-1z`;
    }
    return { path: d, count: n + 8 };
  }, [value]);
  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${count} ${count}`}
      width={size}
      height={size}
      shapeRendering="crispEdges"
      className="h-auto max-w-full rounded-lg bg-white"
    >
      <rect width={count} height={count} fill="#ffffff" />
      <path d={path} fill="#000000" />
    </svg>
  );
}

/** Backend reachability indicator (pings our own backend only); `compact`: the header's pill. */
export function BackendStatus({ compact = false }: { compact?: boolean }) {
  const [up, setUp] = useState<boolean | null>(null);
  useEffect(() => {
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      let ok = false;
      try {
        ok = await api.health();
      } catch {
        ok = false;
      }
      if (stop) return;
      setUp(ok);
      timer = setTimeout(tick, ok ? 60_000 : 15_000);
    };
    void tick();
    return () => {
      stop = true;
      if (timer) clearTimeout(timer);
    };
  }, []);
  // DEMO: the replay has no backend; the simulator in this tab answers for it.
  const text = REPLAY ? "Replay in this browser" : up === null ? "Checking backend" : up ? "Backend online" : "Backend offline";
  return (
    <span
      className={cx(
        "inline-flex items-center gap-2 text-xs whitespace-nowrap text-muted",
        compact && "min-h-8 rounded-full bg-surface px-3 shadow-[inset_0_0_0_1px_var(--color-border)]",
      )}
      title={api.baseUrl}
    >
      <span aria-hidden="true" className={cx("size-2 rounded-full", REPLAY ? "bg-warn" : up === null ? "bg-faint" : up ? "dot-live bg-ok" : "bg-danger")} />
      <span aria-live="polite" className={cx(compact && "max-[1240px]:sr-only")}>
        {text}
      </span>
    </span>
  );
}

const NAV = [
  { href: "/", label: "Dashboard", short: "Home", Icon: IconHome, exact: true },
  { href: "/borrow", label: "Borrow", short: "Borrow", Icon: IconVault, exact: false },
  { href: "/loans", label: "My loans", short: "Loans", Icon: IconLoans, exact: false },
  { href: "/buy", label: "Swap", short: "Swap", Icon: IconSwap, exact: false },
  { href: "/market", label: "Market", short: "Market", Icon: IconMarket, exact: false },
  { href: "/world", label: "Privacy", short: "Privacy", Icon: IconEye, exact: false },
] as const;

function isActive(pathname: string, href: string, exact: boolean): boolean {
  if (exact) return pathname === href;
  // A loan's own pages belong to "My loans".
  if (href === "/loans" && ["/position", "/verify", "/refund"].some((p) => pathname === p || pathname.startsWith(`${p}/`))) return true;
  return pathname === href || pathname.startsWith(`${href}/`);
}

/** The header's navigation: icon pills, the current page raised. Hidden on phones (BottomNav). */
export function Nav() {
  const pathname = usePathname();
  return (
    <nav aria-label="Main" className="max-[860px]:hidden">
      <ul className="flex items-center gap-0.5">
        {NAV.map(({ href, label, Icon, exact }) => {
          const active = isActive(pathname, href, exact);
          return (
            <li key={href}>
              <Link
                href={href}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "inline-flex min-h-10 items-center gap-2 rounded-full px-3 text-[14px] font-semibold whitespace-nowrap no-underline transition-colors",
                  active ? "bg-surface-2 text-text shadow-[inset_0_0_0_1px_var(--color-line)]" : "text-muted hover:bg-surface hover:text-text",
                )}
              >
                <Icon size={17} className={active ? "text-accent" : undefined} />
                <span className="max-[1100px]:sr-only">{label}</span>
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

/** Phones: the same pages as a tab bar at the bottom of the screen. */
export function BottomNav() {
  const pathname = usePathname();
  return (
    <nav
      aria-label="Main"
      className="fixed inset-x-0 bottom-0 z-40 border-t border-hairline bg-bg/90 pb-[env(safe-area-inset-bottom)] backdrop-blur-xl min-[861px]:hidden"
    >
      <ul className="mx-auto grid max-w-lg grid-cols-6">
        {NAV.map(({ href, short, Icon, exact }) => {
          const active = isActive(pathname, href, exact);
          return (
            <li key={href}>
              <Link
                href={href}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "flex min-h-[58px] flex-col items-center justify-center gap-1 text-[11px] font-semibold no-underline",
                  active ? "text-text" : "text-muted",
                )}
              >
                <Icon size={20} className={active ? "text-accent" : undefined} />
                {short}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

/**
 * The collateral sizes the signers accept, for prose ("0.01 or 0.05 TAZ"). Rendered below
 * ConfigGate, so it shows GET /config's list, never a hard-coded copy.
 */
export function Denominations() {
  return <>{formatDenominations(config.denominations)}</>;
}

/** The market's loan token as the deployment names it ("USDC", "MockUSDC" on a fork); below ConfigGate. */
export function LoanTokenName() {
  return <>{loanTokenName()}</>;
}

/** What is real and what is a mock on this deployment (home page), from GET /config and the build. */
export function RealAndMockList() {
  const { real, mock } = realAndMock();
  return (
    <ul className="list-disc space-y-2 pl-5 text-sm text-soft">
      {real.map((r) => (
        <li key={r}>
          <span className="text-ok">Real: </span>
          {r}
        </li>
      ))}
      {mock.map((m) => (
        <li key={m}>
          <span className="text-warn">Mock: </span>
          {m}
        </li>
      ))}
    </ul>
  );
}

/** What a vault page shows when its URL fragment holds no vault id (lib/routes.ts). */
export function MissingVaultId({ fragment }: { fragment: string }) {
  return fragment ? (
    <Notice tone="danger" title="Not a vault id">
      <p>A vault id is 0x followed by 64 hexadecimal characters, after the # in the address.</p>
    </Notice>
  ) : (
    <Notice tone="info" title="No vault selected">
      <p>
        Open a loan from <Link href="/loans">My loans</Link>.
      </p>
    </Notice>
  );
}

/** True after the first client render (localStorage is only readable then). */
export function useMounted(): boolean {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  return mounted;
}
