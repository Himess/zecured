/**
 * A live run's recording: what the run left behind and what Base holds, turned into the `zecured-replay/1` recording
 * a replay build plays (lib/replay/recording.json), then checked the way the app reads it.
 *
 * Inputs, from a directory of the run's that is never committed (`HarvestInputs`): run.json (the RPC, the vault, the
 * swap's wallet), the backend's public snapshots before the loan (t0), before the swap (t1) and once the close's
 * payout is proven (t2) (scripts/live-snapshot.sh), the deployment records, the loan's public fields (a whitelist of
 * its key backup: never the owner key), the run's Zcash txids and heights, and the leak scan's output. Everything else
 * is read from Base: logs in windows of at most 2,000 blocks, state at t0's block, block times, and a Zcash height's
 * time from the relay.
 *
 * `gatherFacts` reads Base; `buildRecording` is pure; `checkRecording` needs no network. harvest.test.ts runs them
 * with REPLAY_HARVEST=1 and HARVEST_DIR, never in CI.
 */
import {
  decodeAbiParameters,
  decodeEventLog,
  decodeFunctionData,
  getAddress,
  isAddress,
  parseAbi,
  parseAbiItem,
  type Abi,
  type AbiEvent,
  type Address,
  type Hex,
  type Log,
  type PublicClient,
  type TransactionReceipt,
} from "viem";
import { morphoAbi, oracleAbi, payoutConfirmedEvent, payoutProvenEvent, registryEventsAbi, relayAbi } from "../abi";
import { adapterV4Abi, bondViewsAbi, registryViewsAbi, swapAbi } from "../abi4";
import { parseRemoteConfig } from "../api";
import { buildConfig, marketIdOf, marketParams } from "../config";
import { activationPendingEvent, morphoBorrowEvent, morphoRepayEvent, spendCommittedEvent } from "../events";
import { formatUsdc } from "../morpho";
import { checkVaultAddress } from "../owner";
import { predictAccountAddress } from "../safe";
import { verifyVaultOpened } from "../signers";
import { orderIdOf } from "../swap";
import { formatZec, zip321Uri } from "../zcash";
import { replayBuildEnv } from "./pins";
import { replaySteps } from "./placeholder";
import { parseRecording } from "./recording";
import {
  REPLAY_SCHEMA,
  REPLAY_STEP_IDS,
  type ReplayArtifact,
  type ReplayConfigWire,
  type ReplayRecording,
  type ReplayStep,
  type ReplayStepId,
  type ReplayTrack,
  type ReplayVersion,
} from "./types";

export class HarvestError extends Error {
  constructor(what: string) {
    super(`Harvest: ${what}`);
    this.name = "HarvestError";
  }
}

type Json = Record<string, unknown>;

/** What the harvest is about; written by hand on the box (data/live-run/run.json). */
export interface HarvestRun {
  /** A Base Sepolia RPC that takes 2,000-block `eth_getLogs` (not sepolia.base.org, which takes 200). */
  rpc: string;
  vaultId: Hex;
  /** The swap's wallet: the replay's wallet is this address. */
  taker: Address;
  /** The recorded order, when the wallet opened more than one. */
  orderId?: Hex;
  /** "real": RISC Zero's router checked Groth16 seals; "mock": a MOCK verifier checked MOCK seals. */
  verifier: "real" | "mock";
  /** ISO 8601; the vault's opening by default. */
  recordedAt?: string;
  /** The git tag the run's commit carries, if any. */
  tag?: string | null;
  /** Only if the close was not sent straight to the Adapter (its calldata then names no request): from the market maker's log. */
  close?: { executor: Address; marginBps: number };
  /** The leak scan's last block and time; t2's by default. */
  leakScan?: { toBlock: number; at: string };
}

/** A Zcash transaction of the run: its id in display order (as `zecured-wallet balance` prints it) and its height. */
export interface ZcashTx {
  txid: string;
  height: number;
}

/** The loan's public fields: a whitelist of its key backup (`ownerKey` is never read). */
export interface LoanPublic {
  owner: Address;
  account: Address;
  /** zatoshi */
  amount: string;
  requestId: Hex;
  vaultId: Hex;
  vaultAddress: string;
  expiryHeight: number;
  tagKeyHash: Hex;
  vaultSignatures: { index: number; signature: Hex }[];
  /** When the key was made (ms, as the app stores it). */
  createdAt: number;
}

export interface HarvestInputs {
  run: HarvestRun;
  t0: { at: string; baseBlock: number; config: Json; status: Json; demo: Json; stats: Json };
  t1?: { at: string; baseBlock: number; takerUsdc: string } | null;
  t2: { at: string; baseBlock: number; status: Json; vault: Json; order?: Json | null };
  /** contracts/deployments/<network>.json, after the MockUSDC patch (runbook § 3.2). */
  deployment: Json;
  /** contracts/deployments/<network>.swap.json */
  swapDeployment: Json;
  loan: LoanPublic;
  zcash: { deposit: ZcashTx; closePayout: ZcashTx; feePayout?: ZcashTx | null; swapPayment: ZcashTx };
  /** zecured-leak-scan's standard output. */
  leakScan?: string | null;
}

/** One step's facts: when (Unix seconds; null without a record), its hashes, and each artifact's transaction. */
export interface StepFact {
  at: number | null;
  tx: ReplayStep["tx"];
  /** In the order `replaySteps` lists the step's artifacts. */
  artifacts: (string | null)[];
  /** What the replay's model has no step for: the fee's own payout, a confirmation sent apart from its proof. */
  extra: ReplayArtifact[];
}

export interface HarvestFacts {
  recording: Omit<ReplayRecording, "steps">;
  imageIds: { deposit: Hex; violation: Hex; payment: Hex };
  /** As the dock names it, beside each proof. */
  prover: string;
  feePaidAlone: boolean;
  steps: Record<ReplayStepId, StepFact>;
}

