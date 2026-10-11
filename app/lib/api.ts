/**
 * Typed client for the coordinator API (docs/INTERFACES.md § 4).
 *
 * Wire format: camelCase JSON, `0x` hex, amounts / nonces / deadlines as decimal strings.
 * In the app, amounts are bigint (zatoshi or USDC units). Responses are validated and
 * normalized, and tolerate the documented variants (claims carry `paid: bool` or a `status`;
 * older vault views lack `address`, `outstanding` and the heights and may carry
 * `confirmations` / `deposit`; older drain reports are snake_case). Nothing here logs request
 * bodies: they contain addresses and salts.
 */
import { isAddress, isHex, getAddress, type Address, type Hex } from "viem";
import {
  config,
  parseSigners,
  RUNTIME_CONFIG_TIMEOUT_MS,
  runtimeConfigLoader,
  type AppConfig,
  type RemoteConfig,
  type RemoteContracts,
  type RemoteMarket,
} from "./config";
import type { CloseRequestMessage } from "./eip712";

// ------------------------------------------------------------------ types

export const VAULT_STATUSES = ["awaiting_deposit", "deposit_seen", "active", "closing", "closed", "expired"] as const;
export type VaultStatus = (typeof VAULT_STATUSES)[number];

export const CLAIM_KINDS = ["Release", "Liquidation", "CloseUser", "CloseMarketMaker"] as const;
export type ClaimKind = (typeof CLAIM_KINDS)[number];
export type ClaimStatus = "pending" | "paid";

export interface Claim {
  claimId: Hex;
  kind: ClaimKind | "Unknown";
  /** zatoshi */
  amount: bigint;
  status: ClaimStatus;
}

export interface NonConformingNote {
  /** Opaque id (RefundRequest.noteId). */
  noteId: Hex;
  /** zatoshi */
  amount: bigint;
}

export interface Vault {
  vaultId: Hex;
  status: VaultStatus | "unknown";
  /** Collateral denomination, zatoshi. */
  amount: bigint;
  /** Last Zcash height at which a deposit is accepted. */
  expiryHeight: number;
  nonConforming: NonConformingNote[];
  claims: Claim[];
  // In INTERFACES.md § 4; optional because older backends omit them:
  /** The vault's Ironwood-only unified address. */
  address?: string;
  /** zatoshi still locked (the Registry's `outstanding`, as the coordinator last saw it). */
  outstanding?: bigint;
  /** A payout transaction is being confirmed on Zcash. */
  payoutInFlight?: boolean;
  /** Zcash tip when the vault was opened: where a viewing-key scan of the vault starts. */
  birthdayHeight?: number;
  /** Zcash tip at the coordinator's last pass (lets the app show blocks remaining). */
  tipHeight?: number;
  // Earlier INTERFACES.md variant:
  confirmations?: number;
  deposit?: { attested: boolean; txHash?: Hex };
}

export interface OpenVaultBody {
  owner: Address;
  account: Address;
  amount: bigint;
  requestId: Hex;
  deadline: bigint;
  signature: Hex;
}

/** One signer node's signature, by node index. */
export interface VaultSignature {
  index: number;
  /** 65 bytes, r || s || v. */
  signature: Hex;
}

export interface OpenVaultResult {
  vaultId: Hex;
  /** The vault's Ironwood-only unified address. */
  address: string;
  zip321: string;
  amount: bigint;
  expiryHeight: number;
  /** keccak256 of the vault's UFVK, as the signers signed it (VaultOpened). */
  ufvkHash: Hex;
  /** Every signer's VaultOpened signature (docs/INTERFACES.md § 2.5): unchecked as parsed here. */
  signatures: VaultSignature[];
}

export interface DestinationBody {
  receiver: string;
  salt: Hex;
  nonce: bigint;
  deadline: bigint;
  signature: Hex;
}

export interface RefundBody extends DestinationBody {
  noteId: Hex;
}

export interface ViewKeyBody {
  nonce: bigint;
  deadline: bigint;
  signature: Hex;
}

export interface CloseQuote {
  executor: Address;
  marginBps: number;
  /** USDC units the market maker repays. */
  repayAssets: bigint;
  /** zatoshi to the market maker. */
  mmAmount: bigint;
  /** zatoshi to the borrower. */
  userAmount: bigint;
  /** Unix seconds. */
  expiresAt: number;
}

