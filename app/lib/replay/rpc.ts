/**
 * The replay's Base (DEMO): the JSON-RPC methods the app's reads use through `/proxy/rpc`,
 * answered from the replay's state (lib/replay/model.ts). `eth_call` is dispatched by the contract
 * address and the 4-byte selector over the app's own ABIs (lib/abi.ts, lib/abi4.ts) and answered
 * with `encodeFunctionResult`, so every reader runs its real decoding; a function the replay does
 * not model reverts, as an older contract would (several readers expect that of v4-only views).
 * Logs and receipts carry the recorded transactions' hashes.
 */
import {
  decodeFunctionData,
  encodeFunctionResult,
  getAddress,
  keccak256,
  toBytes,
  zeroAddress,
  type Abi,
  type Address,
  type Hex,
} from "viem";
import {
  adapterAbi,
  bondAbi,
  compensatorAbi,
  disputesAbi,
  erc20Abi,
  intakeAbi,
  irmAbi,
  morphoAbi,
  oracleAbi,
  registryAbi,
  relayAbi,
  slasherAbi,
} from "../abi";
import {
  adapterV4Abi,
  bondV4Abi,
  bondViewsAbi,
  erc20AllowanceAbi,
  intakeV4Abi,
  intakeViewsAbi,
  morphoSupplyAbi,
  refundsV4Abi,
  registryViewsAbi,
  swapAbi,
} from "../abi4";
import { SAFE } from "../safe";
import { blockHash, buildModel, quantity, type LogModel, type Model } from "./model";
import type { ReplayWorld } from "./world";

const ZERO32: Hex = `0x${"00".repeat(32)}`;
const ERC1271_MAGIC: Hex = "0x1626ba7e";

const erc1271Abi = [
  {
    type: "function",
    name: "isValidSignature",
    stateMutability: "view",
    inputs: [
      { name: "hash", type: "bytes32" },
      { name: "signature", type: "bytes" },
    ],
    outputs: [{ name: "", type: "bytes4" }],
  },
] as const;

/** A JSON-RPC error the replay answers with (3: an `eth_call` that reverts). */
export class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: Hex,
  ) {
    super(message);
    this.name = "RpcError";
  }
}

const revert = (why = "execution reverted"): never => {
  throw new RpcError(3, why, "0x");
};

type Handler = (fn: string, args: readonly unknown[], m: Model) => unknown;

interface Role {
  abi: Abi;
  handle: Handler;
}

const same = (a: unknown, b: string): boolean => typeof a === "string" && a.toLowerCase() === b.toLowerCase();

