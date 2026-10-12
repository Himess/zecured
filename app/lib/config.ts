/**
 * App configuration, with every security-relevant value pinned at build time (security review
 * H-6).
 *
 * The Base addresses the app signs for or sends calls to (Registry, Adapter, zZEC, MockUSDC,
 * oracle, Morpho, IRM, market id) and the five signer addresses come from `NEXT_PUBLIC_*`
 * variables baked into the build; Morpho and the IRM default to Base Sepolia's canonical
 * deployments. Next.js inlines `process.env.NEXT_PUBLIC_*`, which is why each variable is read
 * with its literal name. Every value here is public.
 *
 * The backend's `GET /config` never overrides a pin. It is a consistency check: any
 * disagreement is a hard stop (owner signatures, Base reads and sponsored transactions are
 * disabled; `problems` says why). It does provide the values that carry no authority: whether
 * sponsored transactions are available, the denominations, the confirmations and the deposit
 * window. Only a development build (`next dev`) may take unpinned addresses from it, and the UI
 * then says so; a production build with a missing pin keeps Base disabled.
 *
 * `config` is updated in place once `loadRuntimeConfig()` (lib/api.ts) has fetched `/config`;
 * components/ConfigGate.tsx renders pages only after that, so nothing may read the config at
 * module-evaluation time.
 */
import { encodeAbiParameters, getAddress, isAddress, isHex, keccak256, type Address, type Hex } from "viem";
import { DENOMINATIONS } from "./zcash";

/** Base Sepolia. Every EIP-712 domain uses this chain id (docs/INTERFACES.md § 2.1). */
export const CHAIN_ID = 84532 as const;

/**
 * Placeholder for the Registry and the Adapter while they are not configured. It matches the
 * placeholder of the local signer network config, so owner signatures verify there before a
 * deployment.
 */
export const PLACEHOLDER_CONTRACT: Address = "0x000000000000000000000000000000000000dEaD";
export const ZERO_BYTES32: Hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
const ZERO_ADDRESS: Address = "0x0000000000000000000000000000000000000000";

/** Real Base Sepolia deployments (docs/ref/FACTS.md, docs/INTERFACES.md § 3.5). */
export const MORPHO_BLUE: Address = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb";
export const ADAPTIVE_CURVE_IRM: Address = "0x46415998764C29aB2a25CbeA6254146D50D22687";
/** LLTV of the Zecured market: 62.5 % (WAD). */
export const MARKET_LLTV = 625_000_000_000_000_000n;
/** Size of the signer set (3-of-5 FROST vaults). */
export const SIGNER_COUNT = 5;

/** How long pages wait for `GET /config` before rendering with the build-time configuration. */
export const RUNTIME_CONFIG_TIMEOUT_MS = 6_000;

export interface AppConfig {
  /** Our backend. Always from the build (`NEXT_PUBLIC_BACKEND_URL`): it serves `/config`. */
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
  /**
   * The five signer addresses, ascending: `NEXT_PUBLIC_SIGNERS`, or, in a development build
   * without it, the backend's (`signersPinned` false); null when unknown.
   */
  signers: readonly Address[] | null;
  /** `signers` is this build's pin (NEXT_PUBLIC_SIGNERS), not the backend's report. */
  signersPinned: boolean;
  /** A development build (`next dev`): unpinned addresses may come from the backend. */
  development: boolean;
  /** The `NEXT_PUBLIC_*` pins this build lacks. */
  unpinned: string[];
  /** Base reads and sponsored transactions may run: every address known and trusted, no problem. */
  contractsDeployed: boolean;
  /** Owner requests may be signed: the Registry is pinned (or this is a development build), no problem. */
  ownerSigning: boolean;
  /** The backend can sponsor UserOps: bundler, paymaster and paymaster allowlist configured. */
  sponsorship: boolean;
  /** Collateral denominations the signers accept, in zatoshi, ascending. */
  denominations: readonly bigint[];
  /** Zcash confirmations before a deposit is attested; null until the backend reports it. */
  confirmations: number | null;
  /** Zcash blocks a vault accepts its deposit for (expiry height = birthday + this); null until known. */
  vaultExpiryBlocks: number | null;
  /** `GET /config`: not fetched yet, checked and applied, or unavailable (`remoteError` says why). */
  remote: "pending" | "loaded" | "unavailable";
  remoteError: string | null;
  /** Hard stops: malformed pins, pins the backend contradicts, an inconsistent backend. */
  problems: string[];
}

