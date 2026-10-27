import type { Metadata, Viewport } from "next";
import localFont from "next/font/local";
import Link from "next/link";
import type { ReactNode } from "react";
import { BackendDot, BackendStatus, BottomNav, Nav } from "@/components/client";
import { ConfigGate } from "@/components/ConfigGate";
import { NetworkPill } from "@/components/marks";
import { ReplayBand, ReplayDock } from "@/components/replay/ReplayDock";
import { SeamMark } from "@/components/seam";
import { Badge } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY, REPLAY_CSP } from "@/lib/replay/flag";
import "./globals.css";

const TESTNET = DEPLOYMENT.kind === "testnet";

/**
 * The site and its docs: beside the app on Pages (`/<repo>/` next to `/<repo>/app/`, or `/` next to `/app/` on a
 * custom domain), else the public site.
 */
const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH ?? "";
const SITE_URL = BASE_PATH
  ? `${BASE_PATH.replace(/\/app\/?$/, "")}/`
  : (process.env.NEXT_PUBLIC_SITE_URL ?? "https://himess.github.io/zecured/");

/*
 * Archivo for text and Martian Mono for chain data, both variable in weight and width and
 * self-hosted from app/fonts (OFL, fonts/OFL.txt): the build reads them from disk, the browser
 * fetches them from this app only. The width ranges are declared so `font-stretch` reaches them.
 */
const archivo = localFont({
  src: "./fonts/archivo.woff2",
  weight: "100 900",
  display: "swap",
  variable: "--font-archivo",
  declarations: [{ prop: "font-stretch", value: "62% 125%" }],
});

const martianMono = localFont({
  src: "./fonts/martian-mono.woff2",
  weight: "100 800",
  display: "swap",
  variable: "--font-martian-mono",
  // An Arial-metric fallback would set chain data in a proportional face: ui-monospace follows instead.
  adjustFontFallback: false,
  declarations: [{ prop: "font-stretch", value: "75% 112.5%" }],
});

