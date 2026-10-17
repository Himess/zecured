import { describe, expect, it } from "vitest";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { parseRemoteConfig } from "./api";
import { buildConfig } from "./config";
import { deploymentProfile } from "./deployment";
import { loanTokenName, priceSourceName, realAndMock } from "./labels";

const testnet = deploymentProfile("testnet").profile;
const beta = deploymentProfile("mainnet-beta").profile;

describe("names from the deployment (section 9.3 of the v4 design)", () => {
  it("Circle's test USDC is USDC, real; only a fork's MockUSDC is called a mock", () => {
    const judge = buildConfig(DRY_RUN_ENV, parseRemoteConfig(DRY_RUN_CONFIG));
    expect(loanTokenName(judge)).toBe("USDC");
    expect(priceSourceName(judge)).toBe("DemoShockOracle, DEMO");
    const list = realAndMock(judge, testnet);
    expect(list.real.join(" ")).toMatch(/Circle's test USDC on Base Sepolia/);
    expect(list.mock.join(" ")).not.toMatch(/USDC/);
    expect(list.mock.join(" ")).toMatch(/MockZecOracle behind the DemoShockOracle \(DEMO\)/);
    expect(list.mock.join(" ")).toMatch(/Signer independence/);
    const fork = buildConfig(
      DRY_RUN_ENV,
      parseRemoteConfig({ ...DRY_RUN_CONFIG, contracts: { ...DRY_RUN_CONFIG.contracts, LoanToken: undefined, MockUSDC: DRY_RUN_CONFIG.contracts.LoanToken } }),
    );
    expect(loanTokenName(fork)).toBe("MockUSDC");
    expect(realAndMock(fork, testnet).mock.join(" ")).toMatch(/MockUSDC \(MOCK\)/);
  });

  it("before GET /config answers, the generic names; the mainnet beta's list names Base and Zcash mainnet", () => {
    const bare = buildConfig({});
    expect(loanTokenName(bare)).toBe("USDC");
    expect(priceSourceName(bare)).toBe("the market's oracle");
    const list = realAndMock(buildConfig({ NEXT_PUBLIC_ZECURED_DEPLOYMENT: "mainnet-beta" }), beta);
    expect(list.real[0]).toMatch(/Zcash mainnet/);
    expect(list.real.join(" ")).toMatch(/Morpho Blue market on Base,/);
    expect(list.real.join(" ")).toMatch(/minting on a zero-knowledge proof/i);
  });
});
