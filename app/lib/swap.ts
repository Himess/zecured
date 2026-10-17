/**
 * Zecured Swap, the Buy direction (the v4 design's section 5): the user locks USDC in ZecuredSwap
 * with a maker's signed quote and the commitment `C = keccak256(salt || receiver)` of an Ironwood
 * address; the maker pays that address on Zcash with the memo "ZSWP" || orderId and proves the
 * payment on Base in zero knowledge; the USDC goes to the maker only then, or back to the user after
 * the pay deadline. Base never sees the receiver or the salt: the app makes both locally, keeps them
 * in this browser, and hands them to the maker off-chain (relayed by our backend) so it can pay.
 *
 * The user's own wallet sends `open` and `refund` (lib/wallet.ts); reads go through our proxy.
 */
import {
  encodeAbiParameters,
  encodeEventTopics,
  encodeFunctionData,
  getAddress,
  keccak256,
  parseEventLogs,
  zeroAddress,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { erc20Abi } from "./abi";
import { swapAbi } from "./abi4";
import { SWAP_SIDE, type MakerOrderStatus, type SignedSwapQuote } from "./api";
import type { KeyValueStorage } from "./keys";

/** ZecuredSwap's order id: `keccak256(abi.encode(block.chainid, address(this), maker, nonce))` (V4 freeze section 5). */
export function orderIdOf(chainId: number, swap: Address, maker: Address, nonce: bigint): Hex {
  return keccak256(
    encodeAbiParameters(
      [{ type: "uint256" }, { type: "address" }, { type: "address" }, { type: "uint256" }],
      [BigInt(chainId), swap, maker, nonce],
    ),
  );
}

/** Why the app would not open an order with `q` (null: it may). */
export function quoteProblem(q: SignedSwapQuote, ctx: { swap: Address | null; taker: Address | null; nowSeconds: number }): string | null {
  if (!ctx.swap) return "This deployment has no swap contract pinned in this build.";
  if (getAddress(q.swap) !== getAddress(ctx.swap)) return "The quote is for another swap contract than this build's: it is not used.";
  if (q.quote.side !== SWAP_SIDE.buy) return "This is not a quote to buy ZEC.";
  if (q.quote.taker !== zeroAddress && ctx.taker && getAddress(q.quote.taker) !== getAddress(ctx.taker)) {
    return "The quote is reserved for another address. Ask for a new one.";
  }
  if (q.quote.quoteDeadline <= BigInt(ctx.nowSeconds)) return "The quote expired. Ask for a new one.";
  if (q.quote.payWindow === 0) return "The quote gives the maker no time to pay.";
  return null;
}

/** The user's wallet's calls: approve the exact USDC when the allowance falls short, then `open`. */
export function buyCalls(p: {
  swap: Address;
  usdcToken: Address;
  quote: SignedSwapQuote;
  commitment: Hex;
  allowance: bigint;
}): { to: Address; data: Hex }[] {
  const out: { to: Address; data: Hex }[] = [];
  if (p.allowance < p.quote.quote.usdc) {
    out.push({ to: p.usdcToken, data: encodeFunctionData({ abi: erc20Abi, functionName: "approve", args: [p.swap, p.quote.quote.usdc] }) });
  }
  out.push({
    to: p.swap,
    data: encodeFunctionData({ abi: swapAbi, functionName: "open", args: [p.quote.quote, p.quote.signature, p.commitment] }),
  });
  return out;
}

export function refundCall(swap: Address, orderId: Hex): { to: Address; data: Hex } {
  return { to: swap, data: encodeFunctionData({ abi: swapAbi, functionName: "refund", args: [orderId] }) };
}

// ------------------------------------------------------------------ orders kept in this browser

export const SWAPS_KEY = "zecured:swaps:v1";

/** An order this browser opened, with what only it knows: the receiver and the salt behind its commitment. */
export interface StoredSwapOrder {
  v: 1;
  orderId: Hex;
  swap: Address;
  chainId: number;
  maker: Address;
  /** The user's wallet that opened it (and gets the USDC back on a refund). */
  taker: Address;
  /** Zatoshi to receive and USDC units locked, decimal strings. */
  zatoshi: string;
  usdc: string;
  /** Unix seconds: after it, without a proof, the USDC can come back. */
  payDeadline: number;
  /** Where to look for the order's events (decimal block number), and how far they were read. */
  openedBlock: string;
  scannedTo: string;
  openTx: Hex;
  /** PRIVATE, never logged or sent anywhere but to the maker: the Ironwood address and the salt behind `commitment`. */
  receiver: string;
  salt: Hex;
  commitment: Hex;
  destinationSent: boolean;
  /** What the order's events showed so far. */
  proven: boolean;
  settled: boolean;
  refunded: boolean;
  createdAt: number;
}

function isHexOf(v: unknown, bytes: number): v is Hex {
  return typeof v === "string" && new RegExp(`^0x[0-9a-fA-F]{${bytes * 2}}$`).test(v);
}

/** Validates a stored order (an untrusted record): null when it is not one. */
export function parseSwapOrder(raw: unknown): StoredSwapOrder | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const dec = (v: unknown) => typeof v === "string" && /^\d+$/.test(v);
  const addr = (v: unknown) => typeof v === "string" && /^0x[0-9a-fA-F]{40}$/.test(v);
  if (!isHexOf(r.orderId, 32) || !isHexOf(r.salt, 32) || !isHexOf(r.commitment, 32) || !isHexOf(r.openTx, 32)) return null;
  if (!addr(r.swap) || !addr(r.maker) || !addr(r.taker)) return null;
  if (!dec(r.zatoshi) || !dec(r.usdc) || !dec(r.openedBlock) || !dec(r.scannedTo)) return null;
  if (typeof r.receiver !== "string" || !r.receiver || typeof r.chainId !== "number" || typeof r.payDeadline !== "number") return null;
  return {
    v: 1,
    orderId: r.orderId.toLowerCase() as Hex,
    swap: getAddress(r.swap as string),
    chainId: r.chainId,
    maker: getAddress(r.maker as string),
    taker: getAddress(r.taker as string),
    zatoshi: r.zatoshi as string,
    usdc: r.usdc as string,
    payDeadline: r.payDeadline,
    openedBlock: r.openedBlock as string,
    scannedTo: r.scannedTo as string,
    openTx: r.openTx.toLowerCase() as Hex,
    receiver: r.receiver,
    salt: r.salt.toLowerCase() as Hex,
    commitment: r.commitment.toLowerCase() as Hex,
    destinationSent: r.destinationSent === true,
    proven: r.proven === true,
    settled: r.settled === true,
    refunded: r.refunded === true,
    createdAt: typeof r.createdAt === "number" ? r.createdAt : 0,
  };
}

