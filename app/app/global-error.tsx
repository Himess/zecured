"use client";

import { REPLAY } from "@/lib/replay/flag";

/** lib/replay/world.ts's REPLAY_STORAGE_PREFIX, without loading the replay here. */
const REPLAY_STORAGE_PREFIX = "zecured:replay:";

/**
 * When the root layout itself fails (its header, the configuration check, the replay's dock), this page replaces
 * it. It renders its own document, without the app's styles: everything it needs is inline. No logging.
 */
export default function GlobalError({ retry }: { error: Error & { digest?: string }; retry: () => void }) {
  // DEMO: forget the replay's progress in this tab (its loans and orders included), then start again.
  const startOver = () => {
    try {
      for (let i = sessionStorage.length - 1; i >= 0; i--) {
        const key = sessionStorage.key(i);
        if (key?.startsWith(REPLAY_STORAGE_PREFIX)) sessionStorage.removeItem(key);
      }
    } catch {
      // Storage blocked: a reload is all that is left.
    }
    window.location.reload();
  };
  const button = {
    font: "600 15px system-ui, sans-serif",
    padding: "11px 20px",
    borderRadius: 999,
    border: "1px solid rgb(205 230 216 / 0.18)",
    cursor: "pointer",
  } as const;
  return (
    <html lang="en">
      <body style={{ margin: 0, minHeight: "100dvh", display: "grid", placeItems: "center", background: "#070d0a", color: "#e8efe9", font: "16px/1.55 system-ui, sans-serif" }}>
        <title>Something went wrong · Zecured</title>
        <main style={{ maxWidth: 520, padding: 24 }}>
          <h1 style={{ fontSize: 26, lineHeight: 1.15, margin: "0 0 12px" }}>Something went wrong while drawing this page</h1>
          <p style={{ color: "#c9d6cc", margin: "0 0 24px" }}>
            {REPLAY
              ? "Try again. If it happens again, start the replay over: it forgets this tab's progress and plays from the first step."
              : "Try again. Nothing was sent: your keys stay in this browser."}
          </p>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 12 }}>
            <button type="button" onClick={retry} style={{ ...button, background: "#f4b728", color: "#1a1100", borderColor: "transparent" }}>
              Try again
            </button>
            {REPLAY ? (
              <button type="button" onClick={startOver} style={{ ...button, background: "transparent", color: "#e8efe9" }}>
                Start the replay over
              </button>
            ) : null}
          </div>
        </main>
      </body>
    </html>
  );
}