/** The contracts of the recorded deployment, each with the ABI the app reads it with and its answers. */
function roles(w: ReplayWorld): Map<string, Role> {
  const r = w.rec;
  const c = r.config.contracts;
  const v3 = r.config.v3;
  const vaultId = r.loan.vault.vaultId;
  const account = r.loan.account;
  const registry = r.config.registry;
  const adapter = r.config.adapter;
  const map = new Map<string, Role>();
  const add = (address: string | undefined | null, abi: readonly unknown[], handle: Handler) => {
    if (address) map.set(address.toLowerCase(), { abi: abi as Abi, handle });
  };
  const claimOf = (m: Model, id: unknown) => m.claims.find((x) => same(id, x.claimId));

  add(registry, [...registryAbi, ...disputesAbi, ...registryViewsAbi, ...refundsV4Abi], (fn, args, m) => {
    const ours = same(args[0], vaultId);
    switch (fn) {
      case "vault":
        return ours && m.vaultStatus !== 0 ? [account, m.outstanding, m.vaultStatus] : [zeroAddress, 0n, 0];
      case "vaultOf":
        return same(args[0], account) && m.proven ? vaultId : ZERO32;
      case "vaultDetails":
        return ours && m.vaultStatus !== 0
          ? {
              account,
              outstanding: m.outstanding,
              status: m.vaultStatus,
              claimCount: BigInt(m.claims.length),
              unpaidClaims: BigInt(m.claims.filter((x) => x.status === 1).length),
            }
          : { account: zeroAddress, outstanding: 0n, status: 0, claimCount: 0n, unpaidClaims: 0n };
      case "claimDetails": {
        const x = claimOf(m, args[0]);
        return x
          ? { vaultId, kind: x.kind, status: x.status, amount: x.amount, commitment: x.commitment, createdAt: x.createdAt, bondPaid: false, claimant: x.claimant }
          : { vaultId: ZERO32, kind: 0, status: 0, amount: 0n, commitment: ZERO32, createdAt: 0, bondPaid: false, claimant: zeroAddress };
      }
      case "claim": {
        const x = claimOf(m, args[0]);
        return x ? [vaultId, x.kind, x.amount, x.commitment, x.status] : [ZERO32, 0, 0n, ZERO32, 0];
      }
      case "intake":
        return r.config.intake;
      case "slasher":
        return c.ZecuredSlasher ?? zeroAddress;
      case "compensator":
        return c.ZecuredCompensator ?? zeroAddress;
      case "signerBond":
        return c.SignerBond ?? zeroAddress;
      case "refundAbandonPeriod":
        return BigInt(v3.refundAbandonPeriod);
      case "disputeWindow":
        return BigInt(r.links.disputeWindow);
      case "inclusionWindow":
        return BigInt(r.links.inclusionWindow);
      case "payoutDispute":
        // Paid claims are proven in the same step (final: paidAt 0); pending ones have neither.
        return [0n, 0n];
      case "claimInput":
        return claimOf(m, args[0])?.input ?? ZERO32;
      case "lockedZatoshi":
        return m.lockedZat;
      case "openVaultCount":
        return m.openVaults;
      case "refundDormancy":
        return BigInt(r.links.refundDormancy);
      case "isRefundDormant":
        return false;
      default:
        return revert();
    }
  });

  add(r.config.intake, [...intakeAbi, ...intakeV4Abi, ...intakeViewsAbi], (fn, args, m) => {
    switch (fn) {
      case "pendingActivation":
        return same(args[0], vaultId) && m.pending
          ? m.pending
          : { account: zeroAddress, amount: 0n, depositTag: ZERO32, anchor: ZERO32, readyHeight: 0n, readyTime: 0n, versionId: ZERO32 };
      case "mintingPaused":
        return false;
      case "pausedUntil":
      case "pausedSince":
        return 0n;
      case "pendingRefundDelay":
        return BigInt(v3.pendingRefundDelay);
      case "relay":
        return r.links.relay;
      case "signerBond":
        return c.SignerBond ?? zeroAddress;
      case "pendingRefund":
        return { vaultId: ZERO32, account: zeroAddress, amount: 0n, reason: 0, anchor: ZERO32, readyHeight: 0n, readyTime: 0n, versionId: ZERO32 };
      case "pendingRefundTotal":
        return 0n;
      case "capRoom": {
        const room = BigInt(v3.maxTotalZat) - m.lockedZat - m.pendingTotal;
        return room > 0n ? room : 0n;
      }
      case "pendingTotal":
        return m.pendingTotal;
      case "maxTotalZat":
        return BigInt(v3.maxTotalZat);
      case "coverageBps":
        return v3.coverageBps;
      default:
        return revert();
    }
  });

  add(r.config.market.morphoBlue, [...morphoAbi, ...morphoSupplyAbi], (fn, args, m) => {
    switch (fn) {
      case "position": {
        const ours = same(args[1], account);
        const collateral = m.activated && !m.closed ? m.collateral : 0n;
        return ours ? [0n, m.borrowShares, collateral] : [0n, 0n, 0n];
      }
      case "market":
        return [m.market.totalSupplyAssets, m.market.totalSupplyShares, m.market.totalBorrowAssets, m.market.totalBorrowShares, m.market.lastUpdate, m.market.fee];
      case "isAuthorized":
        return same(args[0], account) && m.authorized && (same(args[1], registry) || same(args[1], adapter));
      default:
        return revert();
    }
  });

  add(r.config.market.oracle, oracleAbi, (fn) => (fn === "price" ? BigInt(r.chain.price) : revert()));
  add(r.config.market.irm, irmAbi, (fn) => (fn === "borrowRateView" ? BigInt(r.chain.borrowRate) : revert()));

  add(r.config.market.loanToken, [...erc20Abi, ...erc20AllowanceAbi], (fn, args, m) => {
    switch (fn) {
      case "balanceOf":
        return same(args[0], account) ? m.accountUsdc : same(args[0], r.swap.taker) ? m.takerUsdc : 0n;
      case "allowance":
        return same(args[0], r.swap.taker) && same(args[1], r.swap.swap) ? m.allowance : 0n;
      case "approve":
      case "transfer":
        return true;
      default:
        return revert();
    }
  });
  add(r.config.market.collateralToken, erc20Abi, (fn) => (fn === "balanceOf" ? 0n : revert()));

  add(adapter, [...adapterAbi, ...adapterV4Abi], (fn, args, m) => {
    switch (fn) {
      case "nonceUsed":
        return same(args[0], account) && m.closed;
      case "MAX_MARGIN_BPS":
        return r.links.maxMarginBps;
      case "preLiquidationTerms": {
        const t = same(args[0], account) && m.borrowed ? w.inputs.terms : undefined;
        return t
          ? { preLltv: BigInt(t.preLltv), preLcf1: BigInt(t.preLcf1), preLcf2: BigInt(t.preLcf2), preLif1: BigInt(t.preLif1), preLif2: BigInt(t.preLif2), expiry: BigInt(t.expiry) }
          : { preLltv: 0n, preLcf1: 0n, preLcf2: 0n, preLif1: 0n, preLif2: 0n, expiry: 0n };
      }
      default:
        return revert();
    }
  });

  add(r.links.relay, relayAbi, (fn, _args, m) => {
    if (fn === "bestChainTip") {
      return [keccak256(toBytes(`zecured-replay zcash block ${m.zcashTip}`)), BigInt(m.zcashTip), BigInt(r.chain.relayChainWork) + BigInt(m.zcashTip)];
    }
    if (fn === "isInBestChain") return true;
    return revert();
  });

  const bonds = r.chain.bonds;
  add(c.SignerBond, [...bondAbi, ...bondV4Abi, ...bondViewsAbi], (fn, args) => {
    switch (fn) {
      case "halted":
        return false;
      case "fullyBonded":
        return true;
      case "livenessDue":
        // Never due in the replay: its payouts come seconds after the close. `livenessArming` reverts,
        // as a v3 bond's would, so the ledger offers no arming of a claim the replay pays at once.
        return [0n, 0n];
      case "totalSlashable":
        return BigInt(bonds.totalSlashable);
      case "effectiveBond":
        return BigInt(bonds.effective);
      case "minBond":
        return BigInt(bonds.minBond);
      case "bondOf":
        return BigInt(bonds.each[Number(args[0])] ?? "0");
      default:
        return revert();
    }
  });

  add(c.ZecuredSlasher, slasherAbi, (fn) => {
    if (fn === "vaultFrozen" || fn === "isCompromised") return false;
    if (fn === "openCasesOf") return 0n;
    return revert();
  });

  add(c.ZecuredCompensator, compensatorAbi, (fn) => {
    switch (fn) {
      case "victim":
        return { incident: 0n, tallied: false, settled: false, authorized: false, pendingClaims: 0n, talliedClaims: 0n, outstanding: 0n, debt: 0n };
      case "entitlement":
        return { incident: 0n, layer: 0, zat: 0n, to: zeroAddress, paid: false };
      case "equityOf":
        return 0n;
      case "incident":
        return {
          openedAt: 0n,
          closesAt: 0n,
          finalized: false,
          victims: 0,
          talliedVictims: 0,
          price: 0n,
          layer2: 0n,
          layer3: 0n,
          layer4: 0n,
          r2: 0n,
          r3: 0n,
          r4: 0n,
        };
      default:
        return revert();
    }
  });

  add(r.swap.swap, swapAbi, (fn) => {
    if (fn === "open") {
      // The order the wallet is about to open: refused, as the contract would, unless it is the replay's next step.
      const next = w.nextManual("swap");
      if (next?.id !== "swap-opened") revert("ZecuredSwap: not the recorded order");
      return r.swap.orderId;
    }
    return revert();
  });

  if (r.swap.signing === "erc1271") add(r.swap.maker, erc1271Abi, (fn) => (fn === "isValidSignature" ? ERC1271_MAGIC : revert()));
  return map;
}