/** `GET /config` (docs/INTERFACES.md § 4), as parsed by `parseRemoteConfig` (lib/api.ts). */
export interface RemoteConfig {
  chainId: number;
  /** The Registry and the Adapter the signer network uses (its EIP-712 domains). */
  registry: Address;
  adapter: Address;
  /** The deployment file's contracts; null before deployment. */
  contracts: RemoteContracts | null;
  /** The Zecured Morpho market; null before deployment. */
  market: RemoteMarket | null;
  /** The signer set, ascending, when the backend reports it. */
  signers: Address[] | null;
  /** zatoshi, ascending, without duplicates. */
  denominations: bigint[];
  confirmations: number;
  vaultExpiryBlocks: number;
  /** Bundler, paymaster and paymaster allowlist all configured on the backend. */
  sponsorship: boolean;
}

export interface RemoteContracts {
  /** The market's loan token: Circle's testnet USDC on Base Sepolia, a MockUSDC (MOCK) on the local fork. */
  LoanToken: Address;
  /** The market's oracle: the DemoShockOracle (MOCK / DEMO) around the base price (older deployments: MockZecOracle). */
  MarketOracle: Address;
  SettlementAdapter: Address;
  ZZEC: Address;
  ZecuredRegistry: Address;
}

export interface RemoteMarket {
  collateralToken: Address;
  irm: Address;
  lltv: bigint;
  loanToken: Address;
  marketId: Hex;
  morphoBlue: Address;
  oracle: Address;
}

/** The backend's configuration, or why it could not be loaded. */
export type RemoteConfigResult = RemoteConfig | { error: string };

export type ConfigEnv = Record<string, string | undefined>;

type PinField = "registry" | "adapter" | "morpho" | "marketId" | "usdc" | "oracle" | "zzec" | "irm";

/** The build-time pin of each Base value. */
const ENV_VAR: Record<PinField, string> = {
  registry: "NEXT_PUBLIC_REGISTRY",
  adapter: "NEXT_PUBLIC_ADAPTER",
  morpho: "NEXT_PUBLIC_MORPHO",
  marketId: "NEXT_PUBLIC_MARKET_ID",
  usdc: "NEXT_PUBLIC_USDC",
  oracle: "NEXT_PUBLIC_ORACLE",
  zzec: "NEXT_PUBLIC_ZZEC",
  irm: "NEXT_PUBLIC_IRM",
};
const FIELDS = Object.keys(ENV_VAR) as PinField[];

/** Pins without a safe default: a production build must set them (and NEXT_PUBLIC_SIGNERS). */
const REQUIRED: readonly PinField[] = ["registry", "adapter", "marketId", "usdc", "oracle", "zzec"];

/** Built-in pins: Base Sepolia's canonical Morpho Blue and AdaptiveCurveIRM. */
const BUILT_IN: Partial<Record<PinField, Hex>> = { morpho: MORPHO_BLUE, irm: ADAPTIVE_CURVE_IRM };

/** What an unknown value is set to: Base stays disabled until it is known. */
const UNSET: Record<PinField, Hex> = {
  registry: PLACEHOLDER_CONTRACT,
  adapter: PLACEHOLDER_CONTRACT,
  morpho: MORPHO_BLUE,
  marketId: ZERO_BYTES32,
  usdc: ZERO_ADDRESS,
  oracle: ZERO_ADDRESS,
  zzec: ZERO_ADDRESS,
  irm: ADAPTIVE_CURVE_IRM,
};

const MARKET_MISMATCH = "The market id does not match the market parameters (loan token, collateral, oracle, IRM, LLTV).";

/** 0x1234…abcd (contract addresses only: they are public). */
function abbreviate(hex: string): string {
  return hex.length > 14 ? `${hex.slice(0, 6)}…${hex.slice(-4)}` : hex;
}

function same(a: string, b: string): boolean {
  return a.toLowerCase() === b.toLowerCase();
}

