/**
 * The seam between the two chains, as the design draws it (server-safe, no hooks): Zcash dark and
 * round, Base light and square, and the gold π seal where a proof crosses from one to the other.
 */
import type { ReactNode } from "react";
import { IconCheck } from "./icons";
import { ZecMark } from "./marks";
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
function Pi({ width, height, color = "#0F1B2A" }: { width: number; height: number; color?: string }) {
  return (
    <svg width={width} height={height} viewBox="0 0 28 24" aria-hidden="true">
      <path d="M3 6.5h22" stroke={color} strokeWidth="3.4" strokeLinecap="round" />
      <path d="M9.5 6.5v14.5" stroke={color} strokeWidth="3.4" strokeLinecap="round" />
      <path d="M18.2 6.5v10.6c0 2.5 1.3 3.9 3.8 3.9" stroke={color} strokeWidth="3.4" strokeLinecap="round" fill="none" />
    </svg>
  );
}

/**
 * The gold π seal: a proof crossing between the chains. `pulse` only while one is being made (none under
 * prefers-reduced-motion); `stamp` once, as a proof lands; `idle`: a proof still to come, an outline.
 */
export function ProofSeal({
  size = "lg",
  pulse = false,
  stamp = false,
  idle = false,
  className,
}: {
  size?: "lg" | "sm";
  pulse?: boolean;
  stamp?: boolean;
  idle?: boolean;
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      className={cx(
        "seal",
        size === "sm" && "seal-sm",
        idle && "!bg-surface !shadow-[inset_0_0_0_1.5px_var(--color-line)]",
        stamp && "animate-stamp",
        className,
      )}
    >
      {pulse ? <span className="seal-pulse" /> : null}
      {size === "lg" ? <Pi width={26} height={22} color={idle ? "#82978C" : undefined} /> : <Pi width={11} height={9} color={idle ? "#82978C" : undefined} />}
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

/**
 * A step's mark: round on Zcash, square on Base. To do: a ring (a dash on Base); current: a bright ring with a core;
 * done: filled, with a check.
 */
export function StepMark({ chain, state, size = "md", className }: { chain: Chain; state: MarkState; size?: "md" | "sm"; className?: string }) {
  const box = size === "md" ? "size-4" : "size-3";
  if (chain === "zcash") {
    if (state === "done")
      return (
        <span aria-hidden="true" className={cx("inline-flex flex-none items-center justify-center rounded-full bg-soft text-bg", box, className)}>
          <IconCheck size={size === "md" ? 11 : 9} strokeWidth={3.2} />
        </span>
      );
    if (state === "now")
      return (
        <span aria-hidden="true" className={cx("inline-flex flex-none items-center justify-center rounded-full shadow-[inset_0_0_0_2px_var(--color-text)]", box, className)}>
          <span className="size-1.5 rounded-full bg-text" />
        </span>
      );
    return <span aria-hidden="true" className={cx("flex-none rounded-full shadow-[inset_0_0_0_1.5px_var(--color-control)]", box, className)} />;
  }
  if (state === "done")
    return (
      <span aria-hidden="true" className={cx("inline-flex flex-none items-center justify-center rounded-[2px] bg-ink text-paper", box, className)}>
        <IconCheck size={size === "md" ? 11 : 9} strokeWidth={3.2} />
      </span>
    );
  // Current: a small solid square (an empty box would read as a checkbox left unticked).
  if (state === "now")
    return (
      <span aria-hidden="true" className={cx("inline-flex flex-none items-center justify-center", box, className)}>
        <span className="size-2 rounded-[1px] bg-ink" />
      </span>
    );
  return (
    <span aria-hidden="true" className={cx("inline-flex flex-none items-center justify-center", box, className)}>
      <span className="h-[2px] w-2.5 bg-ink-faint" />
    </span>
  );
}

/** The chain a record belongs to: Zcash dark and round, Base light and square. */
export function ChainChip({ chain }: { chain: Chain }) {
  return (
    <span
      className={cx(
        "inline-flex min-h-[26px] items-center justify-self-start px-2.5 text-[12.5px] font-semibold",
        chain === "zcash"
          ? "rounded-full bg-bg text-text shadow-[inset_0_0_0_1px_var(--color-line)]"
          : "rounded-[2px] bg-white text-ink shadow-[inset_0_0_0_1px_var(--color-ink)]",
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

export interface TrackerRow {
  key: string;
  label: ReactNode;
  /** Right-aligned: a count, a timer, a transaction. */
  detail?: ReactNode;
  /** A line under the label. */
  note?: ReactNode;
  state: MarkState;
  /** The row a proof completes: its mark is the π once done. */
  proof?: boolean;
}

const ROW_TEXT: Record<Chain, Record<MarkState, string>> = {
  zcash: { done: "text-soft", now: "font-semibold text-text", todo: "text-muted" },
  base: { done: "text-ink", now: "font-semibold text-ink", todo: "text-ink-muted" },
};

function Rows({ chain, rows, big, still }: { chain: Chain; rows: TrackerRow[]; big: boolean; still: boolean }) {
  return (
    <ol className={cx("flex flex-col", big ? "mt-4 gap-3" : "mt-4 gap-3.5")}>
      {rows.map((r) => (
        <li key={r.key} aria-current={r.state === "now" ? "step" : undefined} className="grid grid-cols-[20px_minmax(0,1fr)_auto] items-start gap-x-3">
          {r.proof && r.state === "done" ? (
            <ProofSeal size="sm" stamp={!still} className="mt-[1px] !size-[18px] !shadow-none" />
          ) : (
            <StepMark chain={chain} state={r.state} className="mt-[3px]" />
          )}
          <span className={cx("min-w-0 leading-[1.45]", big ? "text-[15px]" : "text-[14.5px]", ROW_TEXT[chain][r.state])}>
            {r.label}
            {r.state === "done" ? <span className="sr-only"> (done)</span> : null}
            {r.note ? <span className={cx("mt-0.5 block text-[12.5px] font-normal", chain === "zcash" ? "text-muted" : "text-ink-muted")}>{r.note}</span> : null}
          </span>
          {r.detail ? (
            <span className={cx("pt-[2px] text-right font-mono text-[12px] tabular-nums", chain === "zcash" ? "text-muted" : "text-ink-muted")}>{r.detail}</span>
          ) : (
            <span />
          )}
        </li>
      ))}
    </ol>
  );
}

/**
 * A crossing from Zcash to Base, as one object: the dark, round Zcash half with its rows; the seam, a dashed
 * hairline that lights up gold once the proof lands, with the π on it (an outline while to come, pulsing while
 * made, stamped once landed); the light, square Base half, muted until the proof is there. Zcash identifiers never
 * appear in the Base half. `hero`: the 720 px size a step is about; `side`: the column beside a form.
 */
export function SeamTracker({
  title,
  zcash,
  base,
  seam,
  provingLabel,
  zcashExtra,
  footer,
  size = "side",
  still = false,
}: {
  title: string;
  zcash: TrackerRow[];
  base: TrackerRow[];
  seam: "idle" | "proving" | "proven";
  provingLabel?: ReactNode;
  zcashExtra?: ReactNode;
  footer?: ReactNode;
  size?: "side" | "hero";
  /** The proof landed on an earlier screen: drawn landed, not landing again. */
  still?: boolean;
}) {
  const big = size === "hero";
  return (
    <section aria-label={title} className="overflow-hidden rounded-t-[24px] rounded-b-[6px] shadow-[0_0_0_1px_var(--color-border),0_30px_60px_-36px_rgb(0_0_0/0.9)]">
      <div className={cx("bg-[radial-gradient(120%_90%_at_20%_0%,#123224_0%,#0a1a12_45%,#060d09_100%)]", big ? "px-8 pt-6 pb-10" : "px-6 pt-6 pb-11")}>
        <ChainLabel chain="zcash">
          <span className="inline-flex items-center gap-2">
            On Zcash <span className="text-faint">· shielded</span>
          </span>
        </ChainLabel>
        <Rows chain="zcash" rows={zcash} big={big} still={still} />
        {zcashExtra}
      </div>
      <div aria-hidden="true" className="relative z-[2] h-0">
        <div className="absolute inset-x-0 top-0 -translate-y-1/2">
          {seam === "proven" ? <div className={cx("seam-line-lit", still && "!animate-none")} /> : <div className="seam-line" />}
        </div>
        <div className="absolute top-0 left-1/2 flex -translate-x-1/2 -translate-y-1/2 items-center gap-3">
          <ProofSeal pulse={seam === "proving"} stamp={seam === "proven" && !still} idle={seam === "idle"} className={big ? undefined : "!size-[52px]"} />
        </div>
        {seam === "proving" && provingLabel ? (
          <span className="absolute top-0 left-[calc(50%+40px)] inline-flex min-h-[26px] -translate-y-1/2 items-center rounded-full bg-accent px-3 text-[12.5px] font-bold whitespace-nowrap text-on-accent tabular-nums max-sm:hidden">
            {provingLabel}
          </span>
        ) : null}
      </div>
      <div className={cx("paper transition-[filter,opacity] duration-700", seam === "proven" ? "" : "opacity-[0.86] saturate-50", big ? "px-8 pt-10 pb-6" : "px-6 pt-11 pb-6")}>
        <ChainLabel chain="base">
          <span className="inline-flex items-center gap-2">
            On Base <span className="text-ink-faint">· in the open</span>
          </span>
        </ChainLabel>
        <Rows chain="base" rows={base} big={big} still={still} />
        {footer ? <div className="mt-4 border-t border-rule pt-3 text-[13px] leading-[1.55] text-ink-muted">{footer}</div> : null}
      </div>
    </section>
  );
}

export type SignerNode = "idle" | "on" | "warn" | "danger";

/**
 * The five signer nodes around a vault: idle rings; a turning arc while they work (this browser's proof of work,
 * then their key generation); lit one after another once their signatures are in (`signed`). On the status page
 * each node takes its own state. Decorative: the caller says what it shows in words.
 */
export function SignerRing({
  state = "idle",
  nodes,
  size = 168,
  center,
}: {
  state?: "idle" | "working" | "signed";
  nodes?: SignerNode[];
  size?: number;
  center?: ReactNode;
}) {
  const c = 84;
  const r = 60;
  const points = [0, 1, 2, 3, 4].map((i) => {
    const a = ((-90 + i * 72) * Math.PI) / 180;
    return { x: c + r * Math.cos(a), y: c + r * Math.sin(a) };
  });
  const nodeState = (i: number): SignerNode => nodes?.[i] ?? (state === "signed" ? "on" : "idle");
  const fill: Record<SignerNode, string> = { idle: "#0b1611", on: "#0f3a2c", warn: "#3a2410", danger: "#3d1512" };
  const stroke: Record<SignerNode, string> = {
    idle: "rgb(205 230 216 / 0.3)",
    on: "var(--color-pool-soft)",
    warn: "var(--color-warn)",
    danger: "var(--color-danger)",
  };
  const lit = state === "signed" || Boolean(nodes);
  return (
    <span className="relative inline-flex flex-none items-center justify-center" style={{ width: size, height: size }} aria-hidden="true">
      <svg viewBox="0 0 168 168" width={size} height={size}>
        {points.map((p, i) => (
          <line
            key={`s${i}`}
            x1={c}
            y1={c}
            x2={p.x}
            y2={p.y}
            stroke={lit && nodeState(i) !== "idle" ? "rgb(127 209 180 / 0.5)" : "rgb(205 230 216 / 0.12)"}
            strokeWidth="1"
            style={{ transition: `stroke 320ms ${i * 60}ms` }}
          />
        ))}
        <circle cx={c} cy={c} r={r} fill="none" stroke="rgb(205 230 216 / 0.08)" strokeWidth="1" />
        {state === "working" ? (
          <circle
            cx={c}
            cy={c}
            r={r}
            fill="none"
            stroke="var(--color-text)"
            strokeWidth="2"
            strokeLinecap="round"
            strokeDasharray="70 310"
            className="animate-spin motion-reduce:animate-none"
            style={{ transformOrigin: `${c}px ${c}px`, animationDuration: "1.2s" }}
          />
        ) : null}
        {points.map((p, i) => (
          <circle
            key={`n${i}`}
            cx={p.x}
            cy={p.y}
            r="13"
            fill={fill[nodeState(i)]}
            stroke={stroke[nodeState(i)]}
            strokeWidth="1.5"
            style={{ transition: `fill 240ms ${i * 60}ms, stroke 240ms ${i * 60}ms` }}
          />
        ))}
        <circle cx={c} cy={c} r="24" fill={state === "signed" ? "#123224" : "#0b1611"} stroke={state === "signed" ? "var(--color-pool-soft)" : "rgb(205 230 216 / 0.3)"} strokeWidth="1.5" style={{ transition: "fill 320ms 300ms, stroke 320ms 300ms" }} />
      </svg>
      <span className="absolute inset-0 flex items-center justify-center">{center ?? <ZecMark size={Math.round(size * 0.17)} />}</span>
    </span>
  );
}
