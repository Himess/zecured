/**
 * The public pages' numbers (P-2 status, P-4 market): read from Base through the backend's proxy
 * (the Registry, the Intake, the signers' bond, the header relay, Morpho, its IRM and the oracle;
 * nothing about any user) and turned into what the pages show, as pure functions the tests pin.
 */
import { encodeFunctionData, type Address, type Hex } from "viem";
import { erc20Abi, irmAbi, morphoAbi, oracleAbi, relayAbi } from "./abi";
import { bondViewsAbi, erc20AllowanceAbi, intakeV4Abi, intakeViewsAbi, morphoSupplyAbi, registryViewsAbi } from "./abi4";
import type { VaultStats } from "./api";
import { ContractsNotDeployedError, publicClient, readLinks, readMarket, type ChainDeps } from "./chain";
import { config as appConfig, marketParams, type MarketParams } from "./config";
import { BPS, collateralValue, expectedMarket, mulDivDown, toAssetsDown, WAD, wTaylorCompounded, type MarketState } from "./morpho";

export const SECONDS_PER_YEAR = 365n * 86_400n;
/** The signer set's size (CLAUDE.md: five signers, three to sign). */
export const SIGNERS = 5;

/** What the status and market pages read from Base in one round. */
export interface ProtocolSnapshot {
  /** The Zcash header relay's best tip height; null when unreadable. */
  relayTip: bigint | null;
  /** `Registry.lockedZatoshi()`: the zatoshi backing outstanding zZEC and unpaid claims. */
  lockedZat: bigint;
  openVaults: bigint;
  /** `Intake.capRoom()`; null while it cannot be read (the cap's price stale). */
  capRoom: bigint | null;
  maxTotalZat: bigint;
  coverageBps: number;
  pendingTotal: bigint;
  /** v4 (R-12); null before v4. */
  pendingRefundTotal: bigint | null;
  /** The signers' bonds (null without a bond): each signer's, B_eff, what a slash takes, the minimum. */
  bonds: { each: bigint[]; effective: bigint; totalSlashable: bigint; minBond: bigint } | null;
  /** Morpho's market with the interest accrued to the latest block, and its borrow rate (per second, WAD). */
  market: MarketState;
  borrowRate: bigint;
  /** The market's oracle price (USDC units per zatoshi × 1e36). */
  price: bigint;
  blockTime: number;
}

/** See `ProtocolSnapshot`. Reads only, through the proxy. */
export async function readProtocol(d: ChainDeps = {}): Promise<ProtocolSnapshot> {
  const cfg = d.config ?? appConfig;
  if (!cfg.contractsDeployed) throw new ContractsNotDeployedError();
  const c = d.client ?? publicClient();
  const deps = { client: c, config: cfg };
  const links = await readLinks(deps);
  const [market, price, block, lockedZat, openVaults, capRoom, maxTotalZat, coverageBps, pendingTotal, relayTip, pendingRefundTotal] =
    await Promise.all([
      readMarket(deps),
      c.readContract({ address: cfg.oracle, abi: oracleAbi, functionName: "price" }),
      c.getBlock({ blockTag: "latest" }),
      c.readContract({ address: cfg.registry, abi: registryViewsAbi, functionName: "lockedZatoshi" }),
      c.readContract({ address: cfg.registry, abi: registryViewsAbi, functionName: "openVaultCount" }),
      c.readContract({ address: cfg.intake, abi: intakeViewsAbi, functionName: "capRoom" }).catch(() => null),
      c.readContract({ address: cfg.intake, abi: intakeViewsAbi, functionName: "maxTotalZat" }),
      c.readContract({ address: cfg.intake, abi: intakeViewsAbi, functionName: "coverageBps" }),
      c.readContract({ address: cfg.intake, abi: intakeViewsAbi, functionName: "pendingTotal" }),
      c
        .readContract({ address: links.relay, abi: relayAbi, functionName: "bestChainTip" })
        .then((t) => t[1])
        .catch(() => null),
      // v4 only: a v3 Intake has no pending refunds.
      c.readContract({ address: cfg.intake, abi: intakeV4Abi, functionName: "pendingRefundTotal" }).catch(() => null),
    ]);
  const borrowRate = await c
    .readContract({ address: cfg.irm, abi: irmAbi, functionName: "borrowRateView", args: [marketParams(cfg), market] })
    .catch(() => 0n);
  let bonds: ProtocolSnapshot["bonds"] = null;
  const bond = links.bond;
  if (bond) {
    const [each, effective, totalSlashable, minBond] = await Promise.all([
      Promise.all(
        Array.from({ length: SIGNERS }, (_, i) => c.readContract({ address: bond, abi: bondViewsAbi, functionName: "bondOf", args: [i] })),
      ),
      c.readContract({ address: bond, abi: bondViewsAbi, functionName: "effectiveBond" }),
      c.readContract({ address: bond, abi: bondViewsAbi, functionName: "totalSlashable" }),
      c.readContract({ address: bond, abi: bondViewsAbi, functionName: "minBond" }),
    ]);
    bonds = { each, effective, totalSlashable, minBond };
  }
  return {
    relayTip,
    lockedZat,
    openVaults,
    capRoom,
    maxTotalZat,
    coverageBps,
    pendingTotal,
    pendingRefundTotal,
    bonds,
    market: expectedMarket(market, borrowRate, block.timestamp),
    borrowRate,
    price,
    blockTime: Number(block.timestamp),
  };
}

