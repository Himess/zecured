/**
 * The replay's state machine (DEMO): which steps of the recorded run this replay has reached and
 * when, on two independent tracks (the loan, the swap). A manual step happens when the judge's
 * action in the app matches it (lib/replay/rest.ts, account.ts, wallet.ts); an automatic one
 * follows the previous step of its track after the recording's (accelerated) delay. Everything the
 * replay's backend and Base answer is derived from these times (lib/replay/model.ts), so a reload
 * resumes where it was: the state is kept in this tab's sessionStorage, never in localStorage.
 */
import type { Address } from "viem";
import { recording as defaultRecording } from "./recording";
import { REPLAY_STEP_IDS, type ReplayRecording, type ReplayStep, type ReplayStepId, type ReplayTrack } from "./types";

/** The judge's own inputs the replay keeps (nothing secret: no address of theirs, no salt). */
export interface WorldInputs {
  /** The borrow sent its USDC to another address than the account. */
  borrowReceiver?: Address;
  /** The pre-liquidation terms the borrow's UserOp set (decimal strings). */
  terms?: { preLltv: string; preLcf1: string; preLcf2: string; preLif1: string; preLif2: string; expiry: string };
  /** When the swap's USDC approval was sent (ms). */
  approvedAt?: number;
  /** The swap order's destination reached the maker. */
  swapDestination?: boolean;
}

interface StoredWorld {
  v: 1;
  /** Which recording this state belongs to: another recording starts afresh. */
  fingerprint: string;
  /** When this replay started (ms): the origin of its Base blocks and Zcash heights. */
  epochMs: number;
  /** When each manual step happened (ms); automatic steps follow from these. */
  done: Partial<Record<ReplayStepId, number>>;
  inputs: WorldInputs;
}

export interface WorldStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export const WORLD_KEY = "zecured:replay:v1";
/** Every key the replay keeps in sessionStorage starts with this (lib/replay/loans.ts namespaces the app's own). */
export const REPLAY_STORAGE_PREFIX = "zecured:replay:";

/** An action the recorded run did not take at this point: the app shows its message (code `replay_off_script`). */
/** The recording's loan token as the app names it: "MockUSDC" when the market lends the deployer's MockUSDC. */
export function loanTokenOf(r: ReplayRecording): string {
  const mock = r.config.contracts.MockUSDC;
  return mock !== undefined && mock.toLowerCase() === r.config.market.loanToken.toLowerCase() ? "MockUSDC" : "USDC";
}

export class OffScriptError extends Error {
  readonly code = "replay_off_script";
  constructor(message: string) {
    super(message);
    this.name = "OffScriptError";
  }
}

/** Something the replay never does (a viewing key, the admin panel…): code `replay_unavailable`. */
export class ReplayUnavailableError extends Error {
  readonly code = "replay_unavailable";
  constructor(message: string) {
    super(message);
    this.name = "ReplayUnavailableError";
  }
}

export interface WorldOptions {
  storage?: WorldStorage | null;
  now?: () => number;
  /** Realistic pauses before answers (off in tests). */
  latency?: boolean;
}

function fingerprint(r: ReplayRecording): string {
  return `${r.recordedAt ?? "placeholder"}:${r.loan.vault.vaultId}:${r.swap.orderId}`;
}

export class ReplayWorld {
  readonly rec: ReplayRecording;
  private readonly storage: WorldStorage | null;
  private readonly clock: () => number;
  private readonly latencyOn: boolean;
  private state: StoredWorld;
  private readonly listeners = new Set<() => void>();

  constructor(rec: ReplayRecording, opts: WorldOptions = {}) {
    this.rec = rec;
    this.storage = opts.storage ?? null;
    this.clock = opts.now ?? (() => Date.now());
    this.latencyOn = opts.latency ?? true;
    this.state = this.load() ?? this.fresh();
  }

  private fresh(): StoredWorld {
    return { v: 1, fingerprint: fingerprint(this.rec), epochMs: this.clock(), done: {}, inputs: {} };
  }

  private load(): StoredWorld | null {
    try {
      const raw = this.storage?.getItem(WORLD_KEY);
      if (!raw) return null;
      const s = JSON.parse(raw) as StoredWorld;
      if (s?.v !== 1 || s.fingerprint !== fingerprint(this.rec) || typeof s.epochMs !== "number") return null;
      return { v: 1, fingerprint: s.fingerprint, epochMs: s.epochMs, done: s.done ?? {}, inputs: s.inputs ?? {} };
    } catch {
      return null;
    }
  }

