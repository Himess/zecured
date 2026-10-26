/**
 * What the replay's Base and backend look like at a moment (DEMO): a pure function of the
 * recording and of when this replay reached each step (lib/replay/world.ts). Morpho's math is the
 * app's own (lib/morpho.ts) and the Registry's lifecycle follows the contracts: the activation
 * mints the deposit less the opening fee and opens a Fee claim; the close opens the user's claim,
 * then the market maker's, and closes the vault; the payout commits, confirms and proves every
 * claim at once.
 */
import {
  encodeAbiParameters,
  encodeEventTopics,
  keccak256,
  numberToHex,
  zeroAddress,
  type Address,
  type Hex,
} from "viem";
import { registryEventsAbi } from "../abi";
import { swapAbi } from "../abi4";
import { activationPendingEvent, morphoBorrowEvent, spendCommittedEvent } from "../events";
import {
  mulDivUp,
  toAssetsUp,
  VIRTUAL_ASSETS,
  VIRTUAL_SHARES,
  wMulDown,
  wTaylorCompounded,
  type MarketState,
} from "../morpho";
import type { ReplayStepId } from "./types";
import type { ReplayWorld } from "./world";

/** Base Sepolia's block time, and the Zcash block time the app assumes (lib/zcash.ts). */
export const BASE_BLOCK_MS = 2_000;
const ZCASH_BLOCK_MS = 25_000;
/** In the replay, after the deposit, Zcash blocks come faster (accelerated): one every this many ms. */
const FAST_BLOCK_MS = 3_000;
/** Confirmations appear one every this many ms once the deposit is seen. */
const CONFIRMATION_MS = 1_500;

/** Registry `VaultStatus` and `ClaimStatus` indices (lib/abi.ts ONCHAIN_VAULT_STATUS, CLAIM_STATUS). */
const VAULT = { None: 0, Active: 1, Closed: 2, Pending: 4 } as const;
const CLAIM = { Pending: 1, Paid: 2 } as const;
/** `ClaimKind` indices. */
export const KIND = { CloseUser: 2, CloseMarketMaker: 3, Fee: 5 } as const;
const KIND_NAME: Record<number, string> = { 2: "CloseUser", 3: "CloseMarketMaker", 5: "Fee" };

export interface ClaimModel {
  claimId: Hex;
  sequence: number;
  kind: number;
  status: number;
  amount: bigint;
  commitment: Hex;
  createdAt: number;
  claimant: Address;
  /** `claimInput`: the payout's input tag once committed, zero before. */
  input: Hex;
}

export interface LogModel {
  address: Address;
  topics: Hex[];
  data: Hex;
  blockNumber: bigint;
  transactionHash: Hex;
  logIndex: number;
}

export interface ReceiptModel {
  hash: Hex;
  blockNumber: bigint;
  from: Address;
  to: Address;
  logs: LogModel[];
}

export interface Model {
  t: number;
  block: bigint;
  blockTime: bigint;
  zcashTip: number;
  /** The loan. */
  opened: boolean;
  paid: boolean;
  seen: boolean;
  confirmations: number | null;
  proving: boolean;
  proven: boolean;
  activated: boolean;
  borrowed: boolean;
  closed: boolean;
  committed: boolean;
  confirmed: boolean;
  fee: bigint;
  collateral: bigint;
  vaultStatus: number;
  outstanding: bigint;
  claims: ClaimModel[];
  pending: { account: Address; amount: bigint; depositTag: Hex; anchor: Hex; readyHeight: bigint; readyTime: bigint; versionId: Hex } | null;
  market: MarketState;
  borrowShares: bigint;
  accountDeployed: boolean;
  authorized: boolean;
  accountUsdc: bigint;
  lockedZat: bigint;
  openVaults: bigint;
  pendingTotal: bigint;
  /** The swap. */
  swapOpened: boolean;
  swapPaid: boolean;
  swapProven: boolean;
  swapSettled: boolean;
  allowance: bigint;
  takerUsdc: bigint;
  payDeadline: bigint;
  logs: LogModel[];
  receipts: Map<string, ReceiptModel>;
}

/** Morpho's `toSharesUp` (SharesMathLib). */
function toSharesUp(assets: bigint, totalAssets: bigint, totalShares: bigint): bigint {
  return mulDivUp(assets, totalShares + VIRTUAL_SHARES, totalAssets + VIRTUAL_ASSETS);
}

