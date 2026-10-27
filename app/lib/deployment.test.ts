import { describe, expect, it } from "vitest";
import { base, baseSepolia } from "viem/chains";
import { DEPLOYMENT, DEPLOYMENT_VALID, deploymentProfile, MAINNET_BETA_BAND, viemChain } from "./deployment";

describe("the deployment a build serves (NEXT_PUBLIC_ZECURED_DEPLOYMENT)", () => {
  it("is the judge deployment on testnets by default: Base Sepolia, Zcash testnet, no band, no invites", () => {
    for (const raw of [undefined, "", " testnet ", "TESTNET"]) {
      const { profile, valid } = deploymentProfile(raw);
      expect(valid).toBe(true);
      expect(profile).toMatchObject({ kind: "testnet", chainId: 84532, zcashNetwork: "test", ticker: "TAZ", band: null, invites: false });
      expect(profile.demoAllowed).toBe(true);
    }
    // The unit tests run without the variable.
    expect(DEPLOYMENT.kind).toBe("testnet");
    expect(DEPLOYMENT_VALID).toBe(true);
  });

  it("the mainnet beta: Base, Zcash mainnet, the band on every page, invite codes, never the demo path", () => {
    const { profile, valid } = deploymentProfile("mainnet-beta");
    expect(valid).toBe(true);
    expect(profile).toMatchObject({ kind: "mainnet-beta", chainId: 8453, zcashNetwork: "main", ticker: "ZEC", invites: true, demoAllowed: false });
    expect(profile.band).toBe(MAINNET_BETA_BAND);
    expect(MAINNET_BETA_BAND).toBe("Mainnet beta — unaudited — invite only — small caps");
    // Circle's USDC on each chain (docs/ref/FACTS.md) and the beta's sizes.
    expect(profile.circleUsdc).toBe("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    expect(deploymentProfile(undefined).profile.circleUsdc).toBe("0x036CbD53842c5426634e7929541eC2318f3dCF7e");
    expect(profile.defaultDenominations).toEqual([5_000_000n, 10_000_000n]);
  });

  it("links explorers on the judge testnet only: the mainnet beta shows ids, since an explorer learns what one looks at", () => {
    expect(deploymentProfile("testnet").profile).toMatchObject({
      baseExplorerTx: "https://sepolia.basescan.org/tx/",
      zcashExplorerTx: "https://testnet.zecblock.com/tx/",
    });
    expect(deploymentProfile("mainnet-beta").profile).toMatchObject({ baseExplorerTx: null, zcashExplorerTx: null });
  });

  it("anything else is not a deployment: the config reports it rather than guess a chain", () => {
    for (const raw of ["mainnet", "beta", "base"]) {
      const { profile, valid } = deploymentProfile(raw);
      expect(valid).toBe(false);
      expect(profile.kind).toBe("testnet");
    }
  });

  it("viem's chain follows it", () => {
    expect(viemChain(deploymentProfile("testnet").profile).id).toBe(baseSepolia.id);
    expect(viemChain(deploymentProfile("mainnet-beta").profile).id).toBe(base.id);
  });
});