  private save(): void {
    try {
      this.storage?.setItem(WORLD_KEY, JSON.stringify(this.state));
    } catch {
      // A replay that cannot persist still runs; a reload then starts afresh.
    }
  }

  private emit(): void {
    for (const l of this.listeners) l();
  }

  now(): number {
    return this.clock();
  }

  get epochMs(): number {
    return this.state.epochMs;
  }

  get inputs(): Readonly<WorldInputs> {
    return this.state.inputs;
  }

  step(id: ReplayStepId): ReplayStep {
    const s = this.rec.steps.find((x) => x.id === id);
    if (!s) throw new Error(`no step ${id}`);
    return s;
  }

  /** The steps of `track`, in order. */
  track(track: ReplayTrack): ReplayStep[] {
    return this.rec.steps.filter((s) => s.track === track);
  }

  /** When `id` happens (ms): a manual step's moment, or the time an automatic one is due; null while not set off. */
  doneAt(id: ReplayStepId): number | null {
    const s = this.step(id);
    if (!s.auto) return this.state.done[id] ?? null;
    const steps = this.track(s.track);
    const prev = steps[steps.indexOf(s) - 1];
    if (!prev) return null;
    const before = this.doneAt(prev.id);
    return before === null ? null : before + s.delayMs;
  }

  /** `id` has happened by `t`. */
  isDone(id: ReplayStepId, t: number = this.now()): boolean {
    const at = this.doneAt(id);
    return at !== null && at <= t;
  }

  /** The first step of `track` that has not happened yet (null: the track is finished). */
  pending(track: ReplayTrack, t: number = this.now()): ReplayStep | null {
    return this.track(track).find((s) => !this.isDone(s.id, t)) ?? null;
  }

  /** The judge's next action on `track`: null while an automatic step runs, or once the track is finished. */
  nextManual(track: ReplayTrack, t: number = this.now()): ReplayStep | null {
    const p = this.pending(track, t);
    return p && !p.auto ? p : null;
  }

  /** Throws `OffScriptError` unless `id` is its track's next manual step now. */
  expect(id: ReplayStepId): void {
    const s = this.step(id);
    if (this.isDone(id)) throw new OffScriptError(`This replay already did that: “${s.title}”.`);
    const p = this.pending(s.track);
    if (p?.id === id) return;
    if (p?.auto) throw new OffScriptError(`One moment: “${p.title}” is still under way in this replay.`);
    throw new OffScriptError(
      p
        ? `This replay follows the recorded run step by step; its next step here is “${p.title}”: ${p.hint}`
        : "The recorded run did not do this.",
    );
  }

  /** Records the judge's action as manual step `id` (checked with `expect`). */
  complete(id: ReplayStepId): void {
    this.expect(id);
    this.state = { ...this.state, done: { ...this.state.done, [id]: this.now() } };
    this.save();
    this.emit();
  }

  update(patch: Partial<WorldInputs>): void {
    this.state = { ...this.state, inputs: { ...this.state.inputs, ...patch } };
    this.save();
    this.emit();
  }

  /** Starts over: a new epoch, nothing done. Also forgets the replay's loans and orders (sessionStorage). */
  reset(): void {
    try {
      if (this.storage) {
        const keys: string[] = [];
        const s = this.storage as WorldStorage & { length?: number; key?: (i: number) => string | null };
        if (typeof s.length === "number" && typeof s.key === "function") {
          for (let i = 0; i < s.length; i++) {
            const k = s.key(i);
            if (k?.startsWith(REPLAY_STORAGE_PREFIX)) keys.push(k);
          }
        }
        for (const k of keys) s.removeItem(k);
      }
    } catch {
      // Nothing to forget.
    }
    this.state = this.fresh();
    this.save();
    this.emit();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** A realistic pause before an answer (none in tests). */
  async latency(ms: number): Promise<void> {
    if (!this.latencyOn || ms <= 0) return;
    await new Promise((resolve) => setTimeout(resolve, ms));
  }

  /** Every step's id, in the recording's order. */
  static readonly STEP_IDS = REPLAY_STEP_IDS;
}

/** This tab's sessionStorage, when the browser has one. */
export function sessionStore(): WorldStorage | null {
  try {
    if (typeof window === "undefined" || !window.sessionStorage) return null;
    return window.sessionStorage;
  } catch {
    return null;
  }
}

let shared: ReplayWorld | null = null;

/** The page's replay world (created on first use, in the browser). */
export function world(): ReplayWorld {
  shared ??= new ReplayWorld(defaultRecording(), { storage: sessionStore() });
  return shared;
}
