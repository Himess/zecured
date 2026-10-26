/**
 * The replay's `fetch` (DEMO): lib/api.ts and the viem transport of lib/chain.ts call it instead of
 * the network in a replay build. Requests to the replay's backend URL are answered in the page
 * (`/proxy/rpc` by the replay's Base, everything else by its backend); the bundler and the
 * paymaster are never reached (the replay's loan account sends nothing to them), and any other
 * URL is refused: nothing ever leaves the page.
 */
import { REPLAY_BASE_URL } from "./flag";
import { handleRest } from "./rest";
import { handleRpc } from "./rpc";
import { world, type ReplayWorld } from "./world";

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

async function bodyOf(input: string | URL | Request, init?: RequestInit): Promise<unknown> {
  const raw = init?.body ?? (input instanceof Request ? await input.clone().text() : undefined);
  if (raw === undefined || raw === null) return undefined;
  const text = typeof raw === "string" ? raw : raw instanceof Uint8Array ? new TextDecoder().decode(raw) : String(raw);
  if (!text) return undefined;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

/** A `fetch` bound to `w` (tests make their own world; the app uses the page's). */
export function createReplayFetch(w?: ReplayWorld): (input: string | URL | Request, init?: RequestInit) => Promise<Response> {
  return async (input, init) => {
    const target = w ?? world();
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
    if (url.origin !== new URL(REPLAY_BASE_URL).origin) {
      // Never the network: a request for anything else is a bug in the replay.
      throw new TypeError(`The replay does not fetch ${url.origin}.`);
    }
    init?.signal?.throwIfAborted();
    const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    const body = await bodyOf(input, init);
    if (url.pathname === "/proxy/rpc") {
      await target.latency(60);
      return json(200, handleRpc(target, body));
    }
    if (url.pathname === "/proxy/bundler" || url.pathname === "/proxy/paymaster") {
      return json(200, { jsonrpc: "2.0", id: null, error: { code: -32601, message: "The replay sends no UserOp: its loan account answers in the page." } });
    }
    const answer = await handleRest(target, method, url.pathname, url.searchParams, body);
    return json(answer.status, answer.json);
  };
}

/** The page's replay fetch (lib/api.ts, lib/chain.ts). */
export const replayFetch = createReplayFetch();
