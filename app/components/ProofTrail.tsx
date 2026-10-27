"use client";

import type { Address, Hex } from "viem";
import { baseTxUrl, sampleTransactions } from "@/lib/explorer";
import { JOURNEY_LABEL, useJourney, type JourneyKind } from "@/lib/journey";
import { short } from "@/lib/keys";
import { REPLAY } from "@/lib/replay/flag";
import { recording } from "@/lib/replay/recording";
import { CopyButton } from "./client";
import { IconCheck, IconExternal, IconLayers, IconSwap } from "./icons";
import { BaseMark, UsdcMark, ZzecMark } from "./marks";
import { ProofSeal } from "./seam";
import { cx, Skeleton } from "./ui";

/** Each step's mark: the gold π where a zero-knowledge proof was verified; ink marks for the rest. */
function StepIcon({ kind }: { kind: JourneyKind }) {
  switch (kind) {
    case "proven":
    case "payout-proven":
      return <ProofSeal size="sm" className="!size-[22px] !shadow-none" />;
    case "activated":
      return <ZzecMark size={22} />;
    case "borrowed":
    case "repaid":
      return <UsdcMark size={22} />;
    case "close":
      return <IconSwap size={17} />;
    case "committed":
      return <IconLayers size={17} />;
    default:
      return <IconCheck size={17} strokeWidth={2.4} />;
  }
}

const time = (s: number) =>
  new Date(s * 1000).toLocaleString("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", second: "2-digit", timeZone: "UTC" }) +
  " UTC";

/**
 * DEMO: when each recorded step's own Base transaction was mined (its block's time in the live run), by hash. The
 * replay's chain has its own blocks and clock; next to a real hash it shows the recorded time instead, or none.
 */
function recordedTimes(): Map<string, number> {
  const times = new Map<string, number>();
  if (!REPLAY) return times;
  const r = recording();
  if (r.placeholder) return times;
  for (const s of r.steps) {
    if (s.tx.base && s.at) times.set(s.tx.base.toLowerCase(), Math.floor(Date.parse(s.at) / 1000));
  }
  return times;
}

/**
 * "What Base saw": the vault's trail on Base (lib/journey.ts) as Base keeps it, a light ruled ledger, each step with
 * its transaction, linked to the deployment's block explorer where the profile allows it. Nothing on it is a Zcash
 * identifier. `since`: the loan's creation, which bounds the search.
 */
export function ProofTrail({ vaultId, account, since, compact = false }: { vaultId: Hex; account?: Address; since?: number; compact?: boolean }) {
  const journey = useJourney(vaultId, account, { since });
  const steps = journey.data?.steps ?? [];
  const sample = sampleTransactions();
  const recorded = recordedTimes();

  const body = (
    <div className="space-y-3">
      {journey.loading && !journey.data ? (
        <div className="space-y-3">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-5/6" />
        </div>
      ) : steps.length === 0 ? (
        <p className="text-sm text-muted">
          {journey.error
            ? `Base could not be read: ${journey.error}`
            : "Nothing on Base yet: the vault was opened off-chain, and Base hears of it only when its deposit is proven."}
        </p>
      ) : (
        <ol className="paper divide-y divide-rule overflow-hidden rounded-[3px] shadow-[0_0_0_1px_rgb(15_27_42/0.08),0_30px_60px_-40px_rgb(0_0_0/0.9)]">
          {steps.map((s, i) => {
            const proof = s.kind === "proven" || s.kind === "payout-proven";
            const url = baseTxUrl(s.txHash);
            const at = REPLAY ? recorded.get(s.txHash.toLowerCase()) : journey.data?.times.get(s.blockNumber);
            return (
              <li
                key={`${s.txHash}:${s.logIndex}`}
                className={cx(
                  "grid grid-cols-[28px_minmax(0,1fr)] items-start gap-x-3 gap-y-1 px-4 py-3 sm:grid-cols-[28px_minmax(0,1fr)_auto]",
                  i === 0 && "border-t-[1.5px] border-t-ink",
                  proof && i === steps.length - 1 && "stamp-row",
                )}
              >
                <span className="mt-0.5 inline-flex size-7 items-center justify-center text-ink">
                  <StepIcon kind={s.kind} />
                </span>
                <div className="min-w-0">
                  <p className="text-[14px] font-semibold text-ink">{JOURNEY_LABEL[s.kind]}</p>
                  {s.detail ? <p className="text-[12.5px] text-ink-muted">{s.detail}</p> : null}
                </div>
                <div className="col-start-2 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11.5px] text-ink-muted sm:col-start-3 sm:justify-end sm:text-right">
                  {sample ? (
                    <span>sample data</span>
                  ) : url ? (
                    <a href={url} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" className="inline-flex items-center gap-1 font-semibold text-ink">
                      {short(s.txHash, 8, 6)}
                      <IconExternal size={12} />
                      <span className="sr-only">(Basescan, opens a new tab)</span>
                    </a>
                  ) : (
                    <span className="inline-flex items-center gap-1.5">
                      {short(s.txHash, 8, 6)}
                      <CopyButton value={s.txHash} label="Copy" className="!min-h-7 !bg-white !text-ink !shadow-[inset_0_0_0_1px_var(--color-rule)]" />
                    </span>
                  )}
                  {REPLAY ? null : <span className="tabular-nums">#{s.blockNumber.toLocaleString("en-US")}</span>}
                  {at ? <span className="tabular-nums">{time(at)}</span> : null}
                </div>
              </li>
            );
          })}
        </ol>
      )}
      {journey.data?.truncated ? (
        <p className="text-[12px] text-muted">Older blocks were not read: this browser does not know when the loan was opened.</p>
      ) : null}
      {steps.length > 0 ? (
        <p className="flex items-start gap-2 text-[12px] text-muted">
          <BaseMark size={12} className="mt-[3px]" />
          <span>
            {sample ? (
              "Sample data, played in this browser. The live app reads this ledger from Base through our proxy."
            ) : (
              <>
                {REPLAY ? "Recorded from Base in the live run." : "Read from Base through our proxy."}{" "}
                {steps.some((s) => baseTxUrl(s.txHash)) ? "A Basescan link opens a third-party site, which then learns which transaction you look at." : null}
              </>
            )}
          </span>
        </p>
      ) : null}
    </div>
  );

  if (compact) return body;
  return (
    <section aria-labelledby="trail-h" className="space-y-3">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h2 id="trail-h" className="flex items-center gap-2.5 text-[19px] font-extrabold font-stretch-116%">
          <BaseMark size={16} />
          What Base saw
        </h2>
        <span className="text-[12.5px] text-muted">every Zecured transaction of this loan</span>
      </div>
      {body}
    </section>
  );
}
