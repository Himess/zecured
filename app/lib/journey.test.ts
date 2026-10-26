import { describe, expect, it } from "vitest";
import { createPublicClient, http, type PublicClient } from "viem";
import { baseSepolia } from "viem/chains";
import { createApi } from "./api";
import { buildConfig } from "./config";
import { readJourney } from "./journey";
import { createReplayFetch } from "./replay/fetch";
import { REPLAY_BASE_URL } from "./replay/flag";
import { replayBuildEnv } from "./replay/pins";
import { recording } from "./replay/recording";
import { ReplayWorld } from "./replay/world";

const rec = recording();

/** The replay's chain on a hand-driven clock: Base as the recorded loan left it, read through the app's own client. */
async function setup() {
  const clock = { t: 1_800_000_000_000 };
  const w = new ReplayWorld(rec, { now: () => clock.t, latency: false, storage: null });
  const fetchFn = createReplayFetch(w);
  const api = createApi({ baseUrl: REPLAY_BASE_URL, fetch: fetchFn });
  const config = buildConfig(replayBuildEnv("production", rec), await api.getConfig());
  const client = createPublicClient({
    chain: baseSepolia,
    cacheTime: 0,
    transport: http(`${REPLAY_BASE_URL}/proxy/rpc`, { fetchFn, retryCount: 0 }),
  }) as PublicClient;
  return { w, clock, deps: { client, config } };
}

describe("a vault's proof trail", () => {
  it("reads each step of the loan from its events on Base, in order, with its transaction", async () => {
    const { w, clock, deps } = await setup();
    const vault = rec.loan.vault.vaultId;
    const account = rec.loan.account;
    expect((await readJourney(vault, account, deps)).steps).toEqual([]);

    // The loan's whole script: the judge's steps by hand, ten seconds apart, the automatic ones by the clock.
    for (const s of w.track("loan")) {
      if (s.auto) {
        clock.t += s.delayMs + 1;
      } else {
        w.expect(s.id);
        w.complete(s.id);
        clock.t += 10_000;
      }
    }

    const j = await readJourney(vault, account, deps);
    expect(j.steps.map((s) => s.kind)).toEqual(["proven", "activated", "borrowed", "close", "committed", "confirmed", "payout-proven"]);
    const tx = (id: Parameters<typeof w.step>[0]) => w.step(id).tx.base!.toLowerCase();
    expect(j.steps.map((s) => s.txHash.toLowerCase())).toEqual([
      tx("proven"),
      tx("activated"),
      tx("borrowed"),
      tx("close-executed"),
      tx("payouts-committed"),
      tx("payouts-confirmed"),
      tx("payouts-confirmed"),
    ]);
    // One payout's proof covers each of its claims: a single step that counts them.
    expect(j.steps.at(-1)!.detail).toMatch(/^\d+ claims$/);
    expect(j.steps.find((s) => s.kind === "committed")!.detail).toContain("signed by 3 of 5 signers");
    // Every step has its block's time.
    for (const s of j.steps) expect(j.times.get(s.blockNumber)).toBeGreaterThan(0);
    expect(j.complete).toBe(true);
  });

  it("finds nothing of another vault", async () => {
    const { w, clock, deps } = await setup();
    for (const s of w.track("loan")) {
      if (s.auto) clock.t += s.delayMs + 1;
      else {
        w.expect(s.id);
        w.complete(s.id);
      }
    }
    const other = `0x${"ab".repeat(32)}` as const;
    expect((await readJourney(other, null, { ...deps, maxChunks: 3 })).steps).toEqual([]);
  });
});
