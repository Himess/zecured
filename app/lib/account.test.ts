import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createPublicClient,
  decodeFunctionData,
  encodeFunctionResult,
  getAddress,
  hexToBigInt,
  http,
  parseAbi,
  size,
  slice,
  type Address,
  type Hex,
  type PublicClient,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { baseSepolia } from "viem/chains";
import { DRY_RUN_CONFIG, DRY_RUN_ENV } from "./__fixtures__/deployment";
import { AccountNotDeployedError, loanAccount, SponsorshipUnavailableError } from "./account";
import { createApi, parseRemoteConfig } from "./api";
import { assertAllowlisted, borrowCalls, partialReleaseCalls, transferUsdcCalls, type Call } from "./calls";
import { buildConfig } from "./config";
import { predictAccountAddress, SAFE, safeFactoryData } from "./safe";

/** The deployment as GET /config reports it, with a bundler and a paymaster configured. */
const config = buildConfig(DRY_RUN_ENV, parseRemoteConfig(DRY_RUN_CONFIG));
const KEY: Hex = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"; // public test key
const OWNER = privateKeyToAccount(KEY).address;
const ACCOUNT = predictAccountAddress(OWNER);
const BASE = "http://backend.test";
const PAYMASTER: Address = "0x7777777777777777777777777777777777777777";
const USER_OP_HASH: Hex = `0x${"9a".repeat(32)}`;
const TX_HASH: Hex = `0x${"5b".repeat(32)}`;

/** Methods backend/src/proxy.rs lets through, per endpoint. */
const ALLOWED = {
  rpc: new Set([
    "eth_chainId",
    "eth_blockNumber",
    "eth_call",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getLogs",
    "eth_getBlockByNumber",
    "eth_getTransactionReceipt",
    "eth_getTransactionCount",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_feeHistory",
    "eth_estimateGas",
  ]),
  bundler: new Set([
    "eth_chainId",
    "eth_supportedEntryPoints",
    "eth_estimateUserOperationGas",
    "eth_sendUserOperation",
    "eth_getUserOperationByHash",
    "eth_getUserOperationReceipt",
    "pimlico_getUserOperationGasPrice",
    "pimlico_getUserOperationStatus",
  ]),
  paymaster: new Set(["pm_getPaymasterStubData", "pm_getPaymasterData", "pm_sponsorUserOperation"]),
};

const safe4337 = parseAbi([
  "function executeUserOp(address to, uint256 value, bytes data, uint8 operation)",
  "function executeUserOpWithErrorString(address to, uint256 value, bytes data, uint8 operation)",
]);
const multiSendAbi = parseAbi(["function multiSend(bytes transactions)"]);
const factoryAbi = parseAbi(["function createProxyWithNonce(address singleton, bytes initializer, uint256 saltNonce)"]);
const entryPointAbi = parseAbi(["function getNonce(address sender, uint192 key) view returns (uint256)"]);

/** MultiSend packing: operation (1) | to (20) | value (32) | data length (32) | data. */
function unpackMultiSend(packed: Hex): { operation: number; to: Address; value: bigint; data: Hex }[] {
  const out = [];
  let pos = 0;
  const total = size(packed);
  while (pos < total) {
    const operation = Number(hexToBigInt(slice(packed, pos, pos + 1)));
    const to = getAddress(slice(packed, pos + 1, pos + 21));
    const value = hexToBigInt(slice(packed, pos + 21, pos + 53));
    const len = Number(hexToBigInt(slice(packed, pos + 53, pos + 85)));
    const data = len ? slice(packed, pos + 85, pos + 85 + len) : "0x";
    out.push({ operation, to, value, data });
    pos += 85 + len;
  }
  return out;
}

/**
 * Mirror of backend/src/allowlist.rs `check`: deployment by the canonical Safe factory and
 * singleton; callData = Safe4337Module execution; no value; a CALL, or a DELEGATECALL to
 * MultiSendCallOnly whose inner operations are CALLs; every inner call allowlisted.
 */
