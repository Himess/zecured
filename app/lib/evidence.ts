/**
 * P-3, the evidence page: the deployment manifest and real transaction ids per flow, from the JSON
 * file the integration owner fills (`GET /evidence`, served by the backend from `ZECURED_EVIDENCE`).
 * Only disposable demo vaults appear in it: anything listed here is public on purpose.
 *
 * Schema `zecured-evidence/1` (every field required unless marked optional):
 *
 * ```text
 * {
 *   "schema": "zecured-evidence/1",
 *   "updatedAt": "<ISO 8601>",
 *   "explorers": {                                       // optional, per network
 *     "base-sepolia": "https://sepolia.basescan.org/tx/", "base": "https://basescan.org/tx/",
 *     "zcash-testnet": "<prefix>", "zcash-mainnet": "<prefix>"
 *   },
 *   "durations": { "<step id>": <seconds> },             // optional: typical measured durations (P-8)
 *   "sections": [{
 *     "id": "judge" | "mainnet-beta" | "<other>",        // [a-z0-9-]{1,40}
 *     "title": "<text>",
 *     "base": "base-sepolia" | "base",
 *     "zcash": "zcash-testnet" | "zcash-mainnet",
 *     "manifest": {
 *       "commit": "<7 to 40 hex>", "tag": "<text>" | null,
 *       "deployedAtBlock": <integer> | null,
 *       "contracts": { "<name>": "<address>" },
 *       "imageIds": { "<guest name>": "<0x + 64 hex>" },
 *       "router": "<address>" | null, "relay": "<address>" | null, "oracle": "<address>" | null,
 *       "mocks": [{ "name": "<text>", "address": "<address>" | null, "label": "MOCK" | "DEMO" | "MOCK / DEMO", "why": "<text>" }]
 *     },
 *     "flows": [{
 *       "id": "<[a-z0-9-]{1,40}>", "title": "<text>", "note": "<text>" | null, "durationSeconds": <integer> | null,
 *       "steps": [{ "label": "<text>", "chain": "base" | "zcash", "tx": "<0x + 64 hex on Base, 64 hex on Zcash>", "at": "<ISO 8601>" | null }]
 *     }],
 *     "leakScan": { "result": "<text, e.g. 0 matches>", "fromBlock": <integer>, "toBlock": <integer>, "identifiers": <integer>, "at": "<ISO 8601>" } | null
 *   }]
 * }
 * ```
 *
 * Recommended flow ids: deposit, borrow, payout, close, deleverage, liquidation, pre-liquidation,
 * refund, dispute, proven-delivery, theft-demo, swap.
 */
import { getAddress, isAddress, type Address } from "viem";

export const EVIDENCE_SCHEMA = "zecured-evidence/1";

export type BaseNetwork = "base-sepolia" | "base";
export type ZcashNetworkName = "zcash-testnet" | "zcash-mainnet";

export interface EvidenceMock {
  name: string;
  address: Address | null;
  label: "MOCK" | "DEMO" | "MOCK / DEMO";
  why: string;
}

export interface EvidenceManifest {
  commit: string;
  tag: string | null;
  deployedAtBlock: number | null;
  contracts: { name: string; address: Address }[];
  imageIds: { name: string; imageId: string }[];
  router: Address | null;
  relay: Address | null;
  oracle: Address | null;
  mocks: EvidenceMock[];
}

export interface EvidenceStep {
  label: string;
  chain: "base" | "zcash";
  /** `0x` + 64 hex on Base, 64 hex (display order) on Zcash. */
  tx: string;
  at: string | null;
}

export interface EvidenceFlow {
  id: string;
  title: string;
  note: string | null;
  durationSeconds: number | null;
  steps: EvidenceStep[];
}

export interface LeakScan {
  result: string;
  fromBlock: number;
  toBlock: number;
  identifiers: number;
  at: string;
}

export interface EvidenceSection {
  id: string;
  title: string;
  base: BaseNetwork;
  zcash: ZcashNetworkName;
  manifest: EvidenceManifest;
  flows: EvidenceFlow[];
  leakScan: LeakScan | null;
}

export interface Evidence {
  updatedAt: string;
  explorers: Partial<Record<BaseNetwork | ZcashNetworkName, string>>;
  durations: Map<string, number>;
  sections: EvidenceSection[];
}

export class EvidenceError extends Error {
  constructor(what: string) {
    super(`The evidence file is malformed (${what}).`);
    this.name = "EvidenceError";
  }
}

type Json = Record<string, unknown>;

