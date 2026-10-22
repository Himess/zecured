/**
 * Small, server-safe UI primitives (no hooks). Interactive pieces live in components/client.tsx,
 * the seam's marks (the π seal, chain labels, chips) in components/seam.tsx.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

export function Page({ title, lead, children }: { title: ReactNode; lead?: ReactNode; children: ReactNode }) {
  return (
    <div className="space-y-8">
      <header className="space-y-3">
        <h1 className="max-w-[24em] text-[length:clamp(30px,3vw,40px)] leading-[1.05] font-extrabold tracking-[-0.01em] text-balance font-stretch-125%">
          {title}
        </h1>
        {lead ? <p className="max-w-2xl text-base text-soft">{lead}</p> : null}
      </header>
      {children}
    </div>
  );
}

export function Card({
  title,
  action,
  children,
  className,
  as: Tag = "section",
}: {
  title?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
  as?: "section" | "div" | "article";
}) {
  return (
    <Tag className={cx("rounded-[22px] bg-surface p-[clamp(24px,3vw,36px)]", className)}>
      {title || action ? (
        <div className="mb-5 flex flex-wrap items-start justify-between gap-3">
          {title ? (
            <h2 className="text-[22px] leading-[1.15] font-extrabold tracking-[-0.006em] font-stretch-118%">{title}</h2>
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

/** A notice's ring: neutral for notes and results, tinted for warnings and dangers. */
const toneRing: Record<Tone, string> = {
  info: "shadow-[inset_0_0_0_1px_var(--color-border)]",
  ok: "shadow-[inset_0_0_0_1px_var(--color-border)]",
  warn: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_45%,transparent)]",
  danger: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_60%,transparent)]",
};

const toneDot: Record<Tone, string> = {
  info: "[--dot:var(--color-info)]",
  warn: "[--dot:var(--color-warn)]",
  danger: "[--dot:var(--color-danger)]",
  ok: "[--dot:var(--color-ok)]",
};

const toneLabel: Record<Tone, string> = { info: "Note", warn: "Warning", danger: "Danger", ok: "Done" };
const toneText: Record<Tone, string> = { info: "text-info", warn: "text-warn", danger: "text-danger", ok: "text-ok" };

/**
 * A callout, as the design's notice: a status dot in the tone's colour, the title, the text. Static
 * notices are plain notes; `live` ones (results and errors that appear after an action) are
 * announced to screen readers.
 */
export function Notice({
  tone = "info",
  title,
  children,
  live = false,
}: {
  tone?: Tone;
  title?: ReactNode;
  children?: ReactNode;
  live?: boolean;
}) {
  const role = live ? (tone === "danger" ? "alert" : "status") : "note";
  return (
    <div
      role={role}
      className={cx(
        "grid grid-cols-[auto_minmax(0,1fr)] gap-x-3.5 rounded-2xl bg-bg px-[18px] py-4 text-[15px] sm:px-[22px] sm:py-5",
        toneRing[tone],
        toneDot[tone],
      )}
    >
      <span aria-hidden="true" className="dot mt-[5px]" />
      <div className="min-w-0">
        <p className="text-base leading-[1.4] font-bold text-text">
          <span className="sr-only">{toneLabel[tone]}: </span>
          {title ?? toneLabel[tone]}
        </p>
        {children ? <div className="mt-1.5 space-y-2 text-soft">{children}</div> : null}
      </div>
    </div>
  );
}

type Variant = "primary" | "secondary" | "ghost" | "danger";

/** Gold only on primary buttons; the others are line and quiet pills. */
const variantClasses: Record<Variant, string> = {
  primary:
    "bg-accent font-bold text-on-accent shadow-[inset_0_1px_0_rgb(255_255_255/0.4),0_14px_30px_-16px_rgb(244_183_40/0.8)] hover:bg-accent-strong",
  secondary: "border border-line font-semibold text-text hover:bg-bg",
  ghost: "font-semibold text-muted hover:bg-surface-2 hover:text-text",
  danger: "border border-danger/60 font-semibold text-danger hover:bg-danger/10",
};

export function buttonClass(variant: Variant = "primary", className?: string): string {
  return cx(
    "inline-flex min-h-11 items-center justify-center gap-2 rounded-full px-5 py-2 text-[15px] leading-tight transition-colors",
    "disabled:cursor-not-allowed disabled:opacity-50",
    variantClasses[variant],
    className,
  );
}

export function Button({
  variant = "primary",
  busy = false,
  className,
  children,
  disabled,
  type = "button",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; busy?: boolean }) {
  return (
    <button
      type={type}
      className={buttonClass(variant, className)}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
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

const toneBadgeRing: Record<Tone, string> = {
  info: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-info)_45%,transparent)]",
  ok: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_45%,transparent)]",
  warn: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_50%,transparent)]",
  danger: "shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_60%,transparent)]",
};

/** The design's tag: a sentence-case pill, its text in the tone's colour (neutral: soft, on a line ring). */
export function Badge({ tone = "info", children }: { tone?: Tone | "neutral"; children: ReactNode }) {
  const cls = tone === "neutral" ? "text-soft shadow-[inset_0_0_0_1px_var(--color-line)]" : cx(toneText[tone], toneBadgeRing[tone]);
  return (
    <span className={cx("inline-flex min-h-[22px] items-center rounded-full px-[9px] py-[3px] text-xs leading-tight font-semibold", cls)}>
      {children}
    </span>
  );
}

/** A figure under a rule: no box around it. */
export function Stat({ label, value, hint }: { label: ReactNode; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="border-t border-border pt-3">
      <dt className="text-sm text-muted">{label}</dt>
      <dd className="mt-1 text-xl font-bold tabular-nums">{value}</dd>
      {hint ? <dd className="mt-0.5 text-xs text-muted">{hint}</dd> : null}
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
    <dl className="divide-y divide-border text-sm">
      {rows.map(([k, v], i) => (
        <div key={i} className="grid items-baseline gap-x-5 gap-y-1 py-3 sm:grid-cols-[minmax(0,11rem)_minmax(0,1fr)]">
          <dt className="text-muted">{k}</dt>
          <dd className="min-w-0 text-[15px]">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

/** Wizard progress, as the design's steps: a filled number once done, a bright ring for the current one. */
export function Steps({ steps, current }: { steps: string[]; current: number }) {
  return (
    <ol className="flex flex-wrap gap-x-9 gap-y-2.5" aria-label="Progress">
      {steps.map((s, i) => (
        <li
          key={s}
          aria-current={i === current ? "step" : undefined}
          className={cx(
            "flex items-center gap-2.5 text-[15px]",
            i === current ? "font-semibold text-text" : i < current ? "text-soft" : "text-faint",
          )}
        >
          <span
            aria-hidden="true"
            className={cx(
              "inline-flex size-[26px] flex-none items-center justify-center rounded-full text-[13px] font-bold",
              i === current
                ? "text-text shadow-[inset_0_0_0_2px_var(--color-text)]"
                : i < current
                  ? "bg-soft text-bg"
                  : "shadow-[inset_0_0_0_1.5px_var(--color-line)]",
            )}
          >
            {i + 1}
          </span>
          <span>
            {s}
            {i < current ? <span className="sr-only"> (done)</span> : null}
          </span>
        </li>
      ))}
    </ol>
  );
}
