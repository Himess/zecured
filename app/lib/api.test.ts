import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, createApi, describeError, parseVault } from "./api";

const BASE = "http://backend.test";
const VAULT = `0x${"ab".repeat(32)}` as const;
const SIG = `0x${"11".repeat(65)}` as const;

interface Recorded {
  url: string;
  init: RequestInit;
  body: unknown;
}

function mockFetch(respond: (url: string, init: RequestInit) => Response | Promise<Response>) {
  const calls: Recorded[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const url = String(input);
    calls.push({ url, init, body: init.body ? JSON.parse(String(init.body)) : undefined });
    return respond(url, init);
  });
  return { api: createApi({ baseUrl: `${BASE}/`, fetch: fetch as unknown as typeof globalThis.fetch }), calls };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

afterEach(() => {
  vi.useRealTimers();
});

describe("owner endpoints", () => {
  it("POST /vaults sends camelCase decimal strings and parses the response", async () => {
    const { api, calls } = mockFetch(() =>
      json({ vaultId: VAULT, address: "utest1vault", zip321: "zcash:utest1vault?amount=0.01", amount: "1000000", expiryHeight: 4_600_288 }),
    );
    const owner = "0x1111111111111111111111111111111111111111";
    const account = "0x2222222222222222222222222222222222222222";
    const r = await api.openVault({
      owner,
      account,
      amount: 1_000_000n,
      requestId: `0x${"33".repeat(32)}`,
      deadline: 1_800_000_000n,
      signature: SIG,
    });
    expect(calls[0]!.url).toBe(`${BASE}/vaults`);
    expect(calls[0]!.init.method).toBe("POST");
    expect(calls[0]!.body).toEqual({
      owner,
      account,
      amount: "1000000",
      requestId: `0x${"33".repeat(32)}`,
      deadline: "1800000000",
      signature: SIG,
    });
    expect(r).toEqual({
      vaultId: VAULT,
      address: "utest1vault",
      zip321: "zcash:utest1vault?amount=0.01",
      amount: 1_000_000n,
      expiryHeight: 4_600_288,
    });
  });

  it("never sends credentials or a referrer, and never caches", async () => {
    const { api, calls } = mockFetch(() => json({ ok: true }));
    await api.health();
    expect(calls[0]!.init).toMatchObject({ credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store" });
    // A GET without a body sends no content-type (no needless CORS preflight).
    expect((calls[0]!.init.headers as Record<string, string>)["content-type"]).toBeUndefined();
  });

  it("destination, refund and viewing-key bodies", async () => {
    const commitment = `0x${"cc".repeat(32)}`;
    const { api, calls } = mockFetch((url) => {
      if (url.endsWith("/destination")) return json({ commitment });
      if (url.endsWith("/refund")) return json({ accepted: true });
      return json({ ufvk: "uviewtest1abc" });
    });
    const salt = `0x${"99".repeat(32)}` as const;
    expect(await api.postDestination(VAULT, { receiver: "utest1dest", salt, nonce: 7n, deadline: 99n, signature: SIG })).toBe(
      commitment,
    );
    expect(calls[0]!.url).toBe(`${BASE}/vaults/${VAULT}/destination`);
    expect(calls[0]!.body).toEqual({ receiver: "utest1dest", salt, nonce: "7", deadline: "99", signature: SIG });

    const noteId = `0x${"44".repeat(32)}` as const;
    expect(
      await api.postRefund(VAULT, { noteId, receiver: "utest1dest", salt, nonce: 8n, deadline: 99n, signature: SIG }),
    ).toBe(true);
    expect(calls[1]!.body).toEqual({ noteId, receiver: "utest1dest", salt, nonce: "8", deadline: "99", signature: SIG });

    expect(await api.postViewingKey(VAULT, { nonce: 9n, deadline: 99n, signature: SIG })).toBe("uviewtest1abc");
    expect(calls[2]!.url).toBe(`${BASE}/vaults/${VAULT}/viewing-key`);
    expect(calls[2]!.body).toEqual({ nonce: "9", deadline: "99", signature: SIG });
  });

  it("refuses malformed vault ids before calling the backend", async () => {
    const { api, calls } = mockFetch(() => json({}));
    await expect(api.getVault("0x1234")).rejects.toMatchObject({ kind: "invalid" });
    await expect(api.getVault(`0x${"ab".repeat(31)}/../x` as `0x${string}`)).rejects.toBeInstanceOf(ApiError);
    expect(calls).toHaveLength(0);
  });
});

describe("GET /vaults/:id normalization", () => {
  it("accepts the INTERFACES.md § 4 shape", () => {
    const v = parseVault({
      vaultId: VAULT.toUpperCase().replace("0X", "0x"),
      status: "deposit_seen",
      amount: "5000000",
      expiryHeight: 100,
      confirmations: 2,
      deposit: { attested: false },
      nonConforming: [{ noteId: VAULT, amount: "1234" }],
      claims: [{ claimId: VAULT, kind: "Release", amount: "5000000", status: "pending" }],
    });
    expect(v).toEqual({
      vaultId: VAULT,
      status: "deposit_seen",
      amount: 5_000_000n,
      expiryHeight: 100,
      confirmations: 2,
      deposit: { attested: false },
      nonConforming: [{ noteId: VAULT, amount: 1234n }],
      claims: [{ claimId: VAULT, kind: "Release", amount: 5_000_000n, status: "pending" }],
    });
  });

  it("accepts the current backend shape (paid flag, outstanding, address, payoutInFlight)", () => {
    const v = parseVault({
      vaultId: VAULT,
      status: "active",
      amount: "1000000",
      address: "utest1vault",
      expiryHeight: 100,
      outstanding: "1000000",
      payoutInFlight: false,
      nonConforming: [],
      claims: [
        { claimId: VAULT, kind: "CloseMarketMaker", amount: "700000", paid: true },
        { claimId: VAULT, kind: "close_user", amount: "300000", paid: false },
        { claimId: VAULT, kind: "Mystery", amount: "1", paid: false },
      ],
    });
    expect(v.outstanding).toBe(1_000_000n);
    expect(v.address).toBe("utest1vault");
    expect(v.payoutInFlight).toBe(false);
    expect(v.claims.map((c) => [c.kind, c.status])).toEqual([
      ["CloseMarketMaker", "paid"],
      ["CloseUser", "pending"],
      ["Unknown", "pending"],
    ]);
  });

  it("maps unknown statuses to 'unknown' and rejects broken responses", () => {
    expect(parseVault({ vaultId: VAULT, status: "weird", amount: 1, expiryHeight: 1 }).status).toBe("unknown");
    expect(() => parseVault({ status: "active" })).toThrow(ApiError);
    expect(() => parseVault({ vaultId: VAULT, status: "active", amount: "-1", expiryHeight: 1 })).toThrow(/amount/);
    expect(() => parseVault(null)).toThrow(ApiError);
  });
});

describe("close from collateral", () => {
  it("parses a quote", async () => {
    const { api, calls } = mockFetch(() =>
      json({
        executor: "0x4444444444444444444444444444444444444444",
        marginBps: 100,
        repayAssets: "160000",
        mmAmount: "707000",
        userAmount: "293000",
        expiresAt: 1_800_000_000,
      }),
    );
    const q = await api.getCloseQuote(VAULT);
    expect(calls[0]!.url).toBe(`${BASE}/quotes/close/${VAULT}`);
    expect(q).toEqual({
      executor: "0x4444444444444444444444444444444444444444",
      marginBps: 100,
      repayAssets: 160_000n,
      mmAmount: 707_000n,
      userAmount: 293_000n,
      expiresAt: 1_800_000_000,
    });
  });

  it("submits the signed request to POST /closes with decimal strings", async () => {
    const txHash = `0x${"ef".repeat(32)}`;
    const { api, calls } = mockFetch(() => json({ txHash }));
    const result = await api.submitCloseRequest(
      {
        vaultId: VAULT,
        account: "0x3333333333333333333333333333333333333333",
        executor: "0x4444444444444444444444444444444444444444",
        marginBps: 100,
        minUserAmount: 290_000n,
        userCommitment: VAULT,
        nonce: 2n ** 100n,
        deadline: 1_800_000_000n,
      },
      SIG,
    );
    expect(result).toBe(txHash);
    expect(calls[0]!.url).toBe(`${BASE}/closes`);
    expect(calls[0]!.init.method).toBe("POST");
    expect(calls[0]!.body).toEqual({
      request: {
        vaultId: VAULT,
        account: "0x3333333333333333333333333333333333333333",
        executor: "0x4444444444444444444444444444444444444444",
        marginBps: 100,
        minUserAmount: "290000",
        userCommitment: VAULT,
        nonce: (2n ** 100n).toString(),
        deadline: "1800000000",
      },
      signature: SIG,
    });
  });
});

describe("demo control", () => {
  it("sends the admin token header and normalizes the snake_case drain report", async () => {
    const { api, calls } = mockFetch((url) => {
      if (url.endsWith("/admin/nodes"))
        return json({ nodes: [{ index: 1, mode: "normal", vaults: 3 }, { index: 2, mode: "unreachable" }] });
      if (url.endsWith("/mode")) return json({ mode: "compromised" });
      return json({
        vault_id: VAULT,
        amount: 1_000_000,
        votes: [
          { index: 1, signed: false, reason: "PCZT matches no plan" },
          { index: 2, signed: true, reason: null },
        ],
        threshold_reached: false,
      });
    });
    expect(await api.adminNodes("tok")).toEqual([
      { index: 1, mode: "normal", vaults: 3 },
      { index: 2, mode: "unreachable" },
    ]);
    await api.adminSetMode("tok", 2, "compromised");
    const report = await api.adminDrain("tok", VAULT, "utest1attacker");
    expect(report).toEqual({
      vaultId: VAULT,
      amount: 1_000_000n,
      thresholdReached: false,
      votes: [
        { index: 1, signed: false, reason: "PCZT matches no plan" },
        { index: 2, signed: true },
      ],
    });
    for (const c of calls) expect((c.init.headers as Record<string, string>)["x-admin-token"]).toBe("tok");
    expect(calls[1]!.url).toBe(`${BASE}/admin/nodes/2/mode`);
    expect(calls[1]!.body).toEqual({ mode: "compromised" });
    expect(calls[2]!.body).toEqual({ to: "utest1attacker" });
  });
});

describe("errors", () => {
  it("reports an unreachable backend", async () => {
    const { api } = mockFetch(() => Promise.reject(new TypeError("Failed to fetch")));
    const e = await api.getVault(VAULT).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect((e as ApiError).kind).toBe("unreachable");
    expect(describeError(e)).toMatch(/Is the backend running/);
  });

  it("surfaces the backend's error message", async () => {
    const { api } = mockFetch(() => json({ error: "destination: only 2 of 3 signers agreed" }, 422));
    const e = (await api.getVault(VAULT).catch((x: unknown) => x)) as ApiError;
    expect(e.kind).toBe("http");
    expect(e.status).toBe(422);
    expect(e.message).toBe("destination: only 2 of 3 signers agreed");
  });

  it("handles non-JSON error bodies", async () => {
    const { api } = mockFetch(() => new Response("Not Found", { status: 404 }));
    await expect(api.getCloseQuote(VAULT)).rejects.toThrow("Backend error (HTTP 404)");
  });

  it("times out", async () => {
    vi.useFakeTimers();
    const { api } = mockFetch(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        }),
    );
    const p = api.getVault(VAULT).catch((x: unknown) => x);
    await vi.advanceTimersByTimeAsync(15_001);
    expect(await p).toMatchObject({ kind: "timeout" });
  });
});
