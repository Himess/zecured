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
import { useLoans } from "@/lib/hooks";
import { loanTokenName, realAndMock } from "@/lib/labels";
import { REPLAY } from "@/lib/replay/flag";
import { formatDenominations } from "@/lib/zcash";
import { IconCheck, IconCopy, IconEye, IconHome, IconLoans, IconPlay, IconSwap, IconVault } from "./icons";
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

/** Whether our backend answers (`GET /health`): one poll for the whole page, however many indicators show it. */
let backendUp: boolean | null = null;
let backendTimer: ReturnType<typeof setTimeout> | undefined;
const backendListeners = new Set<() => void>();

async function pollBackend() {
  let ok = false;
  try {
    ok = await api.health();
  } catch {
    ok = false;
  }
  backendUp = ok;
  backendListeners.forEach((l) => l());
  backendTimer = backendListeners.size ? setTimeout(pollBackend, ok ? 60_000 : 15_000) : undefined;
}

function useBackendUp(): boolean | null {
  const [, render] = useState(0);
  useEffect(() => {
    // DEMO: the replay has no backend; the simulator in this tab answers for it.
    if (REPLAY) return;
    const listener = () => render((n) => n + 1);
    backendListeners.add(listener);
    if (!backendTimer) void pollBackend();
    return () => {
      backendListeners.delete(listener);
      if (!backendListeners.size && backendTimer) {
        clearTimeout(backendTimer);
        backendTimer = undefined;
      }
    };
  }, []);
  return backendUp;
}

/** The backend's state in words, or the replay's. */
function backendText(up: boolean | null): string {
  return REPLAY ? "Replay in this browser: no backend" : up === null ? "Checking the backend" : up ? "Backend online" : "Backend offline";
}

/** The end of the header's network pill: a dot for the backend (announced when it changes), the replay's mark in DEMO. */
export function BackendDot() {
  const up = useBackendUp();
  return (
    <span className="inline-flex items-center" title={REPLAY ? backendText(up) : `${backendText(up)} (${api.baseUrl})`}>
      {REPLAY ? (
        <IconPlay size={13} strokeWidth={2.2} className="text-demo" />
      ) : (
        <span aria-hidden="true" className={cx("size-2 rounded-full", up === null ? "bg-faint" : up ? "bg-ok" : "bg-danger")} />
      )}
      <span aria-live="polite" className="sr-only">
        {backendText(up)}
      </span>
    </span>
  );
}

/** The backend's state in a line (the footer's); not announced (the header's dot is). */
export function BackendStatus() {
  const up = useBackendUp();
  return (
    <span className="inline-flex items-center gap-2 text-xs whitespace-nowrap text-muted" title={api.baseUrl}>
      {REPLAY ? (
        <IconPlay size={12} strokeWidth={2.2} className="text-demo" />
      ) : (
        <span aria-hidden="true" className={cx("size-2 rounded-full", up === null ? "bg-faint" : up ? "bg-ok" : "bg-danger")} />
      )}
      {backendText(up)}
    </span>
  );
}

/** The pages, as the header names them (the logo goes home) and as the phones' tab bar shows them. */
const NAV = [
  { href: "/", label: "Home", Icon: IconHome, exact: true, header: false, tab: true },
  { href: "/borrow", label: "Borrow", Icon: IconVault, exact: false, header: true, tab: true },
  { href: "/loans", label: "Loans", Icon: IconLoans, exact: false, header: true, tab: true },
  { href: "/buy", label: "Swap", Icon: IconSwap, exact: false, header: true, tab: true },
  { href: "/market", label: "Market", Icon: IconLoans, exact: false, header: true, tab: false },
  { href: "/world", label: "Privacy", Icon: IconEye, exact: false, header: true, tab: true },
] as const;

function isActive(pathname: string, href: string, exact: boolean): boolean {
  if (exact) return pathname === href;
  // A loan's own pages belong to "Loans".
  if (href === "/loans" && ["/position", "/verify", "/refund"].some((p) => pathname === p || pathname.startsWith(`${p}/`))) return true;
  return pathname === href || pathname.startsWith(`${href}/`);
}

/** The header's navigation, in words as on the site: the current page white on a faint pill. Hidden on phones. */
export function Nav() {
  const pathname = usePathname();
  const { loans, ready } = useLoans();
  return (
    <nav aria-label="Main" className="max-[860px]:hidden">
      <ul className="flex items-center gap-1">
        {NAV.filter((n) => n.header).map(({ href, label, exact }) => {
          const active = isActive(pathname, href, exact);
          return (
            <li key={href}>
              <Link
                href={href}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "inline-flex min-h-9 items-center gap-1.5 rounded-full px-3 text-[15px] font-medium whitespace-nowrap no-underline transition-colors",
                  active ? "bg-white/[0.07] text-text" : "text-soft hover:text-text",
                )}
              >
                {label}
                {href === "/loans" && ready && loans.length > 0 ? (
                  <span className="inline-flex min-w-[18px] items-center justify-center rounded-full bg-surface-3 px-1 text-[11px] font-semibold text-soft tabular-nums">
                    {loans.length}
                    <span className="sr-only"> in this browser</span>
                  </span>
                ) : null}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

/** Phones: five pages as a tab bar at the bottom of the screen; the current one white, a bar above it. */
export function BottomNav() {
  const pathname = usePathname();
  return (
    <nav aria-label="Main" className="fixed inset-x-0 bottom-0 z-40 border-t border-hairline bg-bg pb-[env(safe-area-inset-bottom)] min-[861px]:hidden">
      <ul className="mx-auto grid max-w-lg grid-cols-5">
        {NAV.filter((n) => n.tab).map(({ href, label, Icon, exact }) => {
          const active = isActive(pathname, href, exact);
          return (
            <li key={href}>
              <Link
                href={href}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "relative flex min-h-[58px] flex-col items-center justify-center gap-1 text-[11px] font-semibold no-underline",
                  active ? "text-text" : "text-muted",
                )}
              >
                {active ? <span aria-hidden="true" className="absolute top-0 h-[2px] w-4 rounded-full bg-text" /> : null}
                <Icon size={20} />
                {label}
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
        Open a loan from <Link href="/loans">your loans</Link>.
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

/** Seconds since `active` last turned true on this page (0 while false), ticking once a second. */
export function useElapsed(active: boolean): number {
  const [start, setStart] = useState<number | null>(null);
  const [now, setNow] = useState(0);
  useEffect(() => {
    if (!active) {
      setStart(null);
      return;
    }
    const t0 = Date.now();
    setStart(t0);
    setNow(t0);
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return start === null ? 0 : Math.max(0, Math.floor((now - start) / 1000));
}

/** "0:21", "4:05", "1:02:10". */
export function clock(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = String(seconds % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/**
 * A figure that counts up to `value` once, as a result settles (600 ms); its final text at once under reduced
 * motion. `value` is the figure's text ("0.04975"): the count keeps its decimals.
 */
export function CountUp({ value }: { value: string }) {
  const [shown, setShown] = useState(value);
  useEffect(() => {
    const target = Number(value);
    const decimals = value.includes(".") ? value.split(".")[1]!.length : 0;
    if (!Number.isFinite(target) || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setShown(value);
      return;
    }
    const t0 = performance.now();
    let frame = 0;
    const step = (t: number) => {
      const k = Math.min(1, (t - t0) / 600);
      const eased = 1 - (1 - k) ** 3;
      setShown(k >= 1 ? value : (target * eased).toFixed(decimals));
      if (k < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [value]);
  return <span className="tabular-nums">{shown}</span>;
}