// ------------------------------------------------------------------ the market (P-4)

export interface MarketNumbers {
  suppliedUsdc: bigint;
  borrowedUsdc: bigint;
  /** What lenders can withdraw right now. */
  availableUsdc: bigint;
  /** Borrowed over supplied (WAD); 0 with nothing supplied. */
  utilizationWad: bigint;
  /** e^(rate × year) − 1 to the third order, as Morpho's own libraries (WAD). */
  borrowApyWad: bigint;
  /** The borrow APY × utilization × (1 − fee): what lenders earn (WAD). */
  supplyApyWad: bigint;
}

export function marketNumbers(m: MarketState, borrowRate: bigint): MarketNumbers {
  const supplied = m.totalSupplyAssets;
  const borrowed = m.totalBorrowAssets;
  const utilizationWad = supplied === 0n ? 0n : mulDivDown(borrowed, WAD, supplied);
  const borrowApyWad = wTaylorCompounded(borrowRate, SECONDS_PER_YEAR);
  const supplyApyWad = mulDivDown(mulDivDown(borrowApyWad, utilizationWad, WAD), WAD - m.fee, WAD);
  return {
    suppliedUsdc: supplied,
    borrowedUsdc: borrowed,
    availableUsdc: supplied > borrowed ? supplied - borrowed : 0n,
    utilizationWad,
    borrowApyWad,
    supplyApyWad,
  };
}

// ------------------------------------------------------------------ the coverage cap and the bonds (P-2, P-4)

export interface CapNumbers {
  lockedZat: bigint;
  /** Pending activations plus pending refunds (v4): they count against the cap too. */
  pendingZat: bigint;
  usedZat: bigint;
  /** What the cap still admits; null while it cannot be read. */
  roomZat: bigint | null;
  maxTotalZat: bigint;
  /** Used over used plus room, in basis points; null when unknown or both are zero. */
  fillBps: bigint | null;
}

export function capNumbers(s: Pick<ProtocolSnapshot, "lockedZat" | "pendingTotal" | "pendingRefundTotal" | "capRoom" | "maxTotalZat">): CapNumbers {
  const pendingZat = s.pendingTotal + (s.pendingRefundTotal ?? 0n);
  const usedZat = s.lockedZat + pendingZat;
  const whole = s.capRoom === null ? 0n : usedZat + s.capRoom;
  return {
    lockedZat: s.lockedZat,
    pendingZat,
    usedZat,
    roomZat: s.capRoom,
    maxTotalZat: s.maxTotalZat,
    fillBps: s.capRoom === null || whole === 0n ? null : (usedZat * BPS) / whole,
  };
}

export interface BondNumbers {
  /** ΣB: the sum of the five bonds (USDC units). */
  sum: bigint;
  /** B_eff = min(ΣB, 5 × S3 / 3), S3 the three smallest: what the coverage cap counts. */
  effective: bigint;
  totalSlashable: bigint;
  minBond: bigint;
  /** Signers holding at least the minimum bond. */
  bonded: number;
  /** B_eff over the locked collateral's value at the market price (WAD); null with nothing locked. */
  perCollateralWad: bigint | null;
}