/** "A", "A and B", "A, B and C". */
function list(items: readonly string[]): string {
  return items.length > 1 ? `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}` : (items[0] ?? "");
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

/** A normalized pin, `undefined` when the variable is empty, `null` when it is malformed. */
function parsePin(field: PinField, raw: string | undefined): Hex | null | undefined {
  const v = raw?.trim();
  if (!v) return undefined;
  if (field === "marketId") return isHex(v) && v.length === 66 ? (v.toLowerCase() as Hex) : null;
  return isAddress(v, { strict: false }) ? getAddress(v) : null;
}

/** Five distinct addresses, sorted ascending (the Registry's order); see `parsePin` for the rest. */
export function parseSigners(raw: string | undefined): Address[] | null | undefined {
  const v = raw?.trim();
  if (!v) return undefined;
  const parts = v.split(",").map((s) => s.trim());
  if (parts.length !== SIGNER_COUNT || !parts.every((p) => isAddress(p, { strict: false }))) return null;
  // Same-length lowercase hex: code-point order is numeric order, as the Registry sorts signers.
  const key = (a: string) => a.toLowerCase();
  const signers = parts.map((p) => getAddress(p)).sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0));
  return new Set(signers.map((s) => s.toLowerCase())).size === SIGNER_COUNT ? signers : null;
}

/** Every value the backend reports for each pin, labelled for messages. */
function backendReports(r: RemoteConfig): Record<PinField, { value: Hex; what: string }[]> {
  const out: Record<PinField, { value: Hex; what: string }[]> = {
    registry: [{ value: r.registry, what: "signer network's Registry" }],
    adapter: [{ value: r.adapter, what: "signer network's Settlement Adapter" }],
    morpho: [],
    marketId: [],
    usdc: [],
    oracle: [],
    zzec: [],
    irm: [],
  };
  const { contracts: c, market: m } = r;
  if (m) {
    out.morpho.push({ value: m.morphoBlue, what: "Morpho Blue" });
    out.marketId.push({ value: m.marketId, what: "market id" });
    out.usdc.push({ value: m.loanToken, what: "market's loan token" });
    out.oracle.push({ value: m.oracle, what: "market's oracle" });
    out.zzec.push({ value: m.collateralToken, what: "market's collateral" });
    out.irm.push({ value: m.irm, what: "market's IRM" });
  }
  if (c) {
    out.registry.push({ value: c.ZecuredRegistry, what: "deployed ZecuredRegistry" });
    out.adapter.push({ value: c.SettlementAdapter, what: "deployed SettlementAdapter" });
    out.usdc.push({ value: c.LoanToken, what: "deployed loan token" });
    out.oracle.push({ value: c.MarketOracle, what: "deployed market oracle" });
    out.zzec.push({ value: c.ZZEC, what: "ZZEC" });
  }
  return out;
}

/**
 * Builds the config from the build's environment (pins, `NODE_ENV`) and, once fetched, the
 * backend's `/config` (exported for tests). See the module comment for the rules.
 */