function backendAllowlistCheck(op: Record<string, Hex>): Call[] {
  if (op.factory) {
    expect(getAddress(op.factory)).toBe(SAFE.proxyFactory);
    const f = decodeFunctionData({ abi: factoryAbi, data: op.factoryData! });
    expect(getAddress(f.args[0])).toBe(SAFE.singleton);
  }
  const exec = decodeFunctionData({ abi: safe4337, data: op.callData! });
  const [to, value, data, operation] = exec.args;
  expect(value).toBe(0n);
  let inner: Call[];
  if (operation === 0) inner = [{ to, data }];
  else {
    expect(operation).toBe(1);
    expect(getAddress(to)).toBe(SAFE.multiSendCallOnly);
    const txs = unpackMultiSend(decodeFunctionData({ abi: multiSendAbi, data }).args[0]);
    for (const t of txs) {
      expect(t.operation).toBe(0);
      expect(t.value).toBe(0n);
    }
    inner = txs.map((t) => ({ to: t.to, data: t.data }));
  }
  assertAllowlisted(inner, getAddress(op.sender!), config);
  return inner;
}

interface Recorded {
  methods: Record<"rpc" | "bundler" | "paymaster", string[]>;
  sent?: Record<string, Hex>;
  entryPoint?: Address;
  paymasterOps: Record<string, Hex>[];
}

function fakeInfra(opts: { deployed?: boolean } = {}): Recorded {
  const rec: Recorded = { methods: { rpc: [], bundler: [], paymaster: [] }, paymasterOps: [] };
  const fetchStub = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input));
    const kind = url.pathname.replace("/proxy/", "") as "rpc" | "bundler" | "paymaster";
    const body = JSON.parse(String(init?.body));
    const reqs = Array.isArray(body) ? body : [body];
    const answers = reqs.map((r: { id: number; method: string; params: unknown[] }) => {
      rec.methods[kind].push(r.method);
      return { jsonrpc: "2.0", id: r.id, result: answer(kind, r.method, r.params) };
    });
    return new Response(JSON.stringify(Array.isArray(body) ? answers : answers[0]), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };
  const answer = (kind: string, method: string, params: unknown[]): unknown => {
    if (method === "eth_chainId") return "0x14a34";
    if (kind === "bundler" && method === "eth_supportedEntryPoints") return [SAFE.entryPoint];
    if (kind === "rpc") {
      if (method === "eth_getCode") return opts.deployed ? "0x6080" : "0x";
      if (method === "eth_call") {
        const { to, data } = params[0] as { to: Address; data: Hex };
        expect(getAddress(to)).toBe(SAFE.entryPoint);
        decodeFunctionData({ abi: entryPointAbi, data });
        return encodeFunctionResult({ abi: entryPointAbi, functionName: "getNonce", result: opts.deployed ? 7n : 0n });
      }
    }
    if (kind === "paymaster") {
      rec.paymasterOps.push(params[0] as Record<string, Hex>);
      expect(getAddress(params[1] as Address)).toBe(SAFE.entryPoint);
      if (method === "pm_getPaymasterStubData") {
        return {
          paymaster: PAYMASTER,
          paymasterData: "0x01",
          paymasterVerificationGasLimit: "0x186a0",
          paymasterPostOpGasLimit: "0x0",
        };
      }
      if (method === "pm_getPaymasterData") return { paymaster: PAYMASTER, paymasterData: "0x02" };
    }
    if (kind === "bundler") {
      if (method === "pimlico_getUserOperationGasPrice") {
        const p = { maxFeePerGas: "0x3b9aca00", maxPriorityFeePerGas: "0x5f5e100" };
        return { slow: p, standard: p, fast: p };
      }
      if (method === "eth_estimateUserOperationGas") {
        return {
          preVerificationGas: "0x186a0",
          verificationGasLimit: "0x7a120",
          callGasLimit: "0x30d40",
          paymasterVerificationGasLimit: "0x186a0",
          paymasterPostOpGasLimit: "0x0",
        };
      }
      if (method === "eth_sendUserOperation") {
        rec.sent = params[0] as Record<string, Hex>;
        rec.entryPoint = params[1] as Address;
        return USER_OP_HASH;
      }
      if (method === "eth_getUserOperationReceipt") {
        return {
          userOpHash: USER_OP_HASH,
          entryPoint: SAFE.entryPoint,
          sender: ACCOUNT,
          nonce: "0x0",
          paymaster: PAYMASTER,
          actualGasCost: "0x1",
          actualGasUsed: "0x1",
          success: true,
          reason: "",
          logs: [],
          receipt: {
            transactionHash: TX_HASH,
            transactionIndex: "0x0",
            blockHash: `0x${"44".repeat(32)}`,
            blockNumber: "0x10",
            from: "0x0000000000000000000000000000000000000001",
            to: SAFE.entryPoint,
            cumulativeGasUsed: "0x1",
            gasUsed: "0x1",
            contractAddress: null,
            logs: [],
            logsBloom: `0x${"00".repeat(256)}`,
            status: "0x1",
            effectiveGasPrice: "0x1",
            type: "0x2",
          },
        };
      }
    }
    throw new Error(`fake ${kind}: unexpected ${method}`);
  };
  vi.stubGlobal("fetch", vi.fn(fetchStub));
  return rec;
}