export const metadata: Metadata = {
  title: { default: "Zecured: borrow against shielded ZEC", template: "%s · Zecured" },
  description: TESTNET
    ? "Borrow against shielded ZEC. Only the proof crosses. A testnet experiment, not a financial product."
    : "Borrow against shielded ZEC. Only the proof crosses. Mainnet beta: unaudited, invite only, small caps.",
  referrer: "no-referrer",
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  themeColor: "#070D0A",
  colorScheme: "dark",
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${archivo.variable} ${martianMono.variable}${REPLAY ? " has-dock" : ""}`}>
      <body className="min-h-dvh antialiased">
        {/* A static replay build has no server to send headers: its policy rides in the page (lib/replay/flag.ts). */}
        {REPLAY ? (
          <meta
            httpEquiv="Content-Security-Policy"
            // `next dev` needs eval for React's debugging, as the live build's header allows in development.
            content={process.env.NODE_ENV === "development" ? REPLAY_CSP.replace("'unsafe-inline'", "'unsafe-inline' 'unsafe-eval'") : REPLAY_CSP}
          />
        ) : null}
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-50 focus:rounded-full focus:bg-surface-2 focus:px-4 focus:py-2"
        >
          Skip to content
        </a>
        {REPLAY ? <ReplayBand /> : null}
        {DEPLOYMENT.band ? (
          <div role="note" className="border-b border-warn/60 bg-warn/10 px-4 py-2 text-center text-sm font-medium text-warn">
            {DEPLOYMENT.band}
          </div>
        ) : null}
        <header className="sticky top-0 z-40 border-b border-hairline bg-bg/95 backdrop-blur-xl">
          <div className="wrap flex min-h-16 items-center gap-x-6 py-2.5">
            <div className="flex items-center gap-3">
              <Link href="/" className="inline-flex items-center gap-[10px] text-text no-underline">
                <SeamMark size={28} />
                <span className="text-[19px] font-extrabold font-stretch-125%">Zecured</span>
              </Link>
              {TESTNET ? null : <Badge tone="neutral">Mainnet beta</Badge>}
            </div>
            <Nav />
            <div className="ml-auto flex items-center">
              <NetworkPill
                zcash={TESTNET ? "Zcash testnet" : "Zcash"}
                base={DEPLOYMENT.chainName}
                short={TESTNET ? "Testnet" : undefined}
                status={REPLAY ? undefined : <BackendDot />}
              />
            </div>
          </div>
        </header>
        {/* Clear of the fixed bars below: the phones' tab bar and, in the replay, its dock (globals.css). */}
        <main id="main" className="wrap min-h-dvh pt-7 pb-[calc(var(--dock-h)+var(--nav-h)+4rem)] sm:pt-9">
          <noscript>
            <p className="mb-6 rounded-2xl bg-surface-2 p-4 text-[15px] text-soft">
              This app needs JavaScript: it checks the deployment and the signers&apos; signatures in your browser. The{" "}
              <a href={`${SITE_URL}docs/`}>docs page</a> reads without it.
            </p>
          </noscript>
          <ConfigGate>{children}</ConfigGate>
        </main>
        <footer className="border-t border-hairline pb-[calc(var(--dock-h)+var(--nav-h))]">
          <div className="wrap grid gap-x-10 gap-y-8 py-10 text-[13.5px] text-muted md:grid-cols-[minmax(0,1.4fr)_repeat(2,minmax(0,1fr))]">
            <div className="space-y-3">
              <p className="inline-flex items-center gap-2.5 text-text">
                <SeamMark size={22} />
                <span className="text-[16px] font-extrabold font-stretch-125%">Zecured</span>
              </p>
              <p className="max-w-md">
                {TESTNET ? (
                  <>
                    Borrow against shielded ZEC without unshielding it. A ZECATHON 2026 testnet experiment,{" "}
                    <strong className="font-semibold text-soft">not a financial product</strong>: TAZ on Zcash testnet and test
                    tokens on Base Sepolia only.
                  </>
                ) : (
                  <>
                    Zecured mainnet beta: <strong className="font-semibold text-soft">unaudited, invite only, small caps</strong>.
                    Use only what you can afford to lose.
                  </>
                )}
              </p>
            </div>
            <nav aria-label="The app" className="space-y-2.5">
              <p className="text-xs font-bold tracking-[0.08em] text-faint uppercase">Use it</p>
              <ul className="space-y-2">
                <li><Link href="/borrow" className="text-soft no-underline hover:text-text hover:underline">Borrow against ZEC</Link></li>
                <li><Link href="/buy" className="text-soft no-underline hover:text-text hover:underline">Buy shielded ZEC</Link></li>
                <li><Link href="/prepare" className="text-soft no-underline hover:text-text hover:underline">Prepare your ZEC</Link></li>
                <li><Link href="/loans" className="text-soft no-underline hover:text-text hover:underline">Your loans and key backups</Link></li>
              </ul>
            </nav>
            <nav aria-label="Proof" className="space-y-2.5">
              <p className="text-xs font-bold tracking-[0.08em] text-faint uppercase">Check it</p>
              <ul className="space-y-2">
                <li><Link href="/evidence" className="text-soft no-underline hover:text-text hover:underline">{REPLAY ? "Evidence (sample data in this replay)" : "Evidence of real transactions"}</Link></li>
                <li><Link href="/status" className="text-soft no-underline hover:text-text hover:underline">System status</Link></li>
                <li><Link href="/world" className="text-soft no-underline hover:text-text hover:underline">What the world sees</Link></li>
                <li><a href={SITE_URL} className="text-soft no-underline hover:text-text hover:underline">The site</a> · <a href={`${SITE_URL}docs/`} className="text-soft no-underline hover:text-text hover:underline">the docs</a></li>
                {/* The operators' panel needs our backend's admin token: not in the judges' replay. */}
                {TESTNET && !REPLAY ? (
                  <li><Link href="/demo" className="text-soft no-underline hover:text-text hover:underline">Demo panel</Link></li>
                ) : null}
              </ul>
            </nav>
          </div>
          <div className="wrap flex flex-wrap items-center justify-between gap-3 border-t border-hairline py-5 text-xs text-faint">
            <p>ZEC, USDC and Morpho marks belong to their owners and only name the asset or protocol.</p>
            <BackendStatus />
          </div>
        </footer>
        <BottomNav />
        {REPLAY ? <ReplayDock /> : null}
      </body>
    </html>
  );
}
