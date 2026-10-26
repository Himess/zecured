"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";
import { useReplay, type ReplayArtifact, type ReplayStepView } from "@/lib/replay";
import { useMounted } from "../client";
import { IconArrowRight, IconCheck, IconChevronDown, IconExternal, IconRefresh, IconX } from "../icons";
import { BaseMark, ZecMark } from "../marks";
import { ProofSeal } from "../seam";
import { Badge, buttonClass, cx } from "../ui";

const when = (iso: string | null) => (iso ? `${iso.slice(0, 16).replace("T", " ")} UTC` : null);

/** A path without its trailing slash (the static export serves `/borrow/`) and fragment, to compare pages. */
const page = (p: string) => p.split("#")[0]!.replace(/\/+$/, "") || "/";

function took(s: number | null): string | null {
  if (s === null) return null;
  if (s < 90) return `${s} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${Math.round(s / 360) / 10} h`;
}

function ArtifactLink({ a, url }: { a: ReplayArtifact; url: string | null }) {
  const mark = a.chain === "zcash" ? <ZecMark size={14} /> : a.chain === "base" ? <BaseMark size={13} /> : null;
  return (
    <li className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[12.5px]">
      {a.kind === "proof" ? <ProofSeal size="sm" className="!size-[14px] !shadow-none" /> : mark}
      <span className="text-soft">{a.label}</span>
      {url ? (
        <a href={url} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="inline-flex items-center gap-1 font-semibold text-info no-underline hover:underline">
          {a.chain === "zcash" ? "Zcash explorer" : "Basescan"}
          <IconExternal size={12} />
          <span className="sr-only">(opens a new tab)</span>
        </a>
      ) : a.tx ? (
        <span className="font-mono text-faint">{`${a.tx.slice(0, 10)}…${a.tx.slice(-6)}`}</span>
      ) : null}
      {a.meta
        ? Object.entries(a.meta).map(([k, v]) => (
            <span key={k} className="text-faint">
              · {k} <span className={/^0x[0-9a-f]+$/i.test(v) ? "font-mono" : undefined}>{/^0x[0-9a-f]{20,}$/i.test(v) ? `${v.slice(0, 10)}…${v.slice(-6)}` : v}</span>
            </span>
          ))
        : null}
    </li>
  );
}

function StepRow({ s, explorerUrl }: { s: ReplayStepView; explorerUrl: (a: ReplayArtifact) => string | null }) {
  return (
    <li className="grid grid-cols-[26px_minmax(0,1fr)] gap-3 py-2.5">
      <span
        aria-hidden="true"
        className={cx(
          "mt-0.5 inline-flex size-[26px] items-center justify-center rounded-full text-[11px] font-bold",
          s.status === "done"
            ? "bg-ok/15 text-ok"
            : s.status === "now"
              ? "bg-accent/15 text-accent shadow-[inset_0_0_0_1.5px_var(--color-accent)]"
              : s.status === "next"
                ? "bg-surface-3 text-text shadow-[inset_0_0_0_1.5px_var(--color-soft)]"
                : "text-faint shadow-[inset_0_0_0_1px_var(--color-line)]",
        )}
      >
        {s.status === "done" ? <IconCheck size={14} strokeWidth={2.6} /> : s.status === "now" ? <span className="dot-live size-2 rounded-full bg-accent" /> : null}
      </span>
      <div className="min-w-0">
        <p className="flex flex-wrap items-baseline justify-between gap-x-3">
          <span className={cx("font-semibold", s.status === "later" ? "text-muted" : "text-text")}>
            {s.title}
            <span className="sr-only"> ({s.status})</span>
          </span>
          {s.at ? (
            <span className="text-[11.5px] text-faint">
              live run {when(s.at)}
              {took(s.durationSeconds) ? ` · took ${took(s.durationSeconds)}` : ""}
            </span>
          ) : null}
        </p>
        <p className="text-[13px] text-muted">{s.status === "next" ? s.hint : s.detail}</p>
        {s.status === "done" && s.artifacts.length > 0 ? (
          <ul className="mt-1.5 space-y-1">
            {s.artifacts.map((a) => (
              <ArtifactLink key={`${a.label}:${a.tx ?? ""}`} a={a} url={explorerUrl(a)} />
            ))}
          </ul>
        ) : null}
      </div>
    </li>
  );
}

/**
 * The replay's dock (DEMO builds only): where the recorded live run stands, what to do next, and, for every step
 * done, the real transactions that made it, linked to their explorers. It floats above the page's bottom edge.
 */