function ethCall(w: ReplayWorld, m: Model, call: { to?: string; data?: string; input?: string }): Hex {
  const to = call.to?.toLowerCase();
  const data = (call.data ?? call.input) as Hex | undefined;
  if (!to || !data) return revert();
  const role = roles(w).get(to);
  if (!role) return "0x";
  let decoded: { functionName: string; args?: readonly unknown[] };
  try {
    decoded = decodeFunctionData({ abi: role.abi, data }) as { functionName: string; args?: readonly unknown[] };
  } catch {
    return revert();
  }
  const result = role.handle(decoded.functionName, decoded.args ?? [], m);
  return encodeFunctionResult({ abi: role.abi, functionName: decoded.functionName, result } as never);
}

// ------------------------------------------------------------------ blocks, logs, receipts

function blockNumberOf(tag: unknown, m: Model): bigint {
  if (tag === undefined || tag === null || tag === "latest" || tag === "pending" || tag === "safe" || tag === "finalized") return m.block;
  if (tag === "earliest") return 0n;
  if (typeof tag === "string" && /^0x[0-9a-fA-F]+$/.test(tag)) return BigInt(tag);
  throw new RpcError(-32602, "invalid block tag");
}

function blockJson(w: ReplayWorld, m: Model, n: bigint): Record<string, unknown> | null {
  if (n > m.block || n < 0n) return null;
  const block0 = BigInt(w.rec.chain.baseBlock);
  const epochSec = BigInt(Math.floor(w.epochMs / 1000));
  const timestamp = epochSec + (n - block0) * 2n;
  return {
    number: quantity(n),
    hash: blockHash(n),
    parentHash: blockHash(n - 1n),
    timestamp: quantity(timestamp < 0n ? 0n : timestamp),
    nonce: "0x0000000000000000",
    difficulty: "0x0",
    totalDifficulty: "0x0",
    gasLimit: "0x1c9c380",
    gasUsed: "0x0",
    baseFeePerGas: "0xf4240",
    miner: "0x4200000000000000000000000000000000000011",
    extraData: "0x",
    size: "0x220",
    logsBloom: `0x${"00".repeat(256)}`,
    mixHash: ZERO32,
    sha3Uncles: ZERO32,
    stateRoot: ZERO32,
    receiptsRoot: ZERO32,
    transactionsRoot: ZERO32,
    transactions: [],
    uncles: [],
  };
}

