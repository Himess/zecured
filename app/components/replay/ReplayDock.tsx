"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useId, useRef, useState } from "react";
import { loanTokenName } from "@/lib/labels";
import { formatUsdc } from "@/lib/morpho";
import { useReplay, type ReplayArtifact, type ReplayRecording, type ReplayStepId, type ReplayStepView } from "@/lib/replay";
import { formatTaz, TICKER } from "@/lib/zcash";
import { zec5 } from "../loan";
import { useMounted } from "../client";
import { IconArrowRight, IconCheck, IconChevronDown, IconExternal, IconPlay, IconRefresh, IconX } from "../icons";
import { ProofSeal } from "../seam";
import { Badge, buttonClass, cx, Stat } from "../ui";
import { clearSpotlight, showOnArrival, spotlight, takeArrival } from "./spotlight";

/** The run in the site's five chapters (the recording's steps keep their own order). */
const CHAPTERS: { n: string; name: string; ids: ReplayStepId[] }[] = [
  { n: "01", name: "Deposit", ids: ["opened", "deposit-paid", "deposit-seen"] },
  { n: "02", name: "Prove", ids: ["proven", "activated"] },
  { n: "03", name: "Borrow", ids: ["borrowed"] },
  { n: "04", name: "Close", ids: ["destination", "close-executed", "payouts-committed", "payouts-sent", "payouts-confirmed"] },
  { n: "05", name: "Swap", ids: ["swap-opened", "swap-paid", "swap-proven", "swap-settled"] },
];

/** The steps a zero-knowledge proof completes: gold on the rail once done. */
const PROOF_STEPS: ReadonlySet<ReplayStepId> = new Set(["proven", "payouts-confirmed", "swap-proven"]);

/** What an automatic step says while it is under way. */
const UNDER_WAY: Partial<Record<ReplayStepId, string>> = {
  "deposit-seen": "Confirming the deposit on Zcash…",
  proven: "Proving the deposit in zero knowledge…",
  activated: "Minting zZEC into Morpho…",
  "payouts-committed": "Committing the payout on Base…",
  "payouts-sent": "Paying on Zcash: 3 of 5 signers sign…",
  "payouts-confirmed": "Proving the payout in zero knowledge…",
  "swap-paid": "The maker pays on Zcash…",
  "swap-proven": "Proving the maker's payment in zero knowledge…",
  "swap-settled": "Releasing the locked tokens to the maker…",
};

/** A path without its trailing slash (the static export serves `/borrow/`) and fragment, to compare pages. */
const page = (p: string) => p.split("#")[0]!.replace(/\/+$/, "") || "/";

const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH ?? "";
/** The site beside the replay on Pages (`/<repo>/` next to `/<repo>/app/`). */
const SITE = BASE_PATH ? `${BASE_PATH.replace(/\/app\/?$/, "")}/` : "https://himess.github.io/zecured/";

function took(s: number | null): string | null {
  if (s === null) return null;
  if (s < 90) return `${s} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${Math.round(s / 360) / 10} h`;
}

const shortHash = (h: string) => `${h.slice(0, 8)}…${h.slice(-6)}`;

