import type { NextConfig } from "next";

const isDev = process.env.NODE_ENV === "development";

/** Origin of our backend: the only server besides this app the browser may talk to. */
function backendOrigin(): string {
  try {
    return new URL(process.env.NEXT_PUBLIC_BACKEND_URL || "http://127.0.0.1:8080").origin;
  } catch {
    return "http://127.0.0.1:8080";
  }
}

/**
 * Content Security Policy. `connect-src` is the privacy guarantee "the browser never calls RPC,
 * bundler or paymaster providers directly" enforced by the browser itself: fetches may only go
 * to this app and to our backend proxy. No third-party scripts, fonts or images either.
 * (Inline scripts are needed by Next.js without nonces; see the bundled CSP guide.)
 */
const csp = [
  "default-src 'self'",
  `script-src 'self' 'unsafe-inline'${isDev ? " 'unsafe-eval'" : ""}`,
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "font-src 'self'",
  `connect-src 'self' ${backendOrigin()}${isDev ? " ws: wss:" : ""}`,
  "worker-src 'self' blob:",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
].join("; ");

/**
 * REPLAY (DEMO, lib/replay): the judges' build, `NEXT_PUBLIC_REPLAY=1`, is a static export for a
 * static host (GitHub Pages, under `NEXT_PUBLIC_BASE_PATH`). A static host sends no headers of
 * ours, so the replay's Content Security Policy is a <meta> tag (lib/replay/flag.ts, REPLAY_CSP),
 * and `headers()` is left out (a static export cannot serve it).
 */
const replay = process.env.NEXT_PUBLIC_REPLAY === "1";

const replayConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  output: "export",
  basePath: process.env.NEXT_PUBLIC_BASE_PATH || "",
  trailingSlash: true,
  images: { unoptimized: true },
  // A machine short of memory may skip the build's own type check once `pnpm typecheck` passed on the same tree
  // (ZECURED_TYPECHECKED=1). CI never sets it.
  typescript: { ignoreBuildErrors: process.env.ZECURED_TYPECHECKED === "1" },
};

const nextConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  async headers() {
    return [
      {
        source: "/(.*)",
        headers: [
          { key: "Content-Security-Policy", value: csp },
          { key: "Referrer-Policy", value: "no-referrer" },
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "X-Frame-Options", value: "DENY" },
          { key: "Cross-Origin-Opener-Policy", value: "same-origin" },
          {
            key: "Permissions-Policy",
            // The camera only for this app itself: reading an Ironwood address from a QR code (Buy shielded ZEC).
            value: "camera=(self), microphone=(), geolocation=(), payment=(), usb=(), browsing-topics=()",
          },
        ],
      },
    ];
  },
};

export default replay ? replayConfig : nextConfig;
