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
import { CLAIM_KIND, MINT_BLOCK } from "./abi";
import {
  config,
  parseSigners,
  RUNTIME_CONFIG_TIMEOUT_MS,
  runtimeConfigLoader,
  type AppConfig,
  type RemoteConfig,
  type RemoteContracts,
  type RemoteMarket,
  type V3Params,
} from "./config";
import type { CloseRequestMessage } from "./eip712";
import { parseEvidence, type Evidence } from "./evidence";
import { STAMP_MAX_BITS } from "./stamp";

// ------------------------------------------------------------------ types

export const VAULT_STATUSES = ["awaiting_deposit", "deposit_seen", "active", "closing", "closed", "expired", "cancelled"] as const;
export type VaultStatus = (typeof VAULT_STATUSES)[number];

/** The Registry's claim kinds (v3: Refund, Fee and Retired too). */
export const CLAIM_KINDS = CLAIM_KIND;
export type ClaimKind = (typeof CLAIM_KINDS)[number];
/** As the coordinator reports it (a `paid` flag, or a status). */
export type ClaimStatus = "pending" | "paid" | "writtenOff" | "compensated" | "disputed";

export interface Claim {
  claimId: Hex;
  kind: ClaimKind | "Unknown";
  /** zatoshi */
  amount: bigint;
  status: ClaimStatus;
}