export function bondNumbers(b: NonNullable<ProtocolSnapshot["bonds"]>, lockedZat: bigint, price: bigint): BondNumbers {
  const value = collateralValue(lockedZat, price);
  return {
    sum: b.each.reduce((a, x) => a + x, 0n),
    effective: b.effective,
    totalSlashable: b.totalSlashable,
    minBond: b.minBond,
    bonded: b.each.filter((x) => x >= b.minBond && x > 0n).length,
    perCollateralWad: value === 0n ? null : mulDivDown(b.effective, WAD, value),
  };
}

// ------------------------------------------------------------------ the status page (P-2)

/** Open vaults of each of the deployment's sizes (the anonymity sets); null when the backend has no counts. */
export function anonymitySets(denominations: readonly bigint[], stats: VaultStats | null): { amount: bigint; open: number }[] | null {
  if (!stats) return null;
  return denominations.map((amount) => ({ amount, open: stats.byAmount.get(amount) ?? 0 }));
}

/** Zcash blocks the header relay is behind the coordinator's light wallet server (0 when level or ahead); null when unknown. */
export function relayLag(relayTip: bigint | null, zcashTip: number): number | null {
  if (relayTip === null || zcashTip <= 0) return null;
  const lag = zcashTip - Number(relayTip);
  return lag > 0 ? lag : 0;
}

// ------------------------------------------------------------------ lending from the user's own wallet (P-4)

export interface WalletCall {
  to: Address;
  data: Hex;
}

/** Lend `assets` USDC from the user's wallet: approve Morpho for the shortfall, then `supply` on its own behalf. */
export function supplyCalls(p: {
  morpho: Address;
  usdc: Address;
  params: MarketParams;
  wallet: Address;
  assets: bigint;
  allowance: bigint;
}): WalletCall[] {
  if (p.assets <= 0n) throw new Error("Lend a positive amount.");
  const out: WalletCall[] = [];
  if (p.allowance < p.assets) {
    out.push({ to: p.usdc, data: encodeFunctionData({ abi: erc20Abi, functionName: "approve", args: [p.morpho, p.assets] }) });
  }
  out.push({
    to: p.morpho,
    data: encodeFunctionData({ abi: morphoSupplyAbi, functionName: "supply", args: [p.params, p.assets, 0n, p.wallet, "0x"] }),
  });
  return out;
}

/**
 * Withdraw from the wallet's supply to itself: `assets` USDC, or everything by its `shares` (no
 * dust left). Morpho refuses what borrowers are using (only `availableUsdc` can leave now).
 */
export function withdrawCall(p: { morpho: Address; params: MarketParams; wallet: Address } & ({ assets: bigint } | { shares: bigint })): WalletCall {
  const [assets, shares] = "assets" in p ? [p.assets, 0n] : [0n, p.shares];
  if (assets <= 0n && shares <= 0n) throw new Error("Withdraw a positive amount.");
  return {
    to: p.morpho,
    data: encodeFunctionData({ abi: morphoSupplyAbi, functionName: "withdraw", args: [p.params, assets, shares, p.wallet, p.wallet] }),
  };
}

/** The wallet's supply in the market: its shares, and their USDC now (rounded down, as Morpho pays). */
export async function readSupply(wallet: Address, d: ChainDeps = {}): Promise<{ shares: bigint; assets: bigint; balance: bigint; allowance: bigint }> {
  const cfg = d.config ?? appConfig;
  if (!cfg.contractsDeployed) throw new ContractsNotDeployedError();
  const c = d.client ?? publicClient();
  const [pos, market, block, balance, allowance] = await Promise.all([
    c.readContract({ address: cfg.morpho, abi: morphoAbi, functionName: "position", args: [cfg.marketId, wallet] }),
    readMarket({ client: c, config: cfg }),
    c.getBlock({ blockTag: "latest" }),
    c.readContract({ address: cfg.usdc, abi: erc20Abi, functionName: "balanceOf", args: [wallet] }),
    c.readContract({ address: cfg.usdc, abi: erc20AllowanceAbi, functionName: "allowance", args: [wallet, cfg.morpho] }),
  ]);
  const rate = await c
    .readContract({ address: cfg.irm, abi: irmAbi, functionName: "borrowRateView", args: [marketParams(cfg), market] })
    .catch(() => 0n);
  const now = expectedMarket(market, rate, block.timestamp);
  const shares = pos[0];
  return { shares, assets: toAssetsDown(shares, now.totalSupplyAssets, now.totalSupplyShares), balance, allowance };
}
