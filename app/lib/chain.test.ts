import { describe, expect, it } from "vitest";
import {
  concat,
  createPublicClient,
  custom,
  decodeFunctionData,
  encodeAbiParameters,
  encodeEventTopics,
  encodeFunctionResult,
  getAddress,
  keccak256,
  numberToHex,
  pad,
  type Abi,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { baseSepolia } from "viem/chains";
import { DRY_RUN_CONTRACTS, DRY_RUN_ENV } from "./__fixtures__/deployment";
import {
  adapterAbi,
  bondAbi,
  compensatorAbi,
  depositRefundedEvent,
  disputesAbi,
  erc20Abi,
  intakeAbi,
  irmAbi,
  morphoAbi,
  oracleAbi,
  REFUND_REASON,
  registryAbi,
  registryEventsAbi,
  relayAbi,
  slasherAbi,
} from "./abi";
import {
  BondMismatchError,
  claimIdOf,
  ContractsNotDeployedError,
  IntakeMismatchError,
  LOG_CHUNK,
  MAX_LEDGER_CLAIMS,
  readCompensation,
  readPosition,
  readRecentRegistryEvents,
  readRefundReason,
  readReleaseClaim,
  readVault,
  readVaultLedger,
} from "./chain";
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
          case "eth_getTransactionReceipt": {
            const release = registryEventsAbi[1];
            const releaseLog = (address: Address, vaultId: Hex, claimId: Hex, amount: bigint, logIndex: number) => ({
              address,
              topics: encodeEventTopics({ abi: [release], eventName: "ReleaseRequested", args: { vaultId, claimId } }),
              data: encodeAbiParameters([{ type: "uint64" }, { type: "bytes32" }], [amount, `0x${"cd".repeat(32)}`]),
              blockNumber: numberToHex(LATEST),
              blockHash: `0x${"33".repeat(32)}`,
              transactionHash: p[0],
              transactionIndex: "0x0",
              logIndex: numberToHex(logIndex),
              removed: false,
            });
            return {
              transactionHash: p[0],
              transactionIndex: "0x0",
              blockHash: `0x${"33".repeat(32)}`,
              blockNumber: numberToHex(LATEST),
              from: "0x0000000000000000000000000000000000000001",
              to: "0x0000000071727De22E5E9d8BAf0edAc6f37da032",
              cumulativeGasUsed: "0x1",
              gasUsed: "0x1",
              contractAddress: null,
              logs: [
                // The same event from another contract, and another vault's release in the same bundle.
                releaseLog(ACCOUNT, VAULT, `0x${"01".repeat(32)}`, 1n, 0),
                releaseLog(config.registry, `0x${"ef".repeat(32)}`, `0x${"02".repeat(32)}`, 2n, 1),
                releaseLog(config.registry, VAULT, `0x${"0A".repeat(32)}`, 200_000n, 2),
              ],
              logsBloom: `0x${"00".repeat(256)}`,
              status: "0x1",
              effectiveGasPrice: "0x1",
              type: "0x2",
            };
          }
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
                encodeAbiParameters([{ type: "uint64" }, { type: "bytes32" }], [1_000_000n, `0x${"7a".repeat(32)}`]),
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

describe("readReleaseClaim", () => {
  it("finds the vault's ReleaseRequested event in the receipt, from the Registry only", async () => {
    const { client, calls } = fakeChain();
    const claim = await readReleaseClaim(VAULT, `0x${"77".repeat(32)}`, { client, config });
    expect(claim).toEqual({ claimId: `0x${"0a".repeat(32)}`, amount: 200_000n });
    expect(calls.methods).toEqual(["eth_getTransactionReceipt"]);
    expect(await readReleaseClaim(`0x${"99".repeat(32)}`, `0x${"77".repeat(32)}`, { client, config })).toBeNull();
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
    expect(events[1]!.fields).toMatchObject({ account: ACCOUNT, amount: "1000000", depositTag: `0x${"7a".repeat(32)}` });
  });

  it("keeps what it has when a later chunk fails, and fails if the first one does", async () => {
    const partial = fakeChain({ failLogsAfter: 1 });
    expect(await readRecentRegistryEvents({ client: partial.client, config, chunks: 5 })).toHaveLength(2);
    const none = fakeChain({ failLogsAfter: 0 });
    await expect(readRecentRegistryEvents({ client: none.client, config })).rejects.toThrow();
  });
});

// ------------------------------------------------------------------ v3: the vault's ledger

const SLASHER: Address = DRY_RUN_CONTRACTS.ZecuredSlasher;
const COMPENSATOR: Address = DRY_RUN_CONTRACTS.ZecuredCompensator;
const BOND: Address = DRY_RUN_CONTRACTS.SignerBond;
const RELAY: Address = "0xab07317f7B9390394271D9c03FE2E912C0E58FA5";
const ZERO: Address = "0x0000000000000000000000000000000000000000";
const NONE32: Hex = `0x${"00".repeat(32)}`;
const ANCHOR: Hex = `0x${"a7".repeat(32)}`;
const COMMITMENT: Hex = `0x${"cd".repeat(32)}`;
/** Block n of the fake chain is mined at T0 + 2n (two-second blocks). */
const T0 = 1_790_000_000n;

interface V3World {
  /** What the Registry names as its Intake (the pinned one unless a test says otherwise). */
  intake?: Address;
  slasher?: Address;
  /** The bond the Registry and the Intake name (BOND unless a test says otherwise), and what it holds. */
  bond?: Address;
  intakeBond?: Address;
  slashable?: bigint;
  vault: { account: Address; outstanding: bigint; status: number };
  claims: {
    kind: number;
    status: number;
    amount: bigint;
    commitment?: Hex;
    createdAt: number;
    bondPaid?: boolean;
    claimant: Address;
    paidAt?: bigint;
    dueAt?: bigint;
    /** The bond's `livenessDue(claimId)`: [dueAt, amount] ([0, 0] unless given). */
    liveness?: [bigint, bigint];
  }[];
  pending?: { amount: bigint; readyHeight: bigint; readyTime: bigint };
  frozen?: boolean;
  compromised?: boolean;
  /** Blocks in the chain, and the block whose DepositRefunded (reason) the Intake emitted, if any. */
  latestBlock?: bigint;
  refund?: { block: bigint; claimId: Hex; reason: number };
}

/** A Base node with the v3 contracts of the dry-run deployment, counting the calls to each function. */
function fakeV3Chain(w: V3World): { client: PublicClient; calls: Map<string, number>; logRanges: [bigint, bigint][] } {
  const calls = new Map<string, number>();
  const logRanges: [bigint, bigint][] = [];
  const latest = w.latestBlock ?? 100_000n;
  const claimIds = w.claims.map((_, i) => claimIdOf(VAULT, BigInt(i)));
  const claimOf = (id: Hex) => w.claims[claimIds.findIndex((c) => c === id.toLowerCase())];
  const contracts: [Address, Abi, Record<string, (args: readonly unknown[]) => unknown>][] = [
    [
      config.registry,
      [...registryAbi, ...disputesAbi] as Abi,
      {
        intake: () => w.intake ?? config.intake,
        slasher: () => w.slasher ?? SLASHER,
        compensator: () => COMPENSATOR,
        signerBond: () => w.bond ?? BOND,
        refundAbandonPeriod: () => 2_592_000n,
        disputeWindow: () => 86_400n,
        inclusionWindow: () => 21_600n,
        vaultDetails: ([id]) => {
          expect(id).toBe(VAULT);
          return { ...w.vault, claimCount: BigInt(w.claims.length), unpaidClaims: 2n };
        },
        claimDetails: ([id]) => {
          const c = claimOf(id as Hex)!;
          return {
            vaultId: VAULT,
            kind: c.kind,
            status: c.status,
            amount: c.amount,
            commitment: c.commitment ?? NONE32,
            createdAt: c.createdAt,
            bondPaid: c.bondPaid ?? false,
            claimant: c.claimant,
          };
        },
        payoutDispute: ([id]) => {
          const c = claimOf(id as Hex)!;
          return [c.paidAt ?? 0n, c.dueAt ?? 0n];
        },
      },
    ],
    [
      config.intake,
      intakeAbi as Abi,
      {
        signerBond: () => w.intakeBond ?? w.bond ?? BOND,
        relay: () => RELAY,
        pendingRefundDelay: () => 86_400n,
        mintingPaused: () => true,
        pausedUntil: () => NOW + 3_600n,
        pausedSince: () => NOW - 90_000n,
        pendingActivation: () =>
          w.pending
            ? { account: w.vault.account, depositTag: NONE32, anchor: ANCHOR, ...w.pending }
            : { account: ZERO, amount: 0n, depositTag: NONE32, anchor: NONE32, readyHeight: 0n, readyTime: 0n },
      },
    ],
    [RELAY, relayAbi as Abi, { bestChainTip: () => [NONE32, 4_500_000n, 1n], isInBestChain: ([a]) => a === ANCHOR }],
    [
      SLASHER,
      slasherAbi as Abi,
      { vaultFrozen: () => w.frozen ?? false, isCompromised: () => w.compromised ?? false, openCasesOf: () => 0n },
    ],
    [
      BOND,
      bondAbi as Abi,
      {
        halted: () => false,
        fullyBonded: () => true,
        livenessDue: ([id]) => claimOf(id as Hex)?.liveness ?? [0n, 0n],
        totalSlashable: () => w.slashable ?? 0n,
      },
    ],
    [
      COMPENSATOR,
      compensatorAbi as Abi,
      {
        victim: () => ({
          incident: 1n,
          tallied: true,
          settled: true,
          authorized: true,
          pendingClaims: 1n,
          talliedClaims: 2n,
          outstanding: 1_000_000n,
          debt: 40_000_000n,
        }),
        equityOf: () => 12_500_000n,
        entitlement: ([id]) =>
          id === claimIds[0]
            ? { incident: 1n, layer: 4, zat: 200_000n, to: w.vault.account, paid: false }
            : { incident: 0n, layer: 0, zat: 0n, to: ZERO, paid: false },
        incident: () => ({
          openedAt: 1n,
          closesAt: 2n,
          finalized: true,
          victims: 1,
          talliedVictims: 1,
          price: PRICE,
          layer2: 0n,
          layer3: 0n,
          layer4: 0n,
          r2: 10n ** 18n,
          r3: 10n ** 18n,
          r4: 5n * 10n ** 17n,
        }),
      },
    ],
  ];
  const block = (n: bigint) => ({
    number: numberToHex(n),
    hash: `0x${"11".repeat(32)}`,
    parentHash: `0x${"22".repeat(32)}`,
    timestamp: numberToHex(T0 + 2n * n),
    gasLimit: "0x1c9c380",
    gasUsed: "0x0",
    baseFeePerGas: "0x1",
    miner: ZERO,
    extraData: "0x",
    size: "0x1",
    transactions: [],
    uncles: [],
  });
  const client = createPublicClient({
    chain: baseSepolia,
    transport: custom({
      async request({ method, params }: { method: string; params?: unknown }) {
        const p = params as unknown[];
        const count = (k: string) => calls.set(k, (calls.get(k) ?? 0) + 1);
        count(method);
        switch (method) {
          case "eth_call": {
            const { to, data } = p[0] as { to: Address; data: Hex };
            const entry = contracts.find(([a]) => a.toLowerCase() === to.toLowerCase());
            if (!entry) throw new Error(`no contract at ${to}`);
            const [, abi, handlers] = entry;
            const d = decodeFunctionData({ abi, data });
            count(d.functionName);
            const handler = handlers[d.functionName];
            if (!handler) throw new Error(`unexpected ${d.functionName}`);
            return encodeFunctionResult({ abi, functionName: d.functionName, result: handler(d.args ?? []) } as never);
          }
          case "eth_getBlockByNumber":
            return block(p[0] === "latest" ? latest : BigInt(p[0] as Hex));
          case "eth_getLogs": {
            const f = p[0] as { address: Address; topics: (Hex | null)[]; fromBlock: Hex; toBlock: Hex };
            const [from, to] = [BigInt(f.fromBlock), BigInt(f.toBlock)];
            logRanges.push([from, to]);
            expect(to - from).toBeLessThan(LOG_CHUNK);
            expect(f.address.toLowerCase()).toBe(config.intake.toLowerCase());
            const r = w.refund;
            if (!r || r.block < from || r.block > to) return [];
            const topics = encodeEventTopics({ abi: [depositRefundedEvent], eventName: "DepositRefunded", args: { vaultId: VAULT, claimId: r.claimId } });
            if (f.topics.some((t, i) => t !== null && t.toLowerCase() !== String(topics[i]).toLowerCase())) return [];
            return [
              {
                address: config.intake,
                topics,
                data: encodeAbiParameters([{ type: "uint8" }], [r.reason]),
                blockNumber: numberToHex(r.block),
                blockHash: `0x${"33".repeat(32)}`,
                transactionHash: `0x${"44".repeat(32)}`,
                transactionIndex: "0x0",
                logIndex: "0x0",
                removed: false,
              },
            ];
          }
          default:
            throw new Error(`unexpected RPC ${method}`);
        }
      },
    }),
  }) as PublicClient;
  return { client, calls, logRanges };
}

const ACTIVE = { account: ACCOUNT, outstanding: 975_000n, status: 1 };

describe("claimIdOf", () => {
  it("is the Registry's keccak256(abi.encode(vaultId, uint64 sequence))", () => {
    expect(claimIdOf(VAULT, 3n)).toBe(keccak256(concat([VAULT, pad(numberToHex(3), { size: 32 })])));
    expect(claimIdOf(VAULT, 0n)).not.toBe(claimIdOf(VAULT, 1n));
  });
});

describe("readVaultLedger", () => {
  it("reads every claim with v3's kinds and statuses, and the dispute times of confirmed ones only", async () => {
    const { client, calls } = fakeV3Chain({
      vault: ACTIVE,
      claims: [
        { kind: 5, status: 1, amount: 25_000n, createdAt: 1_790_000_100, claimant: ZERO }, // Fee, pending
        { kind: 0, status: 2, amount: 200_000n, commitment: COMMITMENT, createdAt: 1_790_000_200, claimant: ACCOUNT, paidAt: 1_790_000_900n },
        { kind: 4, status: 1, amount: 20_000n, createdAt: 1_790_000_300, claimant: ACCOUNT }, // Refund, no destination
        { kind: 0, status: 5, amount: 100_000n, commitment: COMMITMENT, createdAt: 1_790_000_400, claimant: ACCOUNT, dueAt: 1_790_100_000n },
      ],
    });
    const l = await readVaultLedger(VAULT, { client, config });
    expect(l).toMatchObject({ vaultId: VAULT, account: ACCOUNT, outstanding: 975_000n, status: "Active", unpaidClaims: 2, pending: null });
    expect(l.claims.map((c) => [c.kind, c.status, c.paidAt, c.disputeDueAt])).toEqual([
      ["Fee", "Pending", 0, 0],
      ["Release", "Paid", 1_790_000_900, 0],
      ["Refund", "Pending", 0, 0],
      ["Release", "Disputed", 0, 1_790_100_000],
    ]);
    expect(l.claims[2]).toMatchObject({ claimId: claimIdOf(VAULT, 2n), sequence: 2, amount: 20_000n, commitment: NONE32, claimant: ACCOUNT });
    expect(calls.get("payoutDispute")).toBe(2);
    expect(l.links).toEqual({
      slasher: SLASHER,
      compensator: COMPENSATOR,
      bond: BOND,
      relay: RELAY,
      disputeWindow: 86_400,
      inclusionWindow: 21_600,
      refundAbandonPeriod: 2_592_000,
      pendingRefundDelay: 86_400,
    });
    expect(l.blockTime).toBe(Number(T0 + 200_000n));
    // Nothing about a pending activation when there is none.
    expect(calls.get("bestChainTip")).toBeUndefined();
    // The links are immutables: read once per client.
    await readVaultLedger(VAULT, { client, config });
    expect(calls.get("intake")).toBe(1);
    expect(calls.get("vaultDetails")).toBe(2);
    // Only read methods the backend proxy allows.
    expect([...calls.keys()].filter((k) => k.startsWith("eth_"))).toEqual(["eth_call", "eth_getBlockByNumber"]);
  });

  it("reads a pending activation with the relay's tip and the gates that hold it", async () => {
    const { client } = fakeV3Chain({
      vault: { account: ACCOUNT, outstanding: 0n, status: 4 },
      claims: [],
      pending: { amount: 1_000_000n, readyHeight: 4_500_006n, readyTime: NOW + 150n },
    });
    const l = await readVaultLedger(VAULT, { client, config });
    expect(l.status).toBe("Pending");
    expect(l.pending).toEqual({
      amount: 1_000_000n,
      readyHeight: 4_500_006n,
      readyTime: Number(NOW + 150n),
      relayTip: 4_500_000n,
      anchorOnBestChain: true,
      openCases: 0n,
      paused: true,
      pausedUntil: Number(NOW + 3_600n),
      pausedSince: Number(NOW - 90_000n),
      halted: false,
      fullyBonded: true,
    });
  });

  it("refuses a Registry that names another Intake than the pinned one", async () => {
    const { client } = fakeV3Chain({ intake: ACCOUNT, vault: ACTIVE, claims: [] });
    await expect(readVaultLedger(VAULT, { client, config })).rejects.toBeInstanceOf(IntakeMismatchError);
  });

  it("a deployment without a slasher freezes nothing and reads no slasher", async () => {
    const { client, calls } = fakeV3Chain({ slasher: ZERO, vault: ACTIVE, claims: [], frozen: true });
    const l = await readVaultLedger(VAULT, { client, config });
    expect(l).toMatchObject({ frozen: false, compromised: false });
    expect(l.links.slasher).toBeNull();
    expect(calls.get("vaultFrozen")).toBeUndefined();
  });

  it("reads the newest claims only, so a wrong count cannot flood the proxy", async () => {
    const many = Array.from({ length: 200 }, (_, i) => ({ kind: 0, status: 1, amount: BigInt(i + 1), createdAt: 1, claimant: ACCOUNT }));
    const { client, calls } = fakeV3Chain({ vault: ACTIVE, claims: many });
    const l = await readVaultLedger(VAULT, { client, config });
    expect(l.claims).toHaveLength(MAX_LEDGER_CLAIMS);
    expect(l.claims[0]!.sequence).toBe(200 - MAX_LEDGER_CLAIMS);
    expect(l.claims.at(-1)).toMatchObject({ sequence: 199, amount: 200n, claimId: claimIdOf(VAULT, 199n) });
    expect(calls.get("claimDetails")).toBe(MAX_LEDGER_CLAIMS);
  });

  it("reports a frozen and a compromised vault", async () => {
    const { client } = fakeV3Chain({ vault: ACTIVE, claims: [], frozen: true, compromised: true });
    expect(await readVaultLedger(VAULT, { client, config })).toMatchObject({ frozen: true, compromised: true });
  });

  /** The fake chain's latest block time (block 100,000). */
  const BLOCK_TIME = T0 + 200_000n;
  const LIQUIDATOR: Address = "0x4444444444444444444444444444444444444444";

  it("T-05: reads when the bonds pay the account's own unpaid claims, and what they hold once one is due", async () => {
    const { client, calls } = fakeV3Chain({
      vault: ACTIVE,
      slashable: 5_000_000n,
      claims: [
        // The account's Release, overdue: the bonds pay 82,000 USDC units now.
        { kind: 0, status: 1, amount: 200_000n, commitment: COMMITMENT, createdAt: 1, claimant: ACCOUNT, liveness: [BLOCK_TIME - 60n, 82_000n] },
        // The account's Refund with a destination, due in an hour.
        { kind: 4, status: 1, amount: 50_000n, commitment: COMMITMENT, createdAt: 2, claimant: ACCOUNT, liveness: [BLOCK_TIME + 3_600n, 20_500n] },
        // Not read: the liquidator's claim, a refund without a destination, a paid claim, one the bonds already paid.
        { kind: 1, status: 1, amount: 100_000n, commitment: COMMITMENT, createdAt: 3, claimant: LIQUIDATOR, liveness: [1n, 1n] },
        { kind: 4, status: 1, amount: 50_000n, createdAt: 4, claimant: ACCOUNT, liveness: [1n, 1n] },
        { kind: 0, status: 2, amount: 100_000n, commitment: COMMITMENT, createdAt: 5, claimant: ACCOUNT, paidAt: 6n, liveness: [1n, 1n] },
        { kind: 0, status: 1, amount: 100_000n, createdAt: 6, bondPaid: true, claimant: ACCOUNT, liveness: [1n, 1n] },
        // Read, but the bonds never pay it (e.g. its kind): livenessDue is zero.
        { kind: 2, status: 1, amount: 10_000n, commitment: COMMITMENT, createdAt: 7, claimant: ACCOUNT },
      ],
    });
    const l = await readVaultLedger(VAULT, { client, config });
    expect(l.claims.map((c) => c.liveness)).toEqual([
      { dueAt: Number(BLOCK_TIME - 60n), usdc: 82_000n },
      { dueAt: Number(BLOCK_TIME + 3_600n), usdc: 20_500n },
      null,
      null,
      null,
      null,
      null,
    ]);
    expect(calls.get("livenessDue")).toBe(3);
    expect(l.bondsSlashable).toBe(5_000_000n);
    expect(calls.get("totalSlashable")).toBe(1);
  });

  it("T-05: reads what the bonds hold only when one of the claims is due, and nothing on a frozen vault", async () => {
    const later = { kind: 0, status: 1, amount: 200_000n, commitment: COMMITMENT, createdAt: 1, claimant: ACCOUNT, liveness: [BLOCK_TIME + 1n, 82_000n] as [bigint, bigint] };
    const notDue = fakeV3Chain({ vault: ACTIVE, claims: [later] });
    expect(await readVaultLedger(VAULT, { client: notDue.client, config })).toMatchObject({ bondsSlashable: null });
    expect(notDue.calls.get("totalSlashable")).toBeUndefined();
    const frozen = fakeV3Chain({ vault: ACTIVE, claims: [{ ...later, liveness: [1n, 82_000n] }], frozen: true });
    const l = await readVaultLedger(VAULT, { client: frozen.client, config });
    expect(l.claims[0]!.liveness).toBeNull();
    expect(frozen.calls.get("livenessDue")).toBeUndefined();
  });

  it("T-05: the bond is the pinned Registry's; one the Intake does not share is refused, none reads nothing", async () => {
    const { client } = fakeV3Chain({ vault: ACTIVE, claims: [], intakeBond: SLASHER });
    await expect(readVaultLedger(VAULT, { client, config })).rejects.toBeInstanceOf(BondMismatchError);
    const none = fakeV3Chain({
      bond: ZERO,
      vault: ACTIVE,
      claims: [{ kind: 0, status: 1, amount: 1n, commitment: COMMITMENT, createdAt: 1, claimant: ACCOUNT, liveness: [1n, 1n] }],
    });
    const l = await readVaultLedger(VAULT, { client: none.client, config });
    expect(l.links.bond).toBeNull();
    expect(l.claims[0]!.liveness).toBeNull();
    expect(none.calls.get("livenessDue")).toBeUndefined();
  });
});

describe("readRefundReason", () => {
  const claimId = claimIdOf(VAULT, 0n);

  it("finds the block of the claim's second and reads its DepositRefunded, in 2,000-block windows", async () => {
    const block = 61_234n;
    const { client, calls, logRanges } = fakeV3Chain({ vault: ACTIVE, claims: [], refund: { block, claimId, reason: 5 } });
    const reason = await readRefundReason(VAULT, claimId, Number(T0 + 2n * block), { client, config });
    expect(reason).toBe("CapExceeded");
    expect(logRanges).toHaveLength(1);
    const [from, to] = logRanges[0]!;
    expect(from <= block && block <= to).toBe(true);
    expect(calls.get("eth_getBlockByNumber")!).toBeLessThanOrEqual(1 + 40);
  });

  it("each reason the Intake emits, in plain words for the app", async () => {
    for (const [i, name] of REFUND_REASON.entries()) {
      const { client } = fakeV3Chain({ vault: ACTIVE, claims: [], refund: { block: 99_000n, claimId, reason: i } });
      expect(await readRefundReason(VAULT, claimId, Number(T0 + 2n * 99_000n), { client, config })).toBe(name);
    }
  });

  it("null when not found, in the future, or beyond the probe budget", async () => {
    const { client } = fakeV3Chain({ vault: ACTIVE, claims: [], refund: { block: 5n, claimId: VAULT, reason: 1 } });
    expect(await readRefundReason(VAULT, claimId, Number(T0 + 10n), { client, config })).toBeNull();
    expect(await readRefundReason(VAULT, claimId, Number(T0 + 10_000_000n), { client, config })).toBeNull();
    const far = fakeV3Chain({ vault: ACTIVE, claims: [], refund: { block: 5n, claimId, reason: 1 } });
    expect(await readRefundReason(VAULT, claimId, Number(T0 + 10n), { client: far.client, config, maxProbes: 2 })).toBeNull();
    expect(await readRefundReason(VAULT, claimId, Number(T0 + 10n), { client: far.client, config })).toBe("VaultNotNew");
  });
});

describe("readCompensation", () => {
  it("reads the vault's tally, its equity and its claims' entitlements, in USDC once finalized", async () => {
    const { client } = fakeV3Chain({ vault: ACTIVE, claims: [{ kind: 0, status: 4, amount: 200_000n, createdAt: 1, claimant: ACCOUNT }] });
    const c = await readCompensation(VAULT, [claimIdOf(VAULT, 0n), claimIdOf(VAULT, 9n)], { client, config });
    expect(c).toMatchObject({ incident: 1n, equity: 12_500_000n, victim: { tallied: true, settled: true, authorized: true } });
    expect(c!.incidentState).toMatchObject({ finalized: true, price: PRICE, r4: 5n * 10n ** 17n });
    // 200,000 zatoshi (0.002 ZEC) at $40 is 0.08 USDC; layer 4 is paid at half: 0.04 USDC.
    expect(c!.entitlements).toEqual([
      { claimId: claimIdOf(VAULT, 0n), layer: 4, zat: 200_000n, to: ACCOUNT, paid: false, usdc: 40_000n },
    ]);
  });
});