function ArtifactChip({ a, url, sample }: { a: ReplayArtifact; url: string | null; sample: boolean }) {
  const chip =
    a.chain === "zcash"
      ? "rounded-full bg-bg text-text shadow-[inset_0_0_0_1px_var(--color-line)]"
      : a.chain === "base"
        ? "rounded-[2px] bg-paper text-ink"
        : "rounded-full text-muted shadow-[inset_0_0_0_1px_var(--color-hairline)]";
  return (
    <li className="flex min-h-7 flex-wrap items-center gap-x-2 gap-y-1 text-[12.5px]">
      {a.kind === "proof" ? <ProofSeal size="sm" className="!size-[16px] !shadow-none" /> : null}
      <span className={cx("inline-flex min-h-[22px] items-center px-2 text-[11.5px] font-semibold", chip)}>
        {a.chain === "zcash" ? "Zcash" : a.chain === "base" ? "Base" : "Off-chain"}
      </span>
      <span className="text-soft">{a.label}</span>
      {url ? (
        <a
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          referrerPolicy="no-referrer"
          className="inline-flex min-h-6 items-center gap-1 font-mono text-[11.5px] text-info no-underline hover:underline"
        >
          {a.tx ? shortHash(a.tx) : a.chain === "zcash" ? "Zcash explorer" : "Basescan"}
          <IconExternal size={12} />
          <span className="sr-only">(opens a new tab, a third-party site)</span>
        </a>
      ) : sample ? (
        <span className="text-faint">· sample data</span>
      ) : a.tx ? (
        <span className="font-mono text-[11.5px] text-faint">{shortHash(a.tx)}</span>
      ) : null}
      {a.meta
        ? Object.entries(a.meta).map(([k, v]) => (
            <span key={k} className="text-faint">
              · {k} <span className={/^0x[0-9a-f]+$/i.test(v) ? "font-mono" : undefined}>{/^0x[0-9a-f]{20,}$/i.test(v) ? shortHash(v) : v}</span>
            </span>
          ))
        : null}
    </li>
  );
}

function StepRow({ s, explorerUrl, sample }: { s: ReplayStepView; explorerUrl: (a: ReplayArtifact) => string | null; sample: boolean }) {
  return (
    <li className="grid grid-cols-[22px_minmax(0,1fr)] gap-3 py-2.5">
      <span
        aria-hidden="true"
        className={cx(
          "mt-0.5 inline-flex size-[22px] items-center justify-center rounded-full",
          s.status === "done"
            ? PROOF_STEPS.has(s.id)
              ? "bg-accent text-on-accent"
              : "bg-soft text-bg"
            : s.status === "now"
              ? "shadow-[inset_0_0_0_2px_var(--color-text)]"
              : s.status === "next"
                ? "shadow-[inset_0_0_0_2px_var(--color-demo)]"
                : "shadow-[inset_0_0_0_1px_var(--color-line)]",
        )}
      >
        {s.status === "done" ? <IconCheck size={13} strokeWidth={3} /> : s.status === "now" ? <span className="dot-live size-1.5 rounded-full bg-text" /> : null}
      </span>
      <div className="min-w-0">
        <p className="flex flex-wrap items-baseline justify-between gap-x-3">
          <span className={cx("text-[14px] font-semibold", s.status === "later" ? "text-muted" : "text-text")}>
            {s.status === "now" ? (UNDER_WAY[s.id] ?? s.title) : s.title}
            <span className="sr-only"> ({s.status === "next" ? "your next step" : s.status === "now" ? "under way" : s.status})</span>
          </span>
          {!sample && took(s.durationSeconds) ? <span className="text-[11.5px] text-faint">took {took(s.durationSeconds)} in the live run</span> : null}
        </p>
        <p className={cx("text-[13px]", s.status === "next" ? "text-demo" : "text-muted")}>{s.status === "next" ? s.hint : s.detail}</p>
        {s.status === "done" && s.artifacts.length > 0 ? (
          <ul className="mt-1.5 space-y-0.5">
            {s.artifacts.map((a) => (
              <ArtifactChip key={`${a.label}:${a.tx ?? ""}`} a={a} url={explorerUrl(a)} sample={sample} />
            ))}
          </ul>
        ) : null}
      </div>
    </li>
  );
}

/** The run as a rail of 15 notches in five chapters: soft once done, gold where a proof landed, white for now. */
function Rail({ steps }: { steps: ReplayStepView[] }) {
  const byId = new Map(steps.map((s) => [s.id, s]));
  return (
    <span aria-hidden="true" className="flex items-center gap-1.5">
      {CHAPTERS.map((c) => (
        <span key={c.n} className="flex items-center gap-[2px]">
          {c.ids.map((id) => {
            const s = byId.get(id);
            const status = s?.status ?? "later";
            return (
              <span
                key={id}
                className={cx(
                  "h-1 w-2 rounded-full transition-colors duration-300",
                  status === "done"
                    ? PROOF_STEPS.has(id)
                      ? "bg-accent shadow-[0_0_6px_rgb(244_183_40/0.8)]"
                      : "bg-soft/70"
                    : status === "now"
                      ? "dot-live bg-text"
                      : status === "next"
                        ? "bg-demo"
                        : "bg-surface-3",
                )}
              />
            );
          })}
        </span>
      ))}
    </span>
  );
}

