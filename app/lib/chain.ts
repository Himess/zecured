/**
 * Base Sepolia reads. Every request goes to our backend's `/proxy/rpc`; the browser never
 * talks to an RPC provider directly (CLAUDE.md, "Hard privacy rules").
 *
 * Each reader takes optional dependencies (a viem client and the app config) so tests can run
 * them against a mocked JSON-RPC transport.
 */
import { createPublicClient, getAddress, http, parseEventLogs, type Address, type Hex, type PublicClient } from "viem";
import { baseSepolia } from "viem/chains";
import {
  adapterAbi,
  erc20Abi,
  irmAbi,
  morphoAbi,
  ONCHAIN_VAULT_STATUS,
  oracleAbi,
  registryAbi,
  registryEventsAbi,
} from "./abi";
import { api } from "./api";
import { config as appConfig, marketParams, type AppConfig } from "./config";
import { borrowAssets, expectedMarket, type MarketState, type PositionState } from "./morpho";

let defaultClient: PublicClient | null = null;

/** viem public client bound to the backend proxy. */
export function publicClient(): PublicClient {
  defaultClient ??= createPublicClient({
    chain: baseSepolia,
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

export async function isNonceUsed(account: Address, nonce: bigint, d?: ChainDeps): Promise<boolean> {
  const { c, cfg } = resolve(d);
  return c.readContract({ address: cfg.adapter, abi: adapterAbi, functionName: "nonceUsed", args: [account, nonce] });
}

export interface RegistryEvent {
  name: "VaultActivated" | "ReleaseRequested" | "LiquidationRelease" | "CloseRelease" | "PayoutConfirmed";
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
      logs = await c.getLogs({ address: cfg.registry, events: registryEventsAbi, fromBlock, toBlock });
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