function account(opts: { deployed?: boolean } = {}) {
  const rec = fakeInfra(opts);
  const api = createApi({ baseUrl: BASE });
  const client = createPublicClient({ chain: baseSepolia, transport: http(`${BASE}/proxy/rpc`) }) as PublicClient;
  return { rec, acct: loanAccount(KEY, { config, api, client, pollingInterval: 5 }) };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("loanAccount.send (ERC-4337 through the backend proxy, faked)", () => {
  it("first UserOp: deploys the pinned Safe, batches authorizations + borrow, is sponsored and lands", async () => {
    const { rec, acct } = account();
    expect(acct.address).toBe(ACCOUNT);
    const calls = borrowCalls({
      account: ACCOUNT,
      assets: 160_000n,
      registryAuthorized: false,
      adapterAuthorized: false,
      config,
    });
    const r = await acct.send(calls);
    expect(r).toEqual({ userOpHash: USER_OP_HASH, transactionHash: TX_HASH, success: true });

    const op = rec.sent!;
    expect(getAddress(rec.entryPoint!)).toBe(SAFE.entryPoint);
    expect(getAddress(op.sender!)).toBe(ACCOUNT);
    expect(BigInt(op.nonce!)).toBe(0n);
    // Deployment with exactly the config the address was predicted from.
    expect(getAddress(op.factory!)).toBe(SAFE.proxyFactory);
    expect(op.factoryData).toBe(safeFactoryData(OWNER));
    // Sponsored: paymaster fields from pm_getPaymasterData; fees from Pimlico's gas price.
    expect(getAddress(op.paymaster!)).toBe(PAYMASTER);
    expect(op.paymasterData).toBe("0x02");
    expect(BigInt(op.maxFeePerGas!)).toBe(1_000_000_000n);
    expect(BigInt(op.preVerificationGas!)).toBe(100_000n);
    expect(size(op.signature!)).toBeGreaterThanOrEqual(65);
    // What the backend allowlist sees: the same three calls, in order.
    expect(backendAllowlistCheck(op)).toEqual(calls.map((c) => ({ to: getAddress(c.to), data: c.data })));
    for (const pmOp of rec.paymasterOps) backendAllowlistCheck(pmOp);
    // Only methods the backend proxy lets through.
    for (const kind of ["rpc", "bundler", "paymaster"] as const) {
      for (const m of rec.methods[kind]) expect(ALLOWED[kind].has(m), `${kind}: ${m}`).toBe(true);
    }
    expect(rec.methods.paymaster).toEqual(["pm_getPaymasterStubData", "pm_getPaymasterData"]);
  });

  it("later UserOps carry no deployment and a single call goes out as a plain CALL", async () => {
    const { rec, acct } = account({ deployed: true });
    const to: Address = "0x5555555555555555555555555555555555555555";
    await acct.send(transferUsdcCalls({ to, amount: 5n, config }));
    const op = rec.sent!;
    expect(op.factory ?? null).toBeNull();
    expect(BigInt(op.nonce!)).toBe(7n);
    const exec = decodeFunctionData({ abi: safe4337, data: op.callData! });
    expect(exec.args[3]).toBe(0);
    expect(getAddress(exec.args[0])).toBe(config.usdc);
    backendAllowlistCheck(op);
  });

  it("a partial release (D14) is one sponsored UserOp the backend allowlist accepts", async () => {
    const { rec, acct } = account({ deployed: true });
    const calls = partialReleaseCalls({
      vaultId: `0x${"55".repeat(32)}`,
      amount: 200_000n,
      commitment: `0x${"77".repeat(32)}`,
      registryAuthorized: false,
      config,
    });
    await acct.send(calls);
    expect(backendAllowlistCheck(rec.sent!)).toEqual(calls.map((c) => ({ to: getAddress(c.to), data: c.data })));
  });

  it("fails fast without sponsorship (GET /config), before any request or signature", async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    const deps = { api: createApi({ baseUrl: BASE }) };
    const calls = transferUsdcCalls({ to: ACCOUNT, amount: 1n, config });
    const noBundler = loanAccount(KEY, { ...deps, config: { ...config, sponsorship: false } });
    await expect(noBundler.send(calls)).rejects.toBeInstanceOf(SponsorshipUnavailableError);
    await expect(noBundler.send(calls)).rejects.toThrow(
      "Sponsored transactions are not available right now (the backend has no bundler or paymaster configured).",
    );
    const unreachable = loanAccount(KEY, { ...deps, config: buildConfig(DRY_RUN_ENV, { error: "timeout" }) });
    await expect(unreachable.send(calls)).rejects.toThrow(/configuration could not be loaded/);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("never contacts the paymaster for a call outside the allowlist", async () => {
    const { rec, acct } = account();
    await expect(acct.send([{ to: "0x5555555555555555555555555555555555555555", data: "0x" }])).rejects.toThrow(/Not sponsorable/);
    expect(rec.methods.paymaster).toEqual([]);
    expect(rec.methods.bundler).toEqual([]);
  });

  it("reports an unconfigured bundler before signing anything", async () => {
    const bodies: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        bodies.push(`${new URL(String(input)).pathname} ${String(init?.body)}`);
        return new Response(JSON.stringify({ error: "bundler not configured" }), {
          status: 503,
          headers: { "content-type": "application/json" },
        });
      }),
    );
    const acct = loanAccount(KEY, {
      config,
      api: createApi({ baseUrl: BASE }),
      client: createPublicClient({ chain: baseSepolia, transport: http(`${BASE}/proxy/rpc`) }) as PublicClient,
    });
    await expect(acct.send(transferUsdcCalls({ to: ACCOUNT, amount: 1n, config }))).rejects.toThrow(
      "Sponsored transactions are not available right now (bundler: bundler not configured).",
    );
    expect(bodies).toHaveLength(1);
    expect(bodies[0]).toMatch(/^\/proxy\/bundler .*eth_supportedEntryPoints/);
  });

  it("refuses everything while the contracts are placeholders", async () => {
    fakeInfra();
    const acct = loanAccount(KEY, { config: buildConfig({}), api: createApi({ baseUrl: BASE }) });
    await expect(acct.send(transferUsdcCalls({ to: ACCOUNT, amount: 1n, config }))).rejects.toThrow(/not deployed/);
  });
});

