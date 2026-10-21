/**
 * What the UI calls the market's loan token and price, and what it lists as real or mock, from the
 * deployment the backend reports (section 9.3 of the v4 design: "MockUSDC" only where the deployment
 * file says the loan token is the deployer's MockUSDC; Circle's test USDC on Base Sepolia, USDC on
 * the mainnet beta). Read below ConfigGate, at render time.
 */
import { config, type AppConfig } from "./config";
import { DEPLOYMENT, type DeploymentProfile } from "./deployment";

/** "USDC", or "MockUSDC" on a local fork. */
export function loanTokenName(c: AppConfig = config): string {
  return c.marketLabels?.loanToken ?? "USDC";
}

/** A short name of the market's price source, for "Price $X per ZEC (…)". */
export function priceSourceName(c: AppConfig = config): string {
  const l = c.marketLabels;
  if (!l) return c.deployment === "mainnet-beta" ? "the market's price feed" : "the market's oracle";
  if (l.oracleIsDemo) return "DemoShockOracle, DEMO";
  return c.deployment === "mainnet-beta" ? "ZecuredPriceFeed: Chainlink and API3" : "the market's oracle";
}

export interface RealOrMock {
  real: string[];
  mock: string[];
}

/** The home page's list, consistent with README "What is real and what is a mock" and this deployment. */
export function realAndMock(c: AppConfig = config, d: DeploymentProfile = DEPLOYMENT): RealOrMock {
  const testnet = d.kind === "testnet";
  const real = [
    testnet
      ? "Shielded Ironwood transactions on Zcash testnet (NU7), into 3-of-5 FROST threshold vaults."
      : "Shielded Ironwood transactions on Zcash mainnet, into 3-of-5 FROST threshold vaults.",
    "Minting on a zero-knowledge proof that the deposit is in a Zcash block, whose header Base verified (no signer's word).",
    `The Morpho Blue market on ${d.chainName}, the Zecured contracts, the signers' USDC bonds, slashing and compensation.`,
    "Our own RPC, bundler and paymaster proxy with a sponsorship allowlist: the loan account never holds ETH.",
  ];
  const l = c.marketLabels;
  const mock: string[] = [];
  if (!l || l.oracleIsDemo) {
    mock.push(
      testnet
        ? "The ZEC/USD price: an owner-set MockZecOracle behind the DemoShockOracle (DEMO), shocked by hand to show a liquidation."
        : "The ZEC/USD price: the deployment reports no real feed yet.",
    );
  }
  if (l?.loanTokenIsMock) mock.push("The loan token: MockUSDC (MOCK), minted by the deployer on the local fork.");
  else real.push(testnet ? "Circle's test USDC on Base Sepolia (testnet funds, no value)." : "Circle's USDC on Base.");
  mock.push(
    testnet
      ? "Signer independence: all five signers are run by the team; the bonds show the mechanism, not economic security."
      : "Signer independence: the five signers are run by the team during the beta; caps are kept small.",
  );
  return { real, mock };
}