function logJson(l: LogModel): Record<string, unknown> {
  return {
    address: l.address,
    topics: l.topics,
    data: l.data,
    blockNumber: quantity(l.blockNumber),
    blockHash: blockHash(l.blockNumber),
    transactionHash: l.transactionHash,
    transactionIndex: "0x0",
    logIndex: quantity(l.logIndex),
    removed: false,
  };
}

interface LogFilter {
  address?: string | string[];
  topics?: (string | string[] | null)[];
  fromBlock?: string;
  toBlock?: string;
}

function getLogs(m: Model, f: LogFilter): Record<string, unknown>[] {
  const from = blockNumberOf(f.fromBlock ?? "latest", m);
  const to = blockNumberOf(f.toBlock ?? "latest", m);
  const addresses = f.address === undefined ? null : (Array.isArray(f.address) ? f.address : [f.address]).map((a) => a.toLowerCase());
  return m.logs
    .filter((l) => l.blockNumber >= from && l.blockNumber <= to)
    .filter((l) => !addresses || addresses.includes(l.address.toLowerCase()))
    .filter((l) =>
      (f.topics ?? []).every((t, i) => {
        if (t === null || t === undefined) return true;
        const options = (Array.isArray(t) ? t : [t]).map((x) => x.toLowerCase());
        const have = l.topics[i]?.toLowerCase();
        return have !== undefined && options.includes(have);
      }),
    )
    .map(logJson);
}

function receiptJson(m: Model, hash: unknown): Record<string, unknown> | null {
  if (typeof hash !== "string") return null;
  const rc = m.receipts.get(hash.toLowerCase());
  if (!rc) return null;
  return {
    transactionHash: rc.hash,
    transactionIndex: "0x0",
    blockHash: blockHash(rc.blockNumber),
    blockNumber: quantity(rc.blockNumber),
    from: rc.from,
    to: rc.to,
    cumulativeGasUsed: "0x30d40",
    gasUsed: "0x30d40",
    contractAddress: null,
    logs: rc.logs.map(logJson),
    logsBloom: `0x${"00".repeat(256)}`,
    status: "0x1",
    effectiveGasPrice: "0xf4240",
    type: "0x2",
  };
}

