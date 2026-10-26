/**
 * The recording a replay build plays back (schema `zecured-replay/1`, lib/replay/recording.json):
 * the live Base Sepolia run's deployment, the loan and the swap it made, and every step with the
 * transactions that made it. Everything in it is public on purpose: chain data, the signers'
 * signatures over a disposable demo vault, explorer links. Never a private key, a salt or a
 * viewing key (the replay's loan carries a dummy owner key instead: lib/replay/loans.ts).
 *
 * Amounts are decimal strings (zatoshi, or USDC units with 6 decimals), hashes `0x` hex, Zcash
 * txids 64 hex digits in display order.
 */
import type { Address, Hex } from "viem";

export const REPLAY_SCHEMA = "zecured-replay/1";

/** The two independent scripts of the run: a loan (open → borrow → close) and a swap. */
export type ReplayTrack = "loan" | "swap";

/** Every step of the run, in the order the recording lists them. */
export const REPLAY_STEP_IDS = [
  "opened",
  "deposit-paid",
  "deposit-seen",
  "proven",
  "activated",
  "borrowed",
  "destination",
  "close-executed",
  "payouts-committed",
  "payouts-sent",
  "payouts-confirmed",
  "swap-opened",
  "swap-paid",
  "swap-proven",
  "swap-settled",
] as const;
export type ReplayStepId = (typeof REPLAY_STEP_IDS)[number];

/** What a step left behind, as the replay's dock links it: a transaction, a proof, or signatures. */
export interface ReplayArtifact {
  label: string;
  /** Where it lives; `offchain` for the signers' signatures. */
  chain: "base" | "zcash" | "offchain";
  /** Base: `0x` + 64 hex; Zcash: 64 hex in display order. Null in the placeholder recording (no live run yet). */
  tx: string | null;
  kind: "tx" | "proof" | "signatures";
  /** Facts shown with it (an image id, the prover, how many signers signed…). */
  meta?: Record<string, string>;
}

export interface ReplayStep {
  id: ReplayStepId;
  track: ReplayTrack;
  title: string;
  /** One plain sentence on what happened. */
  detail: string;
  /** What the judge does for a manual step, or what happens meanwhile for an automatic one. */
  hint: string;
  /** The app page where it happens. */
  href: string;
  /** False: the judge's action in the app makes it happen. True: it follows the previous step of its track. */
  auto: boolean;
  /** Automatic steps: how long after the previous step of its track it happens in the replay (accelerated). */
  delayMs: number;
  /** When it happened in the live run (ISO 8601), and how long it took there after the previous step. */
  at: string | null;
  durationSeconds: number | null;
  /**
   * The hashes the app itself shows for this step: the Base transaction (and the swap's approval,
   * the UserOp hash), or the Zcash transaction. Real in a recording, obviously dummy in the placeholder.
   */
  tx: { base?: Hex; zcash?: string; userOp?: Hex; approve?: Hex };
  artifacts: ReplayArtifact[];
}

/** `GET /config` exactly as the backend serves it (lib/api.ts `parseRemoteConfig`). */
export interface ReplayConfigWire {
  chainId: number;
  registry: string;
  adapter: string;
  intake: string;
  confirmations: number;
  vaultExpiryBlocks: number;
  openVaultStampBits: number;
  sponsorship: boolean;
  /** Only the recorded vault's size: the replay can open nothing else. */
  denominations: string[];
  signers: string[];
  openings: { open: boolean; inviteOnly: boolean };
  contracts: Record<string, string>;
  market: {
    collateralToken: string;
    irm: string;
    lltv: string;
    loanToken: string;
    marketId: string;
    morphoBlue: string;
    oracle: string;
  };
  v3: {
    feeBps: number;
    minFeeZat: number;
    minRefundZat: number;
    refundAbandonPeriod: number;
    pendingRefundDelay: number;
    maxPause: number;
    livenessPeriod: number;
    commitGrace: number;
    livenessPremiumBps: number;
    coverageBps: number;
    maxTotalZat: number;
    incidentWindow: number;
  };
}

/** A verifier version as `GET /status` lists it. */
export interface ReplayVersion {
  versionId: Hex;
  selector: Hex;
  imageId: Hex;
  branches: number[];
  activeAt: number;
  active: boolean;
  removed: boolean;
}

