/**
 * The seam between the two chains, as the design draws it (server-safe, no hooks): Zcash dark and
 * round, Base light and square, and the gold π seal where a proof crosses from one to the other.
 */
import type { ReactNode } from "react";
import { cx } from "./ui";

export type Chain = "zcash" | "base";
export type MarkState = "done" | "now" | "todo";

/** Zecured's mark: the dark Zcash half, the light Base half, and the gold proof between them. */
export function SeamMark({ size = 26 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 30 30" aria-hidden="true" className="flex-none">
      <circle cx="15" cy="15" r="13.25" fill="#0D1611" stroke="#E4ECE6" strokeWidth="1.5" />
      <path d="M15 1.75 A13.25 13.25 0 0 1 15 28.25 Z" fill="#EEF2F6" />
      <circle cx="15" cy="15" r="4" fill="#F4B728" stroke="#0D1611" strokeWidth="1.4" />
    </svg>
  );
}

/** The π of a zero-knowledge proof, in ink. */
function Pi({ width, height }: { width: number; height: number }) {
  return (
    <svg width={width} height={height} viewBox="0 0 28 24" aria-hidden="true">
      <path d="M3 6.5h22" stroke="#0F1B2A" strokeWidth="3.4" strokeLinecap="round" />
      <path d="M9.5 6.5v14.5" stroke="#0F1B2A" strokeWidth="3.4" strokeLinecap="round" />
      <path d="M18.2 6.5v10.6c0 2.5 1.3 3.9 3.8 3.9" stroke="#0F1B2A" strokeWidth="3.4" strokeLinecap="round" fill="none" />
    </svg>
  );
}

/**
 * The gold π seal: a proof crossing between the chains. `pulse` only while one is being made (the
 * one motion the app makes on its own; none under prefers-reduced-motion).
 */
export function ProofSeal({ size = "lg", pulse = false, className }: { size?: "lg" | "sm"; pulse?: boolean; className?: string }) {
  return (
    <span aria-hidden="true" className={cx("seal", size === "sm" && "seal-sm", className)}>
      {pulse ? <span className="seal-pulse" /> : null}
      {size === "lg" ? <Pi width={26} height={22} /> : <Pi width={11} height={9} />}
    </span>
  );
}

/** "On Zcash" (a round dot, blurred: encrypted) or "On Base" (a square, in the open). */
export function ChainLabel({ chain, children }: { chain: Chain; children: ReactNode }) {
  return (
    <p className={cx("flex items-center gap-[9px] text-[13.5px] font-medium", chain === "zcash" ? "text-muted" : "text-ink-muted")}>
      <span
        aria-hidden="true"
        className={cx("size-[9px] flex-none", chain === "zcash" ? "rounded-full bg-muted blur-[1.5px]" : "bg-ink-muted")}
      />
      {children}
    </p>
  );
}

const MARK: Record<Chain, Record<MarkState, string>> = {
  zcash: {
    done: "rounded-full bg-soft",
    now: "rounded-full shadow-[inset_0_0_0_2px_var(--color-text)]",
    todo: "rounded-full shadow-[inset_0_0_0_1.5px_var(--color-line)]",
  },
  base: {
    done: "rounded-[3px] bg-ink-muted",
    now: "rounded-[3px] shadow-[inset_0_0_0_2px_var(--color-ink)]",
    todo: "rounded-[3px] shadow-[inset_0_0_0_1.5px_var(--color-ink-faint)]",
  },
};

/** A step's mark: round on Zcash, square on Base; filled once done, a bright ring while current. */
export function StepMark({ chain, state, size = "md", className }: { chain: Chain; state: MarkState; size?: "md" | "sm"; className?: string }) {
  return <span aria-hidden="true" className={cx("flex-none", size === "md" ? "size-4" : "size-3", MARK[chain][state], className)} />;
}

/** The chain a record belongs to: Zcash dark and round, Base light and square. */
export function ChainChip({ chain }: { chain: Chain }) {
  return (
    <span
      className={cx(
        "inline-flex min-h-[26px] items-center justify-self-start px-2.5 text-[12.5px] font-semibold",
        chain === "zcash"
          ? "rounded-full bg-bg text-text shadow-[inset_0_0_0_1px_var(--color-line)]"
          : "rounded-[3px] bg-white text-ink shadow-[inset_0_0_0_1px_var(--color-ink)]",
      )}
    >
      {chain === "zcash" ? "Zcash" : "Base"}
    </span>
  );
}

/** `text` with the leak mark on `leak`, its one part that ties a record to someone (plain if absent). */
export function Leaky({ text, leak }: { text: string; leak: string }) {
  const at = text.indexOf(leak);
  if (at < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, at)}
      <mark className="leak">{leak}</mark>
      {text.slice(at + leak.length)}
    </>
  );
}
