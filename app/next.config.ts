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
            value: "camera=(), microphone=(), geolocation=(), payment=(), usb=(), browsing-topics=()",
          },
        ],
      },
    ];
  },
};

export default nextConfig;