export type NodeMode = "normal" | "compromised" | "offline";
export interface NodeHealth {
  index: number;
  mode: NodeMode | "unreachable";
  vaults?: number;
  /** The demo panel may switch this node to "compromised" (DEMO_COMPROMISABLE_NODES). */
  compromisable?: boolean;
}

export interface DrainVote {
  index: number;
  signed: boolean;
  reason?: string;
}

export interface DrainReport {
  vaultId: Hex;
  amount: bigint;
  votes: DrainVote[];
  thresholdReached: boolean;
}

/** `GET /admin/oracle`: the MOCK oracle's price. */
export interface OraclePrice {
  /** USD cents per ZEC (rounded down). */
  priceUsdCents: bigint;
  /** `MockZecOracle.price()`: USD per ZEC × 1e34. */
  raw: bigint;
}

/** `POST /admin/oracle`: the price set and the owner's `setPrice` transaction. */
export interface OracleUpdate {
  priceUsdCents: bigint;
  txHash: Hex;
}

export type ApiErrorKind = "unreachable" | "timeout" | "http" | "invalid";

export class ApiError extends Error {
  constructor(
    readonly kind: ApiErrorKind,
    message: string,
    readonly status?: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** A short, user-facing description of any error thrown by this module (or anything else). */
export function describeError(e: unknown): string {
  if (e instanceof ApiError) {
    switch (e.kind) {
      case "unreachable":
        return `${e.message}. Is the backend running?`;
      case "timeout":
        return "The backend did not answer in time. Try again in a moment.";
      default:
        return e.message;
    }
  }
  if (e instanceof Error) {
    const short = (e as { shortMessage?: unknown }).shortMessage;
    return typeof short === "string" && short ? short : e.message;
  }
  return "Unexpected error";
}

// ------------------------------------------------------------------ parsing helpers

type Json = Record<string, unknown>;

function invalid(what: string): ApiError {
  return new ApiError("invalid", `Unexpected backend response (${what})`);
}

function obj(v: unknown, what: string): Json {
  if (!v || typeof v !== "object" || Array.isArray(v)) throw invalid(what);
  return v as Json;
}

function big(v: unknown, what: string): bigint {
  if (typeof v === "string" && /^\d+$/.test(v)) return BigInt(v);
  if (typeof v === "number" && Number.isSafeInteger(v) && v >= 0) return BigInt(v);
  throw invalid(what);
}

function int(v: unknown, what: string): number {
  if (typeof v === "number" && Number.isSafeInteger(v)) return v;
  if (typeof v === "string" && /^\d+$/.test(v) && Number.isSafeInteger(Number(v))) return Number(v);
  throw invalid(what);
}

/** A non-negative integer (heights, counts). */
function uint(v: unknown, what: string): number {
  const n = int(v, what);
  if (n < 0) throw invalid(what);
  return n;
}

function hex32(v: unknown, what: string): Hex {
  if (typeof v === "string" && isHex(v) && v.length === 66) return v.toLowerCase() as Hex;
  throw invalid(what);
}

function addr(v: unknown, what: string): Address {
  if (typeof v === "string" && isAddress(v, { strict: false })) return getAddress(v);
  throw invalid(what);
}

function str(v: unknown, what: string): string {
  if (typeof v === "string" && v.length > 0) return v;
  throw invalid(what);
}

function optional<T>(v: unknown, parse: (x: unknown) => T): T | undefined {
  return v === undefined || v === null ? undefined : parse(v);
}

function claimKind(v: unknown): ClaimKind | "Unknown" {
  if (typeof v !== "string") return "Unknown";
  const k = v.replace(/[_\s-]/g, "").toLowerCase();
  return CLAIM_KINDS.find((c) => c.toLowerCase() === k) ?? "Unknown";
}

export function parseVault(raw: unknown): Vault {
  const r = obj(raw, "vault");
  const status = typeof r.status === "string" && (VAULT_STATUSES as readonly string[]).includes(r.status)
    ? (r.status as VaultStatus)
    : "unknown";
  const claimsRaw = Array.isArray(r.claims) ? r.claims : [];
  const notesRaw = Array.isArray(r.nonConforming) ? r.nonConforming : [];
  const vault: Vault = {
    vaultId: hex32(r.vaultId, "vaultId"),
    status,
    amount: big(r.amount, "amount"),
    expiryHeight: int(r.expiryHeight, "expiryHeight"),
    nonConforming: notesRaw.map((n) => {
      const o = obj(n, "nonConforming");
      return { noteId: hex32(o.noteId, "noteId"), amount: big(o.amount, "note amount") };
    }),
    claims: claimsRaw.map((c) => {
      const o = obj(c, "claim");
      const status: ClaimStatus =
        typeof o.status === "string"
          ? o.status.toLowerCase() === "paid"
            ? "paid"
            : "pending"
          : o.paid === true
            ? "paid"
            : "pending";
      return { claimId: hex32(o.claimId, "claimId"), kind: claimKind(o.kind), amount: big(o.amount, "claim amount"), status };
    }),
  };
  const confirmations = optional(r.confirmations, (x) => int(x, "confirmations"));
  if (confirmations !== undefined) vault.confirmations = confirmations;
  if (r.deposit && typeof r.deposit === "object") {
    const d = r.deposit as Json;
    vault.deposit = { attested: d.attested === true };
    if (typeof d.txHash === "string" && isHex(d.txHash)) vault.deposit.txHash = d.txHash as Hex;
  }
  if (typeof r.address === "string" && r.address) vault.address = r.address;
  const outstanding = optional(r.outstanding, (x) => big(x, "outstanding"));
  if (outstanding !== undefined) vault.outstanding = outstanding;
  if (typeof r.payoutInFlight === "boolean") vault.payoutInFlight = r.payoutInFlight;
  // 0 means "not known yet" (the coordinator reports tip 0 before its first pass).
  const birthday = optional(r.birthdayHeight, (x) => uint(x, "birthdayHeight"));
  if (birthday) vault.birthdayHeight = birthday;
  const tip = optional(r.tipHeight, (x) => uint(x, "tipHeight"));
  if (tip) vault.tipHeight = tip;
  return vault;
}

/**
 * `GET /config`: strict. Its addresses are only checked against the build's pins (or, in a
 * development build, fill unpinned ones; lib/config.ts).
 */
export function parseRemoteConfig(raw: unknown): RemoteConfig {
  const r = obj(raw, "config");
  const chainId = int(r.chainId, "config.chainId");
  if (chainId <= 0) throw invalid("config.chainId");
  const confirmations = uint(r.confirmations, "config.confirmations");
  const vaultExpiryBlocks = uint(r.vaultExpiryBlocks, "config.vaultExpiryBlocks");
  if (vaultExpiryBlocks === 0) throw invalid("config.vaultExpiryBlocks");
  if (typeof r.sponsorship !== "boolean") throw invalid("config.sponsorship");
  if (!Array.isArray(r.denominations) || r.denominations.length === 0) throw invalid("config.denominations");
  const denominations = [...new Set(r.denominations.map((d) => big(d, "config.denominations")))].sort((a, b) =>
    a < b ? -1 : a > b ? 1 : 0,
  );
  if (denominations[0] === 0n) throw invalid("config.denominations");
  let contracts: RemoteContracts | null = null;
  if (r.contracts !== null && r.contracts !== undefined) {
    const c = obj(r.contracts, "config.contracts");
    contracts = {
      MockUSDC: addr(c.MockUSDC, "config.contracts.MockUSDC"),
      MockZecOracle: addr(c.MockZecOracle, "config.contracts.MockZecOracle"),
      SettlementAdapter: addr(c.SettlementAdapter, "config.contracts.SettlementAdapter"),
      ZZEC: addr(c.ZZEC, "config.contracts.ZZEC"),
      ZecuredRegistry: addr(c.ZecuredRegistry, "config.contracts.ZecuredRegistry"),
    };
  }
  let market: RemoteMarket | null = null;
  if (r.market !== null && r.market !== undefined) {
    const m = obj(r.market, "config.market");
    market = {
      collateralToken: addr(m.collateralToken, "config.market.collateralToken"),
      irm: addr(m.irm, "config.market.irm"),
      lltv: big(m.lltv, "config.market.lltv"),
      loanToken: addr(m.loanToken, "config.market.loanToken"),
      marketId: hex32(m.marketId, "config.market.marketId"),
      morphoBlue: addr(m.morphoBlue, "config.market.morphoBlue"),
      oracle: addr(m.oracle, "config.market.oracle"),
    };
  }
  let signers: Address[] | null = null;
  if (r.signers !== null && r.signers !== undefined) {
    const list = Array.isArray(r.signers) ? r.signers.map((s) => addr(s, "config.signers")) : [];
    signers = parseSigners(list.join(",")) ?? null;
    if (!signers) throw invalid("config.signers");
  }
  return {
    chainId,
    registry: addr(r.registry, "config.registry"),
    adapter: addr(r.adapter, "config.adapter"),
    contracts,
    market,
    signers,
    denominations,
    confirmations,
    vaultExpiryBlocks,
    sponsorship: r.sponsorship,
  };
}

export function parseOraclePrice(raw: unknown): OraclePrice {
  const r = obj(raw, "oracle");
  return { priceUsdCents: big(r.priceUsdCents, "priceUsdCents"), raw: big(r.raw, "raw") };
}

export function parseOracleUpdate(raw: unknown): OracleUpdate {
  const r = obj(raw, "oracle update");
  return { priceUsdCents: big(r.priceUsdCents, "priceUsdCents"), txHash: hex32(r.txHash, "txHash") };
}

/** A 65-byte signature, lowercase. */
function sig65(v: unknown, what: string): Hex {
  if (typeof v === "string" && isHex(v) && v.length === 2 + 65 * 2) return v.toLowerCase() as Hex;
  throw invalid(what);
}

export function parseOpenVault(raw: unknown): OpenVaultResult {
  const r = obj(raw, "open vault");
  if (!Array.isArray(r.signatures)) throw invalid("signatures");
  const expiryHeight = uint(r.expiryHeight, "expiryHeight");
  if (expiryHeight > 0xffff_ffff) throw invalid("expiryHeight");
  return {
    vaultId: hex32(r.vaultId, "vaultId"),
    address: str(r.address, "address"),
    zip321: str(r.zip321, "zip321"),
    amount: big(r.amount, "amount"),
    expiryHeight,
    ufvkHash: hex32(r.ufvkHash, "ufvkHash"),
    signatures: r.signatures.map((s) => {
      const o = obj(s, "signature");
      return { index: uint(o.index, "signature index"), signature: sig65(o.signature, "signature") };
    }),
  };
}

export function parseCloseQuote(raw: unknown): CloseQuote {
  const r = obj(raw, "close quote");
  const marginBps = int(r.marginBps, "marginBps");
  if (marginBps < 0 || marginBps > 65_535) throw invalid("marginBps");
  return {
    executor: addr(r.executor, "executor"),
    marginBps,
    repayAssets: big(r.repayAssets, "repayAssets"),
    mmAmount: big(r.mmAmount, "mmAmount"),
    userAmount: big(r.userAmount, "userAmount"),
    expiresAt: int(r.expiresAt, "expiresAt"),
  };
}

export function parseDrainReport(raw: unknown): DrainReport {
  const r = obj(raw, "drain report");
  const votes = Array.isArray(r.votes) ? r.votes : [];
  return {
    vaultId: hex32(r.vaultId ?? r.vault_id, "vaultId"),
    amount: big(r.amount, "amount"),
    thresholdReached: (r.thresholdReached ?? r.threshold_reached) === true,
    votes: votes.map((v) => {
      const o = obj(v, "vote");
      const vote: DrainVote = { index: int(o.index, "vote index"), signed: o.signed === true };
      if (typeof o.reason === "string" && o.reason) vote.reason = o.reason;
      return vote;
    }),
  };
}

export function parseNodes(raw: unknown): NodeHealth[] {
  const r = obj(raw, "nodes");
  const list = Array.isArray(r.nodes) ? r.nodes : [];
  return list.map((n) => {
    const o = obj(n, "node");
    const mode = typeof o.mode === "string" && ["normal", "compromised", "offline", "unreachable"].includes(o.mode)
      ? (o.mode as NodeHealth["mode"])
      : "unreachable";
    const node: NodeHealth = { index: int(o.index, "node index"), mode };
    if (typeof o.vaults === "number") node.vaults = o.vaults;
    if (typeof o.compromisable === "boolean") node.compromisable = o.compromisable;
    return node;
  });
}

// ------------------------------------------------------------------ client

export interface ApiOptions {
  baseUrl?: string;
  fetch?: typeof fetch;
}

/** Timeouts: opening a vault runs a 5-node DKG; the sweep demo builds and proves a PCZT. */
const TIMEOUT = { short: 15_000, owner: 60_000, open: 120_000, prove: 240_000 } as const;

function segment(id: string): string {
  if (!isHex(id) || id.length !== 66) throw new ApiError("invalid", "Not a vault id (expected 0x + 64 hex digits)");
  return encodeURIComponent(id.toLowerCase());
}

export function createApi(options: ApiOptions = {}) {
  const base = (options.baseUrl ?? config.backendUrl).replace(/\/+$/, "");
  const doFetch = options.fetch ?? ((...args: Parameters<typeof fetch>) => globalThis.fetch(...args));

  async function call(
    method: "GET" | "POST",
    path: string,
    body?: unknown,
    opts: { timeoutMs?: number; headers?: Record<string, string> } = {},
  ): Promise<unknown> {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), opts.timeoutMs ?? TIMEOUT.short);
    let res: Response;
    try {
      const headers: Record<string, string> = { accept: "application/json", ...opts.headers };
      if (body !== undefined) headers["content-type"] = "application/json";
      res = await doFetch(`${base}${path}`, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: ctrl.signal,
        cache: "no-store",
        credentials: "omit",
        referrerPolicy: "no-referrer",
      });
    } catch (e) {
      if (ctrl.signal.aborted) throw new ApiError("timeout", "The backend did not answer in time");
      throw new ApiError("unreachable", `Cannot reach the backend at ${base}`);
    } finally {
      clearTimeout(timer);
    }
    let json: unknown = null;
    try {
      json = await res.json();
    } catch {
      json = null;
    }
    if (!res.ok) {
      const err = json && typeof json === "object" ? (json as Json).error : undefined;
      throw new ApiError("http", typeof err === "string" && err ? err : `Backend error (HTTP ${res.status})`, res.status);
    }
    return json;
  }

  return {
    baseUrl: base,

    /** JSON-RPC endpoints of our proxy (the only RPC / bundler / paymaster the browser uses). */
    proxyUrl(kind: "rpc" | "bundler" | "paymaster"): string {
      return `${base}/proxy/${kind}`;
    },

    async health(): Promise<boolean> {
      const r = await call("GET", "/health", undefined, { timeoutMs: 5_000 });
      return Boolean(r && typeof r === "object" && (r as Json).ok === true);
    },

    /** GET /config — the backend's deployment (checked against the pins), denominations, sponsorship (no auth). */
    async getConfig(): Promise<RemoteConfig> {
      return parseRemoteConfig(await call("GET", "/config", undefined, { timeoutMs: RUNTIME_CONFIG_TIMEOUT_MS }));
    },

    /** POST /vaults — open a vault off-chain (OpenVault signed by the owner key). */
    async openVault(b: OpenVaultBody): Promise<OpenVaultResult> {
      const r = await call(
        "POST",
        "/vaults",
        {
          owner: b.owner,
          account: b.account,
          amount: b.amount.toString(),
          requestId: b.requestId,
          deadline: b.deadline.toString(),
          signature: b.signature,
        },
        { timeoutMs: TIMEOUT.open },
      );
      return parseOpenVault(r);
    },

    /** GET /vaults/:id */
    async getVault(vaultId: Hex): Promise<Vault> {
      return parseVault(await call("GET", `/vaults/${segment(vaultId)}`));
    },

    /** POST /vaults/:id/destination — pre-validates a payout destination; returns its commitment. */
    async postDestination(vaultId: Hex, b: DestinationBody): Promise<Hex> {
      const r = obj(
        await call(
          "POST",
          `/vaults/${segment(vaultId)}/destination`,
          {
            receiver: b.receiver,
            salt: b.salt,
            nonce: b.nonce.toString(),
            deadline: b.deadline.toString(),
            signature: b.signature,
          },
          { timeoutMs: TIMEOUT.owner },
        ),
        "destination",
      );
      return hex32(r.commitment, "commitment");
    },

    /** POST /vaults/:id/refund — P5 refund of a non-conforming note. */
    async postRefund(vaultId: Hex, b: RefundBody): Promise<boolean> {
      const r = obj(
        await call(
          "POST",
          `/vaults/${segment(vaultId)}/refund`,
          {
            noteId: b.noteId,
            receiver: b.receiver,
            salt: b.salt,
            nonce: b.nonce.toString(),
            deadline: b.deadline.toString(),
            signature: b.signature,
          },
          { timeoutMs: TIMEOUT.owner },
        ),
        "refund",
      );
      return r.accepted === true;
    },

    /** POST /vaults/:id/viewing-key — the vault's UFVK, for the owner only. */
    async postViewingKey(vaultId: Hex, b: ViewKeyBody): Promise<string> {
      const r = obj(
        await call(
          "POST",
          `/vaults/${segment(vaultId)}/viewing-key`,
          { nonce: b.nonce.toString(), deadline: b.deadline.toString(), signature: b.signature },
          { timeoutMs: TIMEOUT.owner },
        ),
        "viewing key",
      );
      return str(r.ufvk, "ufvk");
    },

    /** POST /recipients — third-party recipients (liquidator / market maker); unused by the app. */
    async postRecipient(receiver: string, salt: Hex): Promise<Hex> {
      const r = obj(await call("POST", "/recipients", { receiver, salt }, { timeoutMs: TIMEOUT.owner }), "recipient");
      return hex32(r.commitment, "commitment");
    },

    /** GET /quotes/close/:vaultId — a market maker's quote to close from collateral. */
    async getCloseQuote(vaultId: Hex): Promise<CloseQuote> {
      return parseCloseQuote(await call("GET", `/quotes/close/${segment(vaultId)}`));
    },

    /**
     * POST /closes — hands the signed CloseRequest to the market maker (relayed by the
     * backend); the market maker executes `closeWithCollateral` and returns its transaction.
     */
    async submitCloseRequest(req: CloseRequestMessage, signature: Hex): Promise<Hex | null> {
      const r = obj(
        await call(
          "POST",
          "/closes",
          {
            request: {
              vaultId: req.vaultId,
              account: req.account,
              executor: req.executor,
              marginBps: req.marginBps,
              minUserAmount: req.minUserAmount.toString(),
              userCommitment: req.userCommitment,
              nonce: req.nonce.toString(),
              deadline: req.deadline.toString(),
            },
            signature,
          },
          { timeoutMs: TIMEOUT.owner },
        ),
        "close request",
      );
      return typeof r.txHash === "string" && isHex(r.txHash) ? (r.txHash as Hex) : null;
    },

    // -------------------------------------------------------------- demo control (x-admin-token)

    async adminNodes(token: string): Promise<NodeHealth[]> {
      return parseNodes(await call("GET", "/admin/nodes", undefined, { headers: { "x-admin-token": token } }));
    },

    async adminSetMode(token: string, index: number, mode: NodeMode): Promise<void> {
      if (!Number.isInteger(index) || index < 0) throw new ApiError("invalid", "bad node index");
      await call("POST", `/admin/nodes/${index}/mode`, { mode }, { headers: { "x-admin-token": token } });
    },

    async adminDrain(token: string, vaultId: Hex, to: string): Promise<DrainReport> {
      return parseDrainReport(
        await call(
          "POST",
          `/admin/vaults/${segment(vaultId)}/drain`,
          { to },
          { headers: { "x-admin-token": token }, timeoutMs: TIMEOUT.prove },
        ),
      );
    },

    /** GET /admin/oracle — the MOCK oracle's current price. */
    async adminGetOracle(token: string): Promise<OraclePrice> {
      return parseOraclePrice(await call("GET", "/admin/oracle", undefined, { headers: { "x-admin-token": token } }));
    },

    /**
     * POST /admin/oracle — sets the MOCK oracle's price (the backend holds the owner key and
     * waits for the transaction). The backend reads `priceUsdCents` as a JSON number.
     */
    async adminSetOracle(token: string, priceUsdCents: bigint): Promise<OracleUpdate> {
      if (priceUsdCents <= 0n || priceUsdCents > BigInt(Number.MAX_SAFE_INTEGER)) {
        throw new ApiError("invalid", "The price must be a positive number of cents");
      }
      return parseOracleUpdate(
        await call(
          "POST",
          "/admin/oracle",
          { priceUsdCents: Number(priceUsdCents) },
          { headers: { "x-admin-token": token }, timeoutMs: TIMEOUT.owner },
        ),
      );
    },
  };
}

export type Api = ReturnType<typeof createApi>;

/** The app's client, bound to NEXT_PUBLIC_BACKEND_URL. */
export const api: Api = createApi();

/**
 * Fetches GET /config once (memoized), checks it against the build's pinned addresses and
 * applies the result to `config` in place (lib/config.ts). components/ConfigGate.tsx awaits it
 * before rendering any page; it never rejects.
 */
export const loadRuntimeConfig: () => Promise<AppConfig> = runtimeConfigLoader(() => api.getConfig());