/**
 * The replay's dock (DEMO builds only), docked at the bottom of every page: where the run stands, what to do next
 * and where ("Show me" points at the control; "Go there" opens its page and points at it there), and, in its
 * sheet, every step by chapter with the transactions that made it. Its state lives in this tab only.
 */
export function ReplayDock() {
  const r = useReplay();
  const pathname = usePathname();
  const [open, setOpen] = useState(false);
  const mounted = useMounted();
  const sheetId = useId();
  const toggle = useRef<HTMLButtonElement | null>(null);
  const sheetHeading = useRef<HTMLHeadingElement | null>(null);
  const [announce, setAnnounce] = useState("");
  const announced = useRef<Set<string> | null>(null);
  const next = r.next;

  // A step done is announced once (screen readers), never on the first render: the newest of those that just turned
  // done, by time (the loan and the swap move independently, so the list's order says nothing).
  const done = r.steps.filter((s) => s.status === "done");
  const doneKey = done.map((s) => s.id).join(",");
  useEffect(() => {
    if (!mounted || !r.active) return;
    const before = announced.current;
    if (before) {
      const fresh = done.filter((s) => !before.has(s.id)).sort((a, b) => (a.doneAt ?? 0) - (b.doneAt ?? 0));
      if (fresh.length > 0) setAnnounce(`Done: ${fresh[fresh.length - 1]!.title}`);
    }
    announced.current = new Set(done.map((s) => s.id));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per change of the set of done steps
  }, [mounted, r.active, doneKey]);

  // A link that brought the judge here ("Go there", "Start") asked for the control to be pointed at, once shown.
  useEffect(() => {
    if (!mounted || !r.active) return;
    clearSpotlight();
    const id = takeArrival();
    if (!id) return;
    let tries = 0;
    const timer = setInterval(() => {
      if (spotlight(id) || ++tries > 25) clearInterval(timer);
    }, 200);
    return () => clearInterval(timer);
  }, [mounted, r.active, pathname]);

  // Closing the sheet with Escape, focus back on its toggle.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        toggle.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    sheetHeading.current?.focus();
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  if (!mounted || !r.active || !r.recording) return null;
  const sample = r.recording.placeholder;
  const total = r.steps.length;
  const finished = !next;
  const here = next && next.status === "next" && page(next.href) === page(pathname);
  const title = next ? (next.status === "now" ? (UNDER_WAY[next.id] ?? next.title) : `Next: ${next.title}`) : "Replay complete";
  const hint = next ? (next.status === "now" ? next.detail : next.hint) : "Every step, with what it left on each chain, is in the recap.";
  const close = () => {
    setOpen(false);
    toggle.current?.focus();
  };
  const restart = () => {
    r.reset();
    window.location.assign(`${BASE_PATH}/`);
  };
  const proofs = r.steps.filter((s) => PROOF_STEPS.has(s.id)).length;

  // On Home the hero's primary already goes there ("Start the replay", "Continue: …").
  const action =
    next && next.status === "next" && page(pathname) !== "/" ? (
      here ? (
        <button type="button" onClick={() => spotlight(next.id)} className={buttonClass("secondary", "flex-none", "sm")}>
          Show me
        </button>
      ) : (
        <Link href={next.href} onClick={() => showOnArrival(next.id)} className={buttonClass("secondary", "flex-none", "sm")}>
          Go there
          <IconArrowRight size={15} />
        </Link>
      )
    ) : null;

  return (
    <aside aria-label="Replay of the run" className="fixed inset-x-0 bottom-[calc(var(--nav-h)+env(safe-area-inset-bottom))] z-30 min-[861px]:bottom-0">
      <p aria-live="polite" className="sr-only">
        {announce}
      </p>
      {open ? <button type="button" aria-label="Close the steps" tabIndex={-1} onClick={close} className="fixed inset-0 -z-10 cursor-default bg-black/45" /> : null}
      <div className="flex flex-col-reverse">
        {/* The bar comes first in the page's order (and the sheet after it), but the sheet is drawn above it. */}
        <div className="border-t border-line/70 bg-[color-mix(in_oklab,var(--color-surface)_94%,transparent)] backdrop-blur-xl">
          <div className="wrap flex min-h-[var(--dock-h)] items-center gap-x-3 py-2.5 sm:gap-x-4">
            <span className="max-sm:hidden">
              <Badge tone="demo">DEMO</Badge>
            </span>
            <div className="min-w-0 flex-1">
              <p className="flex min-w-0 items-baseline gap-2 text-[13.5px] max-[1100px]:flex-col max-[1100px]:gap-0">
                <span
                  className={cx(
                    "font-semibold min-[1101px]:shrink-0 max-[1100px]:max-w-full max-[1100px]:truncate",
                    finished ? "text-text" : next?.status === "now" ? "text-text" : "text-demo",
                  )}
                >
                  {title}
                </span>
                <span className="min-w-0 truncate text-muted max-[1100px]:max-w-full max-[1100px]:text-[12.5px]" title={hint}>
                  {hint}
                </span>
              </p>
              <p className="mt-1 flex items-center gap-2.5">
                <Rail steps={r.steps} />
                <span className="text-[11.5px] text-muted tabular-nums">
                  {done.length}/{total}
                </span>
              </p>
            </div>
            {action}
            {finished && !open ? (
              <button type="button" onClick={() => setOpen(true)} className={buttonClass("primary", "flex-none", "sm")}>
                See the recap
              </button>
            ) : null}
            <button
              ref={toggle}
              type="button"
              aria-expanded={open}
              aria-controls={sheetId}
              onClick={() => setOpen((o) => !o)}
              className={buttonClass("ghost", "flex-none max-sm:!px-2.5", "sm")}
            >
              <span className="max-sm:sr-only">{open ? "Hide the steps" : "Steps and proofs"}</span>
              <IconChevronDown size={15} className={cx("transition-transform", open ? "" : "rotate-180")} />
            </button>
          </div>
        </div>
        {open ? (
          <div
            id={sheetId}
            className="wrap max-h-[min(64vh,calc(100dvh-var(--header-h)-var(--nav-h)-var(--dock-h)-1rem))] overflow-y-auto overscroll-contain"
          >
            <div className="animate-rise rounded-t-[20px] border border-b-0 border-line/70 bg-surface px-5 pt-4 pb-3 shadow-[0_-24px_60px_-20px_rgb(0_0_0/0.85)] sm:px-6">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <h2 ref={sheetHeading} tabIndex={-1} className="text-[16px] font-bold outline-none">
                    {sample ? "The replay" : "The live run"} · {total} steps · {proofs} proofs
                  </h2>
                  <p className="mt-0.5 max-w-2xl text-[12.5px] text-muted">
                    {sample
                      ? "Sample data, played by your browser: the hashes below are not real transactions."
                      : r.recording.note}{" "}
                    Progress is kept in this tab only.
                  </p>
                </div>
                <div className="flex flex-none items-center gap-1">
                  <button type="button" onClick={restart} className={buttonClass("ghost", undefined, "sm")}>
                    <IconRefresh size={15} />
                    <span className="max-sm:sr-only">Replay again</span>
                  </button>
                  <button
                    type="button"
                    onClick={close}
                    className="inline-flex size-9 flex-none items-center justify-center rounded-full text-muted hover:bg-surface-3 hover:text-text"
                  >
                    <IconX size={16} />
                    <span className="sr-only">Close the steps</span>
                  </button>
                </div>
              </div>
              {finished ? <Recap rec={r.recording} /> : null}
              {CHAPTERS.map((c) => {
                const list = c.ids.map((id) => r.steps.find((s) => s.id === id)).filter((s): s is ReplayStepView => Boolean(s));
                return list.length ? (
                  <section key={c.n} className="mt-3 grid gap-x-4 sm:grid-cols-[92px_minmax(0,1fr)]">
                    <h3 className="pt-3 font-mono text-[11.5px] tracking-[0.04em] text-muted uppercase">
                      {c.n} {c.name}
                    </h3>
                    <ol className="divide-y divide-hairline">
                      {list.map((s) => (
                        <StepRow key={s.id} s={s} explorerUrl={r.explorerUrl} sample={sample} />
                      ))}
                    </ol>
                  </section>
                ) : null;
              })}
            </div>
          </div>
        ) : null}
      </div>
    </aside>
  );
}