export function buildConfig(env: ConfigEnv, remote?: RemoteConfigResult): AppConfig {
  const development = env.NODE_ENV === "development";
  const problems: string[] = [];

  const pins: Partial<Record<PinField, Hex>> = {};
  const malformed = new Set<PinField>();
  for (const field of FIELDS) {
    const value = parsePin(field, env[ENV_VAR[field]]);
    if (value === null) {
      malformed.add(field);
      problems.push(`${ENV_VAR[field]} is set but is not a valid ${field === "marketId" ? "market id" : "address"}.`);
    } else if (value !== undefined) {
      pins[field] = value;
    }
  }
  const signersPin = parseSigners(env.NEXT_PUBLIC_SIGNERS);
  if (signersPin === null) problems.push(`NEXT_PUBLIC_SIGNERS is set but is not ${SIGNER_COUNT} distinct addresses separated by commas.`);

  let reports: Partial<Record<PinField, { value: Hex; what: string }[]>> = {};
  let backendSigners: Address[] | null = null;
  let sponsorship = false;
  let denominations: readonly bigint[] = DENOMINATIONS;
  let confirmations: number | null = null;
  let vaultExpiryBlocks: number | null = null;
  let status: AppConfig["remote"] = "pending";
  let remoteError: string | null = null;
  if (remote && "error" in remote) {
    status = "unavailable";
    remoteError = remote.error;
  } else if (remote) {
    status = "loaded";
    // Values without authority: they only shape the UI (the signers enforce their own).
    if (remote.denominations.length > 0) denominations = remote.denominations;
    confirmations = remote.confirmations;
    vaultExpiryBlocks = remote.vaultExpiryBlocks;
    if (remote.chainId !== CHAIN_ID) {
      problems.push(`The backend is configured for chain ${remote.chainId}, this app for Base Sepolia (${CHAIN_ID}).`);
    } else {
      reports = backendReports(remote);
      backendSigners = remote.signers;
      sponsorship = remote.sponsorship;
      const m = remote.market;
      if (m && m.lltv !== MARKET_LLTV) {
        problems.push(`The backend's market has an LLTV of ${Number((m.lltv * 1_000n) / 10n ** 18n) / 10} %; Zecured uses 62.5 %.`);
      }
      if (m && !same(marketIdOf({ loanToken: m.loanToken, collateralToken: m.collateralToken, oracle: m.oracle, irm: m.irm, lltv: m.lltv }), m.marketId)) {
        problems.push("The backend's market id does not match its market parameters.");
      }
    }
  }

  // The pins decide; the backend may only agree (or, in development, fill a gap).
  const v = {} as Record<PinField, Hex>;
  for (const field of FIELDS) {
    const pinned = pins[field] ?? BUILT_IN[field];
    const reported = reports[field] ?? [];
    if (pinned !== undefined) {
      v[field] = pinned;
      const other = reported.find((r) => !same(r.value, pinned));
      if (other) {
        problems.push(`This build pins ${ENV_VAR[field]} to ${abbreviate(pinned)}, but the backend's ${other.what} is ${abbreviate(other.value)}.`);
      }
    } else if (development && !malformed.has(field) && reported[0]) {
      const first = reported[0];
      v[field] = first.value;
      const other = reported.find((r) => !same(r.value, first.value));
      if (other) {
        problems.push(`The backend is inconsistent: its ${first.what} is ${abbreviate(first.value)}, its ${other.what} ${abbreviate(other.value)}.`);
      }
    } else {
      v[field] = UNSET[field];
    }
  }
  let signers: readonly Address[] | null = null;
  if (signersPin) {
    signers = signersPin;
    if (backendSigners && !backendSigners.every((s, i) => same(s, signersPin[i]!))) {
      problems.push("This build pins NEXT_PUBLIC_SIGNERS to a different signer set than the backend's.");
    }
  } else if (signersPin === undefined && development) {
    signers = backendSigners;
  }

  const unpinned = REQUIRED.filter((f) => pins[f] === undefined && !malformed.has(f)).map((f) => ENV_VAR[f]);
  if (signersPin === undefined) unpinned.push("NEXT_PUBLIC_SIGNERS");
  const placeholders: Hex[] = [PLACEHOLDER_CONTRACT, ZERO_ADDRESS];
  const complete = ![v.registry, v.adapter, v.usdc, v.oracle, v.zzec].some((a) => placeholders.includes(a)) && v.marketId !== ZERO_BYTES32;
  if (complete && problems.length === 0) {
    const id = marketIdOf({ loanToken: v.usdc, collateralToken: v.zzec, oracle: v.oracle, irm: v.irm, lltv: MARKET_LLTV });
    if (!same(id, v.marketId)) problems.push(MARKET_MISMATCH);
  }
  return {
    backendUrl: url(env.NEXT_PUBLIC_BACKEND_URL, "http://127.0.0.1:8080"),
    registry: v.registry,
    adapter: v.adapter,
    morpho: v.morpho,
    marketId: v.marketId,
    usdc: v.usdc,
    oracle: v.oracle,
    zzec: v.zzec,
    irm: v.irm,
    lltv: MARKET_LLTV,
    signers,
    signersPinned: Boolean(signersPin),
    development,
    unpinned,
    contractsDeployed: problems.length === 0 && complete && (development || unpinned.length === 0),
    ownerSigning: problems.length === 0 && (development || pins.registry !== undefined),
    sponsorship,
    denominations,
    confirmations,
    vaultExpiryBlocks,
    remote: status,
    remoteError,
    problems,
  };
}

