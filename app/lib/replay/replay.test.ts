import { describe, expect, it } from "vitest";
import { createPublicClient, getAddress, http, type Hex, type PublicClient } from "viem";
import { baseSepolia } from "viem/chains";
import { ApiError, createApi } from "../api";
import { borrowCalls } from "../calls";
import { isNonceUsed, readPosition, readRecentRegistryEvents, readVaultLedger } from "../chain";
import { readJourney } from "../journey";
import { buildConfig, type AppConfig } from "../config";
import { solveDemoStamp } from "../demo";
import { openVault, submitDestination } from "../owner";
import { readProtocol, readSupply } from "../public";
import { buyCalls, followOrder, openedOrder, quoteProblem, quoteSignedByMaker, type StoredSwapOrder } from "../swap";
import { replayLoanAccount, REPLAY_SIGNATURE } from "./account";
import { createReplayFetch } from "./fetch";
import { REPLAY_BASE_URL } from "./flag";
import { stepViews } from "./index";
import { createReplayLoan } from "./loans";
import { BASE_BLOCK_MS } from "./model";
import { replayBuildEnv } from "./pins";
import { recording } from "./recording";
import type { ReplayStepId } from "./types";
import { ReplayWorld, WORLD_KEY, type WorldStorage } from "./world";
import { replayWallet } from "./wallet";

const rec = recording();

/** A replay on a hand-driven clock, with no pauses; its backend client, its Base and the app's config. */
async function setup(storage: WorldStorage | null = null) {
  const clock = { t: 1_800_000_000_000 };
  const w = new ReplayWorld(rec, { now: () => clock.t, latency: false, storage });
  const fetchFn = createReplayFetch(w);
  const api = createApi({ baseUrl: REPLAY_BASE_URL, fetch: fetchFn });
  const config: AppConfig = buildConfig(replayBuildEnv("production", rec), await api.getConfig());
  // No block-number cache: the test's clock jumps seconds between two reads.
  const client = createPublicClient({
    chain: baseSepolia,
    cacheTime: 0,
    transport: http(`${REPLAY_BASE_URL}/proxy/rpc`, { fetchFn, retryCount: 0 }),
  }) as PublicClient;
  const advance = (ms: number) => {
    clock.t += ms;
  };
  return { w, api, config, client, advance, clock };
}

