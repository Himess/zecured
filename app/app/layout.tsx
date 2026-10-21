import type { Metadata, Viewport } from "next";
import Link from "next/link";
import { GeistMono } from "geist/font/mono";
import { GeistSans } from "geist/font/sans";
import type { ReactNode } from "react";
import { BackendStatus, Nav } from "@/components/client";
import { ConfigGate } from "@/components/ConfigGate";
import { DEPLOYMENT } from "@/lib/deployment";
import "./globals.css";

const TESTNET = DEPLOYMENT.kind === "testnet";

export const metadata: Metadata = {
  title: { default: "Zecured: borrow against shielded ZEC", template: "%s · Zecured" },
  description: TESTNET
    ? "Borrow against shielded ZEC. Sell nothing. Reveal nothing. Testnet experiment, not a financial product."
    : "Borrow against shielded ZEC. Sell nothing. Reveal nothing. Mainnet beta: unaudited, invite only, small caps.",
  referrer: "no-referrer",
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  themeColor: "#0b0e12",
  colorScheme: "dark",
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${GeistSans.variable} ${GeistMono.variable}`}>
      <body className="min-h-dvh antialiased">
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-50 focus:rounded-md focus:bg-surface-2 focus:px-3 focus:py-2"
        >
          Skip to content
        </a>
        {DEPLOYMENT.band ? (
          <div role="note" className="border-b border-warn/60 bg-warn/10 px-4 py-2 text-center text-sm font-medium text-warn">
            {DEPLOYMENT.band}
          </div>
        ) : null}
        <header className="border-b border-border">
          <div className="mx-auto flex max-w-5xl flex-wrap items-center justify-between gap-3 px-4 py-3">
            <Link href="/" className="flex items-center gap-2 text-text no-underline hover:text-text">
              <span aria-hidden="true" className="flex h-7 w-7 items-center justify-center rounded-md bg-accent font-mono text-sm font-bold text-on-accent">
                Z
              </span>
              <span className="font-semibold tracking-tight">Zecured</span>
              <span className="rounded border border-warn/50 px-1.5 text-[10px] font-medium uppercase tracking-wider text-warn">
                {TESTNET ? "testnet" : "mainnet beta"}
              </span>
            </Link>
            <Nav />
          </div>
        </header>
        <main id="main" className="mx-auto w-full max-w-5xl px-4 py-8 sm:py-12">
          <ConfigGate>{children}</ConfigGate>
        </main>
        <footer className="border-t border-border">
          <div className="mx-auto flex max-w-5xl flex-col gap-3 px-4 py-6 text-xs text-faint sm:flex-row sm:items-center sm:justify-between">
            <p>
              {TESTNET ? (
                <>
                  Zecured is a ZECATHON 2026 testnet experiment, <strong className="text-muted">not a financial product</strong>.
                  Testnet funds only: TAZ on Zcash testnet and test USDC on Base Sepolia.
                </>
              ) : (
                <>
                  Zecured mainnet beta: <strong className="text-muted">unaudited, invite only, small caps</strong>. Use only what
                  you can afford to lose.
                </>
              )}
            </p>
            <div className="flex flex-wrap items-center gap-4">
              <BackendStatus />
              <Link href="/status" className="text-faint hover:text-muted">
                Status
              </Link>
              <Link href="/evidence" className="text-faint hover:text-muted">
                Evidence
              </Link>
              {TESTNET ? (
                <Link href="/demo" className="text-faint hover:text-muted">
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
