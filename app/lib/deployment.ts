/**
 * Which deployment this build serves, from `NEXT_PUBLIC_ZECURED_DEPLOYMENT` (baked into the
 * build like every pin, lib/config.ts):
 *
 * - `testnet` (the default): the judge deployment, on Base Sepolia (84532) and Zcash testnet (TAZ).
 * - `mainnet-beta`: the capped, invite-only mainnet beta (docs/MAINNET.md), on Base (8453) and
 *   Zcash mainnet (ZEC). Every page carries the beta band, opening a vault takes an invite code,
 *   and the demo path is off.
 *
 * Any other value is a configuration problem (lib/config.ts): the app then keeps Base disabled
 * rather than guess which chain it is on.
 */
import type { Address } from "viem";
import { base, baseSepolia } from "viem/chains";

export type DeploymentKind = "testnet" | "mainnet-beta";

export interface DeploymentProfile {
  kind: DeploymentKind;
  /** Base chain id of every EIP-712 domain, read and transaction. */
  chainId: number;
  /** For messages: "Base Sepolia" or "Base". */
  chainName: string;
  /** The Zcash network of every unified address the app decodes or builds. */
  zcashNetwork: "test" | "main";
  /** "TAZ" on Zcash testnet, "ZEC" on mainnet. */
  ticker: string;
  /** The band every page carries (mainnet beta only). */
  band: string | null;
  /** Opening a vault needs an invite code (mainnet beta only). */
  invites: boolean;
  /** The demo path (P-1) may exist in this build at all (never on mainnet). */
  demoAllowed: boolean;
  /** Circle's USDC on this chain (docs/ref/FACTS.md): the loan token the app names "USDC". */
  circleUsdc: Address;
  /** Collateral sizes before GET /config answers (its list wins), zatoshi. */
  defaultDenominations: readonly bigint[];
  /**
   * Where the app links a Base transaction (a block explorer's `/tx/` prefix), or null for no links. On the judge
   * testnet the trail of a vault links its proofs, so anyone can see where Base verified them; the mainnet beta
   * shows hashes only, since opening an explorer tells it which account one cares about.
   */
  baseExplorerTx: string | null;
}

/** The band of the mainnet beta (the v4 design's section 6), shown on every page. */
export const MAINNET_BETA_BAND = "Mainnet beta — unaudited — invite only — small caps";

const TESTNET: DeploymentProfile = {
  kind: "testnet",
  chainId: 84532,
  chainName: "Base Sepolia",
  zcashNetwork: "test",
  ticker: "TAZ",
  band: null,
  invites: false,
  demoAllowed: true,
  circleUsdc: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  // D7: 0.01 and 0.05 TAZ, and 0.125 TAZ, what the Valar testnet faucet pays (S13).
  defaultDenominations: [1_000_000n, 5_000_000n, 12_500_000n],
  baseExplorerTx: "https://sepolia.basescan.org/tx/",
};

const MAINNET_BETA: DeploymentProfile = {
  kind: "mainnet-beta",
  chainId: 8453,
  chainName: "Base",
  zcashNetwork: "main",
  ticker: "ZEC",
  band: MAINNET_BETA_BAND,
  invites: true,
  demoAllowed: false,
  circleUsdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  // The beta configuration's sizes: 0.05 and 0.1 ZEC.
  defaultDenominations: [5_000_000n, 10_000_000n],
  baseExplorerTx: null,
};

/**
 * The profile `raw` names: `mainnet-beta`, or `testnet` (also for an empty value). `valid` is false
 * for anything else, which lib/config.ts reports as a problem (Base stays disabled).
 */
export function deploymentProfile(raw: string | undefined): { profile: DeploymentProfile; valid: boolean } {
  const v = raw?.trim().toLowerCase() ?? "";
  if (v === "mainnet-beta") return { profile: MAINNET_BETA, valid: true };
  return { profile: TESTNET, valid: v === "" || v === "testnet" };
}

const BUILD = deploymentProfile(process.env.NEXT_PUBLIC_ZECURED_DEPLOYMENT);

/** This build's deployment. */
export const DEPLOYMENT: DeploymentProfile = BUILD.profile;
/** False when NEXT_PUBLIC_ZECURED_DEPLOYMENT holds something else than the two names above. */
export const DEPLOYMENT_VALID: boolean = BUILD.valid;

/** viem's chain for this build's Base network. */
export function viemChain(p: DeploymentProfile = DEPLOYMENT) {
  return p.kind === "mainnet-beta" ? base : baseSepolia;
}
