import { afterEach, describe, expect, it, vi } from "vitest";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { parseRemoteConfig } from "./api";
import {
  buildConfig,
  CHAIN_ID,
  configIssues,
  marketIdOf,
  marketParams,
  parseSigners,
  PLACEHOLDER_CONTRACT,
  RUNTIME_CONFIG_TIMEOUT_MS,
  runtimeConfigLoader,
  type RemoteConfig,
} from "./config";

const REMOTE = parseRemoteConfig(DRY_RUN_CONFIG);
const DEV = { NODE_ENV: "development", NEXT_PUBLIC_BACKEND_URL: DRY_RUN_ENV.NEXT_PUBLIC_BACKEND_URL };
const ATTACKER: `0x${string}` = "0x5555555555555555555555555555555555555555";
const SIGNERS = DRY_RUN_ENV.NEXT_PUBLIC_SIGNERS.split(",");
const ALL_PINS =
  "NEXT_PUBLIC_REGISTRY, NEXT_PUBLIC_ADAPTER, NEXT_PUBLIC_MARKET_ID, NEXT_PUBLIC_USDC, NEXT_PUBLIC_ORACLE, NEXT_PUBLIC_ZZEC and NEXT_PUBLIC_SIGNERS";
const HARD_STOP =
  "Owner signatures, Base reads and sponsored transactions are disabled until this build's pinned addresses and the backend agree.";
const MARKET_MISMATCH = "The market id does not match the market parameters (loan token, collateral, oracle, IRM, LLTV).";

