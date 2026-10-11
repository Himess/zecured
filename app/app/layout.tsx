import type { Metadata, Viewport } from "next";
import Link from "next/link";
import { GeistMono } from "geist/font/mono";
import { GeistSans } from "geist/font/sans";
import type { ReactNode } from "react";
import { BackendStatus, Nav } from "@/components/client";
import { ConfigGate } from "@/components/ConfigGate";
import "./globals.css";

export const metadata: Metadata = {
  title: { default: "Zecured: borrow against shielded ZEC", template: "%s · Zecured" },
  description: "Borrow against shielded ZEC. Sell nothing. Reveal nothing. Testnet experiment, not a financial product.",
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
        <header className="border-b border-border">
          <div className="mx-auto flex max-w-5xl flex-wrap items-center justify-between gap-3 px-4 py-3">
            <Link href="/" className="flex items-center gap-2 text-text no-underline hover:text-text">
              <span aria-hidden="true" className="flex h-7 w-7 items-center justify-center rounded-md bg-accent font-mono text-sm font-bold text-on-accent">
                Z
              </span>
              <span className="font-semibold tracking-tight">Zecured</span>
              <span className="rounded border border-warn/50 px-1.5 text-[10px] font-medium uppercase tracking-wider text-warn">
                testnet
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
              Zecured is a ZECATHON 2026 testnet experiment, <strong className="text-muted">not a financial product</strong>. Testnet
              ZEC (TAZ) and MockUSDC only.
            </p>
            <div className="flex items-center gap-4">
              <BackendStatus />
              <Link href="/demo" className="text-faint hover:text-muted">
                Demo panel
              </Link>
            </div>
          </div>
        </footer>
      </body>
    </html>
  );
}
