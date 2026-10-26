/**
 * A vault's trail on Base: every public transaction of its life, found from its own events through our proxy
 * (never a third party): the deposit's zero-knowledge proof (the Intake's `ActivationPending`), the mint of its
 * zZEC (`VaultActivated`), the account's borrows and repayments on Morpho, its exit (`CloseRelease`,
 * `ReleaseRequested`, `LiquidationRelease`), each payout committed before it is signed (`SpendCommitted`), the
 * signers' confirmation (`PayoutConfirmed`) and the proof of its delivery (`PayoutProven`). Each step carries its
 * transaction hash, so the page can show where Base verified it.
 *
 * The scan goes backwards from the latest block in 2,000-block windows (the proxy's bound) and stops at the window
 * holding the vault's proof, its first event, or after `maxChunks` windows; later reads only scan the new blocks.
 */
import { encodeEventTopics, getAddress, parseEventLogs, type Address, type Hex, type PublicClient } from "viem";
import { useEffect, useRef, useState } from "react";
import { registryEventsAbi } from "./abi";
import { describeError } from "./api";
import { LOG_CHUNK, publicClient, type ChainDeps } from "./chain";
import { config as appConfig } from "./config";
import { activationPendingEvent, morphoBorrowEvent, morphoRepayEvent, spendCommittedEvent } from "./events";
import { REPLAY } from "./replay/flag";
import { loanTokenName } from "./labels";
import { formatUsdc } from "./morpho";
import { formatTaz } from "./zcash";

const REGISTRY_EVENTS = [
  ...registryEventsAbi.filter((e) =>
    ["VaultActivated", "CloseRelease", "ReleaseRequested", "LiquidationRelease", "PayoutConfirmed", "PayoutProven"].includes(e.name),
  ),
  spendCommittedEvent,
] as const;

const topic = (abi: readonly unknown[], name: string): Hex => encodeEventTopics({ abi, eventName: name } as never)[0] as Hex;
const padAddress = (a: Address): Hex => `0x${getAddress(a).slice(2).toLowerCase().padStart(64, "0")}` as Hex;
const hexBlock = (n: bigint) => `0x${n.toString(16)}`;

export type JourneyKind =
  | "proven"
  | "activated"
  | "borrowed"
  | "repaid"
  | "close"
  | "release"
  | "liquidation"
  | "committed"
  | "confirmed"
  | "payout-proven";

export interface JourneyStep {
  kind: JourneyKind;
  /** What happened, in a few words. */
  label: string;
  /** Amounts and the like, for the line under it; null when nothing adds to the label. */
  detail: string | null;
  txHash: Hex;
  blockNumber: bigint;
  /** The transaction's position in its block, then the log's: Base's own order within a block. */
  txIndex: number;
  logIndex: number;
}

/** The labels of each kind, as the trail shows them. */
export const JOURNEY_LABEL: Record<JourneyKind, string> = {
  proven: "Deposit proven in zero knowledge",
  activated: "zZEC minted into Morpho",
  borrowed: "Borrowed on Morpho",
  repaid: "Repaid on Morpho",
  close: "Closed from collateral",
  release: "Release requested",
  liquidation: "Part of the collateral repaid debt",
  committed: "Payout committed before signing",
  confirmed: "Payout confirmed by the signers",
  "payout-proven": "Payout's delivery proven in zero knowledge",
};

type RawLogs = Parameters<typeof parseEventLogs>[0]["logs"];

async function logsOf(c: PublicClient, address: Address, topics: (Hex | Hex[] | null)[], from: bigint, to: bigint): Promise<RawLogs> {
  return (await c.request({
    method: "eth_getLogs",
    params: [{ address, topics, fromBlock: hexBlock(from), toBlock: hexBlock(to) }],
  } as never)) as RawLogs;
}

