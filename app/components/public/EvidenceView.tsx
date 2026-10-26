"use client";

import type { ReactNode } from "react";
import { CopyButton } from "@/components/client";
import { IconClock, IconExternal, IconFileCheck } from "@/components/icons";
import { BaseMark, ZecMark } from "@/components/marks";
import { Badge, Card, Hint, KeyValue, Mono, Notice } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import { sampleTransactions } from "@/lib/explorer";
import { explorerLink, orderedSections, type Evidence, type EvidenceSection } from "@/lib/evidence";
import { useEvidence } from "@/lib/hooks";
import { short } from "@/lib/keys";
import { formatPeriod } from "@/lib/v3";

const DURATION_LABEL: Record<string, string> = {
  "deposit-to-proof": "Deposit on Zcash to its proof on Base",
  "proof-to-activation": "Proof on Base to activation",
  payout: "Payout (commitment to Zcash confirmation)",
  "proven-delivery": "Payout to its delivery proven on Base",
  swap: "Swap (order to settlement)",
};

function when(iso: string | null): string {
  return iso ? `${iso.slice(0, 16).replace("T", " ")} UTC` : "";
}

function Section({ e, s, own }: { e: Evidence; s: EvidenceSection; own: boolean }) {
  const m = s.manifest;
  return (
    <Card
      title={s.title}
      icon={<IconFileCheck size={18} />}
      action={own ? <Badge tone="ok" dot>This deployment</Badge> : <Badge tone="neutral">{s.base === "base" ? "Base mainnet" : "Base Sepolia"}</Badge>}
    >
      <div className="space-y-6">
        <section className="space-y-2" aria-label="Manifest">
          <h3 className="font-semibold">Manifest</h3>
          <KeyValue
            rows={[
              [
                "Commit",
                sampleTransactions() ? (
                  <span key="c" className="text-muted">
                    sample data
                  </span>
                ) : (
                  <span key="c">
                    <Mono>{m.commit}</Mono>
                    {m.tag ? <span className="text-muted"> (tag {m.tag})</span> : null}
                  </span>
                ),
              ],
              ["Deployed at block", m.deployedAtBlock === null ? "—" : `#${m.deployedAtBlock.toLocaleString("en-US")}`],
              ...m.contracts.map(
                (c): [string, ReactNode] => [
                  c.name,
                  <span key={c.name} className="inline-flex flex-wrap items-center gap-2">
                    <Mono>{c.address}</Mono> <CopyButton value={c.address} />
                  </span>,
                ],
              ),
              ...(m.router ? [["Proof router", <Mono key="r">{m.router}</Mono>] as [string, ReactNode]] : []),
              ...(m.relay ? [["Zcash header relay", <Mono key="h">{m.relay}</Mono>] as [string, ReactNode]] : []),
              ...(m.oracle ? [["Market oracle", <Mono key="o">{m.oracle}</Mono>] as [string, ReactNode]] : []),
              ...m.imageIds.map((g): [string, ReactNode] => [`Guest ${g.name}`, <Mono key={g.name}>{g.imageId}</Mono>]),
            ]}
          />
          {m.mocks.length > 0 ? (
            <ul className="space-y-1 text-sm">
              {m.mocks.map((x) => (
                <li key={x.name} className="flex flex-wrap items-baseline gap-2">
                  <Badge tone={x.label === "DEMO" ? "demo" : "warn"}>{x.label}</Badge>
                  <span className="font-medium">{x.name}</span>
                  {x.address ? <Mono>{short(x.address, 10, 6)}</Mono> : null}
                  <span className="text-muted">{x.why}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted">No mocks in this deployment.</p>
          )}
        </section>

        <section className="space-y-3" aria-label="Flows">
          <h3 className="font-semibold">Flows, with their real transactions</h3>
          {s.flows.length === 0 ? (
            <p className="text-sm text-muted">No flow recorded yet.</p>
          ) : (
            <ul className="grid gap-4 xl:grid-cols-2">
              {s.flows.map((f) => (
                <li key={f.id} className="rounded-[20px] bg-bg/70 p-5 shadow-[inset_0_0_0_1px_var(--color-border)]">
                  <div className="flex flex-wrap items-baseline justify-between gap-2">
                    <span className="text-[16px] font-bold">{f.title}</span>
                    {f.durationSeconds !== null ? (
                      <span className="inline-flex items-center gap-1.5 text-xs text-muted">
                        <IconClock size={13} /> took {formatPeriod(f.durationSeconds)}
                      </span>
                    ) : null}
                  </div>
                  {f.note ? <p className="mt-1 text-xs text-muted">{f.note}</p> : null}
                  <ol className="mt-3 space-y-2 text-sm">
                    {f.steps.map((st, i) => {
                      const link = explorerLink(e, s, st);
                      return (
                        <li key={`${st.tx}-${i}`} className="grid grid-cols-[22px_minmax(0,1fr)] gap-2.5">
                          <span className="mt-0.5">{st.chain === "zcash" ? <ZecMark size={18} /> : <BaseMark size={17} />}</span>
                          <span className="min-w-0">
                            <span className="block">{st.label}</span>
                            <span className="flex flex-wrap items-center gap-x-2 text-[12.5px]">
                              {link ? (
                                <a
                                  href={link}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                  referrerPolicy="no-referrer"
                                  className="inline-flex items-center gap-1 text-info no-underline hover:underline"
                                >
                                  <Mono>{short(st.tx, 12, 8)}</Mono>
                                  <IconExternal size={12} />
                                </a>
                              ) : (
                                <Mono>{short(st.tx, 12, 8)}</Mono>
                              )}
                              {st.at ? <span className="text-faint">{when(st.at)}</span> : null}
                            </span>
                          </span>
                        </li>
                      );
                    })}
                  </ol>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="space-y-2" aria-label="Leak scan">
          <h3 className="font-semibold">Leak scan</h3>
          {s.leakScan ? (
            <p className="text-sm text-muted">
              <span className="font-medium text-text">{s.leakScan.result}</span>: {s.leakScan.identifiers} Zcash identifiers (txids,
              note commitments, addresses) searched in every event and calldata of blocks #{s.leakScan.fromBlock.toLocaleString("en-US")} to #
              {s.leakScan.toBlock.toLocaleString("en-US")}, on {when(s.leakScan.at)}.
            </p>
          ) : (
            <p className="text-sm text-muted">Not run on this deployment yet.</p>
          )}
        </section>
      </div>
    </Card>
  );
}

/**
 * P-3, the evidence page: the deployment manifest and the real transaction ids of every flow,
 * from the backend's evidence file (lib/evidence.ts), this build's own deployment first. Links
 * open the explorers; the page fetches nothing from them.
 */
export function EvidenceView() {
  const ev = useEvidence();
  if (ev.loading) return <p className="text-sm text-muted">Reading the evidence file…</p>;
  if (ev.error) {
    return (
      <Notice tone="danger" title="The evidence file could not be read" live>
        <p>{ev.error}</p>
      </Notice>
    );
  }
  const e = ev.data;
  if (!e) {
    return (
      <Notice tone="info" title="No evidence published yet">
        <p>This backend publishes no evidence file. It is filled in as each flow runs on the deployment.</p>
      </Notice>
    );
  }
  const mainnet = DEPLOYMENT.kind === "mainnet-beta";
  const sections = orderedSections(e, mainnet);
  // The leak scan leads: a statement about Base that carries no Zcash identifier itself.
  const scan = sections.map((s) => s.leakScan).find((l) => l !== null) ?? null;
  return (
    <div className="space-y-6">
      {sampleTransactions() ? (
        <Notice tone="demo" title="DEMO: sample data">
          <p>
            The replay shows the evidence file in its real shape, with sample data. The live run&apos;s file names its commit, its
            deployment block, every transaction of every flow, and its leak scan.
          </p>
        </Notice>
      ) : null}
      {scan ? (
        <section aria-label="Leak scan" className="paper rounded-[3px] p-6 shadow-[0_30px_60px_-40px_rgb(0_0_0/0.9)] sm:p-7">
          <p className="text-[13px] font-semibold text-ink-muted">Leak scan of Base</p>
          <p className="mt-2 text-[length:clamp(26px,3.4vw,40px)] leading-tight font-extrabold text-ink font-stretch-110%">
            {scan.result} <span className="text-[0.55em] font-bold text-ink-muted">among {scan.identifiers} Zcash identifiers</span>
          </p>
          <p className="mt-2 max-w-3xl text-[14px] text-ink-muted">
            Txids, note commitments and addresses from our own runs, searched in every event and calldata of blocks #
            {scan.fromBlock.toLocaleString("en-US")} to #{scan.toBlock.toLocaleString("en-US")}, on {when(scan.at)}.
          </p>
        </section>
      ) : null}
      <Hint>
        Public on purpose: only disposable demo vaults appear here. Updated {when(e.updatedAt)}. Links open the block explorers in a new
        tab; this page fetches nothing from them.
      </Hint>
      {e.durations.size > 0 ? (
        <Card title="Measured durations">
          <KeyValue rows={[...e.durations.entries()].map(([k, v]): [string, string] => [DURATION_LABEL[k] ?? k, formatPeriod(v)])} />
        </Card>
      ) : null}
      {sections.map((s) => (
        <Section key={s.id} e={e} s={s} own={(s.base === "base") === mainnet} />
      ))}
    </div>
  );
}
