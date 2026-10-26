/**
 * Small, server-safe UI primitives (no hooks). Interactive pieces live in components/client.tsx, the seam's
 * marks (the π seal, chain labels, chips, trackers) in components/seam.tsx, other projects' marks in
 * components/marks.tsx and the line icons in components/icons.tsx.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { IconAlert, IconCheck, IconChevronDown, IconInfo, IconPlay } from "./icons";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

/** A small π in ink, for gold grounds (seam.tsx draws the seal itself; this file must not import it). */
function PiGlyph({ size = 10, color = "#0F1B2A" }: { size?: number; color?: string }) {
  return (
    <svg width={size} height={(size * 24) / 28} viewBox="0 0 28 24" aria-hidden="true" className="flex-none">
      <path d="M3 6.5h22M9.5 6.5v14.5" stroke={color} strokeWidth="3.6" strokeLinecap="round" />
      <path d="M18.2 6.5v10.6c0 2.5 1.3 3.9 3.8 3.9" stroke={color} strokeWidth="3.6" strokeLinecap="round" fill="none" />
    </svg>
  );
}

/**
 * A page's head: an eyebrow, the title, a line of lead and the page's own actions, then its content. Kept short, so
 * the page's first control is in view.
 */
export function Page({
  title,
  lead,
  eyebrow,
  actions,
  children,
}: {
  title: ReactNode;
  lead?: ReactNode;
  eyebrow?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="space-y-7">
      <header className="flex flex-wrap items-end justify-between gap-x-8 gap-y-3">
        <div className="min-w-0 space-y-2">
          {eyebrow ? <p className="flex items-center gap-2 text-[13px] font-semibold text-muted">{eyebrow}</p> : null}
          <h1 className="max-w-[24em] text-[length:clamp(26px,2.4vw,34px)] leading-[1.04] font-extrabold tracking-[-0.02em] text-balance font-stretch-116%">
            {title}
          </h1>
          {lead ? <p className="max-w-2xl text-[15px] text-soft">{lead}</p> : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </header>
      {children}
    </div>
  );
}

/** A panel: the dark ground raised a step. `lead` marks the page's main panel with a faint edge of light. */
export function Card({
  title,
  action,
  icon,
  children,
  className,
  lead = false,
  as: Tag = "section",
}: {
  title?: ReactNode;
  action?: ReactNode;
  icon?: ReactNode;
  children: ReactNode;
  className?: string;
  lead?: boolean;
  as?: "section" | "div" | "article";
}) {
  return (
    <Tag className={cx("panel p-[clamp(18px,2.4vw,28px)]", lead && "panel-lead", className)}>
      {title || action ? (
        <div className="mb-5 flex flex-wrap items-center justify-between gap-3">
          {title ? (
            <h2 className="flex items-center gap-2.5 text-[19px] leading-[1.15] font-extrabold tracking-[-0.006em] font-stretch-116%">
              {icon ? <span className="inline-flex flex-none text-muted">{icon}</span> : null}
              <span className="min-w-0">{title}</span>
            </h2>
          ) : (
            <span />
          )}
          {action}
        </div>
      ) : null}
      {children}
    </Tag>
  );
}

type Tone = "info" | "warn" | "danger" | "ok";

/** A callout's ground: a quiet panel with a bar of its tone at the left (warnings and errors tinted a little). */
const toneGround: Record<Tone | "demo", string> = {
  info: "bg-surface-2/60 shadow-[inset_3px_0_0_var(--color-info),inset_0_0_0_1px_var(--color-border)]",
  ok: "bg-surface-2/60 shadow-[inset_3px_0_0_var(--color-ok),inset_0_0_0_1px_var(--color-border)]",
  warn: "bg-[color-mix(in_oklab,var(--color-warn)_5%,var(--color-surface))] shadow-[inset_3px_0_0_var(--color-warn),inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_24%,transparent)]",
  danger:
    "bg-[color-mix(in_oklab,var(--color-danger)_7%,var(--color-surface))] shadow-[inset_3px_0_0_var(--color-danger),inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_36%,transparent)]",
  demo: "bg-[rgb(207_224_242/0.045)] shadow-[inset_3px_0_0_var(--color-demo),inset_0_0_0_1px_rgb(207_224_242/0.22)]",
};

const toneLabel: Record<Tone | "demo", string> = { info: "Note", warn: "Warning", danger: "Error", ok: "Done", demo: "DEMO" };
const toneText: Record<Tone | "demo", string> = { info: "text-info", warn: "text-warn", danger: "text-danger", ok: "text-ok", demo: "text-demo" };

function ToneIcon({ tone }: { tone: Tone | "demo" }) {
  if (tone === "ok") return <IconCheck size={17} strokeWidth={2.2} />;
  if (tone === "info") return <IconInfo size={17} />;
  if (tone === "demo") return <IconPlay size={16} />;
  return <IconAlert size={17} />;
}

/**
 * A callout: the tone's icon, the title, the text. Use it for results, errors and at most one standing warning per
 * step (guidance is a Hint, rules a Disclosure). Static notices are plain notes; `live` ones (results and errors
 * that appear after an action) are announced to screen readers. `demo`: what only the judges' replay does.
 */
export function Notice({
  tone = "info",
  title,
  children,
  live = false,
}: {
  tone?: Tone | "demo";
  title?: ReactNode;
  children?: ReactNode;
  live?: boolean;
}) {
  const role = live ? (tone === "danger" ? "alert" : "status") : "note";
  return (
    <div role={role} className={cx("grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 rounded-2xl py-3.5 pr-4 pl-5 text-[14px] sm:pr-5", toneGround[tone])}>
      <span aria-hidden="true" className={cx("mt-[2px]", toneText[tone])}>
        <ToneIcon tone={tone} />
      </span>
      <div className="min-w-0">
        <p className="text-[14.5px] leading-[1.4] font-bold text-text">
          <span className="sr-only">{toneLabel[tone]}: </span>
          {title ?? toneLabel[tone]}
        </p>
        {children ? <div className="mt-1 space-y-2 text-soft">{children}</div> : null}
      </div>
    </div>
  );
}

/** Guidance in a line: a small icon and muted text, no box. */
export function Hint({ icon, children, className }: { icon?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <p className={cx("flex items-start gap-2 text-[13.5px] text-muted", className)}>
      <span aria-hidden="true" className="mt-[2px] flex-none">
        {icon ?? <IconInfo size={15} />}
      </span>
      <span className="min-w-0">{children}</span>
    </p>
  );
}

/** Standing rules and details, closed by default: a 44 px row that opens. */
export function Disclosure({ summary, children, className, open }: { summary: ReactNode; children: ReactNode; className?: string; open?: boolean }) {
  return (
    <details
      open={open}
      className={cx("group rounded-2xl bg-bg/50 shadow-[inset_0_0_0_1px_var(--color-hairline)] [&>summary::-webkit-details-marker]:hidden", className)}
    >
      <summary className="flex min-h-11 cursor-pointer list-none items-center justify-between gap-3 rounded-2xl px-4 text-[14px] font-semibold text-soft hover:text-text">
        <span className="flex min-w-0 items-center gap-2">{summary}</span>
        <IconChevronDown size={16} className="text-muted transition-transform group-open:rotate-180" />
      </summary>
      <div className="space-y-2 px-4 pb-4 text-[14px] text-soft">{children}</div>
    </details>
  );
}

type Variant = "primary" | "secondary" | "ghost" | "danger" | "demo";
type Size = "sm" | "md" | "lg";

/**
 * Gold only on the one primary button of a view; it turns gold the moment its action becomes possible (a disabled
 * primary is neutral). The others are raised, quiet, red-lined or, for the judges' replay, dashed ice pills.
 */
const variantClasses: Record<Variant, string> = {
  primary:
    "bg-accent font-bold text-on-accent hover:bg-accent-strong disabled:bg-surface-2 disabled:font-semibold disabled:text-muted disabled:shadow-[inset_0_0_0_1px_var(--color-border)]",
  secondary: "bg-surface-2 font-semibold text-text shadow-[inset_0_0_0_1px_var(--color-control)] hover:bg-surface-3 disabled:opacity-50",
  ghost: "font-semibold text-muted hover:bg-surface-2 hover:text-text disabled:opacity-50",
  danger: "font-semibold text-danger shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_60%,transparent)] hover:bg-danger/10 disabled:opacity-50",
  demo: "border border-dashed border-demo/60 bg-demo/[0.05] font-semibold text-demo hover:bg-demo/[0.1] disabled:opacity-50",
};

/** The primary's shadow: an edge of light, and a glow except on small buttons. */
const primaryShadow: Record<Size, string> = {
  sm: "shadow-[inset_0_0_0_1px_rgb(255_255_255/0.18)]",
  md: "shadow-[inset_0_0_0_1px_rgb(255_255_255/0.18),0_14px_30px_-16px_rgb(244_183_40/0.45)]",
  lg: "shadow-[inset_0_0_0_1px_rgb(255_255_255/0.18),0_16px_34px_-16px_rgb(244_183_40/0.45)]",
};

const sizeClasses: Record<Size, string> = {
  sm: "min-h-9 px-3.5 text-[13.5px]",
  md: "min-h-11 px-5 text-[15px]",
  lg: "min-h-[52px] px-7 text-[16px]",
};

export function buttonClass(variant: Variant = "primary", className?: string, size: Size = "md"): string {
  return cx(
    "inline-flex items-center justify-center gap-2 rounded-full py-2 leading-tight no-underline transition-[background-color,color,box-shadow] disabled:cursor-not-allowed aria-busy:cursor-progress",
    sizeClasses[size],
    variantClasses[variant],
    variant === "primary" && primaryShadow[size],
    className,
  );
}

/**
 * A button. While `busy`, it keeps its look and its focus (no `disabled`: a disabled button drops focus) and
 * ignores clicks; screen readers hear it as busy.
 */
export function Button({
  variant = "primary",
  size = "md",
  busy = false,
  className,
  children,
  disabled,
  type = "button",
  onClick,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size; busy?: boolean }) {
  return (
    <button
      type={type}
      className={buttonClass(variant, className, size)}
      disabled={disabled && !busy}
      aria-disabled={busy || undefined}
      aria-busy={busy || undefined}
      onClick={busy ? (e) => e.preventDefault() : onClick}
      {...rest}
    >
      {busy ? <Spinner /> : null}
      {children}
    </button>
  );
}

export function Spinner({ label }: { label?: string }) {
  return (
    <span className="inline-flex items-center gap-2">
      <span
        aria-hidden="true"
        className="inline-block h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent"
      />
      {label ? <span>{label}</span> : <span className="sr-only">Working…</span>}
    </span>
  );
}

const toneBadge: Record<Tone, string> = {
  info: "text-info bg-info/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-info)_35%,transparent)]",
  ok: "text-ok bg-ok/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_35%,transparent)]",
  warn: "text-warn bg-warn/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_42%,transparent)]",
  danger: "text-danger bg-danger/[0.09] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_50%,transparent)]",
};

/**
 * A sentence-case tag, its text in the tone's colour (neutral: soft, on a line ring); `dot` adds a status dot.
 * `proof`: solid gold with a π, for what a proof made final. `demo`: what only the judges' replay shows.
 */
export function Badge({ tone = "info", dot = false, children }: { tone?: Tone | "neutral" | "proof" | "demo"; dot?: boolean; children: ReactNode }) {
  const cls =
    tone === "neutral"
      ? "text-soft shadow-[inset_0_0_0_1px_var(--color-line)]"
      : tone === "proof"
        ? "bg-accent text-on-accent"
        : tone === "demo"
          ? "border border-dashed border-demo/60 bg-demo/[0.06] text-demo"
          : toneBadge[tone];
  return (
    <span className={cx("inline-flex min-h-[24px] items-center gap-1.5 rounded-full px-2.5 py-[3px] text-xs leading-tight font-semibold whitespace-nowrap", cls)}>
      {tone === "proof" ? <PiGlyph size={10} /> : tone === "demo" ? <IconPlay size={11} strokeWidth={2.2} /> : null}
      {dot && tone !== "proof" && tone !== "demo" ? <span aria-hidden="true" className="size-1.5 rounded-full bg-current" /> : null}
      {children}
    </span>
  );
}

/** A figure in a ruled strip, as the site sets its numbers: a hairline above, the label, the value, a hint. */
export function Stat({ label, value, hint, icon }: { label: ReactNode; value: ReactNode; hint?: ReactNode; icon?: ReactNode }) {
  return (
    <div className="min-w-0 border-t border-line/80 pt-3">
      <dt className="flex items-center gap-2 text-[12.5px] font-semibold text-muted">
        {icon}
        {label}
      </dt>
      <dd className="mt-1 truncate text-[21px] leading-tight font-bold tabular-nums font-stretch-110%">{value}</dd>
      {hint ? <dd className="mt-1 text-xs text-muted">{hint}</dd> : null}
    </div>
  );
}

/**
 * Chain data in monospace, in the ground's strongest text colour. Long hex and addresses wrap anywhere; a short
 * value (a shortened id such as 0x1563…5508) stays on one line rather than leave a character on the next.
 */
export function Mono({ children, className }: { children: ReactNode; className?: string }) {
  const short = typeof children === "string" && children.length <= 24;
  return (
    <code
      className={cx(
        "font-mono text-[0.85em] text-[color:var(--strong-color,var(--color-text))]",
        short ? "whitespace-nowrap" : "break-all",
        className,
      )}
    >
      {children}
    </code>
  );
}

export function KeyValue({ rows }: { rows: [ReactNode, ReactNode][] }) {
  return (
    <dl className="divide-y divide-hairline text-sm">
      {rows.map(([k, v], i) => (
        <div key={i} className="grid items-baseline gap-x-5 gap-y-1 py-3 first:pt-1 last:pb-1 sm:grid-cols-[minmax(0,12rem)_minmax(0,1fr)]">
          <dt className="text-muted">{k}</dt>
          <dd className="min-w-0 text-[15px]">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

const pad2 = (n: number) => String(n).padStart(2, "0");

/**
 * A flow's chapters, inline: a mono number in a pill (a white ring for the current one, a check once done), the
 * name, hairlines between. `proof`: the chapter a proof ends, whose π turns gold once `proven`. Phones get
 * "03 of 05 · Deposit" over a bar of segments.
 */
export function Steps({
  steps,
  current,
  proof,
}: {
  steps: string[];
  current: number;
  proof?: { index: number; proven: boolean };
}) {
  const n = steps.length;
  return (
    <div>
      <ol aria-label="Progress" className="hidden flex-wrap items-center gap-y-2 sm:flex">
        {steps.map((s, i) => {
          const done = i < current;
          const now = i === current;
          return (
            <li key={s} aria-current={now ? "step" : undefined} className="flex items-center">
              <span
                aria-hidden="true"
                className={cx(
                  "inline-flex h-[22px] min-w-[30px] items-center justify-center rounded-full px-1.5 font-mono text-[11.5px] font-medium",
                  now ? "text-text shadow-[inset_0_0_0_1.5px_var(--color-text)]" : done ? "bg-surface-3 text-soft" : "text-faint shadow-[inset_0_0_0_1px_var(--color-line)]",
                )}
              >
                {done ? <IconCheck size={12} strokeWidth={2.8} /> : pad2(i + 1)}
              </span>
              <span className={cx("ml-2 text-[14px]", now ? "font-semibold text-text" : done ? "text-soft" : "text-faint")}>
                {s}
                {done ? <span className="sr-only"> (done)</span> : null}
              </span>
              {proof?.index === i ? (
                <span
                  aria-hidden="true"
                  className={cx(
                    "ml-1.5 inline-flex size-[18px] items-center justify-center rounded-full",
                    proof.proven ? "bg-accent" : "shadow-[inset_0_0_0_1px_var(--color-line)]",
                  )}
                >
                  <PiGlyph size={9} color={proof.proven ? "#0F1B2A" : "#82978C"} />
                </span>
              ) : null}
              {i < n - 1 ? <span aria-hidden="true" className={cx("mx-3 h-px w-6", done ? "bg-soft/50" : "bg-line")} /> : null}
            </li>
          );
        })}
      </ol>
      <div className="sm:hidden">
        <p className="text-[13.5px] font-semibold" aria-label={`Step ${current + 1} of ${n}: ${steps[current] ?? ""}`}>
          <span className="font-mono text-[12px] text-muted">
            {pad2(Math.min(current + 1, n))} of {pad2(n)}
          </span>{" "}
          · {steps[Math.min(current, n - 1)]}
        </p>
        <div aria-hidden="true" className="mt-2 grid gap-1" style={{ gridTemplateColumns: `repeat(${n}, minmax(0, 1fr))` }}>
          {steps.map((s, i) => (
            <span key={s} className={cx("h-1 rounded-full", i < current ? "bg-soft/60" : i === current ? "bg-text" : "bg-surface-3")} />
          ))}
        </div>
      </div>
    </div>
  );
}

/** A labelled form field: label, control, hint and error, wired for screen readers by the caller's ids. */
export function Field({
  label,
  htmlFor,
  hint,
  error,
  children,
  className,
}: {
  label: ReactNode;
  htmlFor: string;
  hint?: ReactNode;
  error?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cx("space-y-1.5", className)}>
      <label htmlFor={htmlFor} className="block text-[14px] font-semibold text-soft">
        {label}
      </label>
      {children}
      {error ? <p className="text-sm text-danger">{error}</p> : hint ? <p className="text-xs text-muted">{hint}</p> : null}
    </div>
  );
}

/**
 * A horizontal meter: `value` of `max`, with optional markers (a target, a limit) on the track. Used for the market's
 * utilization. Decorative: the caller states the numbers in text.
 */
export function Meter({
  value,
  max = 1,
  markers = [],
  tone = "ok",
  className,
}: {
  value: number;
  max?: number;
  markers?: { at: number; label: string; tone?: "warn" | "danger" | "muted" }[];
  tone?: "ok" | "warn" | "danger" | "info" | "accent";
  className?: string;
}) {
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  const fill = {
    ok: "bg-ok",
    warn: "bg-warn",
    danger: "bg-danger",
    info: "bg-info",
    accent: "bg-accent",
  }[tone];
  return (
    <div aria-hidden="true" className={cx("relative pt-1 pb-5", className)}>
      <div className="h-2 overflow-hidden rounded-full bg-surface-3">
        <div className={cx("h-full rounded-full transition-[width] duration-500", fill)} style={{ width: `${pct}%` }} />
      </div>
      {markers.map((m) => {
        const at = Math.max(0, Math.min(100, (m.at / max) * 100));
        const color = m.tone === "danger" ? "bg-danger text-danger" : m.tone === "warn" ? "bg-warn text-warn" : "bg-muted text-muted";
        return (
          <span key={m.label} className="absolute top-0 flex -translate-x-1/2 flex-col items-center" style={{ left: `${at}%` }}>
            <span className={cx("h-4 w-[2px] rounded-full", color.split(" ")[0])} />
            <span className={cx("mt-0.5 text-[11px] font-semibold whitespace-nowrap", color.split(" ")[1])}>{m.label}</span>
          </span>
        );
      })}
    </div>
  );
}

/** A loading placeholder of the given size. */
export function Skeleton({ className }: { className?: string }) {
  return <span aria-hidden="true" className={cx("skeleton block", className ?? "h-5 w-24")} />;
}

/** What a list shows while it is empty: an icon, a sentence, an action. */
export function EmptyState({ icon, title, children, action }: { icon?: ReactNode; title: ReactNode; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-[20px] border border-dashed border-line/70 px-6 py-9 text-center">
      {icon ? (
        <span className="inline-flex size-11 items-center justify-center rounded-2xl bg-surface-2 text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
          {icon}
        </span>
      ) : null}
      <p className="text-[16.5px] font-bold">{title}</p>
      {children ? <div className="max-w-md text-sm text-muted">{children}</div> : null}
      {action ? <div className="mt-1">{action}</div> : null}
    </div>
  );
}

/** A section's heading inside a page: a title, an optional line of context, an optional action on the right. */
export function SectionTitle({ id, title, sub, action }: { id?: string; title: ReactNode; sub?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-3">
      <div className="space-y-1">
        <h2 id={id} className="text-[length:clamp(20px,1.8vw,24px)] leading-[1.1] font-extrabold tracking-[-0.01em] font-stretch-116%">
          {title}
        </h2>
        {sub ? <p className="text-sm text-muted">{sub}</p> : null}
      </div>
      {action}
    </div>
  );
}