/** One window's steps for the vault (and its account's Morpho position). */
async function windowSteps(c: PublicClient, vaultId: Hex, account: Address | null, from: bigint, to: bigint, d: ChainDeps): Promise<JourneyStep[]> {
  const cfg = d.config ?? appConfig;
  const vault = vaultId.toLowerCase() as Hex;
  const reads: Promise<JourneyStep[]>[] = [
    logsOf(c, cfg.intake, [topic([activationPendingEvent], "ActivationPending"), vault], from, to).then((logs) =>
      parseEventLogs({ abi: [activationPendingEvent], logs }).map((l) => ({
        kind: "proven" as const,
        label: JOURNEY_LABEL.proven,
        detail: `${formatTaz(l.args.amount)} on Zcash, its block header checked by the relay`,
        txHash: l.transactionHash as Hex,
        blockNumber: l.blockNumber as bigint,
        txIndex: l.transactionIndex as number,
        logIndex: l.logIndex as number,
      })),
    ),
    logsOf(c, cfg.registry, [REGISTRY_EVENTS.map((e) => topic([e], e.name)), vault], from, to).then((logs) =>
      parseEventLogs({ abi: REGISTRY_EVENTS, logs }).map((l): JourneyStep => {
        const base = {
          txHash: l.transactionHash as Hex,
          blockNumber: l.blockNumber as bigint,
          txIndex: l.transactionIndex as number,
          logIndex: l.logIndex as number,
        };
        switch (l.eventName) {
          case "VaultActivated":
            return { ...base, kind: "activated", label: JOURNEY_LABEL.activated, detail: `${formatTaz(l.args.amount)} of collateral` };
          case "CloseRelease":
            return {
              ...base,
              kind: "close",
              label: JOURNEY_LABEL.close,
              detail: `${formatTaz(l.args.userAmount)} back to you, ${formatTaz(l.args.mmAmount)} to the market maker`,
            };
          case "ReleaseRequested":
            return { ...base, kind: "release", label: JOURNEY_LABEL.release, detail: formatTaz(l.args.amount) };
          case "LiquidationRelease":
            return { ...base, kind: "liquidation", label: JOURNEY_LABEL.liquidation, detail: formatTaz(l.args.amount) };
          case "SpendCommitted":
            return {
              ...base,
              kind: "committed",
              label: JOURNEY_LABEL.committed,
              detail: `${l.args.claimIds.length} claim${l.args.claimIds.length === 1 ? "" : "s"}, signed by ${countBits(l.args.signerBitmap)} of 5 signers`,
            };
          case "PayoutConfirmed":
            return { ...base, kind: "confirmed", label: JOURNEY_LABEL.confirmed, detail: null };
          default:
            return { ...base, kind: "payout-proven", label: JOURNEY_LABEL["payout-proven"], detail: null };
        }
      }),
    ),
  ];
  if (account) {
    const id = cfg.marketId.toLowerCase() as Hex;
    const who = padAddress(account);
    reads.push(
      logsOf(c, cfg.morpho, [topic([morphoBorrowEvent], "Borrow"), id, who], from, to).then((logs) =>
        parseEventLogs({ abi: [morphoBorrowEvent], logs }).map((l) => ({
          kind: "borrowed" as const,
          label: JOURNEY_LABEL.borrowed,
          detail: `${formatUsdc(l.args.assets)} ${loanTokenName(cfg)}`,
          txHash: l.transactionHash as Hex,
          blockNumber: l.blockNumber as bigint,
          txIndex: l.transactionIndex as number,
          logIndex: l.logIndex as number,
        })),
      ),
      logsOf(c, cfg.morpho, [topic([morphoRepayEvent], "Repay"), id, null, who], from, to).then((logs) =>
        parseEventLogs({ abi: [morphoRepayEvent], logs }).map((l) => ({
          kind: "repaid" as const,
          label: JOURNEY_LABEL.repaid,
          detail: `${formatUsdc(l.args.assets)} ${loanTokenName(cfg)}`,
          txHash: l.transactionHash as Hex,
          blockNumber: l.blockNumber as bigint,
          txIndex: l.transactionIndex as number,
          logIndex: l.logIndex as number,
        })),
      ),
    );
  }
  return (await Promise.all(reads)).flat();
}

function countBits(x: number): number {
  let n = 0;
  for (let v = x; v; v >>= 1) n += v & 1;
  return n;
}

export interface Journey {
  steps: JourneyStep[];
  /** The block of each step's time (Unix s), for the steps read so far. */
  times: Map<bigint, number>;
  /** The scan reached the vault's proof (its first event), or the start of the chain. */
  complete: boolean;
}

/** Scan state per client and vault: what was read, and down to which block. */
interface Scan {
  steps: Map<string, JourneyStep>;
  times: Map<bigint, number>;
  top: bigint;
  bottom: bigint;
  complete: boolean;
}
const scans = new WeakMap<PublicClient, Map<string, Scan>>();

/**
 * See the module comment. `since` (Unix ms, such as a stored loan's creation) bounds the backward scan: nothing of
 * the vault is older than its loan. Reads only, through the proxy.
 */