/** The end of the run: its figures, what happened, and where to look next. */
function Recap({ rec }: { rec: ReplayRecording }) {
  const sample = rec.placeholder;
  const token = loanTokenName();
  return (
    <div className="mt-4 rounded-2xl bg-bg/60 p-4 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      <p className="flex items-center gap-2 text-[15px] font-bold">
        <ProofSeal size="sm" className="!size-[20px] !shadow-none" />
        A whole loan and a swap, replayed
      </p>
      <dl className="mt-3 grid grid-cols-2 gap-4 sm:grid-cols-4">
        <Stat label="Locked, shielded" value={formatTaz(BigInt(rec.loan.amount))} />
        <Stat label="Borrowed" value={`${formatUsdc(BigInt(rec.loan.borrow.assets))} ${token}`} />
        <Stat label="Back to you, shielded" value={`${zec5(BigInt(rec.loan.close.userAmount))} ${TICKER}`} />
        <Stat label={`Swapped ${formatUsdc(BigInt(rec.swap.usdc))} ${token}`} value={formatTaz(BigInt(rec.swap.zatoshi))} />
      </dl>
      <ul className="mt-3 space-y-1 text-[13.5px] text-soft">
        <li>A vault opened off-chain, by five signers; nothing touched Base.</li>
        <li>Its deposit proven on Base in zero knowledge, and zZEC minted on that proof alone.</li>
        <li>USDC borrowed from a brand-new account that never held ETH.</li>
        <li>Closed from collateral: the payout committed on Base, paid on Zcash by 3 of 5 signers, then proven.</li>
        <li>A swap of USDC into shielded ZEC, released by a proof of the maker&apos;s payment.</li>
      </ul>
      <p className="mt-2 text-[12.5px] text-muted">
        {sample ? "This replay plays sample data; the steps and their checks are the app's own code." : "Each on-chain step above links to its real transaction."}
      </p>
      <div className="mt-3 flex flex-wrap gap-2">
        <Link href="/world" className={buttonClass("secondary", undefined, "sm")}>
          What the world saw
        </Link>
        <Link href="/evidence" className={buttonClass("secondary", undefined, "sm")}>
          The evidence
        </Link>
        <a href={`${SITE}docs/`} className={buttonClass("ghost", undefined, "sm")}>
          The docs
        </a>
      </div>
    </div>
  );
}

/** The band above every page of a replay build: what the judge is looking at. */
export function ReplayBand() {
  const r = useReplay();
  if (!r.active || !r.recording) return null;
  const sample = r.recording.placeholder;
  return (
    <div role="note" className="border-b border-demo/20 bg-demo/[0.06] px-4 py-1.5 text-center text-[12.5px] font-medium text-soft">
      <span className="inline-flex items-center gap-1.5 font-bold text-demo">
        <IconPlay size={11} strokeWidth={2.4} />
        DEMO
      </span>
      <span className="max-sm:hidden">
        {" "}
        ·{" "}
        {sample
          ? "A guided replay in your browser, with sample data: nothing here touches a chain."
          : "A replay of the live run on Zcash testnet and Base Sepolia, in your browser: nothing here touches a chain."}
      </span>
      <span className="sm:hidden"> · {sample ? "a guided replay, sample data" : "a replay of the live run"}</span>
    </div>
  );
}