// ------------------------------------------------------------------ what is decoded, as the contracts declare it

/** The calls a recording reads its facts from (contracts/src; their selectors are pinned in harvest.test.ts). */
export const attestAbi = parseAbi(["function attestDepositWithProof(bytes journal, bytes seal, bytes32 imageId, bytes[] openedSigs)"]);
export const closeAbi = [
  {
    type: "function",
    name: "closeWithCollateral",
    stateMutability: "nonpayable",
    inputs: [
      {
        name: "req",
        type: "tuple",
        components: [
          { name: "vaultId", type: "bytes32" },
          { name: "account", type: "address" },
          { name: "executor", type: "address" },
          { name: "marginBps", type: "uint16" },
          { name: "minUserAmount", type: "uint64" },
          { name: "userCommitment", type: "bytes32" },
          { name: "nonce", type: "uint256" },
          { name: "deadline", type: "uint64" },
        ],
      },
      { name: "userSig", type: "bytes" },
      { name: "mmCommitment", type: "bytes32" },
      { name: "maxRepayAssets", type: "uint256" },
      { name: "minMmAmount", type: "uint64" },
    ],
    outputs: [],
  },
] as const;
/** EntryPoint v0.7 (lib/safe.ts), Morpho Blue's EventsLib, ERC-20. */
export const userOperationEvent = parseAbiItem(
  "event UserOperationEvent(bytes32 indexed userOpHash, address indexed sender, address indexed paymaster, uint256 nonce, bool success, uint256 actualGasCost, uint256 actualGasUsed)",
);
export const accrueInterestEvent = parseAbiItem("event AccrueInterest(bytes32 indexed id, uint256 prevBorrowRate, uint256 interest, uint256 feeShares)");
export const approvalEvent = parseAbiItem("event Approval(address indexed owner, address indexed spender, uint256 value)");
/** IZcashHeaderRelay: a height's header on the best chain, and its time. */
export const relayHeadersAbi = parseAbi([
  "function bestBlockHashAt(uint256 height) view returns (bytes32)",
  "function headerOf(bytes32 blockHash) view returns ((bool known, bytes32 prevHash, uint256 height, uint256 time, uint256 bits, uint256 chainWork, bytes32 merkleRoot, bytes32 blockCommitments) info)",
]);
const ENTRY_POINT_V07: Address = "0x0000000071727De22E5E9d8BAf0edAc6f37da032";

/** Journal v2 of the deposit guest, word by word (signer/crates/base-client journal.rs; RealSealsV2.t.sol `_journal`). */
const JOURNAL_V2 = [
  { name: "mode", type: "uint8" },
  { name: "branchId", type: "uint32" },
  { name: "tipHash", type: "bytes32" },
  { name: "vaultId", type: "bytes32" },
  { name: "account", type: "address" },
  { name: "amount", type: "uint64" },
  { name: "depositTag", type: "bytes32" },
  { name: "vaultOpenedHash", type: "bytes32" },
] as const;

export interface DepositJournal {
  mode: number;
  branchId: number;
  tipHash: Hex;
  vaultId: Hex;
  account: Address;
  amount: bigint;
  depositTag: Hex;
  vaultOpenedHash: Hex;
}

export function decodeJournal(journal: Hex): DepositJournal {
  const [mode, branchId, tipHash, vaultId, account, amount, depositTag, vaultOpenedHash] = decodeAbiParameters(JOURNAL_V2, journal);
  return { mode, branchId, tipHash, vaultId, account: getAddress(account), amount, depositTag, vaultOpenedHash };
}

// ------------------------------------------------------------------ helpers

const lower = (h: string) => h.toLowerCase();
const same = (a: string | null | undefined, b: string | null | undefined) => Boolean(a && b && lower(a) === lower(b));
const iso = (s: number) => new Date(s * 1000).toISOString();
const TXID = /^[0-9a-f]{64}$/;

function need<T>(v: T | null | undefined, what: string): T {
  if (v === null || v === undefined) throw new HarvestError(what);
  return v;
}

function str(o: Json, k: string, what: string): string {
  const v = o[k];
  if (typeof v !== "string" || v === "") throw new HarvestError(`${what}.${k}`);
  return v;
}

function num(o: Json, k: string, what: string): number {
  const v = o[k];
  if (typeof v !== "number" || !Number.isSafeInteger(v)) throw new HarvestError(`${what}.${k}`);
  return v;
}

function obj(v: unknown, what: string): Json {
  if (!v || typeof v !== "object" || Array.isArray(v)) throw new HarvestError(what);
  return v as Json;
}

function addr(v: unknown, what: string): Address {
  if (typeof v !== "string" || !isAddress(v, { strict: false })) throw new HarvestError(what);
  return getAddress(v);
}

/** The opening fee the Registry charges on activation (ZecuredRegistry: under the minimum, none). */
export function openingFeeOf(amount: bigint, feeBps: number, minFeeZat: number): bigint {
  const fee = (amount * BigInt(feeBps)) / 10_000n;
  return fee < BigInt(minFeeZat) ? 0n : fee;
}

/** "identifiers: 302 (…)" and "matches: 0" from zecured-leak-scan's output. */
export function parseLeakScan(text: string): { identifiers: number; matches: number } {
  const ids = /^identifiers: (\d+)/m.exec(text);
  const matches = /^matches: (\d+)/m.exec(text);
  if (!ids || !matches) throw new HarvestError("the leak scan's output has no identifiers or matches line");
  return { identifiers: Number(ids[1]), matches: Number(matches[1]) };
}

// ------------------------------------------------------------------ the recording from its facts (pure)

