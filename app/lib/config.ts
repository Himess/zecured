/**
 * App configuration from `NEXT_PUBLIC_*` environment variables.
 *
 * Every value here is public: Next.js inlines `process.env.NEXT_PUBLIC_*` into the browser
 * bundle at build time, which is why each variable is read with its literal name. Until the
 * contracts are deployed, placeholders are used and `config.contractsDeployed` is false; the
 * UI then skips Base reads and sponsored transactions and says so.
 */
import { encodeAbiParameters, getAddress, isAddress, isHex, keccak256, type Address, type Hex } from "viem";

/** Base Sepolia. Every EIP-712 domain uses this chain id (docs/INTERFACES.md § 2.1). */
export const CHAIN_ID = 84532 as const;

/**
 * Placeholder for the Registry and the Adapter before deployment. It matches the placeholder
 * of the local signer network config, so owner signatures (Registry domain) verify locally.
 */
export const PLACEHOLDER_CONTRACT: Address = "0x000000000000000000000000000000000000dEaD";
export const ZERO_BYTES32: Hex = "0x0000000000000000000000000000000000000000000000000000000000000000";

/** Real Base Sepolia deployments (docs/ref/FACTS.md, docs/INTERFACES.md § 3.5). */
export const MORPHO_BLUE: Address = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb";
export const ADAPTIVE_CURVE_IRM: Address = "0x46415998764C29aB2a25CbeA6254146D50D22687";
/** LLTV of the Zecured market: 62.5 % (WAD). */
export const MARKET_LLTV = 625_000_000_000_000_000n;

export interface AppConfig {
  backendUrl: string;
  registry: Address;
  adapter: Address;
  morpho: Address;
  marketId: Hex;
  usdc: Address;
  oracle: Address;
  zzec: Address;
  irm: Address;
  lltv: bigint;
  /** False while any Base contract address is a placeholder. */
  contractsDeployed: boolean;
}

function address(value: string | undefined, fallback: Address): Address {
  const v = value?.trim();
  return v && isAddress(v, { strict: false }) ? getAddress(v) : fallback;
}

function bytes32(value: string | undefined, fallback: Hex): Hex {
  const v = value?.trim();
  return v && isHex(v) && v.length === 66 ? (v.toLowerCase() as Hex) : fallback;
}

function url(value: string | undefined, fallback: string): string {
  const v = value?.trim();
  if (!v) return fallback;
  try {
    const u = new URL(v);
    return u.protocol === "http:" || u.protocol === "https:" ? u.toString().replace(/\/+$/, "") : fallback;
  } catch {
    return fallback;
  }
}

/** Builds the config from raw values (exported for tests). */
export function buildConfig(env: Record<string, string | undefined>): AppConfig {
  const zeroAddress: Address = "0x0000000000000000000000000000000000000000";
  const registry = address(env.NEXT_PUBLIC_REGISTRY, PLACEHOLDER_CONTRACT);
  const adapter = address(env.NEXT_PUBLIC_ADAPTER, PLACEHOLDER_CONTRACT);
  const usdc = address(env.NEXT_PUBLIC_USDC, zeroAddress);
  const oracle = address(env.NEXT_PUBLIC_ORACLE, zeroAddress);
  const zzec = address(env.NEXT_PUBLIC_ZZEC, zeroAddress);
  const marketId = bytes32(env.NEXT_PUBLIC_MARKET_ID, ZERO_BYTES32);
  const placeholders: Address[] = [PLACEHOLDER_CONTRACT, zeroAddress];
  const contractsDeployed =
    ![registry, adapter, usdc, oracle, zzec].some((a) => placeholders.includes(a)) && marketId !== ZERO_BYTES32;
  return {
    backendUrl: url(env.NEXT_PUBLIC_BACKEND_URL, "http://127.0.0.1:8080"),
    registry,
    adapter,
    morpho: address(env.NEXT_PUBLIC_MORPHO, MORPHO_BLUE),
    marketId,
    usdc,
    oracle,
    zzec,
    irm: address(env.NEXT_PUBLIC_IRM, ADAPTIVE_CURVE_IRM),
    lltv: MARKET_LLTV,
    contractsDeployed,
  };
}

export const config: AppConfig = buildConfig({
  NEXT_PUBLIC_BACKEND_URL: process.env.NEXT_PUBLIC_BACKEND_URL,
  NEXT_PUBLIC_REGISTRY: process.env.NEXT_PUBLIC_REGISTRY,
  NEXT_PUBLIC_ADAPTER: process.env.NEXT_PUBLIC_ADAPTER,
  NEXT_PUBLIC_MORPHO: process.env.NEXT_PUBLIC_MORPHO,
  NEXT_PUBLIC_MARKET_ID: process.env.NEXT_PUBLIC_MARKET_ID,
  NEXT_PUBLIC_USDC: process.env.NEXT_PUBLIC_USDC,
  NEXT_PUBLIC_ORACLE: process.env.NEXT_PUBLIC_ORACLE,
  NEXT_PUBLIC_ZZEC: process.env.NEXT_PUBLIC_ZZEC,
  NEXT_PUBLIC_IRM: process.env.NEXT_PUBLIC_IRM,
});

export interface MarketParams {
  loanToken: Address;
  collateralToken: Address;
  oracle: Address;
  irm: Address;
  lltv: bigint;
}

/** Morpho `MarketParams` of the Zecured market (zZEC / MockUSDC / MockZecOracle / IRM / 62.5 %). */
export function marketParams(c: AppConfig = config): MarketParams {
  return {
    loanToken: c.usdc,
    collateralToken: c.zzec,
    oracle: c.oracle,
    irm: c.irm,
    lltv: c.lltv,
  };
}

/** Morpho's market id: `keccak256(abi.encode(MarketParams))` (MarketParamsLib.id). */
export function marketIdOf(p: MarketParams): Hex {
  return keccak256(
    encodeAbiParameters(
      [
        { type: "address" },
        { type: "address" },
        { type: "address" },
        { type: "address" },
        { type: "uint256" },
      ],
      [p.loanToken, p.collateralToken, p.oracle, p.irm, p.lltv],
    ),
  );
}

/** Human-readable configuration problems (shown in the UI, never fatal). */
export function configIssues(c: AppConfig = config): string[] {
  const issues: string[] = [];
  if (!c.contractsDeployed) {
    issues.push(
      "Base contracts are not configured yet (placeholder addresses). Off-chain steps work; Base reads and sponsored transactions are disabled.",
    );
  } else if (marketIdOf(marketParams(c)).toLowerCase() !== c.marketId.toLowerCase()) {
    issues.push("NEXT_PUBLIC_MARKET_ID does not match the configured market parameters.");
  }
  return issues;
}