/** A payment to the vault that cannot back the loan, as the signers saw it on Zcash. */
export interface NonConformingNote {
  /** Opaque id. */
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
  /** v3 refunds them only as Refund claims on Base, once proven (empty when not reported). */
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
  /** Nonce of the request's hashcash stamp (review H-01; lib/stamp.ts). */
  stamp: bigint;
  /** The mainnet beta's invite code (refused without one there: `invite_required`). */
  invite?: string;
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
  /** keccak256 of the vault's tag key (derived from its UFVK), as the signers signed it (VaultOpened v2). */
  tagKeyHash: Hex;
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

export type MintBlock = (typeof MINT_BLOCK)[number];

/**
 * `GET /status` `minting` (design notes T-01, T-03): the Intake's and the bond's minting gates, read
 * by the backend at most every 15 s. None of them ever blocks an exit.
 */
export interface MintingStatus {
  /** `mintingPaused()`: by signatures until `pausedUntil`, or by an open theft case. */
  paused: boolean;
  /** Unix seconds; 0: not paused by signatures. */
  pausedUntil: number;
  /** When the running pause by signatures started (Unix seconds). */
  pausedSince: number;
  /** A slash halted minting for good. */
  halted: boolean;
  /** Every signer holds at least the minimum bond. */
  fullyBonded: boolean;
  /** Zatoshi the coverage and absolute caps still admit (0 while the cap's price is stale). */
  capRoom: bigint;
  /** Per vault denomination (zatoshi): what stops a proven deposit of it now; null while the cap's price is stale. */
  mintBlock: Map<bigint, MintBlock | null>;
  /** When the backend read it (Unix seconds); null if it does not say. */
  readAt: number | null;
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
    /** The backend's fixed refusal code (`{ error, code }`, L-2), when it gave one. */
    readonly code?: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** The backend's refusal code of `e`, if `e` is one of its answers. */
export function apiErrorCode(e: unknown): string | null {
  return e instanceof ApiError && e.code ? e.code : null;
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

/** A claim's status: a `status` string ("paid", "written_off", "compensated", "disputed"), else the `paid` flag. */
function claimStatus(o: Json): ClaimStatus {
  if (typeof o.status === "string") {
    const s = o.status.replace(/[_\s-]/g, "").toLowerCase();
    if (s === "paid") return "paid";
    if (s === "writtenoff") return "writtenOff";
    if (s === "compensated") return "compensated";
    if (s === "disputed") return "disputed";
    return "pending";
  }
  return o.paid === true ? "paid" : "pending";
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
      return {
        claimId: hex32(o.claimId, "claimId"),
        kind: claimKind(o.kind),
        amount: big(o.amount, "claim amount"),
        status: claimStatus(o),
      };
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
  // H-01: absent from backends that predate the stamp.
  const openVaultStampBits = optional(r.openVaultStampBits, (x) => uint(x, "config.openVaultStampBits")) ?? null;
  if (openVaultStampBits !== null && openVaultStampBits > STAMP_MAX_BITS) throw invalid("config.openVaultStampBits");
  if (typeof r.sponsorship !== "boolean") throw invalid("config.sponsorship");
  if (!Array.isArray(r.denominations) || r.denominations.length === 0) throw invalid("config.denominations");
  const denominations = [...new Set(r.denominations.map((d) => big(d, "config.denominations")))].sort((a, b) =>
    a < b ? -1 : a > b ? 1 : 0,
  );
  if (denominations[0] === 0n) throw invalid("config.denominations");
  let contracts: RemoteContracts | null = null;
  if (r.contracts !== null && r.contracts !== undefined) {
    const c = obj(r.contracts, "config.contracts");
    // RQ-06: LoanToken and the market's oracle (MarketOracle; the DemoShockOracle, MOCK / DEMO, on
    // testnets); deployments made before them name only the DemoShockOracle, or only the mocks.
    const loanToken = addr(c.LoanToken ?? c.MockUSDC, "config.contracts.LoanToken");
    const marketOracle = addr(c.MarketOracle ?? c.DemoShockOracle ?? c.MockZecOracle, "config.contracts.MarketOracle");
    const named = (k: string) => optional(c[k], (x) => addr(x, `config.contracts.${k}`)) ?? null;
    const mockUsdc = named("MockUSDC");
    const demoOracle = named("DemoShockOracle");
    contracts = {
      LoanToken: loanToken,
      MarketOracle: marketOracle,
      SettlementAdapter: addr(c.SettlementAdapter, "config.contracts.SettlementAdapter"),
      ZZEC: addr(c.ZZEC, "config.contracts.ZZEC"),
      ZecuredRegistry: addr(c.ZecuredRegistry, "config.contracts.ZecuredRegistry"),
      ZecuredIntake: named("ZecuredIntake"),
      ZecuredSwap: named("ZecuredSwap"),
      // Section 9.3 of the v4 design: what the UI calls the token and the price comes from the deployment file.
      loanTokenIsMock: mockUsdc !== null && mockUsdc === loanToken,
      oracleIsDemo: demoOracle !== null && demoOracle === marketOracle,
      baseOracleIsMock: named("MockZecOracle") !== null,
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
    // v3; absent or null from a backend without a v3 deployment.
    intake: optional(r.intake, (x) => addr(x, "config.intake")) ?? null,
    v3: optional(r.v3, parseV3Params) ?? null,
    contracts,
    market,
    signers,
    denominations,
    confirmations,
    vaultExpiryBlocks,
    openVaultStampBits,
    sponsorship: r.sponsorship,
  };
}

/** `GET /config` `v3`: every field, as JSON numbers or decimal strings. */
function parseV3Params(raw: unknown): V3Params {
  const v = obj(raw, "config.v3");
  const n = (k: string) => uint(v[k], `config.v3.${k}`);
  const z = (k: string) => big(v[k], `config.v3.${k}`);
  const feeBps = n("feeBps");
  if (feeBps > 10_000) throw invalid("config.v3.feeBps");
  return {
    feeBps,
    minFeeZat: z("minFeeZat"),
    minRefundZat: z("minRefundZat"),
    refundAbandonPeriod: n("refundAbandonPeriod"),
    pendingRefundDelay: n("pendingRefundDelay"),
    maxPause: n("maxPause"),
    livenessPeriod: n("livenessPeriod"),
    commitGrace: n("commitGrace"),
    livenessPremiumBps: n("livenessPremiumBps"),
    coverageBps: n("coverageBps"),
    maxTotalZat: z("maxTotalZat"),
    incidentWindow: n("incidentWindow"),
  };
}

/**
 * `GET /status` `minting`, or null when the backend has none (no v3 deployment, the chain
 * unreadable, or a backend from before v3).
 */
export function parseMintingStatus(raw: unknown): MintingStatus | null {
  const r = obj(raw, "status");
  if (r.minting === null || r.minting === undefined) return null;
  const m = obj(r.minting, "status.minting");
  const flag = (k: string) => {
    if (typeof m[k] !== "boolean") throw invalid(`status.minting.${k}`);
    return m[k] as boolean;
  };
  const blocks = new Map<bigint, MintBlock | null>();
  for (const [amount, block] of Object.entries(obj(m.mintBlock, "status.minting.mintBlock"))) {
    if (!/^\d+$/.test(amount)) throw invalid("status.minting.mintBlock");
    if (block !== null && !(MINT_BLOCK as readonly unknown[]).includes(block)) throw invalid("status.minting.mintBlock");
    blocks.set(BigInt(amount), block as MintBlock | null);
  }
  return {
    paused: flag("paused"),
    pausedUntil: uint(m.pausedUntil, "status.minting.pausedUntil"),
    pausedSince: uint(m.pausedSince, "status.minting.pausedSince"),
    halted: flag("halted"),
    fullyBonded: flag("fullyBonded"),
    capRoom: big(m.capRoom, "status.minting.capRoom"),
    mintBlock: blocks,
    readAt: optional(m.readAt, (x) => uint(x, "status.minting.readAt")) ?? null,
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
    tagKeyHash: hex32(r.tagKeyHash, "tagKeyHash"),
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

// ------------------------------------------------------------------ v4

/**
 * R-5: a signer's receipt for a claim's destination (`POST /claims/:claimId/receipt`, served by
 * the coordinator from the first node that holds and validated the destination's preimage):
 * `DestinationReceived(claimId, commitment, receivedAt)` signed under the bond's EIP-712 domain.
 */
export interface LivenessReceipt {
  claimId: Hex;
  commitment: Hex;
  /** When the node received and validated the destination (Unix seconds). */
  receivedAt: bigint;
  /** The node's attestation address (one of `Registry.signers()`). */
  signer: Address;
  signature: Hex;
}

export function parseLivenessReceipt(raw: unknown): LivenessReceipt {
  const r = obj(raw, "receipt");
  const receivedAt = big(r.receivedAt, "receipt.receivedAt");
  if (receivedAt >= 1n << 64n) throw invalid("receipt.receivedAt");
  return {
    claimId: hex32(r.claimId, "receipt.claimId"),
    commitment: hex32(r.commitment, "receipt.commitment"),
    receivedAt,
    signer: addr(r.signer, "receipt.signer"),
    signature: sig65(r.signature, "receipt.signature"),
  };
}

/** R-11: `POST /vaults/:id/prove-refund`, signed by the vault's owner key (lib/owner.ts). */
export interface ProveRefundBody {
  nonce: bigint;
  deadline: bigint;
  signature: Hex;
}

/** P-7 (S18): a market maker's quote for repaying part of the debt from collateral. */
export interface DeleverageQuote {
  executor: Address;
  marginBps: number;
  /** Loan-token units the market maker repays (exactly the amount asked for). */
  repayAssets: bigint;
  /** zatoshi of collateral the market maker takes for it. */
  mmAmount: bigint;
  /** zatoshi left in the vault after it. */
  remaining: bigint;
  /** Unix seconds. */
  expiresAt: number;
}

export function parseDeleverageQuote(raw: unknown): DeleverageQuote {
  const r = obj(raw, "deleverage quote");
  const marginBps = int(r.marginBps, "marginBps");
  if (marginBps < 0 || marginBps > 65_535) throw invalid("marginBps");
  return {
    executor: addr(r.executor, "executor"),
    marginBps,
    repayAssets: big(r.repayAssets, "repayAssets"),
    mmAmount: big(r.mmAmount, "mmAmount"),
    remaining: big(r.remaining, "remaining"),
    expiresAt: int(r.expiresAt, "expiresAt"),
  };
}

/** The DeleverageRequest the account signs (docs/INTERFACES.md § 2.4), as `POST /deleverages` carries it. */
export interface DeleverageRequestBody {
  vaultId: Hex;
  account: Address;
  executor: Address;
  marginBps: number;
  repayAssets: bigint;
  maxCollateral: bigint;
  nonce: bigint;
  deadline: bigint;
}

/** `SwapQuote.side`: the taker buys ZEC with USDC (on-ramp) or sells ZEC for USDC. */
export const SWAP_SIDE = { buy: 0, sell: 1 } as const;

/** The maker's signed EIP-712 `SwapQuote` (the v4 design's section 5.4). */
export interface SwapQuote {
  maker: Address;
  /** The zero address: anyone may take it; otherwise only this address. */
  taker: Address;
  side: number;
  zatoshi: bigint;
  /** USDC units (6 decimals). */
  usdc: bigint;
  makerReceiverCommitment: Hex;
  /** Seconds the maker has, after `open`, to pay and prove. */
  payWindow: number;
  /** The quote is good until (Unix seconds). */
  quoteDeadline: bigint;
  nonce: bigint;
}

export interface SignedSwapQuote {
  quote: SwapQuote;
  signature: Hex;
  /** The swap contract the quote is for: must be this build's pin. */
  swap: Address;
}

export function parseSignedSwapQuote(raw: unknown): SignedSwapQuote {
  const r = obj(raw, "swap quote");
  const q = obj(r.quote, "swap quote.quote");
  const side = uint(q.side, "quote.side");
  if (side > 1) throw invalid("quote.side");
  const payWindow = uint(q.payWindow, "quote.payWindow");
  if (payWindow > 0xffff_ffff) throw invalid("quote.payWindow");
  const zatoshi = big(q.zatoshi, "quote.zatoshi");
  if (zatoshi >= 1n << 64n || zatoshi === 0n) throw invalid("quote.zatoshi");
  const usdc = big(q.usdc, "quote.usdc");
  if (usdc === 0n) throw invalid("quote.usdc");
  const signature = r.signature;
  if (typeof signature !== "string" || !isHex(signature) || signature.length < 4) throw invalid("quote.signature");
  return {
    quote: {
      maker: addr(q.maker, "quote.maker"),
      taker: addr(q.taker, "quote.taker"),
      side,
      zatoshi,
      usdc,
      makerReceiverCommitment: hex32(q.makerReceiverCommitment, "quote.makerReceiverCommitment"),
      payWindow,
      quoteDeadline: big(q.quoteDeadline, "quote.quoteDeadline"),
      nonce: big(q.nonce, "quote.nonce"),
    },
    signature: signature.toLowerCase() as Hex,
    swap: addr(r.swap, "quote.swap"),
  };
}

/** What the maker says about an order it serves (its own view; the contract's events decide). */
export const MAKER_ORDER_STATUSES = [
  "awaiting_destination",
  "destination_received",
  "paying",
  "paid",
  "proving",
  "proven",
  "settled",
  "refunded",
] as const;
export type MakerOrderStatus = (typeof MAKER_ORDER_STATUSES)[number] | "unknown";

export function parseMakerOrder(raw: unknown): MakerOrderStatus {
  const r = obj(raw, "swap order");
  return typeof r.status === "string" && (MAKER_ORDER_STATUSES as readonly string[]).includes(r.status)
    ? (r.status as MakerOrderStatus)
    : "unknown";
}

/** P-1: `GET /demo/status`, the demo wallet as the judge deployment's backend runs it (DEMO). */
export interface DemoStatus {
  enabled: boolean;
  /** The demo wallet's Ironwood-only address: where a demo vault's exits go back when the user gives none. */
  returnAddress: string | null;
  dailyCapZat: bigint;
  paidTodayZat: bigint;
  /** The demo wallet runs low: the operators are alerted; new demo payments may fail. */
  lowBalance: boolean;
  /** Zero bits of the demo payment's stamp (lib/stamp.ts). */
  stampBits: number;
}

export function parseDemoStatus(raw: unknown): DemoStatus {
  const r = obj(raw, "demo status");
  if (r.enabled !== true) {
    return { enabled: false, returnAddress: null, dailyCapZat: 0n, paidTodayZat: 0n, lowBalance: false, stampBits: 0 };
  }
  const stampBits = uint(r.stampBits, "demo.stampBits");
  if (stampBits > STAMP_MAX_BITS) throw invalid("demo.stampBits");
  return {
    enabled: true,
    returnAddress: typeof r.returnAddress === "string" && r.returnAddress ? r.returnAddress : null,
    dailyCapZat: big(r.dailyCapZat, "demo.dailyCapZat"),
    paidTodayZat: big(r.paidTodayZat, "demo.paidTodayZat"),
    lowBalance: r.lowBalance === true,
    stampBits,
  };
}

/** P-1: `POST /demo/vaults/:id/pay`: the vault as all five signers vouched for it (C-4), and the stamp. */
export interface DemoPayBody {
  owner: Address;
  account: Address;
  amount: bigint;
  expiryHeight: number;
  address: string;
  tagKeyHash: Hex;
  signatures: VaultSignature[];
  stamp: bigint;
}

export interface DemoPayResult {
  /** The demo payment's Zcash transaction id (display order). */
  txid: string;
  amount: bigint;
}

export function parseDemoPay(raw: unknown): DemoPayResult {
  const r = obj(raw, "demo payment");
  const txid = str(r.txid, "demo.txid");
  if (!/^[0-9a-f]{64}$/.test(txid)) throw invalid("demo.txid");
  return { txid, amount: big(r.amount, "demo.amount") };
}

/** A verifier version as `GET /status` lists it (T-07a; v4 adds the Swap kind). */
export interface VersionView {
  versionId: Hex;
  selector: Hex;
  imageId: Hex;
  branches: number[];
  activeAt: number;
  active: boolean;
  removed: boolean;
}

/** `GET /status` in full: what the status page shows (P-2). Nothing about any vault or user. */
export interface StatusReport {
  commit: string;
  treeClean: boolean;
  startedAt: number;
  zcashTip: number;
  nodes: { index: number; mode: NodeHealth["mode"] }[];
  depositProofs: { queued: number; running: number; concurrency: number } | null;
  minting: MintingStatus | null;
  versions: Record<string, VersionView[]> | null;
  /** The deployment file's view (`deployment`): chain, contracts and the initial image ids. */
  deployment: {
    chainId: number | null;
    registry: Address | null;
    intake: Address | null;
    signerBond: Address | null;
    mintMode: string | null;
    verifierIsMock: boolean | null;
    depositImageId: Hex | null;
    violationImageId: Hex | null;
  } | null;
}

function optAddr(v: unknown): Address | null {
  return typeof v === "string" && isAddress(v, { strict: false }) ? getAddress(v) : null;
}

function optHex32(v: unknown): Hex | null {
  return typeof v === "string" && isHex(v) && v.length === 66 ? (v.toLowerCase() as Hex) : null;
}

export function parseStatusReport(raw: unknown): StatusReport {
  const r = obj(raw, "status");
  const nodes = (Array.isArray(r.nodes) ? r.nodes : []).map((n) => {
    const o = obj(n, "status.node");
    const mode = typeof o.mode === "string" && ["normal", "compromised", "offline", "unreachable"].includes(o.mode)
      ? (o.mode as NodeHealth["mode"])
      : "unreachable";
    return { index: uint(o.index, "status.node.index"), mode };
  });
  let depositProofs: StatusReport["depositProofs"] = null;
  if (r.depositProofs && typeof r.depositProofs === "object") {
    const d = obj(r.depositProofs, "status.depositProofs");
    depositProofs = {
      queued: uint(d.queued, "depositProofs.queued"),
      running: uint(d.running, "depositProofs.running"),
      concurrency: uint(d.concurrency, "depositProofs.concurrency"),
    };
  }
  let versions: StatusReport["versions"] = null;
  if (r.versions && typeof r.versions === "object" && !Array.isArray(r.versions)) {
    versions = {};
    for (const [kind, list] of Object.entries(r.versions as Json)) {
      if (!Array.isArray(list)) continue;
      versions[kind] = list.map((v) => {
        const o = obj(v, "status.version");
        const selector = o.selector;
        if (typeof selector !== "string" || !isHex(selector) || selector.length !== 10) throw invalid("version.selector");
        return {
          versionId: hex32(o.versionId, "version.versionId"),
          selector: selector.toLowerCase() as Hex,
          imageId: hex32(o.imageId, "version.imageId"),
          branches: Array.isArray(o.branches) ? o.branches.map((b) => uint(b, "version.branches")) : [],
          activeAt: uint(o.activeAt, "version.activeAt"),
          active: o.active === true,
          removed: o.removed === true,
        };
      });
    }
  }
  let deployment: StatusReport["deployment"] = null;
  if (r.deployment && typeof r.deployment === "object") {
    const d = r.deployment as Json;
    deployment = {
      chainId: typeof d.chainId === "number" ? d.chainId : null,
      registry: optAddr(d.registry),
      intake: optAddr(d.intake),
      signerBond: optAddr(d.signerBond),
      mintMode: typeof d.mintMode === "string" ? d.mintMode : null,
      verifierIsMock: typeof d.verifierIsMock === "boolean" ? d.verifierIsMock : null,
      depositImageId: optHex32(d.depositImageId),
      violationImageId: optHex32(d.violationImageId),
    };
  }
  return {
    commit: typeof r.commit === "string" ? r.commit : "unknown",
    treeClean: r.treeClean === true,
    startedAt: typeof r.startedAt === "number" ? r.startedAt : 0,
    zcashTip: typeof r.zcashTip === "number" ? r.zcashTip : 0,
    nodes,
    depositProofs,
    minting: parseMintingStatus(raw),
    versions,
    deployment,
  };
}

/** P-2: `GET /stats/vaults`: open vaults per denomination on Base (the anonymity sets). */
export interface VaultStats {
  /** zatoshi → open vaults of that deposit size. */
  byAmount: Map<bigint, number>;
  total: number;
  /** The scan reached the latest block (false while it catches up). */
  complete: boolean;
  readAt: number | null;
}

export function parseVaultStats(raw: unknown): VaultStats {
  const r = obj(raw, "vault stats");
  const byAmount = new Map<bigint, number>();
  for (const [amount, count] of Object.entries(obj(r.byAmount, "stats.byAmount"))) {
    if (!/^\d+$/.test(amount)) throw invalid("stats.byAmount");
    byAmount.set(BigInt(amount), uint(count, "stats.byAmount"));
  }
  return {
    byAmount,
    total: uint(r.total, "stats.total"),
    complete: r.complete === true,
    readAt: optional(r.readAt, (x) => uint(x, "stats.readAt")) ?? null,
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
      const code = json && typeof json === "object" ? (json as Json).code : undefined;
      throw new ApiError(
        "http",
        typeof err === "string" && err ? err : `Backend error (HTTP ${res.status})`,
        res.status,
        typeof code === "string" && /^[a-z_]{1,64}$/.test(code) ? code : undefined,
      );
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

    /** GET /status — v3's minting gates (public state, nothing about any vault); null when the backend has none. */
    async getMintingStatus(): Promise<MintingStatus | null> {
      return parseMintingStatus(await call("GET", "/status"));
    },

    /** POST /vaults — open a vault off-chain (OpenVault signed by the owner key, with its hashcash stamp). */
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
          stamp: b.stamp.toString(),
          ...(b.invite ? { invite: b.invite } : {}),
        },
        { timeoutMs: TIMEOUT.open },
      );
      return parseOpenVault(r);
    },

    // -------------------------------------------------------------- v4

    /**
     * R-5: POST /claims/:claimId/receipt — the first valid `DestinationReceived` receipt a signer
     * gives for the claim's destination; null while none has one (`404 no_receipt`).
     */
    async getLivenessReceipt(claimId: Hex): Promise<LivenessReceipt | null> {
      try {
        return parseLivenessReceipt(await call("POST", `/claims/${segment(claimId)}/receipt`, {}, { timeoutMs: TIMEOUT.owner }));
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },

    /**
     * R-11: POST /vaults/:id/prove-refund — the coordinator proves the vault's deposit as a refund
     * (Refund mode) instead of waiting for room under the coverage cap. Owner-signed.
     */
    async proveRefund(vaultId: Hex, b: ProveRefundBody): Promise<void> {
      const r = obj(
        await call(
          "POST",
          `/vaults/${segment(vaultId)}/prove-refund`,
          { nonce: b.nonce.toString(), deadline: b.deadline.toString(), signature: b.signature },
          { timeoutMs: TIMEOUT.owner },
        ),
        "prove refund",
      );
      if (r.queued !== true) throw invalid("prove refund");
    },

    /** P-7 (S18): GET /quotes/deleverage/:vaultId?repay= — repay `repay` loan-token units from collateral. */
    async getDeleverageQuote(vaultId: Hex, repay: bigint): Promise<DeleverageQuote> {
      if (repay <= 0n) throw new ApiError("invalid", "The repayment must be positive");
      return parseDeleverageQuote(await call("GET", `/quotes/deleverage/${segment(vaultId)}?repay=${repay.toString()}`));
    },

    /** P-7: POST /deleverages — hands the signed DeleverageRequest to the market maker, who executes it. */
    async submitDeleverage(req: DeleverageRequestBody, signature: Hex): Promise<Hex | null> {
      const r = obj(
        await call(
          "POST",
          "/deleverages",
          {
            request: {
              vaultId: req.vaultId,
              account: req.account,
              executor: req.executor,
              marginBps: req.marginBps,
              repayAssets: req.repayAssets.toString(),
              maxCollateral: req.maxCollateral.toString(),
              nonce: req.nonce.toString(),
              deadline: req.deadline.toString(),
            },
            signature,
          },
          { timeoutMs: TIMEOUT.owner },
        ),
        "deleverage request",
      );
      return typeof r.txHash === "string" && isHex(r.txHash) ? (r.txHash as Hex) : null;
    },

    /**
     * Swap: GET /swap/quote — the maker's signed quote, relayed by the backend (the browser never
     * contacts the maker). `usdc` (units to spend) or `zatoshi` (ZEC to buy); `taker` binds the
     * quote to that address so nobody else can take it first.
     */
    async getSwapQuote(p: { usdc?: bigint; zatoshi?: bigint; taker?: Address }): Promise<SignedSwapQuote> {
      const q = new URLSearchParams({ side: "buy" });
      if (p.usdc !== undefined) q.set("usdc", p.usdc.toString());
      if (p.zatoshi !== undefined) q.set("zatoshi", p.zatoshi.toString());
      if (p.taker) q.set("taker", getAddress(p.taker));
      return parseSignedSwapQuote(await call("GET", `/swap/quote?${q.toString()}`));
    },

    /**
     * Swap (section 5.5): POST /swap/orders/:orderId/destination — the receiver and the salt behind
     * the order's commitment, for the maker only (relayed, never logged); Base only has the commitment.
     */
    async postSwapDestination(orderId: Hex, receiver: string, salt: Hex): Promise<void> {
      await call("POST", `/swap/orders/${segment(orderId)}/destination`, { receiver, salt }, { timeoutMs: TIMEOUT.owner });
    },

    /** Swap: GET /swap/orders/:orderId — what the maker says it did (paying, paid, proving). */
    async getSwapOrder(orderId: Hex): Promise<MakerOrderStatus> {
      return parseMakerOrder(await call("GET", `/swap/orders/${segment(orderId)}`));
    },

    /** P-1: GET /demo/status (DEMO). */
    async getDemoStatus(): Promise<DemoStatus> {
      return parseDemoStatus(await call("GET", "/demo/status"));
    },

    /** P-1: POST /demo/vaults/:id/pay — the demo wallet pays the vault its full amount, once (DEMO). */
    async demoPay(vaultId: Hex, b: DemoPayBody): Promise<DemoPayResult> {
      return parseDemoPay(
        await call(
          "POST",
          `/demo/vaults/${segment(vaultId)}/pay`,
          {
            owner: b.owner,
            account: b.account,
            amount: b.amount.toString(),
            expiryHeight: b.expiryHeight,
            address: b.address,
            tagKeyHash: b.tagKeyHash,
            signatures: b.signatures.map((s) => ({ index: s.index, signature: s.signature })),
            stamp: b.stamp.toString(),
          },
          { timeoutMs: TIMEOUT.prove },
        ),
      );
    },

    /** P-2: GET /status in full. */
    async getStatus(): Promise<StatusReport> {
      return parseStatusReport(await call("GET", "/status"));
    },

    /** P-2: GET /stats/vaults; null when the backend has none. */
    async getVaultStats(): Promise<VaultStats | null> {
      try {
        return parseVaultStats(await call("GET", "/stats/vaults"));
      } catch (e) {
        if (e instanceof ApiError && (e.status === 404 || e.status === 503)) return null;
        throw e;
      }
    },

    /** P-3: GET /evidence; null when the backend serves none. */
    async getEvidence(): Promise<Evidence | null> {
      try {
        return parseEvidence(await call("GET", "/evidence"));
      } catch (e) {
        if (e instanceof ApiError && (e.status === 404 || e.status === 503)) return null;
        throw e;
      }
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
