/**
 * Small, server-safe UI primitives (no hooks). Interactive pieces live in components/client.tsx, the seam's
 * marks (the π seal, chain labels, chips) in components/seam.tsx, other projects' marks in components/marks.tsx
 * and the line icons in components/icons.tsx.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { IconAlert, IconCheck, IconInfo } from "./icons";

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(" ");
}

/** A page's head: an eyebrow, the title, a lead and the page's own actions, then its content. */
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
    <div className="space-y-8">
      <header className="flex flex-wrap items-end justify-between gap-x-8 gap-y-4">
        <div className="min-w-0 space-y-3">
          {eyebrow ? <p className="flex items-center gap-2 text-[13.5px] font-semibold text-muted">{eyebrow}</p> : null}
          <h1 className="max-w-[24em] text-[length:clamp(28px,3vw,40px)] leading-[1.04] font-extrabold tracking-[-0.012em] text-balance font-stretch-125%">
            {title}
          </h1>
          {lead ? <p className="max-w-2xl text-[15.5px] text-soft">{lead}</p> : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </header>
      {children}
    </div>
  );
}

/** A panel: the dark ground raised a step. `lead` gives it the gold edge of the page's main panel. */
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
    <Tag className={cx("panel p-[clamp(20px,2.6vw,32px)]", lead && "panel-lead", className)}>
      {title || action ? (
        <div className="mb-5 flex flex-wrap items-center justify-between gap-3">
          {title ? (
            <h2 className="flex items-center gap-3 text-[20px] leading-[1.15] font-extrabold tracking-[-0.006em] font-stretch-118%">
              {icon ? (
                <span className="inline-flex size-9 flex-none items-center justify-center rounded-xl bg-surface-3 text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
                  {icon}
                </span>
              ) : null}
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

const toneGround: Record<Tone, string> = {
  info: "bg-[color-mix(in_oklab,var(--color-info)_6%,var(--color-bg))] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-info)_22%,transparent)]",
  ok: "bg-[color-mix(in_oklab,var(--color-ok)_6%,var(--color-bg))] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_24%,transparent)]",
  warn: "bg-[color-mix(in_oklab,var(--color-warn)_7%,var(--color-bg))] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_38%,transparent)]",
  danger: "bg-[color-mix(in_oklab,var(--color-danger)_8%,var(--color-bg))] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_50%,transparent)]",
};

const toneLabel: Record<Tone, string> = { info: "Note", warn: "Warning", danger: "Danger", ok: "Done" };
const toneText: Record<Tone, string> = { info: "text-info", warn: "text-warn", danger: "text-danger", ok: "text-ok" };

function ToneIcon({ tone }: { tone: Tone }) {
  if (tone === "ok") return <IconCheck size={18} strokeWidth={2.2} />;
  if (tone === "info") return <IconInfo size={18} />;
  return <IconAlert size={18} />;
}

/**
 * A callout: the tone's icon, the title, the text. Static notices are plain notes; `live` ones (results and errors
 * that appear after an action) are announced to screen readers.
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
      className={cx("grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 rounded-2xl px-4 py-3.5 text-[14.5px] sm:px-5 sm:py-4", toneGround[tone])}
    >
      <span aria-hidden="true" className={cx("mt-[2px]", toneText[tone])}>
        <ToneIcon tone={tone} />
      </span>
      <div className="min-w-0">
        <p className="text-[15px] leading-[1.4] font-bold text-text">
          <span className="sr-only">{toneLabel[tone]}: </span>
          {title ?? toneLabel[tone]}
        </p>
        {children ? <div className="mt-1.5 space-y-2 text-soft">{children}</div> : null}
      </div>
    </div>
  );
}

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md" | "lg";

/** Gold only on primary buttons; the others are raised, quiet or red-lined pills. */
const variantClasses: Record<Variant, string> = {
  primary:
    "bg-[linear-gradient(180deg,#ffc845,#f4b728)] font-bold text-on-accent shadow-[inset_0_1px_0_rgb(255_255_255/0.45),0_14px_30px_-16px_rgb(244_183_40/0.85)] hover:brightness-[1.06]",
  secondary: "bg-surface-2 font-semibold text-text shadow-[inset_0_0_0_1px_var(--color-line)] hover:bg-surface-3",
  ghost: "font-semibold text-muted hover:bg-surface-2 hover:text-text",
  danger: "font-semibold text-danger shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_60%,transparent)] hover:bg-danger/10",
};

const sizeClasses: Record<Size, string> = {
  sm: "min-h-9 px-3.5 text-[13.5px]",
  md: "min-h-11 px-5 text-[15px]",
  lg: "min-h-[52px] px-7 text-[16px]",
};

export function buttonClass(variant: Variant = "primary", className?: string, size: Size = "md"): string {
  return cx(
    "inline-flex items-center justify-center gap-2 rounded-full py-2 leading-tight transition-[background-color,filter,color] no-underline",
    "disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:brightness-100",
    sizeClasses[size],
    variantClasses[variant],
    className,
  );
}

export function Button({
  variant = "primary",
  size = "md",
  busy = false,
  className,
  children,
  disabled,
  type = "button",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size; busy?: boolean }) {
  return (
    <button
      type={type}
      className={buttonClass(variant, className, size)}
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

const toneBadge: Record<Tone, string> = {
  info: "text-info bg-info/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-info)_35%,transparent)]",
  ok: "text-ok bg-ok/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_35%,transparent)]",
  warn: "text-warn bg-warn/[0.08] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-warn)_42%,transparent)]",
  danger: "text-danger bg-danger/[0.09] shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-danger)_50%,transparent)]",
};

/** A sentence-case tag, its text in the tone's colour (neutral: soft, on a line ring); `dot` adds a status dot. */
export function Badge({ tone = "info", dot = false, children }: { tone?: Tone | "neutral"; dot?: boolean; children: ReactNode }) {
  const cls = tone === "neutral" ? "text-soft shadow-[inset_0_0_0_1px_var(--color-line)]" : toneBadge[tone];
  return (
    <span className={cx("inline-flex min-h-[24px] items-center gap-1.5 rounded-full px-2.5 py-[3px] text-xs leading-tight font-semibold", cls)}>
      {dot ? <span aria-hidden="true" className="size-1.5 rounded-full bg-current" /> : null}
      {children}
    </span>
  );
}

/** A figure: its label, the value large, a hint below. */
export function Stat({ label, value, hint, icon }: { label: ReactNode; value: ReactNode; hint?: ReactNode; icon?: ReactNode }) {
  return (
    <div className="min-w-0 rounded-2xl bg-bg/60 px-4 py-3.5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      <dt className="flex items-center gap-2 text-[13px] text-muted">
        {icon}
        {label}
      </dt>
      <dd className="mt-1 truncate text-[21px] leading-tight font-bold tabular-nums font-stretch-112%">{value}</dd>
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

/** Wizard progress: a check once done, a gold ring for the current step, a line joining them. */
export function Steps({ steps, current }: { steps: string[]; current: number }) {
  return (
    <ol className="flex flex-wrap items-center gap-x-2 gap-y-3" aria-label="Progress">
      {steps.map((s, i) => (
        <li
          key={s}
          aria-current={i === current ? "step" : undefined}
          className={cx(
            "flex items-center gap-2.5 text-[14.5px]",
            i === current ? "font-semibold text-text" : i < current ? "text-soft" : "text-faint",
          )}
        >
          <span
            aria-hidden="true"
            className={cx(
              "inline-flex size-7 flex-none items-center justify-center rounded-full text-[13px] font-bold",
              i === current
                ? "bg-accent/10 text-accent shadow-[inset_0_0_0_2px_var(--color-accent)]"
                : i < current
                  ? "bg-ok/15 text-ok"
                  : "shadow-[inset_0_0_0_1.5px_var(--color-line)]",
            )}
          >
            {i < current ? <IconCheck size={15} strokeWidth={2.6} /> : i + 1}
          </span>
          <span>
            {s}
            {i < current ? <span className="sr-only"> (done)</span> : null}
          </span>
          {i < steps.length - 1 ? (
            <span aria-hidden="true" className={cx("mx-1.5 hidden h-px w-8 sm:block", i < current ? "bg-ok/50" : "bg-line")} />
          ) : null}
        </li>
      ))}
    </ol>
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
 * A horizontal meter: `value` of `max`, with optional markers (a target, a limit) on the track. Used for LTV against
 * its limits and the market's utilization. Decorative: the caller states the numbers in text.
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

/**
 * A ring gauge: `value` from 0 to 1 around the circle, the figure and its label in the middle. The health factor's
 * dial. Decorative SVG; the figure is real text.
 */
export function Ring({
  value,
  size = 132,
  tone = "ok",
  children,
}: {
  value: number;
  size?: number;
  tone?: "ok" | "warn" | "danger" | "muted";
  children: ReactNode;
}) {
  const r = 52;
  const c = 2 * Math.PI * r;
  const v = Math.max(0, Math.min(1, value));
  const stroke = { ok: "var(--color-ok)", warn: "var(--color-warn)", danger: "var(--color-danger)", muted: "var(--color-line)" }[tone];
  return (
    <div className="relative inline-flex flex-none items-center justify-center" style={{ width: size, height: size }}>
      <svg width={size} height={size} viewBox="0 0 120 120" aria-hidden="true" className="-rotate-90">
        <circle cx="60" cy="60" r={r} fill="none" stroke="var(--color-surface-3)" strokeWidth="9" />
        <circle
          cx="60"
          cy="60"
          r={r}
          fill="none"
          stroke={stroke}
          strokeWidth="9"
          strokeLinecap="round"
          strokeDasharray={`${c * v} ${c}`}
          className="transition-[stroke-dasharray] duration-700"
        />
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center text-center">{children}</div>
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
    <div className="flex flex-col items-center gap-3 rounded-[20px] border border-dashed border-line/70 px-6 py-10 text-center">
      {icon ? (
        <span className="inline-flex size-12 items-center justify-center rounded-2xl bg-surface-2 text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
          {icon}
        </span>
      ) : null}
      <p className="text-[17px] font-bold">{title}</p>
      {children ? <div className="max-w-md text-sm text-muted">{children}</div> : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

/** A section's heading inside a page: a title, an optional line of context, an optional action on the right. */
export function SectionTitle({ id, title, sub, action }: { id?: string; title: ReactNode; sub?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-3">
      <div className="space-y-1">
        <h2 id={id} className="text-[length:clamp(21px,2vw,26px)] leading-[1.1] font-extrabold tracking-[-0.008em] font-stretch-118%">
          {title}
        </h2>
        {sub ? <p className="text-sm text-muted">{sub}</p> : null}
      </div>
      {action}
    </div>
  );
}