/** The build's environment (literal names: Next.js inlines them). */
const BUILD_ENV: ConfigEnv = {
  NODE_ENV: process.env.NODE_ENV,
  NEXT_PUBLIC_BACKEND_URL: process.env.NEXT_PUBLIC_BACKEND_URL,
  NEXT_PUBLIC_REGISTRY: process.env.NEXT_PUBLIC_REGISTRY,
  NEXT_PUBLIC_ADAPTER: process.env.NEXT_PUBLIC_ADAPTER,
  NEXT_PUBLIC_MORPHO: process.env.NEXT_PUBLIC_MORPHO,
  NEXT_PUBLIC_MARKET_ID: process.env.NEXT_PUBLIC_MARKET_ID,
  NEXT_PUBLIC_USDC: process.env.NEXT_PUBLIC_USDC,
  NEXT_PUBLIC_ORACLE: process.env.NEXT_PUBLIC_ORACLE,
  NEXT_PUBLIC_ZZEC: process.env.NEXT_PUBLIC_ZZEC,
  NEXT_PUBLIC_IRM: process.env.NEXT_PUBLIC_IRM,
  NEXT_PUBLIC_SIGNERS: process.env.NEXT_PUBLIC_SIGNERS,
};

/** The app's configuration. Mutated in place by `loadRuntimeConfig()`; read it at call time. */
export const config: AppConfig = buildConfig(BUILD_ENV);

/**
 * Returns a loader that fetches the backend's configuration once (the promise is memoized, so
 * every caller shares one request) and applies it to `target` in place, checked against the
 * build's pins. It never rejects: on failure, or after `timeoutMs`, the build-time
 * configuration stays and is marked `remote: "unavailable"`. lib/api.ts binds it to the
 * backend client.
 */
export function runtimeConfigLoader(
  fetchRemote: () => Promise<RemoteConfig>,
  { target = config, env = BUILD_ENV, timeoutMs = RUNTIME_CONFIG_TIMEOUT_MS }: { target?: AppConfig; env?: ConfigEnv; timeoutMs?: number } = {},
): () => Promise<AppConfig> {
  let loading: Promise<AppConfig> | null = null;
  return () =>
    (loading ??= (async () => {
      let remote: RemoteConfigResult;
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        remote = await Promise.race([
          fetchRemote(),
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error(`no answer within ${timeoutMs / 1000} s`)), timeoutMs);
          }),
        ]);
      } catch (e) {
        remote = { error: e instanceof Error && e.message ? e.message : "unreachable" };
      } finally {
        clearTimeout(timer);
      }
      return Object.assign(target, buildConfig(env, remote));
    })());
}

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

/** Human-readable configuration problems and warnings (shown by components/ConfigGate.tsx). */
export function configIssues(c: AppConfig = config): string[] {
  const issues = [...c.problems];
  if (c.problems.length > 0) {
    issues.push(
      "Owner signatures, Base reads and sponsored transactions are disabled until this build's pinned addresses and the backend agree.",
    );
  }
  if (c.remote === "unavailable") {
    issues.push(
      `The backend's configuration (GET /config) could not be loaded (${c.remoteError ?? "unknown error"}). Sponsored transactions are disabled until it answers.`,
    );
  }
  if (c.problems.length > 0) return issues;
  if (c.unpinned.length > 0) {
    const them = c.unpinned.length > 1 ? "them" : "it";
    issues.push(
      c.development
        ? `Unpinned development configuration: this build does not pin ${list(c.unpinned)}, so the app takes ${them} from the backend's GET /config. A production build must pin ${them}.`
        : `This build does not pin ${list(c.unpinned)}. ${c.ownerSigning ? "Base reads and sponsored transactions" : "Owner signatures, Base reads and sponsored transactions"} stay disabled until the app is rebuilt with ${them} (see app/.env.example).`,
    );
  }
  if (!c.contractsDeployed) {
    if (c.development || c.unpinned.length === 0) {
      issues.push(
        c.remote === "loaded" && c.unpinned.length > 0
          ? "The backend reports no Base deployment yet. Off-chain steps work; Base reads and sponsored transactions are disabled."
          : "Base contracts are not configured yet (placeholder addresses). Off-chain steps work; Base reads and sponsored transactions are disabled.",
      );
    }
  } else if (!same(marketIdOf(marketParams(c)), c.marketId)) {
    issues.push(MARKET_MISMATCH);
  } else if (c.remote === "loaded" && !c.sponsorship) {
    issues.push(
      "Sponsored transactions are unavailable: the backend has no bundler and paymaster configured. Borrowing, repaying and exits are disabled; off-chain steps and Base reads work.",
    );
  }
  return issues;
}