/**
 * The recording: the facts' sections as they are, and the fifteen steps with the run's words (lib/replay/placeholder.ts
 * `replaySteps`), hashes and times. A step's duration is its time less the previous step's on its track (null when
 * either is unknown).
 */
export function buildRecording(f: HarvestFacts): ReplayRecording {
  const r = f.recording;
  const template = replaySteps({
    amount: BigInt(r.loan.amount),
    borrowAssets: BigInt(r.loan.borrow.assets),
    swapUsdc: BigInt(r.swap.usdc),
    vaultId: r.loan.vault.vaultId,
    imageIds: f.imageIds,
    prover: f.prover,
    feePaidAlone: f.feePaidAlone,
  });
  const previous: Partial<Record<ReplayTrack, number | null>> = {};
  const steps = template.map((s): ReplayStep => {
    const fact = f.steps[s.id];
    if (fact.artifacts.length !== s.artifacts.length) throw new HarvestError(`${s.id}: ${fact.artifacts.length} artifact hashes for ${s.artifacts.length} artifacts`);
    const before = previous[s.track];
    previous[s.track] = fact.at;
    return {
      ...s,
      at: fact.at === null ? null : iso(fact.at),
      durationSeconds: fact.at !== null && before !== undefined && before !== null ? fact.at - before : null,
      tx: fact.tx,
      artifacts: [...s.artifacts.map((a, i) => ({ ...a, tx: fact.artifacts[i] ?? null })), ...fact.extra],
    };
  });
  return { ...r, steps };
}

// ------------------------------------------------------------------ the checks (no network)

/** Names a secret would go by, and Zcash keys by their encodings' prefixes (as recording.test.ts scans the shipped file). */
const SECRET_KEY = /^(salt|ufvk|uivk|viewing_?key|owner_?key|private_?key|spending_?key|secret|mnemonic|seed)$/i;
const SECRET_VALUE = /^(uview|uivk|zxview|secret-extended-key)/i;

export function findSecrets(v: unknown, path = "$", found: string[] = []): string[] {
  if (typeof v === "string" && SECRET_VALUE.test(v)) found.push(path);
  if (Array.isArray(v)) v.forEach((x, i) => findSecrets(x, `${path}[${i}]`, found));
  else if (v && typeof v === "object") {
    for (const [k, x] of Object.entries(v)) {
      if (SECRET_KEY.test(k)) found.push(`${path}.${k}`);
      findSecrets(x, `${path}.${k}`, found);
    }
  }
  return found;
}

/**
 * What the app will make of the recording, before it ships: its shape; the deployment it pins (no problem, the
 * signers pinned, MockUSDC named so, the market's id, the swap); the vault's address and its five signatures, checked
 * by the app's own code; I7 of the close; the order's id; no secret anywhere. Returns the problems (none: it ships).
 */
export async function checkRecording(r: ReplayRecording): Promise<string[]> {
  const problems: string[] = [];
  const attempt = async (what: string, f: () => unknown) => {
    try {
      const ok = await f();
      if (ok === false) problems.push(what);
    } catch (e) {
      problems.push(`${what}: ${e instanceof Error ? e.message : String(e)}`);
    }
  };
  const copy = JSON.parse(JSON.stringify(r)) as ReplayRecording;
  await attempt("its shape (parseRecording)", () => parseRecording(copy) !== null);
  let config: ReturnType<typeof buildConfig> | null = null;
  await attempt("the deployment it pins", () => {
    config = buildConfig(replayBuildEnv("production", copy), parseRemoteConfig(copy.config));
    return true;
  });
  const c = config as ReturnType<typeof buildConfig> | null;
  if (c) {
    if (c.problems.length > 0) problems.push(`the app's configuration problems: ${c.problems.join("; ")}`);
    if (!c.contractsDeployed) problems.push("the app does not see the contracts as deployed");
    if (!c.signersPinned) problems.push("the signers are not pinned");
    if (c.marketLabels?.loanToken !== "MockUSDC") problems.push(`the loan token is named ${String(c.marketLabels?.loanToken)}, not MockUSDC`);
    await attempt("the market's id", () => marketIdOf(marketParams(c)) === lower(r.config.market.marketId));
    await attempt("the swap the app pins", () => same(c.swap, r.swap.swap));
    await attempt("the vault's five signatures, pinned", async () => {
      const check = await verifyVaultOpened(
        {
          vaultId: r.loan.vault.vaultId,
          owner: r.loan.owner,
          account: r.loan.account,
          amount: BigInt(r.loan.amount),
          expiryHeight: r.loan.vault.expiryHeight,
          address: r.loan.vault.address,
          tagKeyHash: r.loan.vault.tagKeyHash,
        },
        r.loan.vault.signatures,
        { config: c },
      );
      return check === "pinned";
    });
  }
  await attempt("the vault's address and payment request", () => checkVaultAddress(r.loan.vault.address, BigInt(r.loan.amount), r.loan.vault.zip321).ok);
  await attempt("the account the owner's key predicts", () => same(predictAccountAddress(r.loan.owner), r.loan.account));
  await attempt("I7: the close's two payouts add up to the collateral", () => {
    const fee = openingFeeOf(BigInt(r.loan.amount), r.config.v3.feeBps, r.config.v3.minFeeZat);
    return BigInt(r.loan.close.mmAmount) + BigInt(r.loan.close.userAmount) === BigInt(r.loan.amount) - fee;
  });
  await attempt("the order's id", () => lower(orderIdOf(r.network.chainId, getAddress(r.swap.swap), getAddress(r.swap.maker), BigInt(r.swap.nonce))) === lower(r.swap.orderId));
  const secrets = findSecrets(r);
  if (secrets.length > 0) problems.push(`secrets at ${secrets.join(", ")}`);
  if (!r.placeholder) {
    const hashes = r.steps.flatMap((s) => [s.tx.base, s.tx.zcash].filter((x): x is string => Boolean(x)));
    if (hashes.some((h) => /^(0x)?0{48}/.test(h))) problems.push("a dummy hash in a recorded step");
  }
  return problems;
}

