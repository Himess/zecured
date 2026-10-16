"use client";

import { config } from "@/lib/config";
import { useMintingStatus } from "@/lib/hooks";
import { mintingNotices } from "@/lib/v3";
import { Notice } from "./ui";

/**
 * What holds new deposits now (GET /status: a halt, a pause, bonds short, the coverage cap full),
 * above every page while anything does. Exits never wait on any of it.
 */
export function MintingBanner() {
  const status = useMintingStatus();
  if (!status.data) return null;
  const notices = mintingNotices(status.data, config.denominations, Math.floor(Date.now() / 1000));
  if (notices.length === 0) return null;
  return (
    <div className="mb-8 space-y-3">
      {notices.map((n) => (
        <Notice key={n.title} tone={n.tone} title={n.title}>
          {n.body.map((p) => (
            <p key={p}>{p}</p>
          ))}
        </Notice>
      ))}
    </div>
  );
}
