"use client";

import type { Address, Hex } from "viem";
import { baseTxUrl } from "@/lib/explorer";
import { JOURNEY_LABEL, useJourney, type JourneyKind } from "@/lib/journey";
import { short } from "@/lib/keys";
import { CopyButton } from "./client";
import { IconCheck, IconExternal, IconLayers, IconSwap } from "./icons";
import { BaseMark, MorphoMark, UsdcMark, ZzecMark } from "./marks";
import { ProofSeal } from "./seam";
import { Card, cx, Skeleton } from "./ui";

/** Each step's mark: the gold π where a zero-knowledge proof was verified. */
function StepIcon({ kind }: { kind: JourneyKind }) {
  switch (kind) {
    case "proven":
    case "payout-proven":
      return <ProofSeal size="sm" className="!size-[26px]" />;
    case "activated":
      return <ZzecMark size={26} />;
    case "borrowed":
    case "repaid":
      return <UsdcMark size={26} />;
    case "close":
      return <IconSwap size={18} />;
    case "committed":
      return <IconLayers size={18} />;
    default:
      return <IconCheck size={18} strokeWidth={2.4} />;
  }
}

const time = (s: number) =>
  new Date(s * 1000).toLocaleString("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", second: "2-digit", timeZone: "UTC" }) + " UTC";

/**
 * The vault's trail on Base (lib/journey.ts): each step with its transaction, linked to the deployment's block
 * explorer where the profile allows it. `since`: the loan's creation, which bounds the search.
 */
export function ProofTrail({ vaultId, account, since, compact = false }: { vaultId: Hex; account?: Address; since?: number; compact?: boolean }) {
  const journey = useJourney(vaultId, account, { since });
  const steps = journey.data?.steps ?? [];

  const body = (
    <div className="space-y-4">
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
        <ol className="relative space-y-1">
          {steps.map((s, i) => (
            <li key={`${s.txHash}:${s.logIndex}`} className="relative grid grid-cols-[40px_minmax(0,1fr)] gap-3.5 pb-4">
              {i < steps.length - 1 ? <span aria-hidden="true" className="absolute top-10 bottom-0 left-[19px] w-px bg-line" /> : null}
              <span className="relative z-[1] inline-flex size-10 items-center justify-center rounded-xl bg-bg text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
                <StepIcon kind={s.kind} />
              </span>
              <div className="min-w-0 pt-0.5">
                <p className="flex flex-wrap items-baseline justify-between gap-x-3">
                  <span className={cx("font-bold", (s.kind === "proven" || s.kind === "payout-proven") && "text-accent")}>{JOURNEY_LABEL[s.kind]}</span>
                  {journey.data?.times.get(s.blockNumber) ? (
                    <span className="text-[12px] text-muted tabular-nums">{time(journey.data.times.get(s.blockNumber)!)}</span>
                  ) : null}
                </p>
                {s.detail ? <p className="text-[13px] text-muted">{s.detail}</p> : null}
                <p className="mt-1 flex flex-wrap items-center gap-2 text-[12.5px]">
                  <span className="font-mono text-soft">{short(s.txHash, 10, 8)}</span>
                  <span className="text-faint">· block {s.blockNumber.toLocaleString("en-US")}</span>
                  {baseTxUrl(s.txHash) ? (
                    <a
                      href={baseTxUrl(s.txHash)!}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 font-semibold text-info no-underline hover:underline"
                    >
                      Basescan
                      <IconExternal size={13} />
                      <span className="sr-only">(opens a new tab)</span>
                    </a>
                  ) : (
                    <CopyButton value={s.txHash} label="Copy" />
                  )}
                </p>
              </div>
            </li>
          ))}
        </ol>
      )}
      {steps.length > 0 && !compact ? (
        <p className="flex items-start gap-2 text-[12px] text-muted">
          <MorphoMark size={14} className="mt-0.5" />
          Read from Base through our proxy. {steps.some((s) => baseTxUrl(s.txHash)) ? "A Basescan link opens a third-party site, which then learns which transaction you look at." : null}
        </p>
      ) : null}
    </div>
  );

  if (compact) return body;
  return (
    <Card title="Proof trail on Base" icon={<BaseMark size={18} />}>
      {body}
    </Card>
  );
}