// ------------------------------------------------------------------ Base, read (the harvest itself)

type EventArgs = Record<string, unknown>;

interface Found {
  args: EventArgs;
  tx: Hex;
  block: bigint;
  logIndex: number;
}

/**
 * The logs of `event` at `address` matching `args`, from `from` to `to`, in windows of at most 2,000 blocks (what a
 * keyed RPC or publicnode takes), oldest first.
 */
async function logsOf(client: PublicClient, address: Address, event: AbiEvent, args: EventArgs | undefined, from: bigint, to: bigint): Promise<Found[]> {
  const out: Found[] = [];
  for (let a = from; a <= to; a += 2_000n) {
    const b = a + 1_999n < to ? a + 1_999n : to;
    const logs = (await client.getLogs({ address, event, args, fromBlock: a, toBlock: b } as never)) as unknown as (Log & { args: EventArgs })[];
    for (const l of logs) out.push({ args: l.args, tx: l.transactionHash!, block: l.blockNumber!, logIndex: l.logIndex! });
  }
  return out.sort((x, y) => (x.block === y.block ? x.logIndex - y.logIndex : x.block < y.block ? -1 : 1));
}

/** The events of `abi` named `name` that `receipt` holds at `address`, decoded. */
function eventsIn(receipt: TransactionReceipt, address: Address, abi: Abi, name: string): EventArgs[] {
  const out: EventArgs[] = [];
  for (const l of receipt.logs) {
    if (!same(l.address, address)) continue;
    try {
      const d = decodeEventLog({ abi, data: l.data, topics: l.topics });
      if (d.eventName === name) out.push(d.args as unknown as EventArgs);
    } catch {
      // Another event of that contract.
    }
  }
  return out;
}

function only<T>(list: T[], what: string): T {
  if (list.length !== 1) throw new HarvestError(`${list.length} ${what} where one was expected`);
  return list[0]!;
}

