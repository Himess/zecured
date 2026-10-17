import { describe, expect, it } from "vitest";
import {
  createPublicClient,
  custom,
  decodeFunctionData,
  encodeFunctionResult,
  numberToHex,
  type Abi,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { baseSepolia } from "viem/chains";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import { disputesAbi, erc20Abi, intakeAbi, irmAbi, morphoAbi, oracleAbi, registryAbi, relayAbi } from "./abi";
import { bondViewsAbi, erc20AllowanceAbi, intakeV4Abi, intakeViewsAbi, morphoSupplyAbi, registryViewsAbi } from "./abi4";
import { buildConfig, marketParams } from "./config";
import { expectedMarket, WAD } from "./morpho";
import {
  anonymitySets,
  bondNumbers,
  capNumbers,
  marketNumbers,
  readProtocol,
  readSupply,
  relayLag,
  SECONDS_PER_YEAR,
  supplyCalls,
  withdrawCall,
} from "./public";

const config = buildConfig(DRY_RUN_ENV);
const BOND: Address = "0x5555555555555555555555555555555555555555";
const RELAY: Address = "0x6666666666666666666666666666666666666666";
const WALLET: Address = "0x7777777777777777777777777777777777777777";
const ZERO: Address = "0x0000000000000000000000000000000000000000";
const T0 = 1_790_000_000n;
const PRICE = 40n * 10n ** 34n;
const market = {
  totalSupplyAssets: 1_000_000_000_000n,
  totalSupplyShares: 10n ** 18n,
  totalBorrowAssets: 1_249_000n,
  totalBorrowShares: 1_249_000_000_000n,
  lastUpdate: T0,
  fee: 0n,
};
const RATE = 85_220_420n;

/** A Base node with the contracts the public pages read; `v3`: an Intake without pending refunds. */
function fakeChain(o: { v3?: boolean; bond?: Address } = {}): { client: PublicClient; methods: string[] } {
  const methods: string[] = [];
  const handlers: [Address, Abi, Record<string, (args: readonly unknown[]) => unknown>][] = [
    [
      config.registry,
      [...registryAbi, ...disputesAbi, ...registryViewsAbi] as Abi,
      {
        intake: () => config.intake,
        slasher: () => ZERO,
        compensator: () => ZERO,
        signerBond: () => o.bond ?? BOND,
        refundAbandonPeriod: () => 2_592_000n,
        disputeWindow: () => 86_400n,
        inclusionWindow: () => 21_600n,
        lockedZatoshi: () => 30_000_000n,
        openVaultCount: () => 4n,
      },
    ],
    [
      config.intake,
      [...intakeAbi, ...intakeViewsAbi, ...intakeV4Abi] as Abi,
      {
        signerBond: () => o.bond ?? BOND,
        relay: () => RELAY,
        pendingRefundDelay: () => 86_400n,
        capRoom: () => 120_000_000n,
        maxTotalZat: () => 150_000_000n,
        coverageBps: () => 10_000,
        pendingTotal: () => 5_000_000n,
        ...(o.v3 ? {} : { pendingRefundTotal: () => 1_000_000n }),
      },
    ],
    [RELAY, relayAbi as Abi, { bestChainTip: () => [`0x${"11".repeat(32)}`, 4_500_000n, 1n] }],
    [
      config.morpho,
      [...morphoAbi, ...morphoSupplyAbi] as Abi,
      {
        market: () => [market.totalSupplyAssets, market.totalSupplyShares, market.totalBorrowAssets, market.totalBorrowShares, market.lastUpdate, market.fee],
        position: ([, user]) => (user === WALLET ? [5n * 10n ** 17n, 0n, 0n] : [0n, 0n, 0n]),
      },
    ],
    [config.oracle, oracleAbi as Abi, { price: () => PRICE }],
    [config.irm, irmAbi as Abi, { borrowRateView: () => RATE }],
    [config.usdc, [...erc20Abi, ...erc20AllowanceAbi] as Abi, { balanceOf: () => 42_000_000n, allowance: () => 7n }],
    [
      BOND,
      bondViewsAbi as Abi,
      {
        bondOf: ([i]) => [100_000_000n, 100_000_000n, 100_000_000n, 50_000_000n, 0n][Number(i)],
        effectiveBond: () => 250_000_000n,
        totalSlashable: () => 360_000_000n,
        minBond: () => 100_000_000n,
      },
    ],
  ];
  const client = createPublicClient({
    chain: baseSepolia,
    transport: custom({
      async request({ method, params }: { method: string; params?: unknown }) {
        const p = params as unknown[];
        methods.push(method);
        if (method === "eth_getBlockByNumber") {
          return {
            number: numberToHex(100_000n),
            hash: `0x${"22".repeat(32)}`,
            parentHash: `0x${"33".repeat(32)}`,
            timestamp: numberToHex(T0 + SECONDS_PER_YEAR),
            gasLimit: "0x1",
            gasUsed: "0x0",
            baseFeePerGas: "0x1",
            miner: ZERO,
            extraData: "0x",
            size: "0x1",
            transactions: [],
            uncles: [],
          };
        }
        if (method !== "eth_call") throw new Error(`unexpected RPC ${method}`);
        const { to, data } = p[0] as { to: Address; data: Hex };
        const entry = handlers.find(([a]) => a.toLowerCase() === to.toLowerCase());
        if (!entry) throw new Error(`no contract at ${to}`);
        const [, abi, fns] = entry;
        const d = decodeFunctionData({ abi, data });
        const fn = fns[d.functionName];
        if (!fn) throw new Error(`execution reverted: ${d.functionName}`);
        return encodeFunctionResult({ abi, functionName: d.functionName, result: fn(d.args ?? []) } as never);
      },
    }),
  }) as PublicClient;
  return { client, methods };
}

describe("readProtocol: what the status and market pages read from Base", () => {
  it("the relay, the Registry's and the Intake's totals, the bonds, and the market accrued to the latest block", async () => {
    const { client, methods } = fakeChain();
    const s = await readProtocol({ client, config });
    expect(s).toMatchObject({
      relayTip: 4_500_000n,
      lockedZat: 30_000_000n,
      openVaults: 4n,
      capRoom: 120_000_000n,
      maxTotalZat: 150_000_000n,
      coverageBps: 10_000,
      pendingTotal: 5_000_000n,
      pendingRefundTotal: 1_000_000n,
      borrowRate: RATE,
      price: PRICE,
      blockTime: Number(T0 + SECONDS_PER_YEAR),
    });
    expect(s.bonds).toEqual({
      each: [100_000_000n, 100_000_000n, 100_000_000n, 50_000_000n, 0n],
      effective: 250_000_000n,
      totalSlashable: 360_000_000n,
      minBond: 100_000_000n,
    });
    // The same accrual as Morpho's (the parity vector of lib/morpho.test.ts).
    expect(s.market).toEqual(expectedMarket(market, RATE, T0 + SECONDS_PER_YEAR));
    expect(s.market.totalBorrowAssets).toBe(1_252_361n);
    // Reads only, through the proxy.
    expect(new Set(methods)).toEqual(new Set(["eth_call", "eth_getBlockByNumber"]));
  });

  it("a v3 Intake has no pending refunds; a deployment without a bond has no bond numbers", async () => {
    const v3 = await readProtocol({ client: fakeChain({ v3: true }).client, config });
    expect(v3.pendingRefundTotal).toBeNull();
    const none = await readProtocol({ client: fakeChain({ bond: ZERO }).client, config });
    expect(none.bonds).toBeNull();
  });

  it("the wallet's supply: its shares in USDC now (rounded down), its balance and Morpho's allowance", async () => {
    const s = await readSupply(WALLET, { client: fakeChain().client, config });
    // Half the supply shares: half of 1,000,000.003361 USDC, rounded down.
    expect(s).toEqual({ shares: 5n * 10n ** 17n, assets: 500_000_001_680n, balance: 42_000_000n, allowance: 7n });
  });
});

describe("the market's numbers (P-4)", () => {
  it("utilization, the borrow APY as Morpho compounds it, and what lenders earn after the fee", () => {
    const m = expectedMarket(market, RATE, T0 + SECONDS_PER_YEAR);
    const n = marketNumbers(m, RATE);
    expect(n.suppliedUsdc).toBe(1_000_000_003_361n);
    expect(n.borrowedUsdc).toBe(1_252_361n);
    expect(n.availableUsdc).toBe(1_000_000_003_361n - 1_252_361n);
    expect(n.utilizationWad).toBe((1_252_361n * WAD) / 1_000_000_003_361n);
    // 0.2687 % a year at 85,220,420 per second, compounded to the third order: 0.26911 %.
    expect(n.borrowApyWad).toBe(2_691_125_758_439_755n);
    expect(n.supplyApyWad).toBe((((n.borrowApyWad * n.utilizationWad) / WAD) * WAD) / WAD);
    const withFee = marketNumbers({ ...m, fee: WAD / 10n }, RATE);
    expect(withFee.supplyApyWad).toBe((((n.borrowApyWad * n.utilizationWad) / WAD) * (WAD - WAD / 10n)) / WAD);
    expect(marketNumbers({ ...m, totalSupplyAssets: 0n, totalBorrowAssets: 0n }, RATE)).toMatchObject({ utilizationWad: 0n, supplyApyWad: 0n });
  });
});

describe("the coverage cap and the bonds (P-2, P-4)", () => {
  it("counts pending activations and pending refunds against the cap", () => {
    const c = capNumbers({ lockedZat: 30_000_000n, pendingTotal: 5_000_000n, pendingRefundTotal: 1_000_000n, capRoom: 114_000_000n, maxTotalZat: 150_000_000n });
    expect(c).toEqual({ lockedZat: 30_000_000n, pendingZat: 6_000_000n, usedZat: 36_000_000n, roomZat: 114_000_000n, maxTotalZat: 150_000_000n, fillBps: 2_400n });
    expect(capNumbers({ lockedZat: 0n, pendingTotal: 0n, pendingRefundTotal: null, capRoom: 0n, maxTotalZat: 1n }).fillBps).toBeNull();
    expect(capNumbers({ lockedZat: 1n, pendingTotal: 0n, pendingRefundTotal: null, capRoom: null, maxTotalZat: 1n }).fillBps).toBeNull();
  });

  it("ΣB, B_eff, the signers at the minimum, and the bonds per dollar of locked collateral", () => {
    const b = bondNumbers(
      { each: [100_000_000n, 100_000_000n, 100_000_000n, 50_000_000n, 0n], effective: 250_000_000n, totalSlashable: 360_000_000n, minBond: 100_000_000n },
      30_000_000n,
      PRICE,
    );
    // 0.3 ZEC at 40 USD: 12 USDC of collateral; B_eff 250 USDC.
    expect(b).toEqual({ sum: 350_000_000n, effective: 250_000_000n, totalSlashable: 360_000_000n, minBond: 100_000_000n, bonded: 3, perCollateralWad: (250_000_000n * WAD) / 12_000_000n });
    expect(bondNumbers({ each: [1n], effective: 1n, totalSlashable: 1n, minBond: 1n }, 0n, PRICE).perCollateralWad).toBeNull();
  });
});

describe("the status page (P-2)", () => {
  it("the anonymity set of every size, zero where none is open; none without the backend's counts", () => {
    const stats = { byAmount: new Map([[5_000_000n, 3]]), total: 3, complete: true, readAt: 1 };
    expect(anonymitySets([1_000_000n, 5_000_000n], stats)).toEqual([
      { amount: 1_000_000n, open: 0 },
      { amount: 5_000_000n, open: 3 },
    ]);
    expect(anonymitySets([1_000_000n], null)).toBeNull();
  });

  it("how far the header relay is behind the light wallet server", () => {
    expect(relayLag(4_500_000n, 4_500_006)).toBe(6);
    expect(relayLag(4_500_010n, 4_500_006)).toBe(0);
    expect(relayLag(null, 4_500_006)).toBeNull();
    expect(relayLag(4_500_000n, 0)).toBeNull();
  });
});

describe("lending from the user's own wallet (P-4)", () => {
  const params = marketParams(config);

  it("approves the shortfall, then supplies on the wallet's own behalf", () => {
    const calls = supplyCalls({ morpho: config.morpho, usdc: config.usdc, params, wallet: WALLET, assets: 25_000_000n, allowance: 0n });
    expect(calls.map((c) => c.to)).toEqual([config.usdc, config.morpho]);
    expect(decodeFunctionData({ abi: erc20Abi, data: calls[0]!.data })).toMatchObject({ functionName: "approve", args: [config.morpho, 25_000_000n] });
    expect(decodeFunctionData({ abi: morphoSupplyAbi, data: calls[1]!.data })).toMatchObject({
      functionName: "supply",
      args: [params, 25_000_000n, 0n, WALLET, "0x"],
    });
    expect(supplyCalls({ morpho: config.morpho, usdc: config.usdc, params, wallet: WALLET, assets: 25n, allowance: 25n })).toHaveLength(1);
    expect(() => supplyCalls({ morpho: config.morpho, usdc: config.usdc, params, wallet: WALLET, assets: 0n, allowance: 0n })).toThrow(/positive/);
  });

  it("withdraws an amount, or everything by its shares, to the wallet itself", () => {
    const some = withdrawCall({ morpho: config.morpho, params, wallet: WALLET, assets: 10_000_000n });
    expect(decodeFunctionData({ abi: morphoSupplyAbi, data: some.data }).args).toEqual([params, 10_000_000n, 0n, WALLET, WALLET]);
    const all = withdrawCall({ morpho: config.morpho, params, wallet: WALLET, shares: 5n * 10n ** 17n });
    expect(decodeFunctionData({ abi: morphoSupplyAbi, data: all.data }).args).toEqual([params, 0n, 5n * 10n ** 17n, WALLET, WALLET]);
    expect(() => withdrawCall({ morpho: config.morpho, params, wallet: WALLET, assets: 0n })).toThrow(/positive/);
  });
});
