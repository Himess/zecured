/**
 * Base Sepolia reads. Every request goes to our backend's `/proxy/rpc`; the browser never
 * talks to an RPC provider directly (CLAUDE.md, "Hard privacy rules").
 *
 * Each reader takes optional dependencies (a viem client and the app config) so tests can run
 * them against a mocked JSON-RPC transport.
 */
import {
  createPublicClient,
  encodeAbiParameters,
  getAddress,
  http,
  keccak256,
  parseEventLogs,
  zeroAddress,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import {
  adapterAbi,
  bondAbi,
  CLAIM_KIND,
  CLAIM_STATUS,
  compensatorAbi,
  depositRefundedEvent,
  disputesAbi,
  erc20Abi,
  intakeAbi,
  irmAbi,
  morphoAbi,
  ONCHAIN_VAULT_STATUS,
  oracleAbi,
  REFUND_REASON,
  registryAbi,
  registryEventsAbi,
  relayAbi,
  slasherAbi,
} from "./abi";
import {
  adapterV4Abi,
  bondV4Abi,
  intakeV4Abi,
  refundPendingEvent,
  refundsV4Abi,
  registryV4EventsAbi,
  registryViewsAbi,
} from "./abi4";
import { api } from "./api";
import { config as appConfig, marketParams, ZERO_BYTES32, type AppConfig } from "./config";
import { viemChain } from "./deployment";
import { borrowAssets, expectedMarket, mulDivDown, type MarketState, type PositionState } from "./morpho";
import type { PreLiquidationTerms } from "./preliq";

let defaultClient: PublicClient | null = null;

/** viem public client bound to the backend proxy (Base Sepolia, or Base on the mainnet beta). */
export function publicClient(): PublicClient {
  defaultClient ??= createPublicClient({
    chain: viemChain(),
    transport: http(api.proxyUrl("rpc"), {
      timeout: 15_000,
      retryCount: 1,
      fetchOptions: { credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store" },
    }),
  }) as PublicClient;
  return defaultClient;
}

export interface ChainDeps {
  client?: PublicClient;
  config?: AppConfig;
}

export class ContractsNotDeployedError extends Error {
  constructor() {
    super("The Base contracts are not deployed or not configured consistently (see the configuration notice).");
    this.name = "ContractsNotDeployedError";
  }
}

function resolve(d: ChainDeps = {}, requireContracts = true): { c: PublicClient; cfg: AppConfig } {
  const cfg = d.config ?? appConfig;
  if (requireContracts && !cfg.contractsDeployed) throw new ContractsNotDeployedError();
  return { c: d.client ?? publicClient(), cfg };
}

export async function readPrice(d?: ChainDeps): Promise<bigint> {
  const { c, cfg } = resolve(d);
  return c.readContract({ address: cfg.oracle, abi: oracleAbi, functionName: "price" });
}

export async function readMarket(d?: ChainDeps): Promise<MarketState> {
  const { c, cfg } = resolve(d);
  const m = await c.readContract({ address: cfg.morpho, abi: morphoAbi, functionName: "market", args: [cfg.marketId] });
  return {
    totalSupplyAssets: m[0],
    totalSupplyShares: m[1],
    totalBorrowAssets: m[2],
    totalBorrowShares: m[3],
    lastUpdate: m[4],
    fee: m[5],
  };
}

export async function isDeployedAt(address: Address, d?: ChainDeps): Promise<boolean> {
  const { c } = resolve(d, false);
  const code = await c.getCode({ address });
  return Boolean(code && code !== "0x");
}

export interface OnchainVault {
  account: Address;
  outstanding: bigint;
  status: (typeof ONCHAIN_VAULT_STATUS)[number];
}

export async function readVault(vaultId: Hex, d?: ChainDeps): Promise<OnchainVault> {
  const { c, cfg } = resolve(d);
  const [account, outstanding, status] = await c.readContract({
    address: cfg.registry,
    abi: registryAbi,
    functionName: "vault",
    args: [vaultId],
  });
  return { account, outstanding, status: ONCHAIN_VAULT_STATUS[status] ?? "None" };
}

export interface PositionSnapshot {
  position: PositionState;
  /** Market totals with interest accrued to the latest block. */
  market: MarketState;
  /** USD-per-ZEC × 1e34 */
  price: bigint;
  /** USDC units a full repay pulls right now. */
  debt: bigint;
  registryAuthorized: boolean;
  adapterAuthorized: boolean;
  /** USDC held by the account itself. */
  usdcBalance: bigint;
  accountDeployed: boolean;
  blockTimestamp: bigint;
}

/** Everything the position page needs about `account`, in one round of reads. */
export async function readPosition(account: Address, d?: ChainDeps): Promise<PositionSnapshot> {
  const { c, cfg } = resolve(d);
  const deps = { client: c, config: cfg };
  const [pos, market, price, registryAuthorized, adapterAuthorized, usdcBalance, accountDeployed, block] =
    await Promise.all([
      c.readContract({ address: cfg.morpho, abi: morphoAbi, functionName: "position", args: [cfg.marketId, account] }),
      readMarket(deps),
      readPrice(deps),
      c.readContract({ address: cfg.morpho, abi: morphoAbi, functionName: "isAuthorized", args: [account, cfg.registry] }),
      c.readContract({ address: cfg.morpho, abi: morphoAbi, functionName: "isAuthorized", args: [account, cfg.adapter] }),
      c.readContract({ address: cfg.usdc, abi: erc20Abi, functionName: "balanceOf", args: [account] }),
      isDeployedAt(account, deps),
      c.getBlock({ blockTag: "latest" }),
    ]);
  let accrued = market;
  if (market.totalBorrowAssets > 0n) {
    const rate = await c.readContract({
      address: cfg.irm,
      abi: irmAbi,
      functionName: "borrowRateView",
      args: [marketParams(cfg), market],
    });
    accrued = expectedMarket(market, rate, block.timestamp);
  }
  const position: PositionState = { supplyShares: pos[0], borrowShares: pos[1], collateral: pos[2] };
  return {
    position,
    market: accrued,
    price,
    debt: borrowAssets(position, accrued),
    registryAuthorized,
    adapterAuthorized,
    usdcBalance,
    accountDeployed,
    blockTimestamp: block.timestamp,
  };
}

export interface ReleaseClaim {
  claimId: Hex;
  /** zatoshi */
  amount: bigint;
}

/**
 * The Release claim that `transactionHash` created for `vaultId` (its `ReleaseRequested`
 * event), or null if it has none. The receipt is read through the proxy like every Base read.
 */
export async function readReleaseClaim(vaultId: Hex, transactionHash: Hex, d?: ChainDeps): Promise<ReleaseClaim | null> {
  const { c, cfg } = resolve(d);
  const receipt = await c.getTransactionReceipt({ hash: transactionHash });
  const registry = getAddress(cfg.registry);
  const events = parseEventLogs({
    abi: registryEventsAbi,
    eventName: "ReleaseRequested",
    logs: receipt.logs.filter((l) => getAddress(l.address) === registry),
  });
  const ours = events.find((e) => e.args.vaultId.toLowerCase() === vaultId.toLowerCase());
  return ours ? { claimId: ours.args.claimId.toLowerCase() as Hex, amount: ours.args.amount } : null;
}

/**
 * v4: the account's pre-liquidation terms on the Settlement Adapter (`preLiquidationTerms`; all
 * zero when never set or cleared); null when the Adapter has no pre-liquidation (v3).
 */
export async function readPreLiquidationTerms(account: Address, d?: ChainDeps): Promise<PreLiquidationTerms | null> {
  const { c, cfg } = resolve(d);
  try {
    const t = await c.readContract({ address: cfg.adapter, abi: adapterV4Abi, functionName: "preLiquidationTerms", args: [account] });
    return { preLltv: t.preLltv, preLcf1: t.preLcf1, preLcf2: t.preLcf2, preLif1: t.preLif1, preLif2: t.preLif2, expiry: t.expiry };
  } catch {
    return null;
  }
}

export async function isNonceUsed(account: Address, nonce: bigint, d?: ChainDeps): Promise<boolean> {
  const { c, cfg } = resolve(d);
  return c.readContract({ address: cfg.adapter, abi: adapterAbi, functionName: "nonceUsed", args: [account, nonce] });
}

export interface RegistryEvent {
  name: (typeof registryEventsAbi)[number]["name"] | (typeof registryV4EventsAbi)[number]["name"];
  blockNumber: bigint;
  transactionHash: Hex;
  vaultId: Hex;
  /** What a Base observer sees in the event, as strings for display. */
  fields: Record<string, string>;
}

/** Block span of one eth_getLogs request (public RPCs reject large ranges or responses). */
export const LOG_CHUNK = 2_000n;

/**
 * Recent public Registry events (what anyone watching Base sees), newest first. Reads the
 * last `chunks` × 2,000 blocks backwards; stops early at the first failing chunk and returns
 * what it has (throws only if nothing could be read).
 */
export async function readRecentRegistryEvents(
  opts: { chunks?: number; limit?: number } & ChainDeps = {},
): Promise<RegistryEvent[]> {
  const { chunks = 5, limit = 20 } = opts;
  const { c, cfg } = resolve(opts);
  const latest = await c.getBlockNumber();
  const out: RegistryEvent[] = [];
  let toBlock = latest;
  for (let i = 0; i < chunks && out.length < limit; i++) {
    const fromBlock = toBlock >= LOG_CHUNK ? toBlock - LOG_CHUNK + 1n : 0n;
    let logs;
    try {
      logs = await c.getLogs({ address: cfg.registry, events: [...registryEventsAbi, ...registryV4EventsAbi], fromBlock, toBlock });
    } catch (e) {
      if (i === 0) throw e;
      break;
    }
    for (const log of [...logs].reverse()) {
      const args = log.args as Record<string, unknown>;
      const fields: Record<string, string> = {};
      for (const [k, v] of Object.entries(args)) {
        if (k === "vaultId") continue;
        fields[k] = Array.isArray(v) ? `${v.length} claim(s)` : String(v);
      }
      out.push({
        name: log.eventName,
        blockNumber: log.blockNumber ?? 0n,
        transactionHash: (log.transactionHash ?? "0x") as Hex,
        vaultId: (args.vaultId ?? "0x") as Hex,
        fields,
      });
    }
    if (fromBlock === 0n) break;
    toBlock = fromBlock - 1n;
  }
  return out.slice(0, limit);
}

// ------------------------------------------------------------------ v3: the vault's ledger

export type OnchainVaultStatus = (typeof ONCHAIN_VAULT_STATUS)[number];
export type OnchainClaimKind = (typeof CLAIM_KIND)[number] | "Unknown";
export type OnchainClaimStatus = (typeof CLAIM_STATUS)[number] | "Unknown";
export type RefundReason = (typeof REFUND_REASON)[number];

/**
 * What the pinned Registry and Intake name on-chain (immutables, read once per client): the
 * contracts the app only reads, and the periods the vault page explains.
 */
export interface Links {
  /**
   * `registry.slasher()`, `registry.compensator()`, `registry.signerBond()` (which must be the
   * Intake's too); null where none is deployed. The bond is the one contract the app calls that
   * is not pinned at build time: it pays an overdue claim (T-05).
   */
  slasher: Address | null;
  compensator: Address | null;
  bond: Address | null;
  /** `intake.relay()`: the Zcash header relay a pending activation waits on. */
  relay: Address;
  /** IZecuredDisputes `disputeWindow()` (at the Registry): how long after its confirmation a claim can be disputed. */
  disputeWindow: number;
  /** IZecuredDisputes `inclusionWindow()`: how long the signers have to prove a disputed payout. */
  inclusionWindow: number;
  /** `registry.refundAbandonPeriod()`: a refund without a destination this long can be written off. */
  refundAbandonPeriod: number;
  /** `intake.pendingRefundDelay()`: a pause by signatures this long lets the account refund its pending activation. */
  pendingRefundDelay: number;
  /**
   * v4 (R-11), at the Registry's address: a Refund claim without a destination this long can be
   * marked dormant (out of the coverage cap until a destination is given); null before v4.
   */
  refundDormancy: number | null;
}

/** The pinned Intake is not the one the pinned Registry names: the build's pins are inconsistent. */
export class IntakeMismatchError extends Error {
  constructor() {
    super("The pinned Intake (NEXT_PUBLIC_INTAKE) is not the one the pinned Registry names. Rebuild the app with the deployment's addresses.");
    this.name = "IntakeMismatchError";
  }
}

/** The pinned Registry and Intake name different signer bonds: the deployment is inconsistent. */
export class BondMismatchError extends Error {
  constructor() {
    super("The pinned Registry and Intake name different signer bonds: this deployment is inconsistent, so the app does not use either.");
    this.name = "BondMismatchError";
  }
}

const linksCache = new WeakMap<PublicClient, Map<string, Promise<Links>>>();

function orNull(a: Address): Address | null {
  return a === zeroAddress ? null : a;
}

/**
 * See `Links`; fails with `IntakeMismatchError` unless the Registry names the pinned Intake, and
 * with `BondMismatchError` unless the Registry and the Intake name the same bond.
 */
export function readLinks(d?: ChainDeps): Promise<Links> {
  const { c, cfg } = resolve(d);
  let perClient = linksCache.get(c);
  if (!perClient) linksCache.set(c, (perClient = new Map()));
  const key = `${cfg.registry}:${cfg.intake}`.toLowerCase();
  const cached = perClient.get(key);
  if (cached) return cached;
  const reading = (async (): Promise<Links> => {
    const registry = { address: cfg.registry, abi: registryAbi } as const;
    const intake = { address: cfg.intake, abi: intakeAbi } as const;
    const disputes = { address: cfg.registry, abi: disputesAbi } as const;
    const [named, slasher, compensator, abandon, bond, intakeBond, relay, delay, disputeWindow, inclusionWindow] =
      await Promise.all([
        c.readContract({ ...registry, functionName: "intake" }),
        c.readContract({ ...registry, functionName: "slasher" }),
        c.readContract({ ...registry, functionName: "compensator" }),
        c.readContract({ ...registry, functionName: "refundAbandonPeriod" }),
        c.readContract({ ...registry, functionName: "signerBond" }),
        c.readContract({ ...intake, functionName: "signerBond" }),
        c.readContract({ ...intake, functionName: "relay" }),
        c.readContract({ ...intake, functionName: "pendingRefundDelay" }),
        c.readContract({ ...disputes, functionName: "disputeWindow" }),
        c.readContract({ ...disputes, functionName: "inclusionWindow" }),
      ]);
    if (getAddress(named) !== getAddress(cfg.intake)) throw new IntakeMismatchError();
    if (getAddress(bond) !== getAddress(intakeBond)) throw new BondMismatchError();
    // v4 only: a v3 Registry's fallback has no such function (the call reverts).
    const dormancy = await c
      .readContract({ address: cfg.registry, abi: refundsV4Abi, functionName: "refundDormancy" })
      .catch(() => null);
    return {
      slasher: orNull(slasher),
      compensator: orNull(compensator),
      bond: orNull(bond),
      relay,
      disputeWindow: Number(disputeWindow),
      inclusionWindow: Number(inclusionWindow),
      refundAbandonPeriod: Number(abandon),
      pendingRefundDelay: Number(delay),
      refundDormancy: dormancy === null ? null : Number(dormancy),
    };
  })();
  perClient.set(key, reading);
  // A failed read is retried next time.
  reading.catch(() => perClient.delete(key));
  return reading;
}

/** Most claims `readVaultLedger` reads (the newest). */
export const MAX_LEDGER_CLAIMS = 32;

/** The Registry's `claimIdOf(vaultId, sequence)`: keccak256(abi.encode(vaultId, uint64 sequence)). */
export function claimIdOf(vaultId: Hex, sequence: bigint): Hex {
  return keccak256(encodeAbiParameters([{ type: "bytes32" }, { type: "uint64" }], [vaultId, sequence]));
}

/** One claim of the vault, as the Registry keeps it (`claimDetails`, `payoutDispute`). */
export interface LedgerClaim {
  claimId: Hex;
  /** Its sequence in the vault, from 0 (in creation order). */
  sequence: number;
  kind: OnchainClaimKind;
  status: OnchainClaimStatus;
  /** zatoshi */
  amount: bigint;
  /** The payout's recipient commitment; zero while a Refund has no destination (or the bonds paid the claim). */
  commitment: Hex;
  /** When it became payable (Unix s): its creation, or a Refund's destination. */
  createdAt: number;
  /** The signers' bonds paid its claimant (T-05); its ZEC now reimburses them. */
  bondPaid: boolean;
  /** Who gets its USDC from the bonds or the compensator; zero for Fee and Retired claims. */
  claimant: Address;
  /** When it was confirmed paid (Unix s; 0: not, or the confirmation was upheld). */
  paidAt: number;
  /** While disputed: when the signers' time to prove the payout ends (Unix s; 0 otherwise). */
  disputeDueAt: number;
  /**
   * T-05, read only for the vault's own unpaid claims with a commitment (its account is their
   * claimant): when the signers' bonds can pay it instead (`livenessDue`: Unix s) and the USDC
   * (6 decimals) they would pay at the current price, premium included; null when they never do.
   */
  liveness: { dueAt: number; usdc: bigint } | null;
  /**
   * The signers committed a payout of it on Base (`claimInput(claimId) != 0`): it is on its way on
   * Zcash ("Sent"). Read for pending claims with a destination; null when not read.
   */
  committed: boolean | null;
  /**
   * R-5, read for the account's own pending claims with a destination: the bond's arming
   * (`livenessArming`), whose commitment must be the claim's current one; null when the bond has no
   * arming (v3) or it was not read.
   */
  arming: { commitment: Hex; payableAt: number } | null;
  /** R-11: a Refund claim without a destination set aside as dormant (`isRefundDormant`); null when not read. */
  dormant: boolean | null;
}

/** The vault's pending activation (K3), with what decides when it finalizes or can be refunded. */
export interface PendingActivationView {
  /** zatoshi */
  amount: bigint;
  /** Zcash height the relay must reach, and the time that must pass, before anyone can finalize it. */
  readyHeight: bigint;
  readyTime: number;
  /** The relay's best tip height now. */
  relayTip: bigint;
  /** The block the deposit proof is anchored to is still on the relay's best chain. */
  anchorOnBestChain: boolean;
  /** Theft and other violation cases open on this vault (E2: it waits while any is). */
  openCases: bigint;
  /** The Intake's and the bond's gates now. */
  paused: boolean;
  pausedUntil: number;
  pausedSince: number;
  halted: boolean;
  fullyBonded: boolean;
}

/**
 * R-12 (v4): a deposit proven as a refund that waits, like an activation (K3), before its Refund
 * claim opens (`finalizeRefund`, which the backend's keeper calls once it is ready). Until then it
 * is a pending refund, without a claim, so it cannot take a destination yet.
 */
export interface PendingRefundView {
  depositTag: Hex;
  /** zatoshi */
  amount: bigint;
  reason: RefundReason | null;
  /** Zcash height the relay must reach, and the time that must pass, before its claim can open. */
  readyHeight: bigint;
  readyTime: number;
}

/** How far back the first read of a vault looks for pending refunds: 5 windows, about 5.5 h of Base blocks. */
export const PENDING_REFUND_LOOKBACK = 5n * LOG_CHUNK;

const refundScans = new WeakMap<PublicClient, Map<string, { next: bigint; tags: Set<Hex> }>>();

/**
 * The vault's pending refunds (R-12): the deposit tags of its `RefundPending` events, scanned once
 * from `PENDING_REFUND_LOOKBACK` blocks before `latest` and then incrementally (a pending refund
 * lasts minutes to hours), each still pending per the Intake's `pendingRefund(tag)`. A v3 Intake
 * emits no such event: always empty there.
 */
export async function readPendingRefunds(vaultId: Hex, latest: bigint, d?: ChainDeps): Promise<PendingRefundView[]> {
  const { c, cfg } = resolve(d);
  let perClient = refundScans.get(c);
  if (!perClient) refundScans.set(c, (perClient = new Map()));
  const key = `${cfg.intake}:${vaultId}`.toLowerCase();
  const floor = latest >= PENDING_REFUND_LOOKBACK ? latest - PENDING_REFUND_LOOKBACK + 1n : 0n;
  const scan = perClient.get(key) ?? { next: floor, tags: new Set<Hex>() };
  for (let from = scan.next > floor ? scan.next : floor; from <= latest; ) {
    const to = from + LOG_CHUNK - 1n < latest ? from + LOG_CHUNK - 1n : latest;
    const logs = await c.getLogs({ address: cfg.intake, event: refundPendingEvent, args: { vaultId }, fromBlock: from, toBlock: to });
    for (const log of logs) if (log.args.depositTag) scan.tags.add(log.args.depositTag);
    from = to + 1n;
  }
  scan.next = latest + 1n;
  perClient.set(key, scan);
  const out: PendingRefundView[] = [];
  for (const tag of [...scan.tags]) {
    const p = await c.readContract({ address: cfg.intake, abi: intakeV4Abi, functionName: "pendingRefund", args: [tag] });
    // Finalized into a Refund claim, or dropped: nothing pending any more.
    if (p.account === zeroAddress || p.vaultId.toLowerCase() !== vaultId.toLowerCase()) {
      scan.tags.delete(tag);
      continue;
    }
    out.push({
      depositTag: tag,
      amount: p.amount,
      reason: REFUND_REASON[p.reason] ?? null,
      readyHeight: p.readyHeight,
      readyTime: Number(p.readyTime),
    });
  }
  return out;
}

export interface VaultLedger {
  vaultId: Hex;
  /** Zero while the vault is unknown to Base. */
  account: Address;
  outstanding: bigint;
  status: OnchainVaultStatus;
  unpaidClaims: number;
  claims: LedgerClaim[];
  pending: PendingActivationView | null;
  /** R-12 (v4): deposits proven as refunds whose claims have not opened yet; null when they could not be read. */
  pendingRefunds: PendingRefundView[] | null;
  /** An F1/F2 case is open on the vault or was executed: the Registry refuses its exits (T-04). */
  frozen: boolean;
  /** An executed F1/F2 case: the compensator settles its claims in USDC. */
  compromised: boolean;
  links: Links;
  /** The latest block's timestamp (Unix s). */
  blockTime: number;
  /**
   * What the signers' bonds hold now (`totalSlashable`, USDC), read only when the bonds can pay
   * one of the vault's claims now: a payment beyond it reverts until they hold enough again.
   */
  bondsSlashable: bigint | null;
}

/**
 * Everything Base keeps about `vaultId` that the vault page shows: the vault, every claim (v3
 * kinds and statuses, dispute times, when the bonds pay the account's own claims instead), its
 * pending activation with the gates that hold it, and whether a theft case froze or compromised
 * it. Reads only, through the proxy.
 */
export async function readVaultLedger(vaultId: Hex, d?: ChainDeps): Promise<VaultLedger> {
  const { c, cfg } = resolve(d);
  const links = await readLinks({ client: c, config: cfg });
  const slasher = links.slasher;
  const bond = links.bond;
  const [details, block, pending, frozen, compromised] = await Promise.all([
    c.readContract({ address: cfg.registry, abi: registryAbi, functionName: "vaultDetails", args: [vaultId] }),
    c.getBlock({ blockTag: "latest" }),
    c.readContract({ address: cfg.intake, abi: intakeAbi, functionName: "pendingActivation", args: [vaultId] }),
    slasher ? c.readContract({ address: slasher, abi: slasherAbi, functionName: "vaultFrozen", args: [vaultId] }) : false,
    slasher ? c.readContract({ address: slasher, abi: slasherAbi, functionName: "isCompromised", args: [vaultId] }) : false,
  ]);
  const blockTime = Number(block.timestamp);
  // The newest MAX_LEDGER_CLAIMS: a vault has a handful, and a wrong count must not flood the proxy.
  const count = Number(details.claimCount);
  const first = Math.max(0, count - MAX_LEDGER_CLAIMS);
  const claims = await Promise.all(
    Array.from({ length: count - first }, async (_, i): Promise<LedgerClaim> => {
      const sequence = first + i;
      const claimId = claimIdOf(vaultId, BigInt(sequence));
      const cd = await c.readContract({ address: cfg.registry, abi: registryAbi, functionName: "claimDetails", args: [claimId] });
      const status = CLAIM_STATUS[cd.status] ?? "Unknown";
      let paidAt = 0;
      let disputeDueAt = 0;
      if (status === "Paid" || status === "Disputed") {
        const [paid, due] = await c.readContract({
          address: cfg.registry,
          abi: disputesAbi,
          functionName: "payoutDispute",
          args: [claimId],
        });
        paidAt = Number(paid);
        disputeDueAt = Number(due);
      }
      // T-05: only the account's own unpaid claims that have a destination; never on a frozen vault.
      let liveness: LedgerClaim["liveness"] = null;
      let arming: LedgerClaim["arming"] = null;
      const own = cd.claimant !== zeroAddress && getAddress(cd.claimant) === getAddress(details.account);
      const withDestination = cd.commitment.toLowerCase() !== ZERO_BYTES32;
      if (bond && !frozen && own && status === "Pending" && !cd.bondPaid && withDestination) {
        const [[dueAt, usdc], armed] = await Promise.all([
          c.readContract({ address: bond, abi: bondAbi, functionName: "livenessDue", args: [claimId] }),
          // R-5 (v4 bonds only; a v3 bond has no arming and needs none).
          c.readContract({ address: bond, abi: bondV4Abi, functionName: "livenessArming", args: [claimId] }).catch(() => null),
        ]);
        if (dueAt !== 0n) liveness = { dueAt: Number(dueAt), usdc };
        if (armed) arming = { commitment: armed[0], payableAt: Number(armed[1]) };
      }
      // Section 3 of the v4 design: a pending claim the signers committed a payout of is "Sent".
      const committed =
        status === "Pending" && withDestination
          ? await c
              .readContract({ address: cfg.registry, abi: registryViewsAbi, functionName: "claimInput", args: [claimId] })
              .then((input) => input.toLowerCase() !== ZERO_BYTES32)
              .catch(() => null)
          : null;
      // R-11 (v4): a Refund claim without a destination may have been set aside as dormant.
      const dormant =
        links.refundDormancy !== null && CLAIM_KIND[cd.kind] === "Refund" && status === "Pending" && !withDestination
          ? await c
              .readContract({ address: cfg.registry, abi: refundsV4Abi, functionName: "isRefundDormant", args: [claimId] })
              .catch(() => null)
          : null;
      return {
        claimId,
        sequence,
        kind: CLAIM_KIND[cd.kind] ?? "Unknown",
        status,
        amount: cd.amount,
        commitment: cd.commitment,
        createdAt: Number(cd.createdAt),
        bondPaid: cd.bondPaid,
        claimant: cd.claimant,
        paidAt,
        disputeDueAt,
        liveness,
        committed,
        arming,
        dormant,
      };
    }),
  );
  const pendingRefunds = await readPendingRefunds(vaultId, block.number, { client: c, config: cfg }).catch(() => null);
  const payableNow = claims.some((cl) => cl.liveness !== null && cl.liveness.dueAt <= blockTime && cl.liveness.usdc > 0n);
  const bondsSlashable =
    bond && payableNow ? await c.readContract({ address: bond, abi: bondAbi, functionName: "totalSlashable" }) : null;
  let pendingView: PendingActivationView | null = null;
  if (pending.account !== zeroAddress) {
    const intake = { address: cfg.intake, abi: intakeAbi } as const;
    const bond = links.bond;
    const [tip, onBest, paused, until, since, halted, bonded, openCases] = await Promise.all([
      c.readContract({ address: links.relay, abi: relayAbi, functionName: "bestChainTip" }),
      c.readContract({ address: links.relay, abi: relayAbi, functionName: "isInBestChain", args: [pending.anchor] }),
      c.readContract({ ...intake, functionName: "mintingPaused" }),
      c.readContract({ ...intake, functionName: "pausedUntil" }),
      c.readContract({ ...intake, functionName: "pausedSince" }),
      bond ? c.readContract({ address: bond, abi: bondAbi, functionName: "halted" }) : false,
      bond ? c.readContract({ address: bond, abi: bondAbi, functionName: "fullyBonded" }) : true,
      slasher ? c.readContract({ address: slasher, abi: slasherAbi, functionName: "openCasesOf", args: [vaultId] }) : 0n,
    ]);
    pendingView = {
      amount: pending.amount,
      readyHeight: pending.readyHeight,
      readyTime: Number(pending.readyTime),
      relayTip: tip[1],
      anchorOnBestChain: onBest,
      openCases,
      paused,
      pausedUntil: Number(until),
      pausedSince: Number(since),
      halted,
      fullyBonded: bonded,
    };
  }
  return {
    vaultId,
    account: details.account,
    outstanding: details.outstanding,
    status: ONCHAIN_VAULT_STATUS[details.status] ?? "None",
    unpaidClaims: Number(details.unpaidClaims),
    claims,
    pending: pendingView,
    pendingRefunds,
    frozen,
    compromised,
    links,
    blockTime,
    bondsSlashable,
  };
}

/**
 * Why a deposit landed as `claimId`, a Refund claim of `vaultId` created at `createdAt` (the
 * Intake's `DepositRefunded`): finds the block of that second (galloping back from the latest
 * block, then bisecting, `getBlock` only), then reads that block's 2,000-block window of logs.
 * Null when it is not found within `maxProbes` block reads. Only valid while the claim has no
 * destination: setting one moves `createdAt`.
 */
export async function readRefundReason(
  vaultId: Hex,
  claimId: Hex,
  createdAt: number,
  opts: { maxProbes?: number } & ChainDeps = {},
): Promise<RefundReason | null> {
  const { maxProbes = 40 } = opts;
  const { c, cfg } = resolve(opts);
  const latest = await c.getBlock({ blockTag: "latest" });
  const target = BigInt(createdAt);
  if (latest.timestamp < target) return null;
  let probes = 0;
  const timeOf = async (n: bigint): Promise<bigint> => {
    if (++probes > maxProbes) throw new Error("too many probes");
    return (await c.getBlock({ blockNumber: n })).timestamp;
  };
  // Invariant: the block at `hi` is at or after the target second, the block at `lo` before it.
  let hi = latest.number;
  let lo = -1n;
  try {
    let step = LOG_CHUNK;
    while (lo < 0n) {
      const n = hi > step ? hi - step : 0n;
      if ((await timeOf(n)) < target) lo = n;
      else if (n === 0n) break;
      else {
        hi = n;
        step *= 4n;
      }
    }
    // The second's blocks start at the first block of `hi`'s kind: stop once one window holds them.
    while (lo >= 0n && hi - lo > LOG_CHUNK - 64n) {
      const mid = (lo + hi) / 2n;
      if ((await timeOf(mid)) < target) lo = mid;
      else hi = mid;
    }
  } catch {
    return null;
  }
  const fromBlock = lo + 1n;
  const toBlock = fromBlock + LOG_CHUNK - 1n < latest.number ? fromBlock + LOG_CHUNK - 1n : latest.number;
  const logs = await c.getLogs({ address: cfg.intake, event: depositRefundedEvent, args: { vaultId, claimId }, fromBlock, toBlock });
  const reason = logs[0]?.args.reason;
  return reason === undefined ? null : (REFUND_REASON[reason] ?? null);
}

/** What the compensator owes a compromised vault (T-04), read-only. */
export interface Compensation {
  /** The incident the vault is a victim in (0: not registered yet). */
  incident: bigint;
  /** The vault's tally: its outstanding and Morpho debt then, and whether its account still authorized the Registry. */
  victim: { tallied: boolean; settled: boolean; authorized: boolean; outstanding: bigint; debt: bigint };
  /** The incident: closes at, finalized, its price P* (Morpho scale) and the ratios paid to layers 2 and 4 (1e18 = in full). */
  incidentState: { closesAt: number; finalized: boolean; price: bigint; r2: bigint; r4: bigint } | null;
  /** USDC (6 decimals) the account's equity comes to, withdrawable once the vault is settled. */
  equity: bigint;
  /** Each tallied claim's entitlement: its layer (2: third party, 4: the owner), zatoshi, and USDC once finalized. */
  entitlements: { claimId: Hex; layer: number; zat: bigint; to: Address; paid: boolean; usdc: bigint | null }[];
}

/** The compensator's view of `vaultId` and of its `claimIds`; null without a compensator. */
export async function readCompensation(vaultId: Hex, claimIds: readonly Hex[], d?: ChainDeps): Promise<Compensation | null> {
  const { c, cfg } = resolve(d);
  const { compensator } = await readLinks({ client: c, config: cfg });
  if (!compensator) return null;
  const at = { address: compensator, abi: compensatorAbi } as const;
  const [victim, equity, entitlements] = await Promise.all([
    c.readContract({ ...at, functionName: "victim", args: [vaultId] }),
    c.readContract({ ...at, functionName: "equityOf", args: [vaultId] }),
    Promise.all(claimIds.map((claimId) => c.readContract({ ...at, functionName: "entitlement", args: [claimId] }))),
  ]);
  const inc = victim.incident === 0n ? null : await c.readContract({ ...at, functionName: "incident", args: [victim.incident] });
  const usdcOf = (zat: bigint, layer: number): bigint | null => {
    if (!inc?.finalized) return null;
    const ratio = layer === 2 ? inc.r2 : inc.r4;
    return mulDivDown(mulDivDown(zat, inc.price, 10n ** 36n), ratio, 10n ** 18n);
  };
  return {
    incident: victim.incident,
    victim: {
      tallied: victim.tallied,
      settled: victim.settled,
      authorized: victim.authorized,
      outstanding: victim.outstanding,
      debt: victim.debt,
    },
    incidentState: inc
      ? { closesAt: Number(inc.closesAt), finalized: inc.finalized, price: inc.price, r2: inc.r2, r4: inc.r4 }
      : null,
    equity,
    entitlements: claimIds
      .map((claimId, i) => ({ claimId, e: entitlements[i]! }))
      .filter(({ e }) => e.incident !== 0n)
      .map(({ claimId, e }) => ({ claimId, layer: e.layer, zat: e.zat, to: e.to, paid: e.paid, usdc: usdcOf(e.zat, e.layer) })),
  };
}
