"use client";

import type { ReactNode } from "react";
import { CopyButton } from "@/components/client";
import { Badge, Card, KeyValue, Mono, Notice } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
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
    <Card title={s.title} action={own ? <Badge tone="ok">This deployment</Badge> : <Badge tone="neutral">{s.base === "base" ? "Base mainnet" : "Base Sepolia"}</Badge>}>
      <div className="space-y-6">
        <section className="space-y-2" aria-label="Manifest">
          <h3 className="font-semibold">Manifest</h3>
          <KeyValue
            rows={[
              [
                "Commit",
                <span key="c">
                  <Mono>{m.commit}</Mono>
                  {m.tag ? <span className="text-muted"> (tag {m.tag})</span> : null}
                </span>,
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
                  <Badge tone="warn">{x.label}</Badge>
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
            <ul className="space-y-4">
              {s.flows.map((f) => (
                <li key={f.id} className="rounded-lg border border-border p-4">
                  <div className="flex flex-wrap items-baseline justify-between gap-2">
                    <span className="font-medium">{f.title}</span>
                    {f.durationSeconds !== null ? <span className="text-xs text-muted">took {formatPeriod(f.durationSeconds)}</span> : null}
                  </div>
                  {f.note ? <p className="text-xs text-muted">{f.note}</p> : null}
                  <ol className="mt-2 space-y-1 text-sm">
                    {f.steps.map((st, i) => {
                      const link = explorerLink(e, s, st);
                      return (
                        <li key={`${st.tx}-${i}`} className="flex flex-wrap items-baseline gap-2">
                          <Badge tone={st.chain === "base" ? "info" : "ok"}>{st.chain === "base" ? "Base" : "Zcash"}</Badge>
                          <span>{st.label}</span>
                          {link ? (
                            <a href={link} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">
                              <Mono>{short(st.tx, 12, 8)}</Mono>
                            </a>
                          ) : (
                            <Mono>{short(st.tx, 12, 8)}</Mono>
                          )}
                          {st.at ? <span className="text-xs text-faint">{when(st.at)}</span> : null}
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
  return (
    <div className="space-y-6">
      <Notice tone="info" title="Public on purpose">
        <p>
          Only disposable demo vaults appear here: every transaction listed is public on purpose. Updated {when(e.updatedAt)}. Links
          open the block explorers in a new tab; this page fetches nothing from them.
        </p>
      </Notice>
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