describe("loanAccount.signCloseRequest", () => {
  const req = {
    vaultId: `0x${"55".repeat(32)}` as Hex,
    account: ACCOUNT,
    executor: "0x4444444444444444444444444444444444444444" as Address,
    marginBps: 100,
    minUserAmount: 1n,
    userCommitment: `0x${"77".repeat(32)}` as Hex,
    nonce: 1n,
    deadline: 1n,
  };

  it("refuses to sign for an Adapter that is not pinned or that the backend contradicts (H-6)", async () => {
    fakeInfra({ deployed: true });
    const api = createApi({ baseUrl: BASE });
    const conflicting = buildConfig(DRY_RUN_ENV, parseRemoteConfig({ ...DRY_RUN_CONFIG, adapter: "0x5555555555555555555555555555555555555555" }));
    for (const cfg of [buildConfig({}), conflicting]) {
      await expect(loanAccount(KEY, { config: cfg, api }).signCloseRequest(req)).rejects.toThrow(/not deployed or not configured/);
    }
  });

  it("requires a deployed account (no ERC-6492 wrapping)", async () => {
    const { acct } = account({ deployed: false });
    await expect(acct.signCloseRequest(req)).rejects.toBeInstanceOf(AccountNotDeployedError);
  });

  it("returns a 65-byte Safe-wrapped signature once deployed", async () => {
    const { acct } = account({ deployed: true });
    const sig = await acct.signCloseRequest(req);
    expect(size(sig)).toBe(65);
  });
});
