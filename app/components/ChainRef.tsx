/**
 * A transaction, an address or a commit, in monospace like Mono, linked to its explorer where this build links one
 * (lib/explorer.ts): Basescan for Base, ZecBlock for Zcash, GitHub for a commit, in a new tab with no referrer. Where
 * the build links none (the mainnet beta, the replay's placeholder), it is plain text. Server-safe: no hooks.
 */
import type { ReactNode } from "react";
import { IconExternal } from "./icons";
import { cx } from "./ui";
import { baseAddressUrl, baseTxUrl, commitUrl, zcashAddressUrl, zcashTxUrl } from "@/lib/explorer";

export type ChainRefKind = "base-tx" | "base-address" | "zcash-tx" | "zcash-address" | "commit";

const SITE: Record<ChainRefKind, string> = {
  "base-tx": "Basescan",
  "base-address": "Basescan",
  "zcash-tx": "ZecBlock",
  "zcash-address": "ZecBlock",
  commit: "GitHub",
};

export function chainRefUrl(kind: ChainRefKind, value: string): string | null {
  switch (kind) {
    case "base-tx":
      return baseTxUrl(value);
    case "base-address":
      return baseAddressUrl(value);
    case "zcash-tx":
      return zcashTxUrl(value);
    case "zcash-address":
      return zcashAddressUrl(value);
    case "commit":
      return commitUrl(value);
  }
}

/** `children` is what shows: the whole value by default, or its shortened form. */
export function ChainRef({ kind, value, children, className }: { kind: ChainRefKind; value: string; children?: ReactNode; className?: string }) {
  const shown = children ?? value;
  const url = chainRefUrl(kind, value);
  const text = cx(
    "font-mono text-[0.85em] text-[color:var(--strong-color,var(--color-text))]",
    typeof shown === "string" && shown.length > 24 ? "break-all" : "whitespace-nowrap",
    className,
  );
  if (!url) return <code className={text}>{shown}</code>;
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      referrerPolicy="no-referrer"
      title={`Opens ${SITE[kind]} in a new tab: a third-party site, which learns what you look at.`}
      className={cx(text, "underline decoration-dotted decoration-1 underline-offset-[3px] hover:decoration-solid")}
    >
      {shown}
      <IconExternal size={11} className="ml-0.5 inline align-[-1px]" />
      <span className="sr-only"> (opens {SITE[kind]}, a third-party site, in a new tab)</span>
    </a>
  );
}