/** The orders this browser keeps; every access is failure-tolerant, like the loan store. */
export class SwapStore {
  constructor(private readonly storage: KeyValueStorage | null) {}

  get available(): boolean {
    return this.storage !== null;
  }

  list(): StoredSwapOrder[] {
    try {
      const raw = this.storage?.getItem(SWAPS_KEY);
      const all = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
      return Object.values(all ?? {})
        .map(parseSwapOrder)
        .filter((o): o is StoredSwapOrder => o !== null)
        .sort((a, b) => b.createdAt - a.createdAt);
    } catch {
      return [];
    }
  }

  save(order: StoredSwapOrder): boolean {
    if (!this.storage) return false;
    try {
      const all = Object.fromEntries(this.list().map((o) => [o.orderId, o]));
      all[order.orderId] = order;
      this.storage.setItem(SWAPS_KEY, JSON.stringify(all));
      return true;
    } catch {
      return false;
    }
  }

  remove(orderId: Hex): boolean {
    if (!this.storage) return false;
    try {
      const all = Object.fromEntries(this.list().filter((o) => o.orderId !== orderId.toLowerCase()).map((o) => [o.orderId, o]));
      this.storage.setItem(SWAPS_KEY, JSON.stringify(all));
      return true;
    } catch {
      return false;
    }
  }
}