export function ReplayDock() {
  const r = useReplay();
  const pathname = usePathname();
  const [open, setOpen] = useState(false);
  // The replay's state lives in this tab's sessionStorage: drawn only once mounted, never in the static HTML.
  const mounted = useMounted();
  // A step done closes nothing; a new page keeps the dock as the judge left it.
  useEffect(() => {
    if (r.next?.status === "next" && page(r.next.href) === page(pathname)) setOpen(false);
  }, [r.next?.id, r.next?.status, r.next?.href, pathname]);
  if (!mounted || !r.active || !r.recording) return null;
  const done = r.steps.filter((s) => s.status === "done").length;
  const total = r.steps.length;
  const next = r.next;
  const loan = r.steps.filter((s) => s.track === "loan");
  const swap = r.steps.filter((s) => s.track === "swap");

  return (
    <aside
      aria-label="Replay of the live run"
      className="fixed inset-x-0 bottom-[calc(58px+env(safe-area-inset-bottom))] z-30 px-3 pb-3 min-[861px]:bottom-0 min-[861px]:pb-5"
    >
      <div className="mx-auto max-w-3xl overflow-hidden rounded-[22px] bg-[color-mix(in_oklab,var(--color-surface)_92%,transparent)] shadow-[0_0_0_1px_var(--color-line),0_30px_60px_-20px_rgb(0_0_0/0.9)] backdrop-blur-xl">
        {open ? (
          <div className="max-h-[60vh] overflow-y-auto border-b border-hairline px-5 pt-4 pb-2">
            <div className="flex items-start justify-between gap-3">
              <div>
                <p className="text-[15px] font-bold">The live run, step by step</p>
                <p className="mt-0.5 max-w-xl text-[12.5px] text-muted">{r.recording.note}</p>
              </div>
              <button
                type="button"
                onClick={() => setOpen(false)}
                className="inline-flex size-8 flex-none items-center justify-center rounded-full text-muted hover:bg-surface-3 hover:text-text"
              >
                <IconX size={16} />
                <span className="sr-only">Close the steps</span>
              </button>
            </div>
            {[
              { name: "Borrow against shielded ZEC", list: loan },
              { name: "Buy shielded ZEC", list: swap },
            ].map((g) =>
              g.list.length ? (
                <section key={g.name} className="mt-3">
                  <h3 className="text-[12px] font-bold tracking-[0.06em] text-faint uppercase">{g.name}</h3>
                  <ol className="divide-y divide-hairline">
                    {g.list.map((s) => (
                      <StepRow key={s.id} s={s} explorerUrl={r.explorerUrl} />
                    ))}
                  </ol>
                </section>
              ) : null,
            )}
            <div className="flex justify-end py-3">
              <button
                type="button"
                onClick={() => {
                  r.reset();
                  window.location.assign(`${process.env.NEXT_PUBLIC_BASE_PATH ?? ""}/`);
                }}
                className={buttonClass("ghost", undefined, "sm")}
              >
                <IconRefresh size={15} />
                Start the replay over
              </button>
            </div>
          </div>
        ) : null}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
          <Badge tone="warn">DEMO</Badge>
          <div className="min-w-0 flex-1">
            <p className="truncate text-[13.5px] font-semibold">
              {next ? (next.status === "now" ? next.title : `Next: ${next.title}`) : "The whole run is replayed"}
            </p>
            <div className="mt-1.5 flex items-center gap-2">
              <span aria-hidden="true" className="h-1 flex-1 overflow-hidden rounded-full bg-surface-3">
                <span className="block h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: `${(done / total) * 100}%` }} />
              </span>
              <span className="text-[11.5px] text-muted tabular-nums">
                {done}/{total}
              </span>
            </div>
          </div>
          {next && next.status === "next" && page(next.href) !== page(pathname) ? (
            <Link href={next.href} className={buttonClass("primary", undefined, "sm")}>
              Go there
              <IconArrowRight size={15} />
            </Link>
          ) : null}
          <button
            type="button"
            aria-expanded={open}
            onClick={() => setOpen((o) => !o)}
            className={buttonClass("secondary", undefined, "sm")}
          >
            {open ? "Hide steps" : (
              <>
                <span className="max-sm:hidden">All steps and proofs</span>
                <span className="sm:hidden">Steps</span>
              </>
            )}
            <IconChevronDown size={15} className={cx("transition-transform", open ? "" : "rotate-180")} />
          </button>
        </div>
      </div>
    </aside>
  );
}

/** The band above every page of a replay build: what the judge is looking at. */
export function ReplayBand() {
  const r = useReplay();
  if (!r.active || !r.recording) return null;
  return (
    <div role="note" className="border-b border-warn/40 bg-warn/[0.08] px-4 py-2 text-center text-[13px] font-medium text-soft">
      <strong className="font-bold text-warn">DEMO</strong> · A replay of the live run on Base Sepolia, played in your browser: nothing here
      touches a chain. {r.recording.placeholder ? "Placeholder data until the live run is recorded." : "Every link opens the real transaction."}
    </div>
  );
}