/** Reads the run from Base and the inputs, and returns the recording's facts. */
export async function gatherFacts(inp: HarvestInputs, client: PublicClient): Promise<HarvestFacts> {
  const { run, loan } = inp;
  const d = inp.deployment;
  const contracts = obj(d.contracts, "deployment.contracts");
  const market = obj(d.market, "deployment.market");
  const proofs = obj(d.depositProofs, "deployment.depositProofs");
  const v3 = obj(d.v3, "deployment.v3");
  const tier3 = obj(d.tier3, "deployment.tier3");
  const chainId = num(d, "chainId", "deployment");
  const registry = addr(contracts.ZecuredRegistry, "deployment.contracts.ZecuredRegistry");
  const intake = addr(contracts.ZecuredIntake, "deployment.contracts.ZecuredIntake");
  const adapter = addr(contracts.SettlementAdapter, "deployment.contracts.SettlementAdapter");
  const bond = addr(contracts.SignerBond, "deployment.contracts.SignerBond");
  const token = addr(contracts.LoanToken, "deployment.contracts.LoanToken");
  if (!same(contracts.MockUSDC as string | undefined, token)) {
    throw new HarvestError("the deployment file does not name its loan token as MockUSDC: patch it first (runbook § 3.2)");
  }
  const morpho = addr(market.morphoBlue, "deployment.market.morphoBlue");
  const oracle = addr(market.oracle, "deployment.market.oracle");
  const marketId = str(market, "marketId", "deployment.market") as Hex;
  const relay = addr(proofs.relay, "deployment.depositProofs.relay");
  const swap = addr(inp.swapDeployment.ZecuredSwap, "swap deployment ZecuredSwap");
  const vaultId = lower(run.vaultId) as Hex;
  if (!same(loan.vaultId, vaultId)) throw new HarvestError("run.json's vault is not the loan's");
  const account = getAddress(loan.account);
  const taker = getAddress(run.taker);
  const amount = BigInt(loan.amount);
  const from = BigInt(num(d, "block", "deployment"));
  const to = BigInt(inp.t2.baseBlock);
  const logs = (address: Address, event: AbiEvent, args?: EventArgs) => logsOf(client, address, event, args, from, to);
  const receipts = new Map<string, TransactionReceipt>();
  const receipt = async (hash: Hex) => {
    const key = lower(hash);
    if (!receipts.has(key)) receipts.set(key, await client.getTransactionReceipt({ hash }));
    const r = receipts.get(key)!;
    if (r.status !== "success") throw new HarvestError(`transaction ${hash} reverted`);
    return r;
  };
  const times = new Map<bigint, number>();
  const blockTime = async (n: bigint) => {
    if (!times.has(n)) times.set(n, Number((await client.getBlock({ blockNumber: n })).timestamp));
    return times.get(n)!;
  };
  const zcashTime = async (height: number) => {
    const hash = await client.readContract({ address: relay, abi: relayHeadersAbi, functionName: "bestBlockHashAt", args: [BigInt(height)] });
    const info = await client.readContract({ address: relay, abi: relayHeadersAbi, functionName: "headerOf", args: [hash] });
    if (!info.known || info.height !== BigInt(height)) throw new HarvestError(`the relay has no header at Zcash height ${height}`);
    return Number(info.time);
  };
  for (const [k, z] of Object.entries(inp.zcash)) {
    if (z && (!TXID.test(z.txid) || !Number.isSafeInteger(z.height))) throw new HarvestError(`zcash.json's ${k}`);
  }

  // The deposit's proof: its event, then its call (journal, seal, guest and the five signatures).
  const pending = only(await logs(intake, activationPendingEvent as AbiEvent, { vaultId }), "ActivationPending events of the vault");
  if (!same(pending.args.account as string, account)) throw new HarvestError("the proven deposit names another account");
  const attestTx = await client.getTransaction({ hash: pending.tx });
  const attest = decodeFunctionData({ abi: attestAbi, data: attestTx.input });
  const [journalBytes, seal, depositImageId, openedSigs] = attest.args;
  const journal = decodeJournal(journalBytes);
  if (!same(journal.vaultId, vaultId) || !same(journal.account, account) || journal.amount !== amount || !same(journal.depositTag, pending.args.depositTag as Hex)) {
    throw new HarvestError("the deposit proof's journal is not the loan's");
  }
  const selector = lower(str(proofs, "sealSelector", "deployment.depositProofs"));
  if (lower(seal.slice(0, 10)) !== selector) throw new HarvestError(`the deposit's seal is not the deployment's kind (${selector})`);
  if ((run.verifier === "real") !== (selector === "0x73c457ba")) throw new HarvestError(`run.json says ${run.verifier}, the deployment's seals are ${selector}`);
  const signed = new Set(loan.vaultSignatures.map((s) => lower(s.signature)));
  if (openedSigs.length !== 5 || openedSigs.some((s) => !signed.has(lower(s)))) {
    throw new HarvestError("the proof's five VaultOpened signatures are not the loan's");
  }

  // The activation, and the opening fee it charged.
  const activated = only(await logs(registry, registryEventsAbi.find((e) => e.name === "VaultActivated") as AbiEvent, { vaultId }), "activations of the vault");
  const feeCharged = eventsIn(await receipt(activated.tx), registry, registryEventsAbi as Abi, "FeeCharged");
  const feeClaimId = feeCharged.length > 0 ? (feeCharged[0]!.claimId as Hex) : null;

  // The borrow: the first Morpho borrow on the account, its UserOp and any protection set with it.
  const borrows = await logs(morpho, morphoBorrowEvent as AbiEvent, { id: marketId, onBehalf: account });
  const borrow = need(borrows[0], "no borrow on the account");
  const borrowReceipt = await receipt(borrow.tx);
  const userOp = only(
    eventsIn(borrowReceipt, ENTRY_POINT_V07, [userOperationEvent], "UserOperationEvent").filter((e) => same(e.sender as string, account)),
    "UserOperationEvents of the account in the borrow's bundle",
  );
  if (userOp.success !== true) throw new HarvestError("the borrow's UserOp did not succeed");
  const isProtected = eventsIn(borrowReceipt, adapter, adapterV4Abi as Abi, "PreLiquidationSet").some((e) => same(e.account as string, account));

  // The close: its event, the debt it repaid, the rate the borrow ran at, and the request the market maker sent.
  const closed = only(await logs(registry, registryEventsAbi.find((e) => e.name === "CloseRelease") as AbiEvent, { vaultId }), "closes of the vault");
  const closeReceipt = await receipt(closed.tx);
  const repaid = only(
    eventsIn(closeReceipt, morpho, [morphoRepayEvent], "Repay").filter((e) => same(e.onBehalf as string, account)),
    "repayments of the account in the close",
  );
  const accrued = eventsIn(closeReceipt, morpho, [accrueInterestEvent], "AccrueInterest").filter((e) => same(e.id as string, marketId));
  const borrowRate = need(accrued[0], "no AccrueInterest of the market in the close").prevBorrowRate as bigint;
  let executor: Address;
  let marginBps: number;
  const closeTx = await client.getTransaction({ hash: closed.tx });
  try {
    const call = decodeFunctionData({ abi: closeAbi, data: closeTx.input });
    executor = getAddress(call.args[0].executor);
    marginBps = call.args[0].marginBps;
  } catch {
    const o = need(run.close, "the close's calldata is not closeWithCollateral's: give run.json's close {executor, marginBps}");
    executor = getAddress(o.executor);
    marginBps = o.marginBps;
  }
  const userClaimId = closed.args.userClaimId as Hex;
  const mmClaimId = closed.args.mmClaimId as Hex;

  // The payouts: committed before signing, confirmed, proven.
  const commits = await logs(registry, spendCommittedEvent as AbiEvent, { vaultId });
  const holds = (c: Found, claim: Hex | null) => Boolean(claim) && (c.args.claimIds as Hex[]).some((x) => same(x, claim));
  const closeCommit = only(commits.filter((c) => holds(c, userClaimId)), "payout commitments holding the close's claim");
  if (!holds(closeCommit, mmClaimId)) throw new HarvestError("the close's two claims are not in one payout");
  const feeCommit = feeClaimId && !holds(closeCommit, feeClaimId) ? (commits.find((c) => holds(c, feeClaimId)) ?? null) : null;
  const feePaidAlone = feeClaimId !== null && !holds(closeCommit, feeClaimId);
  const confirms = await logs(registry, payoutConfirmedEvent as AbiEvent, { vaultId });
  const confirmOf = (c: Found | null) => (c ? (confirms.find((x) => same(x.args.payoutRef as Hex, c.args.payoutRef as Hex)) ?? null) : null);
  const proven = await logs(registry, payoutProvenEvent as AbiEvent, { vaultId });
  const provenOf = (claim: Hex | null) => (claim ? (proven.find((x) => same(x.args.claimId as Hex, claim)) ?? null) : null);
  const closeProof = need(provenOf(userClaimId), "the close's payout is not proven on Base yet: harvest once PayoutProven landed");
  const closeConfirm = confirmOf(closeCommit);

  // The swap: the order, the call that opened it, its approval, the proof and the settlement.
  const orders = await logs(swap, swapAbi.find((e) => e.type === "event" && e.name === "OrderOpened") as AbiEvent, { taker });
  const order = need(run.orderId ? orders.find((o) => same(o.args.orderId as Hex, run.orderId!)) : orders[orders.length - 1], "no order of the swap's wallet");
  const orderId = order.args.orderId as Hex;
  const openTx = await client.getTransaction({ hash: order.tx });
  const open = decodeFunctionData({ abi: swapAbi, data: openTx.input });
  if (open.functionName !== "open") throw new HarvestError("the order's transaction is not ZecuredSwap.open");
  const [quote, makerSig] = open.args as readonly [{ maker: Address; makerReceiverCommitment: Hex; payWindow: number; nonce: bigint }, Hex, Hex];
  const approvals = (await logs(token, approvalEvent, { owner: taker, spender: swap })).filter((a) => a.block <= order.block);
  const approval = need(approvals[approvals.length - 1], "no approval of the order's tokens");
  const settling = only(await logs(swap, swapAbi.find((e) => e.type === "event" && e.name === "SettlementPending") as AbiEvent, { orderId }), "SettlementPending events of the order");
  const settled = only(await logs(swap, swapAbi.find((e) => e.type === "event" && e.name === "OrderSettled") as AbiEvent, { orderId }), "settlements of the order");

  // Base before the recorded loan (t0's block), as the replay starts.
  const t0Block = BigInt(inp.t0.baseBlock);
  const at = { blockNumber: t0Block };
  const [m, price, locked, tip, effective, totalSlashable, minBond] = await Promise.all([
    client.readContract({ address: morpho, abi: morphoAbi, functionName: "market", args: [marketId], ...at }),
    client.readContract({ address: oracle, abi: oracleAbi, functionName: "price", ...at }),
    client.readContract({ address: registry, abi: registryViewsAbi, functionName: "lockedZatoshi", ...at }),
    client.readContract({ address: relay, abi: relayAbi, functionName: "bestChainTip", ...at }),
    client.readContract({ address: bond, abi: bondViewsAbi, functionName: "effectiveBond", ...at }),
    client.readContract({ address: bond, abi: bondViewsAbi, functionName: "totalSlashable", ...at }),
    client.readContract({ address: bond, abi: bondViewsAbi, functionName: "minBond", ...at }),
  ]);
  const each = await Promise.all([0, 1, 2, 3, 4].map((i) => client.readContract({ address: bond, abi: bondViewsAbi, functionName: "bondOf", args: [i], ...at })));
  const takerUsdc =
    inp.t1?.takerUsdc ??
    (BigInt(order.args.usdc as bigint) +
      ((await client.readContract({ address: token, abi: parseAbi(["function balanceOf(address) view returns (uint256)"]), functionName: "balanceOf", args: [taker] })) as bigint)
    ).toString();

  // The backend's word, before the loan (t0) and at the end (t2).
  const t0 = inp.t0;
  const status0 = obj(t0.status, "t0 status");
  const deployment0 = obj(status0.deployment, "t0 status.deployment");
  const versions0 = obj(status0.versions, "t0 status.versions (retry the snapshot: the backend had not read them yet)");
  const depositVersions = need(versions0.deposit as ReplayVersion[] | undefined, "t0 status.versions.deposit");
  const version = need(depositVersions.find((v) => v.active && same(v.imageId, depositImageId)), "no active deposit version with the proof's image id");
  const config0 = obj(t0.config, "t0 config") as unknown as ReplayConfigWire;
  const demo0 = obj(t0.demo, "t0 demo status");
  const stats0 = obj(obj(t0.stats, "t0 stats").byAmount, "t0 stats.byAmount");
  const vault2 = obj(inp.t2.vault, "t2 vault");
  const confirmations = num(config0 as unknown as Json, "confirmations", "t0 config");
  /** The replay's backend checks no stamp: a few bits keep the judge's browser from working for nothing. */
  const REPLAY_STAMP_BITS = 8;

  const imageIds = {
    deposit: lower(depositImageId) as Hex,
    violation: lower(str(tier3, "violationImageId", "deployment.tier3")) as Hex,
    payment: lower(str(inp.swapDeployment, "paymentImageId", "swap deployment")) as Hex,
  };
  const prover = run.verifier === "real" ? "RISC Zero zkVM (RTX 4090), Groth16" : "MOCK seal (MockRiscZeroVerifier)";
  const openedAt = Math.floor(loan.createdAt / 1000);
  const date = new Date((run.recordedAt ? Date.parse(run.recordedAt) : openedAt * 1000)).toISOString().slice(0, 10);
  const note =
    run.verifier === "real"
      ? `Our live run of ${date} on Base Sepolia and Zcash testnet, with real zero-knowledge proofs verified by RISC Zero's router: every hash below is that run's transaction. Your browser replays it (DEMO): no backend, no chain.`
      : `Our live run of ${date} on Base Sepolia and Zcash testnet, its proofs MOCK seals checked by MockRiscZeroVerifier (MOCK): every hash below is that run's transaction. Your browser replays it (DEMO): no backend, no chain.`;
  const leak = inp.leakScan ? parseLeakScan(inp.leakScan) : null;
  if (leak && leak.matches !== 0) throw new HarvestError(`the leak scan found ${leak.matches} match(es): nothing is published`);

  const recording: Omit<ReplayRecording, "steps"> = {
    schema: REPLAY_SCHEMA,
    placeholder: false,
    recordedAt: run.recordedAt ?? iso(openedAt),
    note,
    network: { base: "base-sepolia", zcash: "zcash-testnet", chainId },
    explorers: { base: "https://sepolia.basescan.org/tx/", zcash: null },
    config: {
      ...config0,
      openVaultStampBits: Math.min(config0.openVaultStampBits, REPLAY_STAMP_BITS),
      denominations: [amount.toString()],
      contracts: { ...config0.contracts, ZecuredSwap: swap },
    },
    links: {
      relay,
      disputeWindow: num(v3, "disputeWindow", "deployment.v3"),
      inclusionWindow: num(v3, "inclusionWindow", "deployment.v3"),
      refundDormancy: num(v3, "refundDormancy", "deployment.v3"),
      maxMarginBps: num(d, "maxMarginBps", "deployment"),
      feeCommitment: str(v3, "feeCommitment", "deployment.v3") as Hex,
    },
    status: {
      commit: str(status0, "commit", "t0 status"),
      treeClean: status0.treeClean === true,
      deployment: {
        chainId: num(deployment0, "chainId", "t0 status.deployment"),
        registry: addr(deployment0.registry, "t0 status.deployment.registry"),
        intake: addr(deployment0.intake, "t0 status.deployment.intake"),
        signerBond: addr(deployment0.signerBond, "t0 status.deployment.signerBond"),
        mintMode: str(deployment0, "mintMode", "t0 status.deployment"),
        verifierIsMock: deployment0.verifierIsMock === true,
        depositImageId: str(deployment0, "depositImageId", "t0 status.deployment") as Hex,
        violationImageId: str(deployment0, "violationImageId", "t0 status.deployment") as Hex,
      },
      versions: versions0 as Record<string, ReplayVersion[]>,
    },
    chain: {
      baseBlock: inp.t0.baseBlock,
      zcashTip: num(status0, "zcashTip", "t0 status"),
      price: (price as bigint).toString(),
      borrowRate: borrowRate.toString(),
      market: {
        totalSupplyAssets: m[0].toString(),
        totalSupplyShares: m[1].toString(),
        totalBorrowAssets: m[2].toString(),
        totalBorrowShares: m[3].toString(),
        fee: m[5].toString(),
      },
      otherLockedZat: (locked as bigint).toString(),
      otherOpenVaults: { [amount.toString()]: typeof stats0[amount.toString()] === "number" ? (stats0[amount.toString()] as number) : 0 },
      bonds: {
        each: each.map((b) => (b as bigint).toString()),
        effective: (effective as bigint).toString(),
        totalSlashable: (totalSlashable as bigint).toString(),
        minBond: (minBond as bigint).toString(),
      },
      relayChainWork: tip[2].toString(),
    },
    loan: {
      owner: getAddress(loan.owner),
      account,
      requestId: lower(loan.requestId) as Hex,
      amount: amount.toString(),
      vault: {
        vaultId,
        address: loan.vaultAddress,
        zip321: zip321Uri(loan.vaultAddress, amount),
        expiryHeight: loan.expiryHeight,
        birthdayHeight: num(vault2, "birthdayHeight", "t2 vault"),
        tagKeyHash: lower(loan.tagKeyHash) as Hex,
        signatures: loan.vaultSignatures.map((s) => ({ index: s.index, signature: s.signature })),
      },
      depositTag: lower(pending.args.depositTag as Hex) as Hex,
      anchor: lower(journal.tipHash) as Hex,
      versionId: lower(version.versionId) as Hex,
      borrow: { assets: (borrow.args.assets as bigint).toString(), protected: isProtected },
      close: {
        executor,
        marginBps,
        repayAssets: (repaid.assets as bigint).toString(),
        mmAmount: (closed.args.mmAmount as bigint).toString(),
        userAmount: (closed.args.userAmount as bigint).toString(),
        userCommitment: lower(closed.args.userCommitment as Hex) as Hex,
        mmCommitment: lower(closed.args.mmCommitment as Hex) as Hex,
        payoutRef: lower(closeCommit.args.payoutRef as Hex) as Hex,
        inputTag: lower(closeCommit.args.inputTag as Hex) as Hex,
      },
    },
    demo: {
      returnAddress: str(demo0, "returnAddress", "t0 demo"),
      stampBits: Math.min(num(demo0, "stampBits", "t0 demo"), REPLAY_STAMP_BITS),
      dailyCapZat: String(need(demo0.dailyCapZat as string | number | undefined, "t0 demo.dailyCapZat")),
    },
    swap: {
      swap,
      maker: getAddress(order.args.maker as Address),
      taker,
      takerUsdc,
      zatoshi: (order.args.zatoshi as bigint).toString(),
      usdc: (order.args.usdc as bigint).toString(),
      makerReceiverCommitment: lower(quote.makerReceiverCommitment) as Hex,
      receiverCommitment: lower(order.args.receiverCommitment as Hex) as Hex,
      payWindow: Number(quote.payWindow),
      nonce: quote.nonce.toString(),
      orderId: lower(orderId) as Hex,
      anchor: lower(settling.args.anchor as Hex) as Hex,
      signing: "erc1271",
      signature: makerSig,
    },
    evidence: {
      commit: str(status0, "commit", "t0 status"),
      tag: run.tag ?? null,
      deployedAtBlock: Number(from),
      mocks: [
        { name: "MockUSDC", address: token, label: "MOCK", why: "The loan token, minted by the deployer." },
        { name: "DemoShockOracle", address: addr(contracts.DemoShockOracle, "deployment.contracts.DemoShockOracle"), label: "MOCK / DEMO", why: "An owner-set shock on the price, to show a liquidation." },
        { name: "MockZecOracle", address: addr(contracts.MockZecOracle, "deployment.contracts.MockZecOracle"), label: "MOCK", why: "An owner-set ZEC/USD price." },
        { name: "DemoCapPrice", address: addr(contracts.CapPrice, "deployment.contracts.CapPrice"), label: "MOCK / DEMO", why: "The coverage cap's price, always fresh." },
        ...(run.verifier === "mock"
          ? [{ name: "MockRiscZeroVerifier", address: addr(proofs.verifier, "deployment.depositProofs.verifier"), label: "MOCK" as const, why: "Accepts MOCK seals in place of RISC Zero's router." }]
          : []),
        { name: "This replay", address: null, label: "DEMO", why: "The judges' build replays the run in the browser: no backend, no chain." },
      ],
      leakScan: leak
        ? { result: "0 matches", fromBlock: Number(from), toBlock: run.leakScan?.toBlock ?? inp.t2.baseBlock, identifiers: leak.identifiers, at: run.leakScan?.at ?? inp.t2.at }
        : null,
    },
  };

  // The steps: when each happened, and the hashes that made it.
  const z = inp.zcash;
  const base = (f: Found) => lower(f.tx) as Hex;
  const proofArtifact = (label: string, tx: Hex): ReplayArtifact => ({ label, chain: "base", tx, kind: "proof", meta: { imageId: imageIds.violation, prover } });
  const feeExtras: ReplayArtifact[] = [];
  if (feePaidAlone) {
    if (feeCommit) feeExtras.push({ label: "Opening fee: the payout's commitment", chain: "base", tx: base(feeCommit), kind: "tx" });
    if (z.feePayout) feeExtras.push({ label: "Opening fee: paid on Zcash, alone", chain: "zcash", tx: z.feePayout.txid, kind: "tx" });
    const feeConfirm = confirmOf(feeCommit);
    if (feeConfirm) feeExtras.push({ label: "Opening fee: confirmed by the signers", chain: "base", tx: base(feeConfirm), kind: "tx" });
    const feeProof = provenOf(feeClaimId);
    if (feeProof) feeExtras.push(proofArtifact("Opening fee: its payout proven on Base", base(feeProof)));
  }
  const confirmExtras: ReplayArtifact[] =
    closeConfirm && !same(closeConfirm.tx, closeProof.tx)
      ? [{ label: "PayoutConfirmed: the signers' confirmation of the payout", chain: "base", tx: base(closeConfirm), kind: "tx" }]
      : [];
  const fact = (atS: number | null, tx: ReplayStep["tx"], artifacts: (string | null)[], extra: ReplayArtifact[] = []): StepFact => ({ at: atS, tx, artifacts, extra });
  const steps: Record<ReplayStepId, StepFact> = {
    opened: fact(openedAt, {}, [null]),
    "deposit-paid": fact(await zcashTime(z.deposit.height), { zcash: z.deposit.txid }, [z.deposit.txid]),
    "deposit-seen": fact(await zcashTime(z.deposit.height + confirmations - 1), {}, []),
    proven: fact(await blockTime(pending.block), { base: base(pending) }, [base(pending)]),
    activated: fact(await blockTime(activated.block), { base: base(activated) }, [base(activated)], feeExtras),
    borrowed: fact(await blockTime(borrow.block), { base: base(borrow), userOp: lower(userOp.userOpHash as Hex) as Hex }, [base(borrow)]),
    destination: fact(null, {}, []),
    "close-executed": fact(await blockTime(closed.block), { base: base(closed) }, [base(closed)]),
    "payouts-committed": fact(await blockTime(closeCommit.block), { base: base(closeCommit) }, [base(closeCommit)]),
    "payouts-sent": fact(await zcashTime(z.closePayout.height), { zcash: z.closePayout.txid }, [z.closePayout.txid]),
    "payouts-confirmed": fact(await blockTime(closeProof.block), { base: base(closeProof) }, [base(closeProof)], confirmExtras),
    "swap-opened": fact(await blockTime(order.block), { base: base(order), approve: base(approval) }, [base(approval), base(order)]),
    "swap-paid": fact(await zcashTime(z.swapPayment.height), { zcash: z.swapPayment.txid }, [z.swapPayment.txid]),
    "swap-proven": fact(await blockTime(settling.block), { base: base(settling) }, [base(settling)]),
    "swap-settled": fact(await blockTime(settled.block), { base: base(settled) }, [base(settled)]),
  };
  return { recording, imageIds, prover, feePaidAlone, steps };
}