// ------------------------------------------------------------------ status

export type SwapStatus =
  /** Locked; the maker has until `payDeadline` to pay and prove. */
  | { kind: "open"; payDeadline: number }
  /** The maker says it paid on Zcash; its proof is not on Base yet. */
  | { kind: "paid"; payDeadline: number }
  /** The proof of the payment is on Base: the USDC goes to the maker once the Zcash header is deep enough. */
  | { kind: "proven" }
  | { kind: "settled" }
  /** The deadline passed without a proof: the USDC can come back to the user's wallet. */
  | { kind: "refundable" }
  | { kind: "refunded" };

/** The order's state, from its events on Base first, the maker's word only for "paid", and the time. */
export function swapStatus(o: Pick<StoredSwapOrder, "proven" | "settled" | "refunded" | "payDeadline">, maker: MakerOrderStatus | null, nowSeconds: number): SwapStatus {
  if (o.refunded) return { kind: "refunded" };
  if (o.settled) return { kind: "settled" };
  if (o.proven) return { kind: "proven" };
  if (nowSeconds > o.payDeadline) return { kind: "refundable" };
  if (maker === "paid" || maker === "proving") return { kind: "paid", payDeadline: o.payDeadline };
  return { kind: "open", payDeadline: o.payDeadline };
}

/** Steps of the order as the page lists them: open → paid → proven → settled. */
export const SWAP_STEPS = ["open", "paid", "proven", "settled"] as const;

// ------------------------------------------------------------------ events

const ORDER_EVENTS = swapAbi.filter((i) => i.type === "event" && i.name !== "OrderOpened");

/** Blocks per eth_getLogs (the proxy's bound). */
const LOG_SPAN = 2_000n;

/**
 * Reads the order's events after `o.scannedTo` up to the latest block (2,000 blocks per request,
 * at most `maxChunks`), through the proxy, and returns the order with its flags and cursor updated.
 * A pending settlement later dropped is not a proof.
 */
export async function followOrder(c: PublicClient, o: StoredSwapOrder, maxChunks = 12): Promise<StoredSwapOrder> {
  const latest = await c.getBlockNumber();
  let from = BigInt(o.scannedTo) + 1n;
  if (from < BigInt(o.openedBlock)) from = BigInt(o.openedBlock);
  const next = { ...o };
  const topics = [
    ORDER_EVENTS.map((e) => encodeEventTopics({ abi: [e], eventName: e.name } as never)[0] as Hex),
    o.orderId,
  ];
  for (let i = 0; i < maxChunks && from <= latest; i++) {
    const to = from + LOG_SPAN - 1n < latest ? from + LOG_SPAN - 1n : latest;
    const logs = (await c.request({
      method: "eth_getLogs",
      params: [{ address: o.swap, topics, fromBlock: `0x${from.toString(16)}`, toBlock: `0x${to.toString(16)}` }],
    } as never)) as Parameters<typeof parseEventLogs>[0]["logs"];
    for (const log of parseEventLogs({ abi: ORDER_EVENTS, logs })) {
      if (log.eventName === "SettlementPending") next.proven = true;
      if (log.eventName === "SettlementDropped") next.proven = false;
      if (log.eventName === "OrderSettled") next.settled = true;
      if (log.eventName === "OrderRefunded") next.refunded = true;
    }
    next.scannedTo = to.toString();
    from = to + 1n;
  }
  return next;
}

/** The `OrderOpened` event of the open transaction: the order id and its pay deadline. */
export function openedOrder(
  receiptLogs: Parameters<typeof parseEventLogs>[0]["logs"],
  swap: Address,
): { orderId: Hex; payDeadline: number } | null {
  const ours = receiptLogs.filter((l) => getAddress(l.address) === getAddress(swap));
  const opened = parseEventLogs({ abi: swapAbi, eventName: "OrderOpened", logs: ours })[0];
  return opened ? { orderId: opened.args.orderId.toLowerCase() as Hex, payDeadline: Number(opened.args.payDeadline) } : null;
}