/** The fixture with some fields replaced. */
function remote(patch: Partial<RemoteConfig>): RemoteConfig {
  return { ...REMOTE, ...patch };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("build-time pins (production rules)", () => {
  it("without pins nothing on Base runs, owner signatures included", () => {
    const c = buildConfig({});
    expect(c.backendUrl).toBe("http://127.0.0.1:8080");
    expect(c.registry).toBe(PLACEHOLDER_CONTRACT);
    expect(c.adapter).toBe(PLACEHOLDER_CONTRACT);
    expect(c.morpho).toBe("0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
    expect(c.signers).toBeNull();
    expect(c).toMatchObject({ contractsDeployed: false, ownerSigning: false, sponsorship: false, remote: "pending", signersPinned: false });
    expect(c.denominations).toEqual([1_000_000n, 5_000_000n, 12_500_000n]);
    expect(c.confirmations).toBeNull();
    expect(c.openVaultStampBits).toBeNull();
    expect(configIssues(c)).toEqual([
      `This build does not pin ${ALL_PINS}. Owner signatures, Base reads and sponsored transactions stay disabled until the app is rebuilt with them (see app/.env.example).`,
    ]);
  });

  it("reads the pinned deployment; the market id matches Morpho's MarketParamsLib.id", () => {
    const c = buildConfig(DRY_RUN_ENV);
    expect(c).toMatchObject({ contractsDeployed: true, ownerSigning: true, unpinned: [], problems: [], signersPinned: true });
    expect(c.signers).toEqual(SIGNERS);
    // The dry run's marketId was computed on-chain by the Registry constructor.
    expect(marketIdOf(marketParams(c))).toBe(DRY_RUN_ENV.NEXT_PUBLIC_MARKET_ID);
    expect(configIssues(c)).toEqual([]);
    expect(configIssues({ ...c, oracle: c.usdc })).toEqual([MARKET_MISMATCH]);
  });

  it("a pinned Registry alone allows owner requests; Base stays disabled", () => {
    const c = buildConfig({ NEXT_PUBLIC_REGISTRY: DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY });
    expect(c).toMatchObject({ ownerSigning: true, contractsDeployed: false });
    expect(configIssues(c)[0]).toMatch(/^This build does not pin NEXT_PUBLIC_ADAPTER, .*\. Base reads and sponsored transactions stay disabled/);
  });

  it("normalizes pins; a malformed pin is a hard stop", () => {
    const c = buildConfig({
      ...DRY_RUN_ENV,
      NEXT_PUBLIC_BACKEND_URL: " https://api.example.test/ ",
      NEXT_PUBLIC_REGISTRY: DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY.toLowerCase(),
      NEXT_PUBLIC_ADAPTER: "not-an-address",
      NEXT_PUBLIC_MARKET_ID: "0x1234",
    });
    expect(c.backendUrl).toBe("https://api.example.test");
    expect(c.registry).toBe(DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY);
    expect(c.adapter).toBe(PLACEHOLDER_CONTRACT);
    expect(c).toMatchObject({ contractsDeployed: false, ownerSigning: false, unpinned: [] });
    expect(configIssues(c)).toEqual([
      "NEXT_PUBLIC_ADAPTER is set but is not a valid address.",
      "NEXT_PUBLIC_MARKET_ID is set but is not a valid market id.",
      HARD_STOP,
    ]);
    expect(buildConfig({ NEXT_PUBLIC_BACKEND_URL: "javascript:alert(1)" }).backendUrl).toBe("http://127.0.0.1:8080");
  });

  it("a pinned market id must match the pinned market parameters", () => {
    const c = buildConfig({ ...DRY_RUN_ENV, NEXT_PUBLIC_MARKET_ID: `0x${"12".repeat(32)}` });
    expect(c.contractsDeployed).toBe(false);
    expect(configIssues(c)).toEqual([MARKET_MISMATCH, HARD_STOP]);
  });

  it("NEXT_PUBLIC_SIGNERS: five distinct addresses, sorted like the Registry's signer set", () => {
    expect(parseSigners(undefined)).toBeUndefined();
    expect(parseSigners(" ")).toBeUndefined();
    expect(parseSigners([...SIGNERS].reverse().map((s) => s.toLowerCase()).join(" , "))).toEqual(SIGNERS);
    expect(parseSigners(SIGNERS.slice(0, 4).join(","))).toBeNull();
    expect(parseSigners([...SIGNERS.slice(0, 4), SIGNERS[0]!].join(","))).toBeNull();
    expect(parseSigners([...SIGNERS.slice(0, 4), "0x1234"].join(","))).toBeNull();
    const c = buildConfig({ ...DRY_RUN_ENV, NEXT_PUBLIC_SIGNERS: SIGNERS.slice(1).join(",") });
    expect(c.problems).toEqual(["NEXT_PUBLIC_SIGNERS is set but is not 5 distinct addresses separated by commas."]);
  });
});

describe("GET /config is a consistency check, never a source of addresses (H-6)", () => {
  it("pins that agree with the backend: everything runs, with the backend's non-security values", () => {
    const c = buildConfig(DRY_RUN_ENV, remote({ denominations: [1_000_000n, 5_000_000n, 10_000_000n] }));
    expect(c).toMatchObject({
      registry: DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY,
      adapter: DRY_RUN_ENV.NEXT_PUBLIC_ADAPTER,
      contractsDeployed: true,
      ownerSigning: true,
      sponsorship: true,
      denominations: [1_000_000n, 5_000_000n, 10_000_000n],
      confirmations: 3,
      vaultExpiryBlocks: 288,
      openVaultStampBits: 20,
      remote: "loaded",
      problems: [],
    });
    expect(configIssues(c)).toEqual([]);
  });

  it("a compromised backend's consistent config cannot redirect the account's authorizations", () => {
    // Adapter = attacker everywhere the backend reports it: the app would otherwise sign
    // Morpho.setAuthorization(attacker, true).
    const c = buildConfig(DRY_RUN_ENV, remote({ adapter: ATTACKER, contracts: { ...REMOTE.contracts!, SettlementAdapter: ATTACKER } }));
    expect(c.adapter).toBe(DRY_RUN_ENV.NEXT_PUBLIC_ADAPTER);
    expect(c).toMatchObject({ contractsDeployed: false, ownerSigning: false });
    expect(configIssues(c)).toEqual([
      "This build pins NEXT_PUBLIC_ADAPTER to 0xC0a2…00fc, but the backend's signer network's Settlement Adapter is 0x5555…5555.",
      HARD_STOP,
    ]);
    // Morpho = attacker (USDC.approve(attacker) during a repay): the built-in pin holds too.
    const morpho = buildConfig(DRY_RUN_ENV, remote({ market: { ...REMOTE.market!, morphoBlue: ATTACKER } }));
    expect(morpho.morpho).toBe("0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
    expect(morpho.problems).toEqual([
      "This build pins NEXT_PUBLIC_MORPHO to 0xBBBB…FFCb, but the backend's Morpho Blue is 0x5555…5555.",
    ]);
  });

  it("checks every value the backend reports, the deployment file included", () => {
    const c = buildConfig(DRY_RUN_ENV, remote({ contracts: { ...REMOTE.contracts!, ZecuredRegistry: ATTACKER } }));
    expect(c.problems).toEqual([
      "This build pins NEXT_PUBLIC_REGISTRY to 0xbdB9…3AAc, but the backend's deployed ZecuredRegistry is 0x5555…5555.",
    ]);
    const signers = buildConfig(DRY_RUN_ENV, remote({ signers: [...SIGNERS.slice(0, 4), ATTACKER] as `0x${string}`[] }));
    expect(signers.problems).toEqual(["This build pins NEXT_PUBLIC_SIGNERS to a different signer set than the backend's."]);
    expect(buildConfig(DRY_RUN_ENV, remote({ signers: SIGNERS as `0x${string}`[] })).problems).toEqual([]);
  });

  it("rejects a backend configured for another chain or another market", () => {
    const chain = buildConfig(DRY_RUN_ENV, remote({ chainId: 8453 }));
    expect(chain.problems).toEqual([`The backend is configured for chain 8453, this app for Base Sepolia (${CHAIN_ID}).`]);
    expect(chain).toMatchObject({ sponsorship: false, contractsDeployed: false, confirmations: 3 });

    const market = { ...REMOTE.market!, lltv: 500_000_000_000_000_000n };
    const lltv = buildConfig(DRY_RUN_ENV, remote({ market: { ...market, marketId: marketIdOf(market) } }));
    expect(lltv.problems).toContain("The backend's market has an LLTV of 50 %; Zecured uses 62.5 %.");
    const id = buildConfig(DRY_RUN_ENV, remote({ market: { ...REMOTE.market!, marketId: `0x${"12".repeat(32)}` } }));
    expect(id.problems).toContain("The backend's market id does not match its market parameters.");
  });

  it("a production build never takes missing pins from the backend", () => {
    const c = buildConfig({}, REMOTE);
    expect(c.registry).toBe(PLACEHOLDER_CONTRACT);
    expect(c).toMatchObject({ contractsDeployed: false, ownerSigning: false, sponsorship: true });
    expect(configIssues(c)).toEqual([
      `This build does not pin ${ALL_PINS}. Owner signatures, Base reads and sponsored transactions stay disabled until the app is rebuilt with them (see app/.env.example).`,
    ]);
  });

  it("says clearly when sponsored transactions are unavailable", () => {
    const c = buildConfig(DRY_RUN_ENV, remote({ sponsorship: false }));
    expect(c).toMatchObject({ contractsDeployed: true, sponsorship: false });
    expect(configIssues(c)).toEqual([
      "Sponsored transactions are unavailable: the backend has no bundler and paymaster configured. Borrowing, repaying and exits are disabled; off-chain steps and Base reads work.",
    ]);
  });

  it("an unreachable backend leaves the pins in force, without sponsorship", () => {
    const c = buildConfig(DRY_RUN_ENV, { error: "Cannot reach the backend at http://127.0.0.1:8080" });
    expect(c).toMatchObject({ remote: "unavailable", contractsDeployed: true, ownerSigning: true, sponsorship: false });
    expect(configIssues(c)).toEqual([
      "The backend's configuration (GET /config) could not be loaded (Cannot reach the backend at http://127.0.0.1:8080). Sponsored transactions are disabled until it answers.",
    ]);
  });
});

describe("development builds (next dev) may run unpinned", () => {
  it("takes unpinned values from the backend and says so", () => {
    const c = buildConfig(DEV, REMOTE);
    expect(c).toMatchObject({
      development: true,
      registry: DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY,
      adapter: DRY_RUN_ENV.NEXT_PUBLIC_ADAPTER,
      usdc: DRY_RUN_ENV.NEXT_PUBLIC_USDC,
      marketId: DRY_RUN_ENV.NEXT_PUBLIC_MARKET_ID,
      contractsDeployed: true,
      ownerSigning: true,
      signers: null,
    });
    expect(configIssues(c)).toEqual([
      `Unpinned development configuration: this build does not pin ${ALL_PINS}, so the app takes them from the backend's GET /config. A production build must pin them.`,
    ]);
    // The backend's signer set fills the gap, marked as not pinned (lib/signers.ts says so in the UI).
    const fromBackend = buildConfig(DEV, remote({ signers: SIGNERS as `0x${string}`[] }));
    expect(fromBackend).toMatchObject({ signers: SIGNERS, signersPinned: false });
  });

  it("still refuses an inconsistent backend, and pins still win", () => {
    const inconsistent = buildConfig(DEV, remote({ registry: ATTACKER }));
    expect(inconsistent.problems).toEqual([
      "The backend is inconsistent: its signer network's Registry is 0x5555…5555, its deployed ZecuredRegistry 0xbdB9…3AAc.",
    ]);
    expect(inconsistent.contractsDeployed).toBe(false);
    const pinned = buildConfig({ ...DEV, NEXT_PUBLIC_REGISTRY: ATTACKER }, REMOTE);
    expect(pinned.registry).toBe(ATTACKER);
    expect(pinned).toMatchObject({ contractsDeployed: false, ownerSigning: false });
  });

  it("before deployment, or without a backend: off-chain steps only", () => {
    const empty = buildConfig(
      DEV,
      remote({ registry: PLACEHOLDER_CONTRACT, adapter: PLACEHOLDER_CONTRACT, contracts: null, market: null, sponsorship: false }),
    );
    expect(empty).toMatchObject({ registry: PLACEHOLDER_CONTRACT, contractsDeployed: false, ownerSigning: true });
    expect(configIssues(empty)[1]).toBe(
      "The backend reports no Base deployment yet. Off-chain steps work; Base reads and sponsored transactions are disabled.",
    );
    const offline = buildConfig(DEV, { error: "timeout" });
    expect(offline).toMatchObject({ registry: PLACEHOLDER_CONTRACT, ownerSigning: true, contractsDeployed: false });
    expect(configIssues(offline)).toHaveLength(3);
  });
});

describe("runtimeConfigLoader", () => {
  it("fetches once, shares the promise and updates the target in place", async () => {
    const target = buildConfig(DRY_RUN_ENV);
    const fetchRemote = vi.fn(async () => REMOTE);
    const load = runtimeConfigLoader(fetchRemote, { target, env: DRY_RUN_ENV });
    const [a, b] = await Promise.all([load(), load()]);
    expect(await load()).toBe(target);
    expect(a).toBe(target);
    expect(b).toBe(target);
    expect(fetchRemote).toHaveBeenCalledTimes(1);
    expect(target).toMatchObject({ remote: "loaded", sponsorship: true, confirmations: 3, problems: [] });
  });

  it("never rejects: a failing backend leaves the pins, marked unavailable", async () => {
    const target = buildConfig(DRY_RUN_ENV);
    const load = runtimeConfigLoader(() => Promise.reject(new Error("Cannot reach the backend")), { target, env: DRY_RUN_ENV });
    await expect(load()).resolves.toBe(target);
    expect(target).toMatchObject({ remote: "unavailable", remoteError: "Cannot reach the backend", sponsorship: false });
    expect(target.registry).toBe(DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY);
  });

  it("gives up after the timeout and renders with the build's configuration", async () => {
    vi.useFakeTimers();
    const target = buildConfig({});
    const load = runtimeConfigLoader(() => new Promise<RemoteConfig>(() => {}), { target, env: {} });
    const done = load();
    await vi.advanceTimersByTimeAsync(RUNTIME_CONFIG_TIMEOUT_MS - 1);
    expect(target.remote).toBe("pending");
    await vi.advanceTimersByTimeAsync(1);
    await done;
    expect(target.remote).toBe("unavailable");
    expect(target.remoteError).toBe("no answer within 6 s");
  });
});
