/**
 * The evidence page's file (`zecured-evidence/1`, lib/evidence.ts) built from the recording, so the
 * existing Evidence page lists the live run's transactions with their explorer links. Only steps
 * with a recorded transaction appear: the placeholder lists its deployment and mocks, no flow.
 */
import { EVIDENCE_SCHEMA } from "../evidence";
import type { ReplayRecording, ReplayStepId } from "./types";

/** The evidence page's flows, from the recording's steps. */
const FLOWS: { id: string; title: string; steps: ReplayStepId[] }[] = [
  { id: "deposit", title: "Deposit from Ironwood, proven on Base", steps: ["deposit-paid", "proven", "activated"] },
  { id: "borrow", title: "Borrow from a fresh, gasless account", steps: ["borrowed"] },
  { id: "close", title: "Close from collateral, paid on Zcash and proven on Base", steps: ["close-executed", "payouts-committed", "payouts-sent", "payouts-confirmed"] },
  { id: "swap", title: "Swap USDC into shielded ZEC", steps: ["swap-opened", "swap-paid", "swap-proven", "swap-settled"] },
];

/** P-8's measured durations, from the live run (none in the placeholder). */
function durations(r: ReplayRecording): Record<string, number> {
  const d = (ids: ReplayStepId[]): number | null => {
    const parts = ids.map((id) => r.steps.find((s) => s.id === id)?.durationSeconds ?? null);
    return parts.every((p): p is number => p !== null) ? parts.reduce((a, b) => a + b, 0) : null;
  };
  const out: Record<string, number> = {};
  const set = (key: string, v: number | null) => {
    if (v !== null) out[key] = v;
  };
  set("deposit-to-proof", d(["deposit-seen", "proven"]));
  set("proof-to-activation", d(["activated"]));
  set("payout", d(["payouts-committed", "payouts-sent"]));
  set("proven-delivery", d(["payouts-confirmed"]));
  set("swap", d(["swap-paid", "swap-proven", "swap-settled"]));
  return out;
}

export function evidenceDocument(r: ReplayRecording): unknown {
  const explorers: Record<string, string> = { "base-sepolia": r.explorers.base };
  if (r.explorers.zcash) explorers["zcash-testnet"] = r.explorers.zcash;
  const contracts: Record<string, string> = { ...r.config.contracts };
  const imageIds: Record<string, string> = {};
  for (const [kind, versions] of Object.entries(r.status.versions)) {
    const active = versions.find((v) => v.active && !v.removed);
    if (active) imageIds[kind] = active.imageId;
  }
  const flows = FLOWS.map((f) => {
    const steps = f.steps.flatMap((id) => {
      const s = r.steps.find((x) => x.id === id);
      if (!s) return [];
      return s.artifacts
        .filter((a) => a.tx !== null && a.chain !== "offchain")
        .map((a) => ({ label: a.label, chain: a.chain, tx: a.tx, at: s.at }));
    });
    const total = f.steps.map((id) => r.steps.find((s) => s.id === id)?.durationSeconds ?? null);
    return {
      id: f.id,
      title: f.title,
      note: r.placeholder ? null : "Recorded in the live run that this replay plays back.",
      durationSeconds: total.every((x): x is number => x !== null) ? total.reduce((a, b) => a + b, 0) : null,
      steps,
    };
  }).filter((f) => f.steps.length > 0);
  return {
    schema: EVIDENCE_SCHEMA,
    updatedAt: r.recordedAt ?? "2026-10-26T00:00:00Z",
    explorers,
    durations: durations(r),
    sections: [
      {
        id: "judge",
        title: r.placeholder ? "Testnet deployment (sample data)" : "Testnet deployment, Base Sepolia and Zcash testnet",
        base: r.network.base,
        zcash: r.network.zcash,
        manifest: {
          commit: r.evidence.commit,
          tag: r.evidence.tag,
          deployedAtBlock: r.evidence.deployedAtBlock,
          contracts,
          imageIds,
          router: null,
          relay: r.links.relay,
          oracle: r.config.market.oracle,
          mocks: r.evidence.mocks,
        },
        flows,
        leakScan: r.evidence.leakScan,
      },
    ],
  };
}
