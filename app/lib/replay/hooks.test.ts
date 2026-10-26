import { afterEach, describe, expect, it, vi } from "vitest";
import { getAddress } from "viem";
import { REPLAY_BASE_URL } from "./flag";
import { REPLAY_OWNER_KEY } from "./demo-keys";
import raw from "./recording.json";

// The app's modules are imported only after the variable is set (each test reloads them): their
// replay hook points read NEXT_PUBLIC_REPLAY when they load, as Next.js inlines it at build time.
async function replayBuild() {
  vi.stubEnv("NEXT_PUBLIC_REPLAY", "1");
  vi.resetModules();
  return {
    api: await import("../api"),
    config: await import("../config"),
    account: await import("../account"),
    keys: await import("../keys"),
    chain: await import("../chain"),
    abi: await import("../abi"),
  };
}

afterEach(() => {
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
  vi.resetModules();
});

describe("a replay build's hook points", () => {
  it("pin the recorded deployment and answer in the page, never on the network", async () => {
    const network = vi.spyOn(globalThis, "fetch");
    const { api, config, account, keys, chain, abi } = await replayBuild();
    expect(api.api.baseUrl).toBe(REPLAY_BASE_URL);
    expect(config.config.signersPinned).toBe(true);
    expect(config.config.registry).toBe(getAddress(raw.config.registry));
    await api.loadRuntimeConfig();
    expect(config.configIssues()).toEqual([]);
    expect(await api.api.health()).toBe(true);
    const price = await chain.publicClient().readContract({ address: config.config.oracle, abi: abi.oracleAbi, functionName: "price" });
    expect(price).toBe(BigInt(raw.chain.price));

    // The loan is the recorded one, with the dummy key; its account is the recorded account.
    const loan = keys.createLoan(BigInt(raw.loan.amount));
    expect([loan.ownerKey, loan.owner, loan.account]).toEqual([REPLAY_OWNER_KEY, getAddress(raw.loan.owner), getAddress(raw.loan.account)]);
    expect(keys.parseLoan(JSON.parse(JSON.stringify(loan))).account).toBe(getAddress(raw.loan.account));
    // A real loan's backup (a key that is not the replay's) is refused.
    expect(() => keys.parseLoan({ ...loan, ownerKey: `0x${"11".repeat(32)}` })).toThrow(/only its own demo loan/);
    expect(account.loanAccount(loan.ownerKey).address).toBe(getAddress(raw.loan.account));
    expect(network).not.toHaveBeenCalled();
  });

  it("leave a live build untouched", async () => {
    vi.resetModules();
    const keys = await import("../keys");
    const { api } = await import("../api");
    expect(api.baseUrl).not.toBe(REPLAY_BASE_URL);
    const loan = keys.createLoan(5_000_000n);
    expect(loan.ownerKey).not.toBe(REPLAY_OWNER_KEY);
    expect(() => keys.parseLoan({ ...loan, ownerKey: REPLAY_OWNER_KEY })).toThrow(/does not match the owner key/);
  });
});