function obj(v: unknown, what: string): Json {
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new EvidenceError(what);
  return v as Json;
}

function arr(v: unknown, what: string): unknown[] {
  if (!Array.isArray(v)) throw new EvidenceError(what);
  return v;
}

function text(v: unknown, what: string, max = 400): string {
  if (typeof v !== "string" || v.trim() === "" || v.length > max) throw new EvidenceError(what);
  return v;
}

function optText(v: unknown, what: string, max = 400): string | null {
  return v === null || v === undefined ? null : text(v, what, max);
}

function count(v: unknown, what: string): number {
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw new EvidenceError(what);
  return v;
}

function optCount(v: unknown, what: string): number | null {
  return v === null || v === undefined ? null : count(v, what);
}

function address(v: unknown, what: string): Address {
  if (typeof v !== "string" || !isAddress(v, { strict: false })) throw new EvidenceError(what);
  return getAddress(v);
}

function optAddress(v: unknown, what: string): Address | null {
  return v === null || v === undefined ? null : address(v, what);
}

function iso(v: unknown, what: string): string {
  const s = text(v, what, 40);
  if (Number.isNaN(Date.parse(s))) throw new EvidenceError(what);
  return s;
}

const ID = /^[a-z0-9-]{1,40}$/;
const BASE_NETWORKS: readonly BaseNetwork[] = ["base-sepolia", "base"];
const ZCASH_NETWORKS: readonly ZcashNetworkName[] = ["zcash-testnet", "zcash-mainnet"];
const MOCK_LABELS: readonly EvidenceMock["label"][] = ["MOCK", "DEMO", "MOCK / DEMO"];

/** An explorer prefix: an https URL (it is linked to; nothing is fetched from it). */
function explorer(v: unknown, what: string): string {
  const s = text(v, what, 200);
  let u: URL;
  try {
    u = new URL(s);
  } catch {
    throw new EvidenceError(what);
  }
  if (u.protocol !== "https:") throw new EvidenceError(what);
  return s;
}

function step(raw: unknown, i: string): EvidenceStep {
  const s = obj(raw, `${i}`);
  const chain = s.chain;
  if (chain !== "base" && chain !== "zcash") throw new EvidenceError(`${i}.chain`);
  const tx = text(s.tx, `${i}.tx`, 66);
  const ok = chain === "base" ? /^0x[0-9a-fA-F]{64}$/.test(tx) : /^[0-9a-f]{64}$/.test(tx);
  if (!ok) throw new EvidenceError(`${i}.tx`);
  return { label: text(s.label, `${i}.label`, 120), chain, tx: tx.toLowerCase(), at: s.at === null || s.at === undefined ? null : iso(s.at, `${i}.at`) };
}

