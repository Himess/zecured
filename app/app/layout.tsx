import type { Metadata, Viewport } from "next";
import localFont from "next/font/local";
import Link from "next/link";
import type { ReactNode } from "react";
import { BackendStatus, Nav } from "@/components/client";
import { ConfigGate } from "@/components/ConfigGate";
import { SeamMark } from "@/components/seam";
import { Badge } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import "./globals.css";

const TESTNET = DEPLOYMENT.kind === "testnet";

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
    ? "Borrow against shielded ZEC. Sell nothing. Reveal nothing. Testnet experiment, not a financial product."
    : "Borrow against shielded ZEC. Sell nothing. Reveal nothing. Mainnet beta: unaudited, invite only, small caps.",
  referrer: "no-referrer",
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  themeColor: "#0D1611",
  colorScheme: "dark",
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${archivo.variable} ${martianMono.variable}`}>
      <body className="min-h-dvh antialiased">
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-50 focus:rounded-full focus:bg-surface-2 focus:px-4 focus:py-2"
        >
          Skip to content
        </a>
        {DEPLOYMENT.band ? (
          <div role="note" className="border-b border-warn/60 bg-warn/10 px-4 py-2 text-center text-sm font-medium text-warn">
            {DEPLOYMENT.band}
          </div>
        ) : null}
        <header className="border-b border-hairline">
          <div className="wrap flex min-h-[72px] flex-wrap items-center justify-between gap-x-7 gap-y-3 py-3">
            <div className="flex items-center gap-3.5">
              <Link href="/" className="inline-flex items-center gap-[11px] text-text no-underline">
                <SeamMark />
                <span className="text-[19px] font-extrabold font-stretch-125%">Zecured</span>
              </Link>
              <Badge tone="neutral">{TESTNET ? "Testnet" : "Mainnet beta"}</Badge>
            </div>
            <Nav />
          </div>
        </header>
        <main id="main" className="wrap pt-8 pb-[72px] sm:pt-10">
          <ConfigGate>{children}</ConfigGate>
        </main>
        <footer className="border-t border-hairline">
          <div className="wrap flex flex-col gap-x-10 gap-y-3 py-6 text-[13.5px] text-faint sm:flex-row sm:items-center sm:justify-between">
            <p className="max-w-2xl">
              {TESTNET ? (
                <>
                  Zecured is a ZECATHON 2026 testnet experiment, <strong className="font-semibold text-soft">not a financial product</strong>.
                  Testnet funds only: TAZ on Zcash testnet and test USDC on Base Sepolia.
                </>
              ) : (
                <>
                  Zecured mainnet beta: <strong className="font-semibold text-soft">unaudited, invite only, small caps</strong>. Use only
                  what you can afford to lose.
                </>
              )}
            </p>
            <div className="flex flex-wrap items-center gap-x-6 gap-y-2 sm:shrink-0">
              <BackendStatus />
              <Link href="/status" className="text-soft no-underline hover:text-text hover:underline">
                Status
              </Link>
              <Link href="/evidence" className="text-soft no-underline hover:text-text hover:underline">
                Evidence
              </Link>
              {TESTNET ? (
                <Link href="/demo" className="text-soft no-underline hover:text-text hover:underline">
                  Demo panel
                </Link>
              ) : null}
            </div>
          </div>
        </footer>
      </body>
    </html>
  );
}
