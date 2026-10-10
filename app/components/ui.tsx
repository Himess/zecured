/**
 * Small, server-safe UI primitives (no hooks). Interactive pieces live in components/client.tsx.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

export function Page({ title, lead, children }: { title: ReactNode; lead?: ReactNode; children: ReactNode }) {
  return (
    <div className="space-y-8">
      <header className="space-y-3">
        <h1 className="text-2xl font-semibold tracking-tight sm:text-3xl">{title}</h1>
        {lead ? <p className="max-w-2xl text-base text-muted">{lead}</p> : null}
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
    <Tag className={cx("rounded-xl border border-border bg-surface p-4 sm:p-6", className)}>
      {title || action ? (
        <div className="mb-4 flex flex-wrap items-start justify-between gap-2">
          {title ? <h2 className="text-lg font-semibold">{title}</h2> : <span />}
          {action}
        </div>
      ) : null}
      {children}
    </Tag>
  );
}

type Tone = "info" | "warn" | "danger" | "ok";

const toneClasses: Record<Tone, string> = {
  info: "border-info/40 bg-info/5",
  warn: "border-warn/50 bg-warn/5",
  danger: "border-danger/60 bg-danger/5",
  ok: "border-ok/50 bg-ok/5",
};

const toneLabel: Record<Tone, string> = { info: "Note", warn: "Warning", danger: "Danger", ok: "Done" };
const toneText: Record<Tone, string> = { info: "text-info", warn: "text-warn", danger: "text-danger", ok: "text-ok" };

/**
 * A callout. Static notices are plain notes; `live` ones (results and errors that appear after
 * an action) are announced to screen readers.
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
    <div role={role} className={cx("rounded-lg border px-4 py-3 text-sm", toneClasses[tone])}>
      <p className={cx("font-semibold", toneText[tone])}>
        <span className="sr-only">{toneLabel[tone]}: </span>
        {title ?? toneLabel[tone]}
      </p>
      {children ? <div className="mt-1 space-y-2 text-muted">{children}</div> : null}
    </div>
  );
}

type Variant = "primary" | "secondary" | "ghost" | "danger";

const variantClasses: Record<Variant, string> = {
  primary: "bg-accent text-on-accent hover:bg-accent-strong font-semibold",
  secondary: "border border-border bg-surface-2 text-text hover:border-faint",
  ghost: "text-muted hover:text-text hover:bg-surface-2",
  danger: "border border-danger/60 text-danger hover:bg-danger/10",
};

export function buttonClass(variant: Variant = "primary", className?: string): string {
  return cx(
    "inline-flex min-h-11 items-center justify-center gap-2 rounded-lg px-4 py-2 text-sm transition-colors",
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

export function Badge({ tone = "info", children }: { tone?: Tone | "neutral"; children: ReactNode }) {
  const cls =
    tone === "neutral"
      ? "border-border text-muted"
      : cx(toneText[tone], tone === "info" ? "border-info/40" : tone === "ok" ? "border-ok/50" : tone === "warn" ? "border-warn/50" : "border-danger/60");
  return (
    <span className={cx("inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium", cls)}>
      {children}
    </span>
  );
}

export function Stat({ label, value, hint }: { label: ReactNode; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-surface-2 p-3">
      <dt className="text-xs uppercase tracking-wide text-faint">{label}</dt>
      <dd className="mt-1 text-lg font-semibold tabular-nums">{value}</dd>
      {hint ? <dd className="mt-0.5 text-xs text-muted">{hint}</dd> : null}
    </div>
  );
}

/** Long hex / addresses: monospace, wrap anywhere. */
export function Mono({ children, className }: { children: ReactNode; className?: string }) {
  return <code className={cx("font-mono text-[0.85em] break-all text-text", className)}>{children}</code>;
}

export function KeyValue({ rows }: { rows: [ReactNode, ReactNode][] }) {
  return (
    <dl className="divide-y divide-border text-sm">
      {rows.map(([k, v], i) => (
        <div key={i} className="grid gap-1 py-2 sm:grid-cols-[12rem_1fr] sm:gap-4">
          <dt className="text-muted">{k}</dt>
          <dd className="min-w-0">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

/** Wizard progress. */
export function Steps({ steps, current }: { steps: string[]; current: number }) {
  return (
    <ol className="flex flex-wrap gap-x-4 gap-y-2 text-sm" aria-label="Progress">
      {steps.map((s, i) => (
        <li
          key={s}
          aria-current={i === current ? "step" : undefined}
          className={cx(
            "flex items-center gap-2",
            i === current ? "text-text" : i < current ? "text-ok" : "text-faint",
          )}
        >
          <span
            aria-hidden="true"
            className={cx(
              "flex h-6 w-6 items-center justify-center rounded-full border text-xs",
              i === current ? "border-accent text-accent" : i < current ? "border-ok" : "border-border",
            )}
          >
            {i < current ? "✓" : i + 1}
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
