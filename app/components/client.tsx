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
import { formatDenominations } from "@/lib/zcash";
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
        "inline-flex items-center border border-border text-muted hover:border-faint hover:text-text",
        size === "md" ? "min-h-11 rounded-lg bg-surface-2 px-4 text-sm" : "min-h-9 rounded-md px-3 text-xs",
        className,
      )}
    >
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

/** Backend reachability indicator (pings our own backend only). */
export function BackendStatus() {
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
  const text = up === null ? "Checking backend" : up ? "Backend online" : "Backend offline";
  return (
    <span className="inline-flex items-center gap-2 text-xs text-muted" title={api.baseUrl}>
      <span
        aria-hidden="true"
        className={cx("h-2 w-2 rounded-full", up === null ? "bg-faint" : up ? "bg-ok" : "bg-danger")}
      />
      <span aria-live="polite">{text}</span>
    </span>
  );
}

const NAV = [
  { href: "/borrow", label: "Borrow" },
  { href: "/loans", label: "My loans" },
  { href: "/buy", label: "Buy shielded ZEC" },
  { href: "/market", label: "Market" },
  { href: "/prepare", label: "Prepare" },
  { href: "/world", label: "What the world sees" },
] as const;

export function Nav() {
  const pathname = usePathname();
  return (
    <nav aria-label="Main">
      <ul className="flex flex-wrap items-center gap-1 text-sm">
        {NAV.map((n) => {
          const active = pathname === n.href || pathname.startsWith(`${n.href}/`);
          return (
            <li key={n.href}>
              <Link
                href={n.href}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "inline-flex min-h-9 items-center rounded-md px-3 no-underline",
                  active ? "bg-surface-2 text-text" : "text-muted hover:bg-surface-2 hover:text-text",
                )}
              >
                {n.label}
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
    <ul className="list-disc space-y-2 pl-5 text-sm text-muted">
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
