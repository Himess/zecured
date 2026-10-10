import { describe, expect, it } from "vitest";
import {
  createPublicClient,
  custom,
  decodeFunctionData,
  encodeAbiParameters,
  encodeEventTopics,
  encodeFunctionResult,
  getAddress,
  numberToHex,
  type Abi,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { baseSepolia } from "viem/chains";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import { adapterAbi, erc20Abi, irmAbi, morphoAbi, oracleAbi, registryAbi, registryEventsAbi } from "./abi";
import { ContractsNotDeployedError, LOG_CHUNK, readPosition, readRecentRegistryEvents, readVault } from "./chain";
import { buildConfig, marketParams } from "./config";
import { borrowAssets, expectedMarket } from "./morpho";

const config = buildConfig(DRY_RUN_ENV);
const ACCOUNT: Address = "0x3333333333333333333333333333333333333333";
const VAULT: Hex = `0x${"ab".repeat(32)}`;
const NOW = 1_800_000_000n;
const LATEST = 50_000n;

const market = {
  totalSupplyAssets: 250_000_000_000n,
  totalSupplyShares: 250_000_000_000n * 10n ** 6n,
  totalBorrowAssets: 160_000n,
  totalBorrowShares: 160_000n * 10n ** 6n,
  lastUpdate: NOW - 30n * 86_400n,
  fee: 0n,
};
const RATE = 1_000_000_000n; // per second, WAD
const PRICE = 40n * 10n ** 34n;

interface Calls {
  methods: string[];
  logRanges: [bigint, bigint][];
  irmArgs?: unknown;
}

/** A fake Base node that only knows the contracts the app reads. */
function fakeChain(opts: { failLogsAfter?: number; deployed?: boolean } = {}): { client: PublicClient; calls: Calls } {
  const calls: Calls = { methods: [], logRanges: [] };
  const contracts: [Address, Abi, (fn: string, args: readonly unknown[]) => unknown][] = [
    [
      config.morpho,
      morphoAbi,
      (fn, args) => {
        if (fn === "position") {
          expect(args).toEqual([config.marketId, ACCOUNT]);
          return [0n, 160_000n * 10n ** 6n, 1_000_000n];
        }
        if (fn === "market") return [market.totalSupplyAssets, market.totalSupplyShares, market.totalBorrowAssets, market.totalBorrowShares, market.lastUpdate, market.fee];
        if (fn === "isAuthorized") return getAddress(args[1] as Address) === config.registry;
        throw new Error(fn);
      },
    ],
    [config.oracle, oracleAbi, () => PRICE],
    [config.usdc, erc20Abi, () => 42_000n],
    [
      config.irm,
      irmAbi,
      (_fn, args) => {
        calls.irmArgs = args;
        return RATE;
      },
    ],
    [config.registry, registryAbi, () => [ACCOUNT, 1_000_000n, 1]],
    [config.adapter, adapterAbi, () => false],
  ];
  let logCalls = 0;
  const client = createPublicClient({
    chain: baseSepolia,
    transport: custom({
      async request({ method, params }: { method: string; params?: unknown }) {
        calls.methods.push(method);
        const p = params as unknown[];
        switch (method) {
          case "eth_call": {
            const { to, data } = p[0] as { to: Address; data: Hex };
            const entry = contracts.find(([a]) => a.toLowerCase() === to.toLowerCase());
            if (!entry) throw new Error(`no contract at ${to}`);
            const [, abi, handler] = entry;
            const d = decodeFunctionData({ abi, data });
            return encodeFunctionResult({ abi, functionName: d.functionName, result: handler(d.functionName, d.args ?? []) } as never);
          }
          case "eth_getCode":
            return opts.deployed ? "0x6080" : "0x";
          case "eth_blockNumber":
            return numberToHex(LATEST);
          case "eth_getBlockByNumber":
            return {
              number: numberToHex(LATEST),
              hash: `0x${"11".repeat(32)}`,
              parentHash: `0x${"22".repeat(32)}`,
              timestamp: numberToHex(NOW),
              gasLimit: "0x1c9c380",
              gasUsed: "0x0",
              baseFeePerGas: "0x1",
              miner: "0x0000000000000000000000000000000000000000",
              extraData: "0x",
              size: "0x1",
              transactions: [],
              uncles: [],
            };
          case "eth_getLogs": {
            const f = p[0] as { fromBlock: Hex; toBlock: Hex };
            calls.logRanges.push([BigInt(f.fromBlock), BigInt(f.toBlock)]);
            logCalls++;
            if (opts.failLogsAfter !== undefined && logCalls > opts.failLogsAfter) throw new Error("response too large");
            if (logCalls !== 1) return [];
            const activated = registryEventsAbi[0];
            const release = registryEventsAbi[1];
            const log = (topics: Hex[], data: Hex, block: bigint, i: number) => ({
              address: config.registry,
              topics,
              data,
              blockNumber: numberToHex(block),
              blockHash: `0x${"33".repeat(32)}`,
              transactionHash: `0x${String(i).repeat(64)}`,
              transactionIndex: "0x0",
              logIndex: numberToHex(i),
              removed: false,
            });
            return [
              log(
                encodeEventTopics({ abi: [activated], eventName: "VaultActivated", args: { vaultId: VAULT, account: ACCOUNT } }) as Hex[],
                encodeAbiParameters([{ type: "uint64" }], [1_000_000n]),
                LATEST - 10n,
                1,
              ),
              log(
                encodeEventTopics({ abi: [release], eventName: "ReleaseRequested", args: { vaultId: VAULT, claimId: VAULT } }) as Hex[],
                encodeAbiParameters([{ type: "uint64" }, { type: "bytes32" }], [1_000_000n, `0x${"cd".repeat(32)}`]),
                LATEST - 5n,
                2,
              ),
            ];
          }
          default:
            throw new Error(`unexpected RPC ${method}`);
        }
      },
    }),
  }) as PublicClient;
  return { client, calls };
}

describe("readPosition", () => {
  it("reads the position, accrues interest with the IRM rate and computes the debt", async () => {
    const { client, calls } = fakeChain({ deployed: true });
    const s = await readPosition(ACCOUNT, { client, config });
    expect(s.position).toEqual({ supplyShares: 0n, borrowShares: 160_000n * 10n ** 6n, collateral: 1_000_000n });
    expect(s.price).toBe(PRICE);
    expect(s.registryAuthorized).toBe(true);
    expect(s.adapterAuthorized).toBe(false);
    expect(s.usdcBalance).toBe(42_000n);
    expect(s.accountDeployed).toBe(true);
    expect(s.blockTimestamp).toBe(NOW);
    const accrued = expectedMarket(market, RATE, NOW);
    expect(s.market).toEqual(accrued);
    expect(s.debt).toBe(borrowAssets(s.position, accrued));
    expect(s.debt).toBeGreaterThan(160_000n);
    // The IRM got the market params and the raw (stale) market state.
    expect(calls.irmArgs).toEqual([marketParams(config), market]);
    // Read-only methods the backend proxy allows.
    expect(new Set(calls.methods)).toEqual(new Set(["eth_call", "eth_getCode", "eth_getBlockByNumber"]));
  });

  it("refuses to read while the contracts are placeholders", async () => {
    const { client } = fakeChain();
    await expect(readPosition(ACCOUNT, { client, config: buildConfig({}) })).rejects.toBeInstanceOf(ContractsNotDeployedError);
  });
});

describe("readVault", () => {
  it("decodes the Registry's vault view", async () => {
    const { client } = fakeChain();
    expect(await readVault(VAULT, { client, config })).toEqual({ account: ACCOUNT, outstanding: 1_000_000n, status: "Active" });
  });
});

describe("readRecentRegistryEvents", () => {
  it("reads 2,000-block chunks backwards and decodes the public events, newest first", async () => {
    const { client, calls } = fakeChain();
    const events = await readRecentRegistryEvents({ client, config, chunks: 3 });
    expect(calls.logRanges).toEqual([
      [LATEST - LOG_CHUNK + 1n, LATEST],
      [LATEST - 2n * LOG_CHUNK + 1n, LATEST - LOG_CHUNK],
      [LATEST - 3n * LOG_CHUNK + 1n, LATEST - 2n * LOG_CHUNK],
    ]);
    expect(events.map((e) => e.name)).toEqual(["ReleaseRequested", "VaultActivated"]);
    expect(events[0]!.vaultId).toBe(VAULT);
    expect(events[0]!.fields).toMatchObject({ amount: "1000000", destCommitment: `0x${"cd".repeat(32)}` });
    expect(events[1]!.fields).toMatchObject({ account: ACCOUNT, amount: "1000000" });
  });

  it("keeps what it has when a later chunk fails, and fails if the first one does", async () => {
    const partial = fakeChain({ failLogsAfter: 1 });
    expect(await readRecentRegistryEvents({ client: partial.client, config, chunks: 5 })).toHaveLength(2);
    const none = fakeChain({ failLogsAfter: 0 });
    await expect(readRecentRegistryEvents({ client: none.client, config })).rejects.toThrow();
  });
});
