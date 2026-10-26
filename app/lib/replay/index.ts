/**
 * The replay's view for its dock (DEMO): the recorded steps, where this replay is in them, and the
 * explorer link of each artifact. Re-renders on every step the judge takes and while an automatic
 * step is due. Outside a replay build it is inert (`active` false, no steps).
 */
import { useCallback, useEffect, useState } from "react";
import { REPLAY } from "./flag";
import type { ReplayArtifact, ReplayRecording, ReplayStep } from "./types";
import { world, type ReplayWorld } from "./world";

export type { ReplayArtifact, ReplayRecording, ReplayStep, ReplayStepId, ReplayTrack } from "./types";
export { REPLAY, REPLAY_BASE_URL, REPLAY_CSP } from "./flag";

/** A step as the dock shows it: done, under way (an automatic step due), the judge's next action, or later. */
export type ReplayStepStatus = "done" | "now" | "next" | "later";

export interface ReplayStepView extends ReplayStep {
  status: ReplayStepStatus;
  /** When it happened in this replay (ms), once done. */
  doneAt: number | null;
}

export interface ReplayView {
  active: boolean;
  recording: ReplayRecording | null;
  steps: ReplayStepView[];
  /** The last done step's index in `steps` (-1: none yet). */
  index: number;
  /** The judge's next action (the loan's first, then the swap's), or the step under way. */
  next: ReplayStepView | null;
  /** Starts the replay over (its loan and orders too); the page should then reload or navigate home. */
  reset: () => void;
  /** The explorer page of an artifact; null without a transaction (the placeholder) or an explorer. */
  explorerUrl: (a: ReplayArtifact) => string | null;
}

/** The steps' statuses at `t`. */
export function stepViews(w: ReplayWorld, t: number = w.now()): ReplayStepView[] {
  const pendingOf = { loan: w.pending("loan", t), swap: w.pending("swap", t) };
  return w.rec.steps.map((s) => {
    const done = w.isDone(s.id, t);
    const p = pendingOf[s.track];
    const status: ReplayStepStatus = done ? "done" : p?.id === s.id ? (s.auto ? "now" : "next") : "later";
    return { ...s, status, doneAt: done ? w.doneAt(s.id) : null };
  });
}

export function explorerUrl(r: ReplayRecording, a: ReplayArtifact): string | null {
  if (!a.tx || r.placeholder) return null;
  if (a.chain === "base") return `${r.explorers.base}${a.tx}`;
  if (a.chain === "zcash" && r.explorers.zcash) return `${r.explorers.zcash}${a.tx}`;
  return null;
}

const INERT: ReplayView = {
  active: false,
  recording: null,
  steps: [],
  index: -1,
  next: null,
  reset: () => undefined,
  explorerUrl: () => null,
};

export function useReplay(): ReplayView {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!REPLAY) return;
    const w = world();
    const unsubscribe = w.subscribe(() => setTick((n) => n + 1));
    // Automatic steps fall due with the clock: look twice a second.
    const timer = setInterval(() => setTick((n) => n + 1), 500);
    return () => {
      unsubscribe();
      clearInterval(timer);
    };
  }, []);
  const reset = useCallback(() => world().reset(), []);
  if (!REPLAY) return INERT;
  void tick;
  const w = world();
  const steps = stepViews(w);
  let index = -1;
  steps.forEach((s, i) => {
    if (s.status === "done") index = i;
  });
  const next = steps.find((s) => s.track === "loan" && (s.status === "next" || s.status === "now")) ??
    steps.find((s) => s.status === "next" || s.status === "now") ??
    null;
  return { active: true, recording: w.rec, steps, index, next, reset, explorerUrl: (a) => explorerUrl(w.rec, a) };
}

/** The demo wallet's Ironwood address, for a one-click destination in the replay (the swap page has no other). */
export function replayDemoAddress(): string | null {
  return REPLAY ? world().rec.demo.returnAddress : null;
}
