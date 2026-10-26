/**
 * A replay build's pins (lib/config.ts `BUILD_ENV`): the recorded deployment's addresses and
 * signer set, exactly as `NEXT_PUBLIC_*` variables would pin them, so the app checks the replay's
 * GET /config, and the signers' signatures on the recorded vault, as strictly as a live build.
 */
import { REPLAY_BASE_URL } from "./flag";
import { recording } from "./recording";
import type { ReplayRecording } from "./types";

export function replayBuildEnv(nodeEnv: string | undefined, r: ReplayRecording = recording()): Record<string, string | undefined> {
  const c = r.config;
  return {
    NODE_ENV: nodeEnv,
    NEXT_PUBLIC_BACKEND_URL: REPLAY_BASE_URL,
    NEXT_PUBLIC_REGISTRY: c.registry,
    NEXT_PUBLIC_ADAPTER: c.adapter,
    NEXT_PUBLIC_INTAKE: c.intake,
    NEXT_PUBLIC_MORPHO: c.market.morphoBlue,
    NEXT_PUBLIC_MARKET_ID: c.market.marketId,
    NEXT_PUBLIC_USDC: c.market.loanToken,
    NEXT_PUBLIC_ORACLE: c.market.oracle,
    NEXT_PUBLIC_ZZEC: c.market.collateralToken,
    NEXT_PUBLIC_IRM: c.market.irm,
    NEXT_PUBLIC_SIGNERS: c.signers.join(","),
    NEXT_PUBLIC_SWAP: r.swap.swap,
    // P-1: the demo wallet's payment, replayed.
    NEXT_PUBLIC_DEMO_PATH: "1",
    NEXT_PUBLIC_ZECURED_DEPLOYMENT: "testnet",
  };
}
