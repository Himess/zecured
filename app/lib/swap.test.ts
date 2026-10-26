import { describe, expect, it } from "vitest";
import {
  concat,
  createPublicClient,
  custom,
  decodeFunctionData,
  encodeAbiParameters,
  encodeEventTopics,
  hashTypedData,
  keccak256,
  numberToHex,
  pad,
  zeroAddress,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";
import { erc20Abi } from "./abi";
import { swapAbi } from "./abi4";
import type { SignedSwapQuote } from "./api";
import type { KeyValueStorage } from "./keys";
import {
  buyCalls,
  followOrder,
  openedOrder,
  orderIdOf,
  parseSwapOrder,
  quoteProblem,
  quoteSignedByMaker,
  refundCall,
  swapQuoteTypedData,
  SwapStore,
  swapStatus,
  type StoredSwapOrder,
} from "./swap";

const SWAP: Address = "0x7777777777777777777777777777777777777777";
const USDC: Address = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const MAKER: Address = "0x6666666666666666666666666666666666666666";
const ME: Address = "0x3333333333333333333333333333333333333333";
const C: Hex = `0x${"cc".repeat(32)}`;
const NOW = 1_800_000_000;

const QUOTE: SignedSwapQuote = {
  quote: {
    maker: MAKER,
    taker: zeroAddress,
    side: 0,
    zatoshi: 10_000_000n,
    usdc: 12_000_000n,
    makerReceiverCommitment: `0x${"00".repeat(32)}`,
    payWindow: 21_600,
    quoteDeadline: BigInt(NOW + 300),
    nonce: 5n,
  },
  signature: `0x${"1b".repeat(65)}`,
  swap: SWAP,
};

describe("the order id (V4 freeze section 5)", () => {
  it("is keccak256(abi.encode(chainid, swap, maker, nonce)), as four padded words", () => {
    const words = concat([pad(numberToHex(84532), { size: 32 }), pad(SWAP, { size: 32 }), pad(MAKER, { size: 32 }), pad(numberToHex(5), { size: 32 })]);
    expect(orderIdOf(84532, SWAP, MAKER, 5n)).toBe(keccak256(words));
    expect(orderIdOf(8453, SWAP, MAKER, 5n)).not.toBe(orderIdOf(84532, SWAP, MAKER, 5n));
  });
});

describe("a maker's quote", () => {
  const ctx = { swap: SWAP, taker: ME, nowSeconds: NOW };

  it("is used only for this build's swap, to buy, for anyone or for this wallet, before its deadline", () => {
    expect(quoteProblem(QUOTE, ctx)).toBeNull();
    expect(quoteProblem({ ...QUOTE, quote: { ...QUOTE.quote, taker: ME } }, ctx)).toBeNull();
    expect(quoteProblem(QUOTE, { ...ctx, swap: null })).toMatch(/no swap contract/);
    expect(quoteProblem({ ...QUOTE, swap: MAKER }, ctx)).toMatch(/another swap contract/);
    expect(quoteProblem({ ...QUOTE, quote: { ...QUOTE.quote, side: 1 } }, ctx)).toMatch(/not a quote to buy/);
    expect(quoteProblem({ ...QUOTE, quote: { ...QUOTE.quote, taker: MAKER } }, ctx)).toMatch(/reserved for another address/);
    expect(quoteProblem(QUOTE, { ...ctx, nowSeconds: NOW + 300 })).toMatch(/expired/);
    expect(quoteProblem({ ...QUOTE, quote: { ...QUOTE.quote, payWindow: 0 } }, ctx)).toMatch(/no time to pay/);
  });
});

describe("the maker's signature (docs/INTERFACES.md § 2.8)", () => {
  it("SwapQuote's digest: the vector of swap/core and contracts/test/unit/ZecuredSwap.t.sol", () => {
    const q = {
      maker: "0x2222222222222222222222222222222222222222" as Address,
      taker: "0x3333333333333333333333333333333333333333" as Address,
      side: 1,
      zatoshi: 50_000_000n,
      usdc: 19_800_000n,
      makerReceiverCommitment: `0x${"44".repeat(32)}` as Hex,
      payWindow: 21_600,
      quoteDeadline: 1_760_000_120n,
      nonce: 7n,
    };
    const typed = swapQuoteTypedData("0x1111111111111111111111111111111111111111", q, 84_532);
    expect(hashTypedData(typed)).toBe("0xd3e594a519b8f06b57c3151ddf712e48edafdccf3d65f5079b0bdbbff4c89c00");
  });

  /** Anvil's first two test keys: public, for test vectors only. */
  const MAKER_KEY: Hex = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
  const OTHER_KEY: Hex = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
  const signed = async (key: Hex, maker: Address): Promise<SignedSwapQuote> => {
    const quote = { ...QUOTE.quote, maker };
    const signature = await privateKeyToAccount(key).signTypedData(swapQuoteTypedData(SWAP, quote, 84_532));
    return { quote, signature, swap: SWAP };
  };
  const isValidSignatureAbi = [
    {
      type: "function",
      name: "isValidSignature",
      stateMutability: "view",
      inputs: [
        { name: "hash", type: "bytes32" },
        { name: "signature", type: "bytes" },
      ],
      outputs: [{ name: "", type: "bytes4" }],
    },
  ] as const;
  /** A node where `MAKER` is an ERC-1271 contract that accepts exactly `accepted`. */
  const node = (accepted: Hex | null): PublicClient =>
    createPublicClient({
      chain: baseSepolia,
      transport: custom({
        async request({ method, params }: { method: string; params?: unknown }) {
          if (method !== "eth_call") throw new Error(`unexpected RPC ${method}`);
          const { to, data } = (params as [{ to: Address; data: Hex }])[0];
          if (to.toLowerCase() !== MAKER.toLowerCase()) return "0x";
          const { args } = decodeFunctionData({ abi: isValidSignatureAbi, data });
          return pad(accepted !== null && args[1] === accepted ? "0x1626ba7e" : "0xffffffff", { size: 32, dir: "right" });
        },
      }),
    }) as PublicClient;

  it("an EOA maker's signature, recovered here; anyone else's, or a quote changed after signing, is refused", async () => {
    const eoa = privateKeyToAccount(MAKER_KEY).address;
    const good = await signed(MAKER_KEY, eoa);
    expect(await quoteSignedByMaker(node(null), good, 84_532)).toBe(true);
    expect(await quoteSignedByMaker(node(null), await signed(OTHER_KEY, eoa), 84_532)).toBe(false);
    expect(await quoteSignedByMaker(node(null), { ...good, quote: { ...good.quote, usdc: good.quote.usdc - 1n } }, 84_532)).toBe(false);
    expect(await quoteSignedByMaker(node(null), good, 8_453), "another chain's domain").toBe(false);
  });

  it("a contract maker answers ERC-1271 through the proxy", async () => {
    const byContract = await signed(OTHER_KEY, MAKER);
    expect(await quoteSignedByMaker(node(byContract.signature), byContract, 84_532)).toBe(true);
    expect(await quoteSignedByMaker(node(null), byContract, 84_532)).toBe(false);
  });
});

describe("the wallet's calls", () => {
  it("approve the exact USDC only when the allowance falls short, then open with the commitment", () => {
    const calls = buyCalls({ swap: SWAP, usdcToken: USDC, quote: QUOTE, commitment: C, allowance: 0n });
    expect(calls.map((c) => c.to)).toEqual([USDC, SWAP]);
    expect(decodeFunctionData({ abi: erc20Abi, data: calls[0]!.data })).toMatchObject({ functionName: "approve", args: [SWAP, 12_000_000n] });
    const open = decodeFunctionData({ abi: swapAbi, data: calls[1]!.data });
    expect(open.functionName).toBe("open");
    expect(open.args).toEqual([QUOTE.quote, QUOTE.signature, C]);
    expect(buyCalls({ swap: SWAP, usdcToken: USDC, quote: QUOTE, commitment: C, allowance: 12_000_000n })).toHaveLength(1);
    const refund = refundCall(SWAP, C);
    expect(decodeFunctionData({ abi: swapAbi, data: refund.data })).toMatchObject({ functionName: "refund", args: [C] });
  });
});

const ORDER: StoredSwapOrder = {
  v: 1,
  orderId: orderIdOf(84532, SWAP, MAKER, 5n),
  swap: SWAP,
  chainId: 84532,
  maker: MAKER,
  taker: ME,
  zatoshi: "10000000",
  usdc: "12000000",
  payDeadline: NOW + 21_600,
  openedBlock: "1000",
  scannedTo: "1000",
  openTx: `0x${"ab".repeat(32)}`,
  receiver: "utest1receiver",
  salt: `0x${"5a".repeat(32)}`,
  commitment: C,
  destinationSent: false,
  proven: false,
  settled: false,
  refunded: false,
  createdAt: 1,
};

class Memory implements KeyValueStorage {
  data = new Map<string, string>();
  getItem(k: string) {
    return this.data.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.data.set(k, v);
  }
  removeItem(k: string) {
    this.data.delete(k);
  }
}

describe("orders kept in this browser", () => {
  it("round-trip, and anything malformed is skipped", () => {
    const s = new SwapStore(new Memory());
    expect(s.save(ORDER)).toBe(true);
    expect(s.list()).toEqual([ORDER]);
    expect(s.save({ ...ORDER, destinationSent: true })).toBe(true);
    expect(s.list()).toHaveLength(1);
    expect(s.list()[0]!.destinationSent).toBe(true);
    expect(parseSwapOrder({ ...ORDER, salt: "0x12" })).toBeNull();
    expect(parseSwapOrder({ ...ORDER, zatoshi: "-1" })).toBeNull();
    expect(parseSwapOrder(null)).toBeNull();
    expect(s.remove(ORDER.orderId)).toBe(true);
    expect(s.list()).toEqual([]);
    expect(new SwapStore(null).save(ORDER)).toBe(false);
  });
});

describe("the order's status: open → paid → proven → settled, or refundable after the deadline", () => {
  it("follows the events on Base first, the maker's word only for paid, then the clock", () => {
    expect(swapStatus(ORDER, null, NOW)).toEqual({ kind: "open", payDeadline: NOW + 21_600 });
    expect(swapStatus(ORDER, "paid", NOW)).toEqual({ kind: "paid", payDeadline: NOW + 21_600 });
    // The maker's "proven" before this browser saw the proof's event: still "paid".
    expect(swapStatus(ORDER, "proven", NOW).kind).toBe("paid");
    // The maker gave the order up: the USDC comes back after the deadline.
    expect(swapStatus(ORDER, "abandoned", NOW)).toEqual({ kind: "abandoned", payDeadline: NOW + 21_600 });
    expect(swapStatus(ORDER, "abandoned", NOW + 21_601)).toEqual({ kind: "refundable" });
    expect(swapStatus({ ...ORDER, proven: true }, "paid", NOW)).toEqual({ kind: "proven" });
    expect(swapStatus({ ...ORDER, proven: true, settled: true }, null, NOW)).toEqual({ kind: "settled" });
    expect(swapStatus(ORDER, "paid", NOW + 21_601)).toEqual({ kind: "refundable" });
    // A proof pending at the deadline holds the refund until it is decided.
    expect(swapStatus({ ...ORDER, proven: true }, null, NOW + 21_601)).toEqual({ kind: "proven" });
    expect(swapStatus({ ...ORDER, refunded: true }, null, NOW + 21_601)).toEqual({ kind: "refunded" });
  });
});

describe("reading the order's events through the proxy", () => {
  function chain(latest: bigint, logsAt: Map<bigint, { name: string }[]>): { client: PublicClient; ranges: [bigint, bigint][] } {
    const ranges: [bigint, bigint][] = [];
    const client = createPublicClient({
      chain: baseSepolia,
      transport: custom({
        async request({ method, params }: { method: string; params?: unknown }) {
          if (method === "eth_blockNumber") return numberToHex(latest);
          if (method !== "eth_getLogs") throw new Error(method);
          const f = (params as [{ address: Address; topics: [Hex[], Hex]; fromBlock: Hex; toBlock: Hex }])[0];
          const [from, to] = [BigInt(f.fromBlock), BigInt(f.toBlock)];
          ranges.push([from, to]);
          expect(to - from).toBeLessThan(2_000n);
          expect(f.topics[1]).toBe(ORDER.orderId);
          const out: unknown[] = [];
          for (const [block, events] of logsAt) {
            if (block < from || block > to) continue;
            for (const e of events) {
              out.push({
                address: SWAP,
                topics: encodeEventTopics({ abi: swapAbi, eventName: e.name as "OrderSettled", args: { orderId: ORDER.orderId } }),
                data: e.name === "SettlementPending" ? encodeAbiParameters([{ type: "bytes32" }, { type: "uint256" }, { type: "uint256" }], [C, 1n, 2n]) : "0x",
                blockNumber: numberToHex(block),
                blockHash: `0x${"11".repeat(32)}`,
                transactionHash: `0x${"22".repeat(32)}`,
                transactionIndex: "0x0",
                logIndex: "0x0",
                removed: false,
              });
            }
          }
          return out;
        },
      }),
    }) as PublicClient;
    return { client, ranges };
  }

  it("reads 2,000-block windows from the open block, keeps a cursor that re-reads the last blocks, and a dropped proof is no proof", async () => {
    const logs = new Map([
      [2_500n, [{ name: "SettlementPending" }]],
      [3_100n, [{ name: "SettlementDropped" }]],
    ]);
    const { client, ranges } = chain(4_500n, logs);
    const after = await followOrder(client, ORDER);
    expect(ranges).toEqual([
      [1_000n, 2_999n],
      [3_000n, 4_500n],
    ]);
    expect(after).toMatchObject({ proven: false, settled: false, scannedTo: "4500" });
    // Proven again, by an event of block 4,495 that a provider trailing its head served only after the cursor
    // passed it, then settled: the new blocks are read, and the last 12 again.
    logs.set(4_495n, [{ name: "SettlementPending" }]);
    logs.set(4_700n, [{ name: "OrderSettled" }]);
    const second = chain(5_000n, logs);
    const done = await followOrder(second.client, after);
    expect(second.ranges).toEqual([[4_489n, 5_000n]]);
    expect(done).toMatchObject({ proven: true, settled: true, scannedTo: "5000" });
  });

  it("finds the order id and deadline in the open transaction's logs", () => {
    const topics = encodeEventTopics({ abi: swapAbi, eventName: "OrderOpened", args: { orderId: ORDER.orderId, maker: MAKER, taker: ME } });
    const data = encodeAbiParameters(
      [{ type: "uint8" }, { type: "uint64" }, { type: "uint256" }, { type: "bytes32" }, { type: "uint64" }],
      [0, 10_000_000n, 12_000_000n, C, BigInt(NOW + 21_600)],
    );
    const log = {
      address: SWAP,
      topics,
      data,
      blockNumber: 1000n,
      blockHash: `0x${"11".repeat(32)}`,
      transactionHash: `0x${"22".repeat(32)}`,
      transactionIndex: 0,
      logIndex: 0,
      removed: false,
    } as const;
    expect(openedOrder([log as never], SWAP)).toEqual({ orderId: ORDER.orderId, payDeadline: NOW + 21_600 });
    expect(openedOrder([{ ...log, address: MAKER } as never], SWAP)).toBeNull();
  });
});