function memoryStorage(): WorldStorage & { map: Map<string, string>; length: number; key: (i: number) => string | null } {
  const map = new Map<string, string>();
  return {
    map,
    get length() {
      return map.size;
    },
    key: (i) => [...map.keys()][i] ?? null,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

const delay = (id: string) => rec.steps.find((s) => s.id === id)!.delayMs;
const txOf = (id: string) => rec.steps.find((s) => s.id === id)!.tx;

/**
 * A poll in the 2 s Base block of automatic step `id`, just before the step, then one a block after it: what a
 * page polling every few seconds can do. The second poll must still see the step's events.
 */
async function aroundStep(w: ReplayWorld, clock: { t: number }, id: ReplayStepId, poll: () => Promise<unknown>) {
  const due = w.doneAt(id)!;
  const blockStart = w.epochMs + Math.floor((due - w.epochMs) / BASE_BLOCK_MS) * BASE_BLOCK_MS;
  expect(blockStart).toBeLessThan(due);
  clock.t = blockStart;
  await poll();
  clock.t = due + BASE_BLOCK_MS;
  await poll();
}

describe("the replay", () => {
  it("walks the recorded loan as the live run did, through the app's own code", async () => {
    const { w, api, config, client, advance } = await setup();
    const vaultId = rec.loan.vault.vaultId;
    const amount = BigInt(rec.loan.amount);
    expect(config.problems).toEqual([]);
    expect(config.contractsDeployed && config.ownerSigning && config.sponsorship && config.demoPath).toBe(true);
    expect(await api.health()).toBe(true);
    expect((await api.getStatus()).nodes).toHaveLength(5);
    expect((await api.getDemoStatus()).enabled).toBe(true);
    expect(await api.getEvidence()).not.toBeNull();

    // Nothing is open yet: the vault is unknown, and a borrow is not the run's next step.
    await expect(api.getVault(vaultId)).rejects.toMatchObject({ status: 404 });
    const account = replayLoanAccount("0x01", w);
    const calls = (assets: bigint) =>
      borrowCalls({ account: rec.loan.account, assets, registryAuthorized: false, adapterAuthorized: false, config });
    await expect(account.send(calls(900_000n))).rejects.toThrow(/next step here is “Open a vault”/);

    // 1. Open the vault: the five pinned signers' signatures check out in the app's own code.
    const loan = createReplayLoan(amount, Date.now());
    expect(loan.account).toBe(getAddress(rec.loan.account));
    const opened = await openVault(loan, { api, config });
    expect(opened.check).toBe("pinned");
    expect(opened.vaultId).toBe(vaultId);
    const stored = { ...loan, vaultId, vaultAddress: opened.address, expiryHeight: opened.expiryHeight, tagKeyHash: opened.tagKeyHash, vaultSignatures: opened.signatures };
    // A retry returns the same vault.
    expect((await openVault(loan, { api, config })).vaultId).toBe(vaultId);
    expect((await api.getVault(vaultId)).status).toBe("awaiting_deposit");
    expect((await readVaultLedger(vaultId, { client, config })).status).toBe("None");

    // 2. The demo wallet pays it.
    const stamp = await solveDemoStamp(vaultId, rec.demo.stampBits);
    const pay = {
      owner: stored.owner,
      account: stored.account,
      amount,
      expiryHeight: stored.expiryHeight,
      address: stored.vaultAddress,
      tagKeyHash: stored.tagKeyHash,
      signatures: stored.vaultSignatures,
      stamp,
    };
    expect((await api.demoPay(vaultId, pay)).txid).toBe(txOf("deposit-paid").zcash);
    await expect(api.demoPay(vaultId, pay)).rejects.toMatchObject({ code: "demo_already_paid" });

    // 3. Confirmations come in, the proof lands on Base, the deposit is activated.
    advance(delay("deposit-seen"));
    let v = await api.getVault(vaultId);
    expect([v.status, v.depositSeen, v.confirmations]).toEqual(["deposit_seen", true, 1]);
    advance(3_000);
    expect((await api.getVault(vaultId)).confirmations).toBe(rec.config.confirmations);
    expect((await api.getStatus()).depositProofs?.running).toBe(1);
    advance(delay("proven"));
    let ledger = await readVaultLedger(vaultId, { client, config });
    expect(ledger.status).toBe("Pending");
    expect(ledger.pending?.amount).toBe(amount);
    expect((await readProtocol({ client, config })).pendingTotal).toBe(amount);
    advance(delay("activated"));
    v = await api.getVault(vaultId);
    const fee = (amount * BigInt(rec.config.v3.feeBps)) / 10_000n;
    expect([v.status, v.outstanding]).toEqual(["active", amount - fee]);
    ledger = await readVaultLedger(vaultId, { client, config });
    expect(ledger.status).toBe("Active");
    expect(ledger.claims.map((c) => [c.kind, c.status, c.amount])).toEqual([["Fee", "Pending", fee]]);
    let p = await readPosition(rec.loan.account, { client, config });
    expect([p.position.collateral, p.debt, p.accountDeployed, p.registryAuthorized]).toEqual([amount - fee, 0n, false, false]);

    // 4. Borrow: exactly the recorded amount, in one UserOp whose transaction is the run's.
    await expect(account.send(calls(950_000n))).rejects.toThrow(/exactly 0.90 USDC/);
    const sent = await account.send(calls(900_000n));
    expect(sent).toEqual({ userOpHash: txOf("borrowed").userOp, transactionHash: txOf("borrowed").base, success: true });
    advance(60_000);
    p = await readPosition(rec.loan.account, { client, config });
    expect(p.accountDeployed && p.registryAuthorized && p.adapterAuthorized).toBe(true);
    expect(p.usdcBalance).toBe(900_000n);
    expect(p.debt).toBeGreaterThanOrEqual(900_000n);
    expect(p.debt).toBeLessThan(900_100n);

    // 5. A destination the signers validate (the app checks the commitment is its own), then the close.
    const dest = await submitDestination(stored, vaultId, rec.demo.returnAddress, { api, config });
    const quote = await api.getCloseQuote(vaultId);
    expect([quote.executor, quote.mmAmount + quote.userAmount]).toEqual([getAddress(rec.loan.close.executor), amount - fee]);
    const req = {
      vaultId,
      account: rec.loan.account,
      executor: quote.executor,
      marginBps: quote.marginBps,
      minUserAmount: (quote.userAmount * 99n) / 100n,
      userCommitment: dest.commitment,
      nonce: 1n,
      deadline: BigInt(Math.floor(Date.now() / 1000) + 600),
    };
    expect(await account.signCloseRequest(req)).toBe(REPLAY_SIGNATURE);
    expect(await api.submitCloseRequest(req, REPLAY_SIGNATURE)).toBe(txOf("close-executed").base);
    expect((await api.getVault(vaultId)).status).toBe("closing");
    expect(await isNonceUsed(rec.loan.account, 1n, { client, config })).toBe(true);
    ledger = await readVaultLedger(vaultId, { client, config });
    expect(ledger.status).toBe("Closed");
    expect(ledger.claims.map((c) => [c.kind, c.status, c.committed])).toEqual([
      ["Fee", "Pending", false],
      ["CloseUser", "Pending", false],
      ["CloseMarketMaker", "Pending", false],
    ]);
    p = await readPosition(rec.loan.account, { client, config });
    expect([p.position.collateral, p.position.borrowShares, p.debt]).toEqual([0n, 0n, 0n]);

    // 6. The payout: committed, paid on Zcash, proven on Base.
    advance(delay("payouts-committed"));
    expect((await api.getVault(vaultId)).payoutInFlight).toBe(true);
    ledger = await readVaultLedger(vaultId, { client, config });
    expect(ledger.claims[1]?.committed).toBe(true);
    advance(delay("payouts-sent") + delay("payouts-confirmed"));
    v = await api.getVault(vaultId);
    expect(v.status).toBe("closed");
    expect(v.claims.every((c) => c.status === "paid" && c.proven)).toBe(true);
    ledger = await readVaultLedger(vaultId, { client, config });
    expect(ledger.unpaidClaims).toBe(0);
    expect(ledger.claims.every((c) => c.status === "Paid" && c.paidAt === 0)).toBe(true);
    // The proof's logs show once its block is sealed, the next one.
    advance(BASE_BLOCK_MS);
    const events = await readRecentRegistryEvents({ client, config });
    expect(events.map((e) => e.name).sort()).toEqual(
      ["CloseRelease", "FeeCharged", "PayoutConfirmed", "PayoutProven", "PayoutProven", "PayoutProven", "VaultActivated"].sort(),
    );
    expect(events.find((e) => e.name === "CloseRelease")?.transactionHash).toBe(txOf("close-executed").base);
    expect(w.pending("loan")).toBeNull();
    expect((await readProtocol({ client, config })).lockedZat).toBe(0n);
  });

  it("walks the recorded swap with the replay's wallet", async () => {
    const { w, api, config, client, advance, clock } = await setup();
    const s = rec.swap;
    // Half a block in: each automatic step is then due inside a block, after its start (see aroundStep).
    advance(BASE_BLOCK_MS / 4);
    const wallet = replayWallet(w);
    const [taker] = (await wallet.request({ method: "eth_requestAccounts" })) as string[];
    expect(taker).toBe(getAddress(s.taker));
    await expect(api.getSwapQuote({ usdc: 10_000_000n, taker: getAddress(taker!) })).rejects.toThrow(/exactly 25 USDC/);
    const q = await api.getSwapQuote({ usdc: BigInt(s.usdc), taker: getAddress(taker!) });
    expect(quoteProblem(q, { swap: config.swap, taker: getAddress(taker!), nowSeconds: Math.floor(w.now() / 1000) })).toBeNull();
    expect(await quoteSignedByMaker(client, q)).toBe(true);
    // The market page's lending form reads the same wallet (it holds the run's USDC, supplies nothing).
    expect(await readSupply(getAddress(taker!), { client, config })).toEqual({ shares: 0n, assets: 0n, balance: BigInt(s.takerUsdc), allowance: 0n });

    const allowance = await client.readContract({
      address: config.usdc,
      abi: [{ type: "function", name: "allowance", stateMutability: "view", inputs: [{ name: "o", type: "address" }, { name: "s", type: "address" }], outputs: [{ name: "", type: "uint256" }] }] as const,
      functionName: "allowance",
      args: [getAddress(taker!), config.swap!],
    });
    expect(allowance).toBe(0n);
    const commitment = `0x${"ab".repeat(32)}` as Hex;
    const [approve, open] = buyCalls({ swap: config.swap!, usdcToken: config.usdc, quote: q, commitment, allowance });
    // Opening before the approval reverts, as the token would.
    await expect(wallet.request({ method: "eth_sendTransaction", params: [{ from: taker, ...open }] })).rejects.toThrow(/not approved/);
    const approveHash = (await wallet.request({ method: "eth_sendTransaction", params: [{ from: taker, ...approve }] })) as Hex;
    expect(approveHash).toBe(txOf("swap-opened").approve);
    expect((await client.waitForTransactionReceipt({ hash: approveHash, pollingInterval: 10 })).status).toBe("success");
    // The app checks the order on Base before the wallet signs it.
    const check = await client.call({ account: getAddress(taker!), to: open!.to, data: open!.data });
    expect(check.data).toBe(s.orderId);
    const openHash = (await wallet.request({ method: "eth_sendTransaction", params: [{ from: taker, ...open }] })) as Hex;
    expect(openHash).toBe(txOf("swap-opened").base);
    const receipt = await client.waitForTransactionReceipt({ hash: openHash, pollingInterval: 10 });
    const order = openedOrder(receipt.logs, config.swap!);
    expect(order?.orderId).toBe(s.orderId.toLowerCase());

    const stored: StoredSwapOrder = {
      v: 1,
      orderId: order!.orderId,
      swap: config.swap!,
      chainId: 84532,
      maker: getAddress(s.maker),
      taker: getAddress(taker!),
      zatoshi: s.zatoshi,
      usdc: s.usdc,
      payDeadline: order!.payDeadline,
      openedBlock: receipt.blockNumber.toString(),
      scannedTo: receipt.blockNumber.toString(),
      openTx: openHash,
      receiver: rec.demo.returnAddress,
      salt: `0x${"cd".repeat(32)}`,
      commitment,
      destinationSent: false,
      proven: false,
      settled: false,
      refunded: false,
      createdAt: Date.now(),
    };
    expect(await api.getSwapOrder(s.orderId)).toBe("awaiting_destination");
    await api.postSwapDestination(s.orderId, rec.demo.returnAddress, stored.salt);
    expect(await api.getSwapOrder(s.orderId)).toBe("destination_received");
    // The page follows the order every few seconds, its cursor carried from one read to the next.
    let followed = stored;
    const follow = async () => {
      followed = await followOrder(client, followed);
    };
    await aroundStep(w, clock, "swap-paid", follow);
    expect(await api.getSwapOrder(s.orderId)).toBe("paid");
    expect(followed.proven).toBe(false);
    await aroundStep(w, clock, "swap-proven", follow);
    expect(followed.proven).toBe(true);
    await aroundStep(w, clock, "swap-settled", follow);
    expect([followed.proven, followed.settled, followed.refunded]).toEqual([true, true, false]);
    expect(await api.getSwapOrder(s.orderId)).toBe("settled");
    expect(w.pending("swap")).toBeNull();
    // The swap's track is independent of the loan's, which has not started.
    expect(w.pending("loan")?.id).toBe("opened");
  });

  it("lists every step of the loan's trail, even when a read lands in a step's block just before it", async () => {
    const { w, config, client, clock } = await setup();
    const poll = () => readJourney(rec.loan.vault.vaultId, getAddress(rec.loan.account), { since: w.epochMs, client, config });
    clock.t += BASE_BLOCK_MS / 4;
    w.complete("opened");
    w.complete("deposit-paid");
    for (const id of ["deposit-seen", "proven", "activated"] as const) await aroundStep(w, clock, id, poll);
    w.complete("borrowed");
    w.complete("destination");
    clock.t += BASE_BLOCK_MS;
    await poll();
    w.complete("close-executed");
    for (const id of ["payouts-committed", "payouts-sent", "payouts-confirmed"] as const) await aroundStep(w, clock, id, poll);
    const j = await poll();
    expect(j.steps.map((s) => s.kind)).toEqual(["proven", "activated", "borrowed", "close", "committed", "confirmed", "payout-proven"]);
    expect(j.steps.map((s) => s.txHash)).toEqual(
      (["proven", "activated", "borrowed", "close-executed", "payouts-committed", "payouts-confirmed", "payouts-confirmed"] as const).map(
        (id) => txOf(id).base,
      ),
    );
    expect([j.complete, j.truncated]).toEqual([true, false]);
  });

  it("refuses what the replay never does", async () => {
    const { api } = await setup();
    await expect(api.adminNodes("token")).rejects.toMatchObject({ status: 401, code: "replay_unavailable" });
    expect(() => createReplayLoan(1n, Date.now())).toThrow(/only the live run's vault size/);
    await expect(api.getVault(rec.loan.vault.vaultId)).rejects.toBeInstanceOf(ApiError);
  });

  it("refuses the viewing key and the deleverage, and resumes after a reload", async () => {
    const storage = memoryStorage();
    const first = await setup(storage);
    const vaultId = rec.loan.vault.vaultId;
    const loan = createReplayLoan(BigInt(rec.loan.amount), Date.now());
    await openVault(loan, { api: first.api, config: first.config });
    await expect(first.api.postViewingKey(vaultId, { nonce: 1n, deadline: 2n, signature: REPLAY_SIGNATURE })).rejects.toMatchObject({
      code: "replay_unavailable",
    });
    await expect(first.api.getDeleverageQuote(vaultId, 1n)).rejects.toMatchObject({ code: "replay_unavailable" });
    expect(storage.map.has(WORLD_KEY)).toBe(true);

    // A new page in the same tab (sessionStorage) resumes where the replay was.
    const again = new ReplayWorld(rec, { storage, latency: false, now: () => first.clock.t });
    expect(again.isDone("opened")).toBe(true);
    expect(stepViews(again).find((s) => s.id === "deposit-paid")?.status).toBe("next");

    // Reset forgets the replay's keys only.
    storage.setItem("zecured:replay:loans:v1", "{}");
    storage.setItem("other", "kept");
    again.reset();
    expect(storage.map.has("zecured:replay:loans:v1")).toBe(false);
    expect(storage.getItem("other")).toBe("kept");
    expect(again.isDone("opened")).toBe(false);
  });
});
