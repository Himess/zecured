/**
 * The replay's backend (DEMO): every route lib/api.ts calls, answered in the exact wire format its
 * parsers accept, from the recording and the replay's state (lib/replay/model.ts). The judge's
 * actions advance the replay only when they are the recorded run's next step; anything else gets
 * the backend's usual refusal shape `{ error, code }`, with a sentence naming what the run did
 * next. What the replay never does (a viewing key, the admin panel) is refused as unavailable.
 */
import { getAddress, hexToBytes, isAddress, isHex, type Address, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { commitment as commitTo } from "../commitment";
import { swapQuoteTypedData } from "../swap";
import { decodeUnifiedAddress } from "../ua";
import { demoKey } from "./demo-keys";
import { evidenceDocument } from "./evidence";
import { buildModel, kindName, type Model } from "./model";
import { OffScriptError, ReplayUnavailableError, type ReplayWorld } from "./world";

export interface RestAnswer {
  status: number;
  json: unknown;
}

type Json = Record<string, unknown>;

const ok = (json: unknown): RestAnswer => ({ status: 200, json });
const refuse = (status: number, error: string, code: string): RestAnswer => ({ status, json: { error, code } });

const nowSeconds = (w: ReplayWorld) => Math.floor(w.now() / 1000);

/** The Ironwood receiver of a testnet unified address, or a 400 refusal. */
function receiverOf(ua: unknown): Uint8Array | RestAnswer {
  if (typeof ua !== "string") return refuse(400, "The destination is not a unified address.", "bad_destination");
  try {
    const d = decodeUnifiedAddress(ua.trim());
    if (d.network !== "test" || !d.orchard) return refuse(400, "The destination has no Ironwood receiver on Zcash testnet.", "bad_destination");
    return d.orchard;
  } catch {
    return refuse(400, "The destination is not a valid unified address.", "bad_destination");
  }
}

/** The commitment the signers would return for `receiver` and `salt` (the app checks it is its own). */
function destinationCommitment(body: Json): Hex | RestAnswer {
  const receiver = receiverOf(body.receiver);
  if (!(receiver instanceof Uint8Array)) return receiver;
  if (typeof body.salt !== "string" || !isHex(body.salt) || body.salt.length !== 66) return refuse(400, "The salt is not 32 bytes.", "bad_destination");
  return commitTo(hexToBytes(body.salt), receiver);
}

function vaultJson(w: ReplayWorld, m: Model): Json {
  const r = w.rec;
  const status = m.confirmed
    ? "closed"
    : m.closed
      ? "closing"
      : m.activated
        ? "active"
        : m.seen
          ? "deposit_seen"
          : "awaiting_deposit";
  const out: Json = {
    vaultId: r.loan.vault.vaultId,
    status,
    amount: r.loan.amount,
    expiryHeight: r.loan.vault.expiryHeight,
    nonConforming: [],
    claims: m.claims.map((c) => ({
      claimId: c.claimId,
      kind: kindName(c.kind),
      amount: c.amount.toString(),
      status: m.confirmed ? "paid" : "pending",
      ...(m.confirmed ? { proven: true } : {}),
    })),
    address: r.loan.vault.address,
    outstanding: m.outstanding.toString(),
    payoutInFlight: m.committed && !m.confirmed,
    birthdayHeight: r.loan.vault.birthdayHeight,
    tipHeight: m.zcashTip,
    depositSeen: m.seen,
  };
  if (m.confirmations !== null) out.confirmations = m.confirmations;
  return out;
}

function statusJson(w: ReplayWorld, m: Model): Json {
  const r = w.rec;
  const capRoom = BigInt(r.config.v3.maxTotalZat) - m.lockedZat - m.pendingTotal;
  return {
    commit: r.status.commit,
    treeClean: r.status.treeClean,
    startedAt: Math.floor(w.epochMs / 1000),
    zcashTip: m.zcashTip,
    nodes: [0, 1, 2, 3, 4].map((index) => ({ index, mode: "normal" })),
    depositProofs: { queued: 0, running: m.proving ? 1 : 0, concurrency: 2 },
    minting: {
      paused: false,
      pausedUntil: 0,
      pausedSince: 0,
      halted: false,
      fullyBonded: true,
      capRoom: (capRoom > 0n ? capRoom : 0n).toString(),
      mintBlock: Object.fromEntries(r.config.denominations.map((d) => [d, "None"])),
      readAt: nowSeconds(w),
    },
    versions: r.status.versions,
    deployment: r.status.deployment,
  };
}

function vaultStatsJson(w: ReplayWorld, m: Model): Json {
  const r = w.rec;
  const byAmount: Record<string, number> = { ...r.chain.otherOpenVaults };
  const ours = m.proven && !(m.closed && m.confirmed);
  if (ours) byAmount[r.loan.amount] = (byAmount[r.loan.amount] ?? 0) + 1;
  return { byAmount, total: Object.values(byAmount).reduce((a, b) => a + b, 0), complete: true, readAt: nowSeconds(w) };
}

/** The maker's signed quote, with a fresh deadline (and, for the placeholder, a fresh DEMO signature). */
async function swapQuote(w: ReplayWorld, query: URLSearchParams): Promise<RestAnswer> {
  const r = w.rec;
  const s = r.swap;
  if (query.get("side") !== "buy") return refuse(400, "Only buying ZEC is part of this replay.", "replay_off_script");
  const usdc = query.get("usdc");
  const zatoshi = query.get("zatoshi");
  if ((usdc !== null && usdc !== s.usdc) || (zatoshi !== null && zatoshi !== s.zatoshi)) {
    const amount = `${Number(BigInt(s.usdc)) / 1e6}`;
    return refuse(409, `This replay buys with exactly ${amount} USDC, what the live run spent: enter ${amount}.`, "replay_off_script");
  }
  try {
    w.expect("swap-opened");
  } catch (e) {
    if (e instanceof OffScriptError) return refuse(409, e.message, e.code);
    throw e;
  }
  const takerParam = query.get("taker");
  const taker = takerParam && isAddress(takerParam, { strict: false }) ? getAddress(takerParam) : ("0x0000000000000000000000000000000000000000" as Address);
  const quote = {
    maker: s.maker,
    taker,
    side: 0,
    zatoshi: BigInt(s.zatoshi),
    usdc: BigInt(s.usdc),
    makerReceiverCommitment: s.makerReceiverCommitment,
    payWindow: s.payWindow,
    quoteDeadline: BigInt(nowSeconds(w) + 300),
    nonce: BigInt(s.nonce),
  };
  let signature: Hex;
  if (s.signing === "demo-key") {
    signature = await privateKeyToAccount(demoKey("swap maker")).signTypedData(swapQuoteTypedData(s.swap, quote, r.network.chainId));
  } else {
    // The recorded maker's signature; the replay's Base vouches for it at the maker's address (ERC-1271).
    signature = s.signature ?? (`0x${"00".repeat(65)}` as Hex);
  }
  await w.latency(400);
  return ok({
    quote: {
      maker: quote.maker,
      taker: quote.taker,
      side: quote.side,
      zatoshi: s.zatoshi,
      usdc: s.usdc,
      makerReceiverCommitment: quote.makerReceiverCommitment,
      payWindow: quote.payWindow,
      quoteDeadline: quote.quoteDeadline.toString(),
      nonce: s.nonce,
    },
    signature,
    swap: s.swap,
  });
}

/** One request to the replay's backend: `path` without the base URL, `body` parsed JSON (or undefined). */
export async function handleRest(w: ReplayWorld, method: string, path: string, query: URLSearchParams, body: unknown): Promise<RestAnswer> {
  try {
    return await route(w, method, path, query, (body && typeof body === "object" ? body : {}) as Json);
  } catch (e) {
    if (e instanceof OffScriptError) return refuse(409, e.message, e.code);
    if (e instanceof ReplayUnavailableError) return refuse(409, e.message, e.code);
    throw e;
  }
}

async function route(w: ReplayWorld, method: string, path: string, query: URLSearchParams, body: Json): Promise<RestAnswer> {
  const r = w.rec;
  const vaultId = r.loan.vault.vaultId.toLowerCase();
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  const m = () => buildModel(w);
  const get = method === "GET";
  const post = method === "POST";

  if (get && path === "/health") return ok({ ok: true });
  if (get && path === "/config") return ok(r.config);
  if (get && path === "/status") return ok(statusJson(w, m()));
  if (get && path === "/stats/vaults") return ok(vaultStatsJson(w, m()));
  if (get && path === "/evidence") return ok(evidenceDocument(r));
  if (parts[0] === "admin") return refuse(401, "The demo control panel is not part of the replay: it needs the operators' running backend.", "replay_unavailable");

  // ---------------------------------------------------------------- the demo wallet (P-1)
  if (get && path === "/demo/status") {
    return ok({
      enabled: true,
      returnAddress: r.demo.returnAddress,
      dailyCapZat: r.demo.dailyCapZat,
      paidTodayZat: w.isDone("deposit-paid") ? r.loan.amount : "0",
      lowBalance: false,
      stampBits: r.demo.stampBits,
    });
  }
  if (post && parts[0] === "demo" && parts[1] === "vaults" && parts[3] === "pay") {
    if (parts[2]?.toLowerCase() !== vaultId || !w.isDone("opened")) return refuse(404, "Unknown vault.", "not_found");
    if (w.isDone("deposit-paid")) return refuse(409, "The demo wallet already paid this vault.", "demo_already_paid");
    w.complete("deposit-paid");
    await w.latency(2_500);
    return ok({ txid: w.step("deposit-paid").tx.zcash, amount: r.loan.amount });
  }

  // ---------------------------------------------------------------- vaults
  if (post && path === "/vaults") {
    if (body.amount !== r.loan.amount) {
      return refuse(409, `This replay opens only the live run's vault, of ${Number(BigInt(r.loan.amount)) / 1e8} TAZ.`, "replay_off_script");
    }
    const same = (a: unknown, b: string) => typeof a === "string" && a.toLowerCase() === b.toLowerCase();
    if (!same(body.owner, r.loan.owner) || !same(body.account, r.loan.account)) {
      return refuse(409, "This replay opens only the live run's vault, for the loan it creates.", "replay_off_script");
    }
    // A retry of the same request returns the same vault, as the backend does.
    if (!w.isDone("opened")) w.complete("opened");
    await w.latency(2_500);
    const v = r.loan.vault;
    return ok({
      vaultId: v.vaultId,
      address: v.address,
      zip321: v.zip321,
      amount: r.loan.amount,
      expiryHeight: v.expiryHeight,
      tagKeyHash: v.tagKeyHash,
      signatures: v.signatures,
    });
  }
  if (parts[0] === "vaults" && parts[1]) {
    if (parts[1].toLowerCase() !== vaultId || !w.isDone("opened")) return refuse(404, "Unknown vault.", "not_found");
    if (get && parts.length === 2) {
      await w.latency(150);
      return ok(vaultJson(w, m()));
    }
    if (post && parts[2] === "destination") {
      const c = destinationCommitment(body);
      if (typeof c !== "string") return c;
      if (!w.isDone("destination")) w.complete("destination");
      await w.latency(1_200);
      return ok({ commitment: c });
    }
    if (post && parts[2] === "viewing-key") {
      throw new ReplayUnavailableError("The replay never publishes a vault's viewing key: in the live app, its owner fetches it here and watches the vault with any Zcash wallet.");
    }
    if (post && parts[2] === "prove-refund") throw new ReplayUnavailableError("Refunds are not part of the recorded run.");
  }
  if (post && parts[0] === "claims" && parts[2] === "receipt") return refuse(404, "No signer has acknowledged this claim's destination yet.", "no_receipt");

  // ---------------------------------------------------------------- the close from collateral (P6)
  if (get && parts[0] === "quotes" && parts[1] === "close") {
    if (parts[2]?.toLowerCase() !== vaultId) return refuse(404, "Unknown vault.", "not_found");
    const model = m();
    if (!model.borrowed || model.closed) {
      const next = w.pending("loan");
      throw new OffScriptError(
        next ? `The live run closed this loan after borrowing; its next step here is “${next.title}”.` : "This replay's loan is closed already.",
      );
    }
    const c = r.loan.close;
    await w.latency(500);
    return ok({
      executor: c.executor,
      marginBps: c.marginBps,
      repayAssets: c.repayAssets,
      mmAmount: c.mmAmount,
      userAmount: c.userAmount,
      expiresAt: nowSeconds(w) + 600,
    });
  }
  if (post && path === "/closes") {
    const req = (body.request ?? {}) as Json;
    if (typeof req.vaultId !== "string" || req.vaultId.toLowerCase() !== vaultId) return refuse(400, "The close request is for another vault.", "bad_request");
    w.complete("close-executed");
    await w.latency(2_000);
    return ok({ txHash: w.step("close-executed").tx.base });
  }
  if ((get && parts[0] === "quotes" && parts[1] === "deleverage") || (post && path === "/deleverages")) {
    throw new ReplayUnavailableError("Reducing debt with collateral is not part of the recorded run.");
  }

  // ---------------------------------------------------------------- the swap
  if (get && path === "/swap/quote") return swapQuote(w, query);
  if (parts[0] === "swap" && parts[1] === "orders" && parts[2]) {
    const known = parts[2].toLowerCase() === r.swap.orderId.toLowerCase() && w.isDone("swap-opened");
    if (!known) return refuse(404, "The maker does not know this order.", "unknown_order");
    if (post && parts[3] === "destination") {
      const receiver = receiverOf(body.receiver);
      if (!(receiver instanceof Uint8Array)) return receiver;
      w.update({ swapDestination: true });
      await w.latency(600);
      return ok({});
    }
    if (get && parts.length === 3) {
      const model = m();
      const status = model.swapSettled
        ? "settled"
        : model.swapProven
          ? "proven"
          : model.swapPaid
            ? "paid"
            : w.inputs.swapDestination
              ? "destination_received"
              : "awaiting_destination";
      return ok({ status });
    }
  }

  // ---------------------------------------------------------------- third-party recipients (unused by the app)
  if (post && path === "/recipients") {
    const c = destinationCommitment(body);
    if (typeof c !== "string") return c;
    return ok({ commitment: c });
  }

  return refuse(404, `The replay's backend has no ${method} ${path}.`, "not_found");
}
