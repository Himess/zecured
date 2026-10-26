/**
 * The recording this replay build plays (lib/replay/recording.json), read once and checked for
 * its shape: a file that is not a `zecured-replay/1` recording is a build error, not a page that
 * half works.
 */
import raw from "./recording.json";
import { REPLAY_SCHEMA, REPLAY_STEP_IDS, type ReplayRecording, type ReplayStep } from "./types";

export class RecordingError extends Error {
  constructor(what: string) {
    super(`The replay's recording is malformed (${what}).`);
    this.name = "RecordingError";
  }
}

const HASH = /^0x[0-9a-fA-F]{64}$/;
const TXID = /^[0-9a-f]{64}$/;

function checkStep(s: ReplayStep, i: number): void {
  const at = `steps[${i}]`;
  if (s.id !== REPLAY_STEP_IDS[i]) throw new RecordingError(`${at}.id: ${String(s.id)} where ${REPLAY_STEP_IDS[i]} belongs`);
  if (s.track !== "loan" && s.track !== "swap") throw new RecordingError(`${at}.track`);
  if (typeof s.auto !== "boolean" || !Number.isSafeInteger(s.delayMs) || s.delayMs < 0) throw new RecordingError(`${at}.auto`);
  for (const k of ["base", "userOp", "approve"] as const) {
    const v = s.tx[k];
    if (v !== undefined && !HASH.test(v)) throw new RecordingError(`${at}.tx.${k}`);
  }
  if (s.tx.zcash !== undefined && !TXID.test(s.tx.zcash)) throw new RecordingError(`${at}.tx.zcash`);
  for (const a of s.artifacts) {
    if (a.tx === null) continue;
    if (a.chain === "base" ? !HASH.test(a.tx) : a.chain === "zcash" ? !TXID.test(a.tx) : true) {
      throw new RecordingError(`${at}.artifacts: ${a.label}`);
    }
  }
}

/** Checks `value` is a `zecured-replay/1` recording with every step, in order. */
export function parseRecording(value: unknown): ReplayRecording {
  const r = value as ReplayRecording;
  if (!r || typeof r !== "object" || r.schema !== REPLAY_SCHEMA) throw new RecordingError("schema");
  if (!Array.isArray(r.steps) || r.steps.length !== REPLAY_STEP_IDS.length) throw new RecordingError("steps");
  r.steps.forEach(checkStep);
  if (!r.placeholder && r.steps.some((s) => s.artifacts.some((a) => a.chain !== "offchain" && a.tx === null))) {
    throw new RecordingError("a recorded step has an artifact without its transaction");
  }
  if (!Array.isArray(r.config?.denominations) || r.config.denominations.length !== 1) throw new RecordingError("config.denominations");
  if (r.config.denominations[0] !== r.loan?.amount) throw new RecordingError("loan.amount");
  if (!Array.isArray(r.loan.vault?.signatures) || r.loan.vault.signatures.length !== 5) throw new RecordingError("loan.vault.signatures");
  return r;
}

let cached: ReplayRecording | null = null;

/** The build's recording (parsed on first use). */
export function recording(): ReplayRecording {
  cached ??= parseRecording(raw);
  return cached;
}
