import { describe, expect, it } from "vitest";
import { DRY_RUN_ENV } from "./__fixtures__/deployment";
import { buildConfig, configIssues, marketIdOf, marketParams, PLACEHOLDER_CONTRACT } from "./config";

describe("buildConfig", () => {
  it("falls back to placeholders and flags the contracts as not deployed", () => {
    const c = buildConfig({});
    expect(c.backendUrl).toBe("http://127.0.0.1:8080");
    expect(c.registry).toBe(PLACEHOLDER_CONTRACT);
    expect(c.adapter).toBe(PLACEHOLDER_CONTRACT);
    expect(c.morpho).toBe("0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
    expect(c.contractsDeployed).toBe(false);
    expect(configIssues(c)).toHaveLength(1);
  });

  it("reads deployed addresses; the market id matches Morpho's MarketParamsLib.id", () => {
    const c = buildConfig(DRY_RUN_ENV);
    expect(c.contractsDeployed).toBe(true);
    // The dry run's marketId was computed on-chain by the Registry constructor.
    expect(marketIdOf(marketParams(c))).toBe(DRY_RUN_ENV.NEXT_PUBLIC_MARKET_ID);
    expect(configIssues(c)).toEqual([]);
    expect(configIssues({ ...c, oracle: c.usdc })).toEqual([
      "NEXT_PUBLIC_MARKET_ID does not match the configured market parameters.",
    ]);
  });

  it("normalizes and validates values", () => {
    const c = buildConfig({
      NEXT_PUBLIC_BACKEND_URL: " https://api.example.test/ ",
      NEXT_PUBLIC_REGISTRY: DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY.toLowerCase(),
      NEXT_PUBLIC_ADAPTER: "not-an-address",
      NEXT_PUBLIC_MARKET_ID: "0x1234",
    });
    expect(c.backendUrl).toBe("https://api.example.test");
    expect(c.registry).toBe(DRY_RUN_ENV.NEXT_PUBLIC_REGISTRY);
    expect(c.adapter).toBe(PLACEHOLDER_CONTRACT);
    expect(c.contractsDeployed).toBe(false);
    expect(buildConfig({ NEXT_PUBLIC_BACKEND_URL: "javascript:alert(1)" }).backendUrl).toBe("http://127.0.0.1:8080");
  });
});
