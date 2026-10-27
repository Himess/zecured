"use client";

import Link from "next/link";
import { IconArrowRight, IconSwap } from "@/components/icons";
import { showOnArrival } from "@/components/replay/spotlight";
import { buttonClass, cx } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import { useReplay } from "@/lib/replay";
import { useMounted } from "../client";

const TESTNET = DEPLOYMENT.kind === "testnet";

/** The run in the site's five chapters; π marks the three that end in a proof on Base. */
const CHAPTERS = [
  { n: "01", name: "Deposit", proof: false },
  { n: "02", name: "Prove", proof: true },
  { n: "03", name: "Borrow", proof: false },
  { n: "04", name: "Close", proof: true },
  { n: "05", name: "Swap", proof: true },
];

/**
 * The home page's actions. In the judges' replay (DEMO) one primary starts or continues the run where it is, with
 * the run's chapters under it; in a live build, borrowing, buying and preparing.
 */
export function HeroActions() {
  const r = useReplay();
  const mounted = useMounted();
  if (r.active && r.recording) {
    // The replay's progress lives in this tab: until mounted, the first step.
    const next = mounted ? r.next : (r.steps[0] ?? null);
    const started = mounted && r.index >= 0;
    return (
      <div className="space-y-5">
        <div className="flex flex-wrap gap-3">
          {next ? (
            <Link href={next.href} onClick={() => showOnArrival(next.id)} className={cx(buttonClass("primary", undefined, "lg"), "max-sm:w-full")}>
              {started ? `Continue: ${next.title}` : "Start the replay: open a vault"}
              <IconArrowRight size={18} />
            </Link>
          ) : (
            <button
              type="button"
              onClick={() => {
                r.reset();
                window.location.assign(`${process.env.NEXT_PUBLIC_BASE_PATH ?? ""}/`);
              }}
              className={cx(buttonClass("primary", undefined, "lg"), "max-sm:w-full")}
            >
              Replay it again
            </button>
          )}
          <Link href="/buy" className={cx(buttonClass("secondary", undefined, "lg"), "max-sm:w-full")}>
            <IconSwap size={18} />
            Buy shielded ZEC
          </Link>
        </div>
        <div className="space-y-2">
          <ol aria-label="The replay's chapters" className="flex flex-wrap items-center gap-x-1 gap-y-2 text-[13px] text-soft max-sm:gap-x-3">
            {CHAPTERS.map((c, i) => (
              <li key={c.n} className="flex items-center">
                <span className="font-mono text-[11.5px] text-muted">{c.n}</span>
                <span className="ml-1.5">{c.name}</span>
                {c.proof ? (
                  <span className="ml-1.5 inline-flex size-[16px] items-center justify-center rounded-full bg-accent">
                    <svg width="9" height="8" viewBox="0 0 28 24" aria-hidden="true">
                      <path d="M3 6.5h22M9.5 6.5v14.5" stroke="#0F1B2A" strokeWidth="3.8" strokeLinecap="round" />
                      <path d="M18.2 6.5v10.6c0 2.5 1.3 3.9 3.8 3.9" stroke="#0F1B2A" strokeWidth="3.8" strokeLinecap="round" fill="none" />
                    </svg>
                    <span className="sr-only">, ends in a proof on Base</span>
                  </span>
                ) : null}
                {i < CHAPTERS.length - 1 ? <span aria-hidden="true" className="mx-2.5 h-px w-4 bg-line max-sm:hidden" /> : null}
              </li>
            ))}
          </ol>
          <p className="text-[12.5px] text-muted">
            {r.recording.placeholder
              ? `${r.steps.length} steps in your browser, with sample data: the app's own code, no chain touched.`
              : `${r.steps.length} steps of the live run, each on-chain one linked to its real transaction.`}
          </p>
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-wrap gap-3">
      <Link href="/borrow" className={cx(buttonClass("primary", undefined, "lg"), "max-sm:w-full")}>
        {TESTNET ? "Borrow on testnet" : "Borrow (invite only)"}
        <IconArrowRight size={18} />
      </Link>
      <Link href="/buy" className={cx(buttonClass("secondary", undefined, "lg"), "max-sm:w-full")}>
        <IconSwap size={18} />
        Buy shielded ZEC
      </Link>
      <Link href="/prepare" className={cx(buttonClass("ghost", undefined, "lg"), "max-sm:w-full")}>
        Prepare your ZEC first
      </Link>
    </div>
  );
}
