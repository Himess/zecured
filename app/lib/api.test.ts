import { afterEach, describe, expect, it, vi } from "vitest";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { ApiError, createApi, describeError, parseOpenVault, parseRemoteConfig, parseVault } from "./api";

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

/** POST /vaults as the backend answers it (docs/INTERFACES.md § 4). */
const OPENED = {
  vaultId: VAULT,
  address: "utest1vault",
  zip321: "zcash:utest1vault?amount=0.01",
  amount: "1000000",
  expiryHeight: 4_600_288,
  ufvkHash: `0x${"EE".repeat(32)}`,
  signatures: [1, 2, 3, 4, 5].map((index) => ({ index, signature: `0x${String(index).repeat(130)}` })),
};

describe("owner endpoints", () => {
  it("POST /vaults sends camelCase decimal strings and parses the response", async () => {
    const { api, calls } = mockFetch(() => json(OPENED));
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
      ufvkHash: `0x${"ee".repeat(32)}`,
      signatures: OPENED.signatures,
    });
  });

  it("POST /vaults: the signers' signatures and the viewing key hash are required (C-4)", () => {
    const { signatures: _s, ...unsigned } = OPENED;
    const { ufvkHash: _h, ...unhashed } = OPENED;
    const broken: [string, unknown][] = [
      ["signatures", unsigned],
      ["signatures", { ...OPENED, signatures: "0x11" }],
      ["ufvkHash", unhashed],
      ["signature", { ...OPENED, signatures: [{ index: 1, signature: "0x1234" }] }],
      ["signature index", { ...OPENED, signatures: [{ index: -1, signature: OPENED.signatures[0]!.signature }] }],
      ["expiryHeight", { ...OPENED, expiryHeight: 2 ** 32 }],
    ];
    for (const [what, raw] of broken) expect(() => parseOpenVault(raw), what).toThrow(`(${what})`);
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
  it("accepts the canonical INTERFACES.md § 4 shape (heights, outstanding, paid flags)", () => {
    const v = parseVault({
      vaultId: VAULT,
      status: "active",
      amount: "1000000",
      address: "utest1vault",
      expiryHeight: 4_600_288,
      birthdayHeight: 4_600_000,
      tipHeight: 4_600_123,
      outstanding: "800000",
      payoutInFlight: true,
      nonConforming: [],
      claims: [
        { claimId: VAULT, kind: "Release", amount: "200000", paid: false },
        { claimId: VAULT, kind: "Liquidation", amount: "150000", paid: true },
      ],
    });
    expect(v).toEqual({
      vaultId: VAULT,
      status: "active",
      amount: 1_000_000n,
      address: "utest1vault",
      expiryHeight: 4_600_288,
      birthdayHeight: 4_600_000,
      tipHeight: 4_600_123,
      outstanding: 800_000n,
      payoutInFlight: true,
      nonConforming: [],
      claims: [
        { claimId: VAULT, kind: "Release", amount: 200_000n, status: "pending" },
        { claimId: VAULT, kind: "Liquidation", amount: 150_000n, status: "paid" },
      ],
    });
  });

  it("treats height 0 as unknown (the coordinator reports tip 0 before its first pass)", () => {
    const v = parseVault({ vaultId: VAULT, status: "awaiting_deposit", amount: "1000000", expiryHeight: 9, birthdayHeight: 0, tipHeight: 0 });
    expect(v.birthdayHeight).toBeUndefined();
    expect(v.tipHeight).toBeUndefined();
    expect(() => parseVault({ vaultId: VAULT, status: "active", amount: "1", expiryHeight: 1, birthdayHeight: -5 })).toThrow(
      /birthdayHeight/,
    );
  });

  it("accepts the earlier INTERFACES.md shape (status, confirmations, deposit)", () => {
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
        return json({
          nodes: [
            { index: 1, mode: "normal", vaults: 3, compromisable: false },
            { index: 2, mode: "unreachable" },
            { index: 4, mode: "compromised", vaults: 3, compromisable: true },
          ],
        });
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
      { index: 1, mode: "normal", vaults: 3, compromisable: false },
      { index: 2, mode: "unreachable" },
      { index: 4, mode: "compromised", vaults: 3, compromisable: true },
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

describe("GET /config", () => {
  it("parses the backend's response: checksummed addresses, bigint amounts, sorted denominations", async () => {
    const { api, calls } = mockFetch(() => json({ ...DRY_RUN_CONFIG, denominations: ["5000000", "1000000", "5000000"] }));
    const c = await api.getConfig();
    expect(calls[0]!.url).toBe(`${BASE}/config`);
    expect(calls[0]!.init.method).toBe("GET");
    expect((calls[0]!.init.headers as Record<string, string>)["x-admin-token"]).toBeUndefined();
    expect(c).toEqual({
      chainId: 84532,
      registry: "0xbdB942d882882bF795aA9c0Cd20ADe2115c73AAc",
      adapter: "0xC0a2A554dcFB5a1bd7e5E769A2Ad0c33BA0500fc",
      // A deployment from before RQ-06 names only the mocks: they are its loan token and oracle.
      contracts: {
        LoanToken: DRY_RUN_CONFIG.contracts.MockUSDC,
        MarketOracle: DRY_RUN_CONFIG.contracts.MockZecOracle,
        SettlementAdapter: DRY_RUN_CONFIG.contracts.SettlementAdapter,
        ZZEC: DRY_RUN_CONFIG.contracts.ZZEC,
        ZecuredRegistry: DRY_RUN_CONFIG.contracts.ZecuredRegistry,
      },
      market: { ...DRY_RUN_CONFIG.market, lltv: 625_000_000_000_000_000n },
      signers: null,
      denominations: [1_000_000n, 5_000_000n],
      confirmations: 3,
      vaultExpiryBlocks: 288,
      sponsorship: true,
    });
  });

  it("reads the loan token and the shock oracle of an RQ-06 deployment first", () => {
    const usdc = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
    const shock = "0x1111111111111111111111111111111111111111";
    const c = parseRemoteConfig({
      ...DRY_RUN_CONFIG,
      contracts: { ...DRY_RUN_CONFIG.contracts, MockUSDC: undefined, LoanToken: usdc, DemoShockOracle: shock },
    });
    expect(c.contracts?.LoanToken).toBe(usdc);
    expect(c.contracts?.MarketOracle).toBe(shock);
  });

  it("accepts a signer set when the backend reports one (sorted), and checks it", () => {
    const signers = DRY_RUN_ENV.NEXT_PUBLIC_SIGNERS.split(",");
    expect(parseRemoteConfig({ ...DRY_RUN_CONFIG, signers: [...signers].reverse() }).signers).toEqual(signers);
    expect(() => parseRemoteConfig({ ...DRY_RUN_CONFIG, signers: signers.slice(1) })).toThrow("(config.signers)");
    expect(() => parseRemoteConfig({ ...DRY_RUN_CONFIG, signers: [...signers.slice(1), "nope"] })).toThrow("(config.signers)");
  });

  it("accepts null contracts and market (before deployment)", () => {
    const c = parseRemoteConfig({ ...DRY_RUN_CONFIG, contracts: null, market: null, sponsorship: false });
    expect(c.contracts).toBeNull();
    expect(c.market).toBeNull();
    expect(c.sponsorship).toBe(false);
  });

  it("is strict about every field", () => {
    const broken: [string, Record<string, unknown>][] = [
      ["config.chainId", { chainId: "base" }],
      ["config.chainId", { chainId: 0 }],
      ["config.registry", { registry: "0x1234" }],
      ["config.adapter", { adapter: undefined }],
      ["config.contracts.ZZEC", { contracts: { ...DRY_RUN_CONFIG.contracts, ZZEC: "nope" } }],
      ["config.market.lltv", { market: { ...DRY_RUN_CONFIG.market, lltv: "62.5" } }],
      ["config.market.marketId", { market: { ...DRY_RUN_CONFIG.market, marketId: "0x12" } }],
      ["config.denominations", { denominations: [] }],
      ["config.denominations", { denominations: ["0"] }],
      ["config.denominations", { denominations: "1000000" }],
      ["config.confirmations", { confirmations: -1 }],
      ["config.vaultExpiryBlocks", { vaultExpiryBlocks: 0 }],
      ["config.sponsorship", { sponsorship: "true" }],
    ];
    for (const [what, patch] of broken) {
      expect(() => parseRemoteConfig({ ...DRY_RUN_CONFIG, ...patch }), what).toThrow(`Unexpected backend response (${what})`);
    }
    expect(() => parseRemoteConfig([DRY_RUN_CONFIG])).toThrow(ApiError);
  });
});

describe("demo control: MOCK oracle", () => {
  it("reads and sets the price with the admin token; the price goes out as a JSON number of cents", async () => {
    const txHash = `0x${"ab".repeat(32)}`;
    const { api, calls } = mockFetch((_url, init) =>
      init.method === "POST"
        ? json({ priceUsdCents: "2000", txHash })
        : json({ priceUsdCents: "4000", raw: "400000000000000000000000000000000000" }),
    );
    expect(await api.adminGetOracle("tok")).toEqual({ priceUsdCents: 4_000n, raw: 40n * 10n ** 34n });
    expect(await api.adminSetOracle("tok", 2_000n)).toEqual({ priceUsdCents: 2_000n, txHash });
    expect(calls.map((c) => [c.init.method, c.url])).toEqual([
      ["GET", `${BASE}/admin/oracle`],
      ["POST", `${BASE}/admin/oracle`],
    ]);
    expect(calls[1]!.body).toEqual({ priceUsdCents: 2000 });
    for (const c of calls) expect((c.init.headers as Record<string, string>)["x-admin-token"]).toBe("tok");
  });

  it("refuses non-positive prices before calling the backend, and malformed answers", async () => {
    const { api, calls } = mockFetch(() => json({ priceUsdCents: "40.5", raw: "1" }));
    await expect(api.adminSetOracle("tok", 0n)).rejects.toMatchObject({ kind: "invalid" });
    await expect(api.adminSetOracle("tok", 2n ** 60n)).rejects.toMatchObject({ kind: "invalid" });
    expect(calls).toHaveLength(0);
    await expect(api.adminGetOracle("tok")).rejects.toThrow(/priceUsdCents/);
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