export interface ReplayRecording {
  schema: typeof REPLAY_SCHEMA;
  /** True until a live run replaces it: its hashes are dummies and nothing links to an explorer. */
  placeholder: boolean;
  /** When the live run was recorded (ISO 8601); null for the placeholder. */
  recordedAt: string | null;
  /** One sentence for the dock: what this recording is. */
  note: string;
  network: { base: "base-sepolia"; zcash: "zcash-testnet"; chainId: number };
  /** Explorer prefixes the dock links to (a transaction hash is appended); Zcash may have none. */
  explorers: { base: string; zcash: string | null };
  config: ReplayConfigWire;
  /** The contracts the app reads through the Registry and the Intake (not in GET /config). */
  links: {
    relay: Address;
    disputeWindow: number;
    inclusionWindow: number;
    refundDormancy: number;
    maxMarginBps: number;
    /** The opening fee's recipient commitment (the signers' treasury), as the Fee claim carries it. */
    feeCommitment: Hex;
  };
  /** `GET /status`'s fixed part. */
  status: {
    commit: string;
    treeClean: boolean;
    deployment: {
      chainId: number;
      registry: Address;
      intake: Address;
      signerBond: Address;
      mintMode: string;
      verifierIsMock: boolean;
      depositImageId: Hex;
      violationImageId: Hex;
    };
    versions: Record<string, ReplayVersion[]>;
  };
  /** The chain as the replay starts: everything that is not the recorded loan or swap. */
  chain: {
    /** Base block and Zcash tip when a replay starts (both then advance with the clock). */
    baseBlock: number;
    zcashTip: number;
    /** The market oracle's price (USDC units per zatoshi × 1e36, i.e. USD per ZEC × 1e34). */
    price: string;
    /** The IRM's borrow rate, per second, WAD. */
    borrowRate: string;
    /** Morpho's market before the recorded loan. */
    market: { totalSupplyAssets: string; totalSupplyShares: string; totalBorrowAssets: string; totalBorrowShares: string; fee: string };
    /** Other vaults: their locked zatoshi and how many are open, per denomination. */
    otherLockedZat: string;
    otherOpenVaults: Record<string, number>;
    bonds: { each: string[]; effective: string; totalSlashable: string; minBond: string };
    relayChainWork: string;
  };
  loan: {
    owner: Address;
    account: Address;
    requestId: Hex;
    /** zatoshi */
    amount: string;
    vault: {
      vaultId: Hex;
      /** The vault's Ironwood-only unified address (testnet). */
      address: string;
      zip321: string;
      expiryHeight: number;
      birthdayHeight: number;
      tagKeyHash: Hex;
      signatures: { index: number; signature: Hex }[];
    };
    depositTag: Hex;
    /** The Zcash block the deposit proof is anchored to, and the deposit verifier version. */
    anchor: Hex;
    versionId: Hex;
    borrow: { assets: string; protected: boolean };
    close: {
      executor: Address;
      marginBps: number;
      repayAssets: string;
      mmAmount: string;
      userAmount: string;
      userCommitment: Hex;
      mmCommitment: Hex;
      /** The payout transaction's reference on Base, and its input tag (`claimInput`). */
      payoutRef: Hex;
      inputTag: Hex;
    };
  };
  demo: { returnAddress: string; stampBits: number; dailyCapZat: string };
  swap: {
    swap: Address;
    maker: Address;
    /** The wallet the recorded run bought with: the replay's wallet is this address. */
    taker: Address;
    takerUsdc: string;
    zatoshi: string;
    usdc: string;
    makerReceiverCommitment: Hex;
    receiverCommitment: Hex;
    payWindow: number;
    nonce: string;
    orderId: Hex;
    anchor: Hex;
    /**
     * `demo-key`: the replay signs each fresh quote with a DEMO maker key (the placeholder's maker
     * is that key's address). `erc1271`: the recorded maker's signature, which the replay's Base
     * accepts through ERC-1271 at the maker's address (a fresh quote needs a new deadline).
     */
    signing: "demo-key" | "erc1271";
    signature: Hex | null;
  };
  evidence: {
    commit: string;
    tag: string | null;
    deployedAtBlock: number | null;
    mocks: { name: string; address: Address | null; label: "MOCK" | "DEMO" | "MOCK / DEMO"; why: string }[];
    leakScan: { result: string; fromBlock: number; toBlock: number; identifiers: number; at: string } | null;
  };
  steps: ReplayStep[];
}