function section(raw: unknown, i: number): EvidenceSection {
  const w = `sections[${i}]`;
  const s = obj(raw, w);
  const id = text(s.id, `${w}.id`, 40);
  if (!ID.test(id)) throw new EvidenceError(`${w}.id`);
  if (!BASE_NETWORKS.includes(s.base as BaseNetwork)) throw new EvidenceError(`${w}.base`);
  if (!ZCASH_NETWORKS.includes(s.zcash as ZcashNetworkName)) throw new EvidenceError(`${w}.zcash`);
  const m = obj(s.manifest, `${w}.manifest`);
  const commit = text(m.commit, `${w}.manifest.commit`, 40);
  if (!/^[0-9a-f]{7,40}$/.test(commit)) throw new EvidenceError(`${w}.manifest.commit`);
  const contracts = Object.entries(obj(m.contracts, `${w}.manifest.contracts`)).map(([name, a]) => ({
    name: text(name, `${w}.manifest.contracts`, 60),
    address: address(a, `${w}.manifest.contracts.${name}`),
  }));
  const imageIds = Object.entries(obj(m.imageIds, `${w}.manifest.imageIds`)).map(([name, id]) => {
    const v = text(id, `${w}.manifest.imageIds.${name}`, 66);
    if (!/^0x[0-9a-fA-F]{64}$/.test(v)) throw new EvidenceError(`${w}.manifest.imageIds.${name}`);
    return { name: text(name, `${w}.manifest.imageIds`, 60), imageId: v.toLowerCase() };
  });
  const mocks = arr(m.mocks, `${w}.manifest.mocks`).map((raw, k) => {
    const x = obj(raw, `${w}.manifest.mocks[${k}]`);
    if (!MOCK_LABELS.includes(x.label as EvidenceMock["label"])) throw new EvidenceError(`${w}.manifest.mocks[${k}].label`);
    return {
      name: text(x.name, `${w}.manifest.mocks[${k}].name`, 80),
      address: optAddress(x.address, `${w}.manifest.mocks[${k}].address`),
      label: x.label as EvidenceMock["label"],
      why: text(x.why, `${w}.manifest.mocks[${k}].why`),
    };
  });
  const flows = arr(s.flows, `${w}.flows`).map((raw, k) => {
    const f = obj(raw, `${w}.flows[${k}]`);
    const fid = text(f.id, `${w}.flows[${k}].id`, 40);
    if (!ID.test(fid)) throw new EvidenceError(`${w}.flows[${k}].id`);
    return {
      id: fid,
      title: text(f.title, `${w}.flows[${k}].title`, 120),
      note: optText(f.note, `${w}.flows[${k}].note`),
      durationSeconds: optCount(f.durationSeconds, `${w}.flows[${k}].durationSeconds`),
      steps: arr(f.steps, `${w}.flows[${k}].steps`).map((x, j) => step(x, `${w}.flows[${k}].steps[${j}]`)),
    };
  });
  let leakScan: LeakScan | null = null;
  if (s.leakScan !== null && s.leakScan !== undefined) {
    const l = obj(s.leakScan, `${w}.leakScan`);
    leakScan = {
      result: text(l.result, `${w}.leakScan.result`, 120),
      fromBlock: count(l.fromBlock, `${w}.leakScan.fromBlock`),
      toBlock: count(l.toBlock, `${w}.leakScan.toBlock`),
      identifiers: count(l.identifiers, `${w}.leakScan.identifiers`),
      at: iso(l.at, `${w}.leakScan.at`),
    };
  }
  return {
    id,
    title: text(s.title, `${w}.title`, 120),
    base: s.base as BaseNetwork,
    zcash: s.zcash as ZcashNetworkName,
    manifest: {
      commit,
      tag: optText(m.tag, `${w}.manifest.tag`, 80),
      deployedAtBlock: optCount(m.deployedAtBlock, `${w}.manifest.deployedAtBlock`),
      contracts,
      imageIds,
      router: optAddress(m.router, `${w}.manifest.router`),
      relay: optAddress(m.relay, `${w}.manifest.relay`),
      oracle: optAddress(m.oracle, `${w}.manifest.oracle`),
      mocks,
    },
    flows,
    leakScan,
  };
}

/** Parses an evidence file (`zecured-evidence/1`), strictly: anything malformed is an `EvidenceError`. */
export function parseEvidence(raw: unknown): Evidence {
  const r = obj(raw, "root");
  if (r.schema !== EVIDENCE_SCHEMA) throw new EvidenceError("schema");
  const explorers: Evidence["explorers"] = {};
  if (r.explorers !== undefined && r.explorers !== null) {
    for (const [k, v] of Object.entries(obj(r.explorers, "explorers"))) {
      if (![...BASE_NETWORKS, ...ZCASH_NETWORKS].includes(k as BaseNetwork)) throw new EvidenceError(`explorers.${k}`);
      explorers[k as BaseNetwork | ZcashNetworkName] = explorer(v, `explorers.${k}`);
    }
  }
  const durations = new Map<string, number>();
  if (r.durations !== undefined && r.durations !== null) {
    for (const [k, v] of Object.entries(obj(r.durations, "durations"))) {
      if (!ID.test(k)) throw new EvidenceError(`durations.${k}`);
      durations.set(k, count(v, `durations.${k}`));
    }
  }
  const sections = arr(r.sections, "sections").map(section);
  const ids = new Set(sections.map((s) => s.id));
  if (ids.size !== sections.length) throw new EvidenceError("sections: duplicate id");
  return { updatedAt: iso(r.updatedAt, "updatedAt"), explorers, durations, sections };
}

/** The explorer link of a step, or null when the file names no explorer for its network. */
export function explorerLink(e: Evidence, s: EvidenceSection, step: EvidenceStep): string | null {
  const prefix = step.chain === "base" ? e.explorers[s.base] : e.explorers[s.zcash];
  return prefix ? `${prefix}${step.tx}` : null;
}

/** The sections in the order this build shows them: its own deployment's first. */
export function orderedSections(e: Evidence, mainnetBeta: boolean): EvidenceSection[] {
  const own = (s: EvidenceSection) => (s.base === "base") === mainnetBeta;
  return [...e.sections.filter(own), ...e.sections.filter((s) => !own(s))];
}