/** The run, step by step, for docs/TESTING.md: UTC time, duration, and the hashes. */
export function summary(r: ReplayRecording): string {
  const rows = r.steps.map((s) => {
    const when = s.at ? s.at.replace("T", " ").replace(/\.\d+Z$/, " UTC") : "—";
    const took = s.durationSeconds === null ? "—" : s.durationSeconds < 90 ? `${s.durationSeconds} s` : `${Math.floor(s.durationSeconds / 60)} min ${s.durationSeconds % 60} s`;
    const hashes = [s.tx.base && `Base \`${s.tx.base}\``, s.tx.userOp && `UserOp \`${s.tx.userOp}\``, s.tx.approve && `approval \`${s.tx.approve}\``, s.tx.zcash && `Zcash \`${s.tx.zcash}\``]
      .filter(Boolean)
      .join("<br>");
    return `| ${s.id} | ${when} | ${took} | ${hashes || "—"} |`;
  });
  return [
    `# The live run, ${r.recordedAt ?? ""}`,
    "",
    `Vault \`${r.loan.vault.vaultId}\`, ${formatZec(BigInt(r.loan.amount))} TAZ; borrowed ${formatUsdc(BigInt(r.loan.borrow.assets))} MockUSDC; swap ${formatUsdc(BigInt(r.swap.usdc))} MockUSDC for ${formatZec(BigInt(r.swap.zatoshi))} TAZ.`,
    "",
    "| Step | When | After the previous step | Transactions |",
    "|---|---|---|---|",
    ...rows,
    "",
  ].join("\n");
}

/** Every step id has a fact (a guard for hand-made facts). */
export function assertAllSteps(steps: Partial<Record<ReplayStepId, StepFact>>): asserts steps is Record<ReplayStepId, StepFact> {
  for (const id of REPLAY_STEP_IDS) if (!steps[id]) throw new HarvestError(`no facts for the step ${id}`);
}