export async function readJourney(
  vaultId: Hex,
  account: Address | null,
  opts: { since?: number; maxChunks?: number } & ChainDeps = {},
): Promise<Journey> {
  const c = opts.client ?? publicClient();
  const cfg = opts.config ?? appConfig;
  const deps = { client: c, config: cfg };
  let perClient = scans.get(c);
  if (!perClient) scans.set(c, (perClient = new Map()));
  const key = `${vaultId}:${account ?? ""}`.toLowerCase();
  const latest = await c.getBlock({ blockTag: "latest" });
  let scan = perClient.get(key);
  if (!scan) {
    scan = { steps: new Map(), times: new Map(), top: latest.number, bottom: latest.number + 1n, complete: false };
    perClient.set(key, scan);
  }
  const add = (list: JourneyStep[]) => {
    for (const s of list) scan!.steps.set(`${s.txHash}:${s.logIndex}`, s);
  };
  // New blocks since the last read.
  if (scan.bottom <= scan.top && latest.number > scan.top) {
    for (let from = scan.top + 1n; from <= latest.number; ) {
      const to = from + LOG_CHUNK - 1n < latest.number ? from + LOG_CHUNK - 1n : latest.number;
      add(await windowSteps(c, vaultId, account, from, to, deps));
      from = to + 1n;
    }
    scan.top = latest.number;
  }
  // Backwards until the proof, the loan's creation, or the bound.
  const floor =
    opts.since !== undefined
      ? (() => {
          // Base blocks come every 2 s; ten minutes of margin before the loan was created.
          const ago = BigInt(Math.max(0, Math.floor((Number(latest.timestamp) * 1000 - opts.since) / 2000) + 300));
          return latest.number > ago ? latest.number - ago : 0n;
        })()
      : 0n;
  const maxChunks = opts.maxChunks ?? 40;
  for (let i = 0; i < maxChunks && !scan.complete; i++) {
    const to = scan.bottom - 1n;
    if (to < floor || scan.bottom === 0n) {
      scan.complete = true;
      break;
    }
    const from = to >= LOG_CHUNK - 1n + floor ? to - LOG_CHUNK + 1n : floor;
    add(await windowSteps(c, vaultId, account, from, to, deps));
    scan.bottom = from;
    if ([...scan.steps.values()].some((s) => s.kind === "proven")) scan.complete = true;
  }
  const sorted = [...scan.steps.values()].sort((a, b) =>
    a.blockNumber !== b.blockNumber ? (a.blockNumber < b.blockNumber ? -1 : 1) : a.txIndex !== b.txIndex ? a.txIndex - b.txIndex : a.logIndex - b.logIndex,
  );
  // One payout's proof covers each of its claims (one event per claim): a single step, with their count.
  const steps: JourneyStep[] = [];
  let claims = 1;
  for (const s of sorted) {
    const last = steps[steps.length - 1];
    if (last && s.kind === "payout-proven" && last.kind === "payout-proven" && last.txHash === s.txHash) {
      claims += 1;
      steps[steps.length - 1] = { ...last, detail: `${claims} claims` };
      continue;
    }
    claims = 1;
    steps.push(s);
  }
  // Each step's time, from its block (a handful of blocks, read once).
  for (const s of steps) {
    if (!scan.times.has(s.blockNumber)) {
      const b = await c.getBlock({ blockNumber: s.blockNumber });
      scan.times.set(s.blockNumber, Number(b.timestamp));
    }
  }
  return { steps, times: new Map(scan.times), complete: scan.complete };
}

/** The vault's trail, read once and then every `intervalMs` for new blocks; disabled without contracts. */
export function useJourney(
  vaultId: Hex | undefined,
  account: Address | undefined,
  { since, intervalMs = 30_000 }: { since?: number; intervalMs?: number } = {},
): { data: Journey | null; error: string | null; loading: boolean } {
  const [data, setData] = useState<Journey | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(Boolean(vaultId));
  const sinceRef = useRef(since);
  sinceRef.current = since;
  useEffect(() => {
    if (!vaultId || !appConfig.contractsDeployed) {
      setLoading(false);
      return;
    }
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const j = await readJourney(vaultId, account ?? null, { since: sinceRef.current });
        if (!stop) {
          setData(j);
          setError(null);
        }
      } catch (e) {
        if (!stop) setError(describeError(e));
      } finally {
        if (!stop) {
          setLoading(false);
          // DEMO: the replay's steps come seconds apart, as the other polls (hooks.ts) follow them.
          timer = setTimeout(tick, REPLAY ? Math.min(intervalMs, 2_500) : intervalMs);
        }
      }
    };
    void tick();
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  }, [vaultId, account, intervalMs]);
  return { data, error, loading };
}
