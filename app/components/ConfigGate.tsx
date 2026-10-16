"use client";

import { useEffect, useState, type ReactNode } from "react";
import { loadRuntimeConfig } from "@/lib/api";
import { config, configIssues } from "@/lib/config";
import { MintingBanner } from "./MintingBanner";
import { Button, Notice, Spinner } from "./ui";

/**
 * Renders the page once the configuration is settled: the backend's GET /config checked against
 * the addresses pinned in this build, and its non-security values (sponsorship, denominations,
 * confirmations, deposit window) applied to `config` in place (lib/config.ts). Pages read the
 * config only below this gate. If the backend does not answer within a few seconds, the page
 * renders anyway with the build-time configuration; problems and warnings are listed above it,
 * and so is whatever holds new deposits on Base (components/MintingBanner.tsx).
 */
export function ConfigGate({ children }: { children: ReactNode }) {
  // Always "loading" on the server and at hydration; the backend is only asked from here.
  const [ready, setReady] = useState(false);
  useEffect(() => {
    let stop = false;
    void loadRuntimeConfig().then(() => {
      if (!stop) setReady(true);
    });
    return () => {
      stop = true;
    };
  }, []);

  if (!ready) {
    return (
      <div role="status" aria-live="polite" className="flex min-h-[40vh] items-center justify-center text-sm text-muted">
        <Spinner label="Loading the deployment configuration…" />
      </div>
    );
  }

  const issues = configIssues();
  return (
    <>
      {issues.length > 0 ? (
        <div className="mb-8">
          <Notice tone="warn" title="Configuration" live>
            <ul className="list-disc space-y-1 pl-5">
              {issues.map((issue) => (
                <li key={issue}>{issue}</li>
              ))}
            </ul>
            {config.remote === "unavailable" ? (
              <Button variant="secondary" onClick={() => window.location.reload()}>
                Try again
              </Button>
            ) : null}
          </Notice>
        </div>
      ) : null}
      <MintingBanner />
      {children}
    </>
  );
}
