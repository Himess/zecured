"use client";

import { useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { cx } from "./ui";

export interface TabItem {
  id: string;
  label: ReactNode;
  icon?: ReactNode;
  content: ReactNode;
  /** A hairline after this tab: it stands apart from the ones that follow (the way out, before the adjustments). */
  divider?: boolean;
}

/**
 * Tabs as the ARIA pattern has them: arrow keys move between tabs, only the active tab is in the tab order, and
 * each panel is labelled by its tab. Panels not shown are not rendered, so their polling stops.
 */
export function Tabs({ items, initial, label }: { items: TabItem[]; initial?: string; label: string }) {
  const base = useId();
  const [active, setActive] = useState(() => (items.some((t) => t.id === initial) ? initial! : items[0]?.id));
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const current = items.find((t) => t.id === active) ?? items[0];
  if (!current) return null;

  const onKey = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
    const n = items.length;
    const to = e.key === "ArrowRight" ? (i + 1) % n : e.key === "ArrowLeft" ? (i - 1 + n) % n : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : -1;
    if (to < 0) return;
    e.preventDefault();
    setActive(items[to]!.id);
    refs.current[to]?.focus();
  };

  return (
    <div className="space-y-5">
      <div
        role="tablist"
        aria-label={label}
        className="flex gap-1 overflow-x-auto rounded-full bg-bg/80 p-1 shadow-[inset_0_0_0_1px_var(--color-border)] [mask-image:linear-gradient(90deg,#000_88%,transparent)] [scrollbar-width:none] sm:[mask-image:none]"
      >
        {items.map((t, i) => {
          const selected = t.id === current.id;
          return (
            <span key={t.id} className="flex flex-none items-center">
            <button
              ref={(el) => {
                refs.current[i] = el;
              }}
              type="button"
              role="tab"
              id={`${base}-tab-${t.id}`}
              aria-selected={selected}
              aria-controls={`${base}-panel-${t.id}`}
              tabIndex={selected ? 0 : -1}
              onClick={() => setActive(t.id)}
              onKeyDown={(e) => onKey(e, i)}
              className={cx(
                "inline-flex min-h-10 flex-none items-center gap-2 rounded-full px-4 text-[14px] font-semibold whitespace-nowrap transition-colors",
                selected ? "bg-surface-3 text-text shadow-[inset_0_0_0_1px_var(--color-line)]" : "text-muted hover:text-text",
              )}
            >
              {t.icon ? <span className={selected ? "text-text" : "text-muted"}>{t.icon}</span> : null}
              {t.label}
            </button>
            {t.divider && i < items.length - 1 ? <span aria-hidden="true" className="mx-1.5 h-5 w-px bg-line" /> : null}
            </span>
          );
        })}
      </div>
      <div role="tabpanel" id={`${base}-panel-${current.id}`} aria-labelledby={`${base}-tab-${current.id}`}>
        {current.content}
      </div>
    </div>
  );
}