/** Morpho's `_accrueInterest` at `now` (no fee). */
function accrue(m: MarketState, rate: bigint, now: bigint): MarketState {
  const elapsed = now > m.lastUpdate ? now - m.lastUpdate : 0n;
  if (elapsed === 0n || m.totalBorrowAssets === 0n) return { ...m, lastUpdate: now > m.lastUpdate ? now : m.lastUpdate };
  const interest = wMulDown(m.totalBorrowAssets, wTaylorCompounded(rate, elapsed));
  return { ...m, totalBorrowAssets: m.totalBorrowAssets + interest, totalSupplyAssets: m.totalSupplyAssets + interest, lastUpdate: now };
}

/** `IZecuredRegistry.claimIdOf`: keccak256(abi.encode(vaultId, uint64 sequence)). */
export function claimIdOf(vaultId: Hex, sequence: number): Hex {
  return keccak256(encodeAbiParameters([{ type: "bytes32" }, { type: "uint64" }], [vaultId, BigInt(sequence)]));
}

/** A deterministic block hash for a block number. */
export function blockHash(n: bigint): Hex {
  return keccak256(encodeAbiParameters([{ type: "string" }, { type: "uint256" }], ["zecured-replay block", n]));
}

/** Who sends the run's Base transactions, as receipts name them (plausible, not exact). */
const ENTRY_POINT: Address = "0x0000000071727De22E5E9d8BAf0edAc6f37da032";
const KEEPER: Address = "0x000000000000000000000000000000000000c0de";