function transactionJson(w: ReplayWorld, m: Model, hash: unknown): Record<string, unknown> | null {
  if (typeof hash !== "string") return null;
  const rc = m.receipts.get(hash.toLowerCase());
  if (!rc) return null;
  return {
    hash: rc.hash,
    nonce: "0x0",
    blockHash: blockHash(rc.blockNumber),
    blockNumber: quantity(rc.blockNumber),
    transactionIndex: "0x0",
    from: rc.from,
    to: rc.to,
    value: "0x0",
    gas: "0x30d40",
    gasPrice: "0xf4240",
    maxFeePerGas: "0xf4240",
    maxPriorityFeePerGas: "0x0",
    input: "0x",
    type: "0x2",
    chainId: quantity(w.rec.network.chainId),
    accessList: [],
    v: "0x0",
    r: "0x1",
    s: "0x1",
    yParity: "0x0",
  };
}

/** Code at `address`: the deployment's contracts always, the loan's Safe once its first UserOp deployed it. */
function code(w: ReplayWorld, m: Model, address: unknown): Hex {
  if (typeof address !== "string") return "0x";
  const a = address.toLowerCase();
  if (same(a, w.rec.loan.account)) return m.accountDeployed ? "0x608060405273ffffffffffffffffffffffffffffffffffffffff600054167f" : "0x";
  if (roles(w).has(a) || same(a, SAFE.entryPoint)) return "0x6080604052348015600f57600080fd5b50";
  return "0x";
}

// ------------------------------------------------------------------ requests

interface RpcRequest {
  jsonrpc?: string;
  id?: number | string | null;
  method?: string;
  params?: unknown[];
}

function answer(w: ReplayWorld, m: Model, req: RpcRequest): unknown {
  const p = req.params ?? [];
  switch (req.method) {
    case "eth_chainId":
      return quantity(w.rec.network.chainId);
    case "net_version":
      return String(w.rec.network.chainId);
    case "eth_blockNumber":
      return quantity(m.block);
    case "eth_getBlockByNumber":
      return blockJson(w, m, blockNumberOf(p[0], m));
    case "eth_getBlockByHash":
      return null;
    case "eth_getCode":
      return code(w, m, p[0]);
    case "eth_getBalance":
      // Nothing in the replay holds ETH: the loan account never does, and the wallet's gas is not modelled.
      return "0x0";
    case "eth_call":
      return ethCall(w, m, (p[0] ?? {}) as { to?: string; data?: string; input?: string });
    case "eth_estimateGas":
      return "0x30d40";
    case "eth_gasPrice":
      return "0xf4240";
    case "eth_maxPriorityFeePerGas":
      return "0x0";
    case "eth_getLogs":
      return getLogs(m, (p[0] ?? {}) as LogFilter);
    case "eth_getTransactionReceipt":
      return receiptJson(m, p[0]);
    case "eth_getTransactionByHash":
      return transactionJson(w, m, p[0]);
    default:
      throw new RpcError(-32601, `The replay's Base does not serve ${String(req.method)}.`);
  }
}

/** One JSON-RPC body (a request or a batch) to its response body. */
export function handleRpc(w: ReplayWorld, body: unknown): unknown {
  const m = buildModel(w);
  const one = (req: RpcRequest) => {
    const id = req.id ?? null;
    try {
      return { jsonrpc: "2.0", id, result: answer(w, m, req) };
    } catch (e) {
      if (e instanceof RpcError) return { jsonrpc: "2.0", id, error: { code: e.code, message: e.message, ...(e.data ? { data: e.data } : {}) } };
      return { jsonrpc: "2.0", id, error: { code: -32603, message: e instanceof Error ? e.message : "internal error" } };
    }
  };
  return Array.isArray(body) ? body.map((x) => one(x as RpcRequest)) : one(body as RpcRequest);
}

/** For the dock and tests: the replay's contract addresses, checksummed. */
export function replayContracts(w: ReplayWorld): Address[] {
  return [...roles(w).keys()].map((a) => getAddress(a));
}