export function buildModel(w: ReplayWorld, t: number = w.now()): Model {
  const r = w.rec;
  const at = (id: ReplayStepId): number | null => (w.isDone(id, t) ? w.doneAt(id) : null);
  const epochMs = w.epochMs;
  const epochSec = BigInt(Math.floor(epochMs / 1000));
  const block0 = BigInt(r.chain.baseBlock);
  const blockAt = (ms: number): bigint => block0 + BigInt(Math.max(0, Math.floor((ms - epochMs) / BASE_BLOCK_MS)));
  const timeOf = (n: bigint): bigint => epochSec + (n - block0) * 2n;
  const secAt = (ms: number): bigint => timeOf(blockAt(ms));
  // The latest block is the last one sealed: the block under way can still take a step's transaction, so a
  // reader whose cursor reached "latest" just before a step in the same 2 s never skips the step's logs.
  const block = blockAt(t) - 1n;

  const amount = BigInt(r.loan.amount);
  const v3 = r.config.v3;
  const feeRaw = (amount * BigInt(v3.feeBps)) / 10_000n;
  const fee = feeRaw < BigInt(v3.minFeeZat) ? 0n : feeRaw;
  const collateral = amount - fee;
  const N = r.config.confirmations;

  const tOpened = at("opened");
  const tPaid = at("deposit-paid");
  const tSeen = at("deposit-seen");
  const tProven = at("proven");
  const tActivated = at("activated");
  const tBorrowed = at("borrowed");
  const tClosed = at("close-executed");
  const tCommitted = at("payouts-committed");
  const tConfirmed = at("payouts-confirmed");
  const tSwapOpened = at("swap-opened");
  const tSwapPaid = at("swap-paid");
  const tSwapProven = at("swap-proven");
  const tSwapSettled = at("swap-settled");

  // ---------------------------------------------------------------- Zcash heights
  const baseTip = (ms: number) => r.chain.zcashTip + Math.max(0, Math.floor((ms - epochMs) / ZCASH_BLOCK_MS));
  const depositHeight = tPaid !== null ? baseTip(tPaid) + 1 : null;
  const confirmations = tSeen !== null ? Math.min(N, 1 + Math.floor((t - tSeen) / CONFIRMATION_MS)) : tPaid !== null ? 0 : null;
  const anchorTip = depositHeight !== null ? depositHeight + N - 1 : 0;
  /** The Zcash tip at `ms` (≤ t): the clock's, pushed ahead by the deposit's confirmations and, accelerated, by the steps that wait for blocks. */
  const tipAt = (ms: number): number => {
    let tip = baseTip(ms);
    if (depositHeight !== null && tSeen !== null && tSeen <= ms) {
      tip = Math.max(tip, depositHeight + Math.min(N, 1 + Math.floor((ms - tSeen) / CONFIRMATION_MS)) - 1);
    }
    if (tProven !== null && tProven <= ms) {
      const until = tActivated !== null ? Math.min(ms, tActivated + 60_000) : ms;
      tip = Math.max(tip, anchorTip + Math.floor((until - tProven) / FAST_BLOCK_MS));
    }
    if (tSwapPaid !== null && tSwapPaid <= ms) {
      const until = tSwapSettled !== null ? Math.min(ms, tSwapSettled + 60_000) : ms;
      tip = Math.max(tip, baseTip(tSwapPaid) + 1 + Math.floor((until - tSwapPaid) / FAST_BLOCK_MS));
    }
    return tip;
  };
  const zcashTip = tipAt(t);

  // ---------------------------------------------------------------- Morpho
  const rate = BigInt(r.chain.borrowRate);
  let market: MarketState = {
    totalSupplyAssets: BigInt(r.chain.market.totalSupplyAssets),
    totalSupplyShares: BigInt(r.chain.market.totalSupplyShares),
    totalBorrowAssets: BigInt(r.chain.market.totalBorrowAssets),
    totalBorrowShares: BigInt(r.chain.market.totalBorrowShares),
    lastUpdate: epochSec,
    fee: BigInt(r.chain.market.fee),
  };
  let borrowShares = 0n;
  // The shares the borrow minted, for its Morpho event (the position's own shares go back to 0 at the close).
  let sharesBorrowed = 0n;
  const borrowAssets = BigInt(r.loan.borrow.assets);
  if (tBorrowed !== null) {
    market = accrue(market, rate, secAt(tBorrowed));
    borrowShares = toSharesUp(borrowAssets, market.totalBorrowAssets, market.totalBorrowShares);
    sharesBorrowed = borrowShares;
    market = { ...market, totalBorrowAssets: market.totalBorrowAssets + borrowAssets, totalBorrowShares: market.totalBorrowShares + borrowShares };
  }
  if (tClosed !== null && borrowShares > 0n) {
    market = accrue(market, rate, secAt(tClosed));
    const repaid = toAssetsUp(borrowShares, market.totalBorrowAssets, market.totalBorrowShares);
    market = { ...market, totalBorrowAssets: market.totalBorrowAssets - repaid, totalBorrowShares: market.totalBorrowShares - borrowShares };
    borrowShares = 0n;
  }

  // ---------------------------------------------------------------- the Registry
  const vaultId = r.loan.vault.vaultId;
  const account = r.loan.account;
  const claims: ClaimModel[] = [];
  const claimStatus = tConfirmed !== null ? CLAIM.Paid : CLAIM.Pending;
  const input = tCommitted !== null ? r.loan.close.inputTag : (`0x${"00".repeat(32)}` as Hex);
  const claim = (kind: number, amountZat: bigint, commitment: Hex, createdMs: number, claimant: Address): ClaimModel => {
    const sequence = claims.length;
    return {
      claimId: claimIdOf(vaultId, sequence),
      sequence,
      kind,
      status: claimStatus,
      amount: amountZat,
      commitment,
      createdAt: Number(secAt(createdMs)),
      claimant,
      input,
    };
  };
  if (tActivated !== null && fee > 0n) claims.push(claim(KIND.Fee, fee, r.links.feeCommitment, tActivated, zeroAddress));
  if (tClosed !== null) {
    claims.push(claim(KIND.CloseUser, BigInt(r.loan.close.userAmount), r.loan.close.userCommitment, tClosed, account));
    claims.push(claim(KIND.CloseMarketMaker, BigInt(r.loan.close.mmAmount), r.loan.close.mmCommitment, tClosed, r.loan.close.executor));
  }
  const vaultStatus = tClosed !== null ? VAULT.Closed : tActivated !== null ? VAULT.Active : tProven !== null ? VAULT.Pending : VAULT.None;
  const outstanding = tActivated !== null && tClosed === null ? collateral : 0n;
  const unpaid = claims.filter((c) => c.status === CLAIM.Pending);
  const ourOpen = tProven !== null && !(tClosed !== null && unpaid.length === 0);
  const otherLocked = BigInt(r.chain.otherLockedZat);
  const lockedZat = otherLocked + outstanding + unpaid.reduce((s, c) => s + c.amount, 0n);
  const otherOpen = Object.values(r.chain.otherOpenVaults).reduce((s, n) => s + n, 0);
  const pendingTotal = tProven !== null && tActivated === null ? amount : 0n;
  const pending =
    tProven !== null && tActivated === null
      ? {
          account,
          amount,
          depositTag: r.loan.depositTag,
          anchor: r.loan.anchor,
          readyHeight: BigInt(anchorTip + 2),
          readyTime: secAt(tProven) + BigInt(Math.ceil(w.step("activated").delayMs / 1000)),
          versionId: r.loan.versionId,
        }
      : null;

  // ---------------------------------------------------------------- logs and receipts
  const logs: LogModel[] = [];
  const receipts = new Map<string, ReceiptModel>();
  const registry = r.config.registry.toLowerCase() as Address;
  const swap = r.swap.swap;
  const addReceipt = (hash: Hex | undefined, ms: number, from: Address, to: Address, emitted: Omit<LogModel, "blockNumber" | "transactionHash" | "logIndex">[]) => {
    if (!hash) return;
    const blockNumber = blockAt(ms);
    const own = emitted.map((e, i) => ({ ...e, blockNumber, transactionHash: hash, logIndex: i }));
    logs.push(...own);
    receipts.set(hash.toLowerCase(), { hash, blockNumber, from, to, logs: own });
  };
  const event = (address: Address, abi: readonly unknown[], eventName: string, args: Record<string, unknown>, data: Hex) => ({
    address,
    topics: encodeEventTopics({ abi, eventName, args } as never) as Hex[],
    data,
  });
  const enc = (types: { type: string }[], values: unknown[]) => encodeAbiParameters(types, values as never);

  // The deposit's proof: the Intake's ActivationPending, the first event of the vault on Base (the trail starts there).
  if (tProven !== null) {
    const intake = r.config.intake as Address;
    addReceipt(w.step("proven").tx.base, tProven, KEEPER, intake, [
      event(
        intake,
        [activationPendingEvent],
        "ActivationPending",
        { vaultId, account },
        enc(
          [{ type: "uint64" }, { type: "bytes32" }, { type: "uint256" }, { type: "uint256" }],
          [amount, r.loan.depositTag, BigInt(anchorTip + 2), secAt(tProven) + BigInt(Math.ceil(w.step("activated").delayMs / 1000))],
        ),
      ),
    ]);
  }
  if (tActivated !== null) {
    const emitted = [
      event(registry, registryEventsAbi, "VaultActivated", { vaultId, account }, enc([{ type: "uint64" }, { type: "bytes32" }], [amount, r.loan.depositTag])),
    ];
    if (fee > 0n) emitted.push(event(registry, registryEventsAbi, "FeeCharged", { vaultId, claimId: claims[0]!.claimId }, enc([{ type: "uint64" }], [fee])));
    addReceipt(w.step("activated").tx.base, tActivated, KEEPER, r.config.intake as Address, emitted);
  }
  // The borrow, as Morpho Blue logs it (on behalf of the account, to the account unless the judge chose another receiver).
  if (tBorrowed !== null) {
    const morpho = r.config.market.morphoBlue as Address;
    addReceipt(w.step("borrowed").tx.base, tBorrowed, KEEPER, ENTRY_POINT, [
      event(
        morpho,
        [morphoBorrowEvent],
        "Borrow",
        { id: r.config.market.marketId, onBehalf: account, receiver: (w.inputs.borrowReceiver as Address | undefined) ?? account },
        enc([{ type: "address" }, { type: "uint256" }, { type: "uint256" }], [account, borrowAssets, sharesBorrowed]),
      ),
    ]);
  }
  if (tClosed !== null) {
    const user = claims.find((c) => c.kind === KIND.CloseUser)!;
    const mm = claims.find((c) => c.kind === KIND.CloseMarketMaker)!;
    addReceipt(w.step("close-executed").tx.base, tClosed, r.loan.close.executor, r.config.adapter as Address, [
      event(
        registry,
        registryEventsAbi,
        "CloseRelease",
        { vaultId },
        enc(
          [{ type: "bytes32" }, { type: "bytes32" }, { type: "uint64" }, { type: "uint64" }, { type: "bytes32" }, { type: "bytes32" }],
          [user.claimId, mm.claimId, user.amount, mm.amount, user.commitment, mm.commitment],
        ),
      ),
    ]);
  }
  // The payout committed before it is signed: three signers' bitmap, every claim of the close, no change left.
  if (tCommitted !== null) {
    const ids = claims.map((c) => c.claimId);
    addReceipt(w.step("payouts-committed").tx.base, tCommitted, KEEPER, registry, [
      event(
        registry,
        [spendCommittedEvent],
        "SpendCommitted",
        { vaultId, payoutRef: r.loan.close.payoutRef },
        enc(
          [{ type: "bytes32" }, { type: "bytes32[]" }, { type: "bytes32[]" }, { type: "bytes32" }, { type: "uint64" }, { type: "uint8" }],
          [r.loan.close.inputTag, ids, ids.map((id) => keccak256(id)), `0x${"00".repeat(32)}`, 0n, 0b00111],
        ),
      ),
    ]);
  }
  if (tConfirmed !== null) {
    const ref = r.loan.close.payoutRef;
    addReceipt(w.step("payouts-confirmed").tx.base, tConfirmed, KEEPER, registry, [
      event(registry, registryEventsAbi, "PayoutConfirmed", { vaultId, payoutRef: ref }, enc([{ type: "bytes32[]" }], [claims.map((c) => c.claimId)])),
      ...claims.map((c) => event(registry, registryEventsAbi, "PayoutProven", { vaultId, claimId: c.claimId }, enc([{ type: "bytes32" }], [ref]))),
    ]);
  }

  // ---------------------------------------------------------------- the swap
  const usdc = BigInt(r.swap.usdc);
  const approvedAt = w.inputs.approvedAt ?? null;
  const payDeadline = tSwapOpened !== null ? secAt(tSwapOpened) + BigInt(r.swap.payWindow) : 0n;
  if (approvedAt !== null && approvedAt <= t) addReceipt(w.step("swap-opened").tx.approve, approvedAt, r.swap.taker, r.config.market.loanToken as Address, []);
  if (tSwapOpened !== null) {
    addReceipt(w.step("swap-opened").tx.base, tSwapOpened, r.swap.taker, swap, [
      event(
        swap,
        swapAbi,
        "OrderOpened",
        { orderId: r.swap.orderId, maker: r.swap.maker, taker: r.swap.taker },
        enc(
          [{ type: "uint8" }, { type: "uint64" }, { type: "uint256" }, { type: "bytes32" }, { type: "uint64" }],
          [0, BigInt(r.swap.zatoshi), usdc, r.swap.receiverCommitment, payDeadline],
        ),
      ),
    ]);
  }
  if (tSwapProven !== null) {
    addReceipt(w.step("swap-proven").tx.base, tSwapProven, r.swap.maker, swap, [
      event(
        swap,
        swapAbi,
        "SettlementPending",
        { orderId: r.swap.orderId },
        enc([{ type: "bytes32" }, { type: "uint256" }, { type: "uint256" }], [r.swap.anchor, BigInt(tipAt(tSwapProven) + 2), secAt(tSwapProven) + 6n]),
      ),
    ]);
  }
  if (tSwapSettled !== null) addReceipt(w.step("swap-settled").tx.base, tSwapSettled, KEEPER, swap, [event(swap, swapAbi, "OrderSettled", { orderId: r.swap.orderId }, "0x")]);

  return {
    t,
    block,
    blockTime: timeOf(block),
    zcashTip,
    opened: tOpened !== null,
    paid: tPaid !== null,
    seen: tSeen !== null,
    confirmations,
    proving: tSeen !== null && tProven === null && (confirmations ?? 0) >= N,
    proven: tProven !== null,
    activated: tActivated !== null,
    borrowed: tBorrowed !== null,
    closed: tClosed !== null,
    committed: tCommitted !== null,
    confirmed: tConfirmed !== null,
    fee,
    collateral,
    vaultStatus,
    outstanding,
    claims,
    pending,
    market,
    borrowShares,
    accountDeployed: tBorrowed !== null,
    authorized: tBorrowed !== null,
    accountUsdc: tBorrowed !== null && !w.inputs.borrowReceiver ? borrowAssets : 0n,
    lockedZat,
    openVaults: BigInt(otherOpen + (ourOpen ? 1 : 0)),
    pendingTotal,
    swapOpened: tSwapOpened !== null,
    swapPaid: tSwapPaid !== null,
    swapProven: tSwapProven !== null,
    swapSettled: tSwapSettled !== null,
    allowance: approvedAt !== null && approvedAt <= t && tSwapOpened === null ? usdc : 0n,
    takerUsdc: BigInt(r.swap.takerUsdc) - (tSwapOpened !== null ? usdc : 0n),
    payDeadline,
    logs,
    receipts,
  };
}

/** The Registry's claim kind names, as the coordinator reports them. */
export function kindName(kind: number): string {
  return KIND_NAME[kind] ?? "Unknown";
}

/** `0x…` of a bigint, for JSON-RPC quantities. */
export function quantity(n: bigint | number): Hex {
  return numberToHex(n);
}
