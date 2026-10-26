"use client";

import { useCallback, useEffect, useId, useState } from "react";
import { getAddress, type Address, type Hex } from "viem";
import { ActionError } from "@/components/ActionError";
import { CopyButton, useMounted } from "@/components/client";
import { IronwoodAddressInput, destinationError } from "@/components/DestinationForm";
import { IconArrowDown, IconArrowRight, IconCheck, IconRefresh, IconShield, IconWallet } from "@/components/icons";
import { UsdcMark, ZecMark } from "@/components/marks";
import { ProofSeal, SeamTracker, type TrackerRow } from "@/components/seam";
import { Badge, Button, Card, Hint, Mono, Notice, Spinner } from "@/components/ui";
import { erc20AllowanceAbi } from "@/lib/abi4";
import { api, describeError, type MakerOrderStatus, type SignedSwapQuote } from "@/lib/api";
import { formatUtc } from "@/lib/calls";
import { publicClient } from "@/lib/chain";
import { prepareDestination, type PreparedDestination } from "@/lib/commitment";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useAction } from "@/lib/hooks";
import { loanTokenName } from "@/lib/labels";
import { replayDemoAddress } from "@/lib/replay";
import { REPLAY } from "@/lib/replay/flag";
import { recording } from "@/lib/replay/recording";
import { browserStorage, short } from "@/lib/keys";
import { formatUsdc, parseUsdc } from "@/lib/morpho";
import { buyCalls, followOrder, openedOrder, quoteProblem, quoteSignedByMaker, refundCall, SwapStore, swapStatus, type StoredSwapOrder, type SwapStatus } from "@/lib/swap";
import { SWAP_PRIVACY_SENTENCE } from "@/lib/v4";
import { connectWallet, ensureChain, injectedWallet, sendFromWallet, WalletError } from "@/lib/wallet";
import { formatTaz, formatZec, TICKER } from "@/lib/zcash";
import { QrScan } from "./QrScan";

const store = () => new SwapStore(browserStorage());
const nowSeconds = () => Math.floor(Date.now() / 1000);

/** Waits for a transaction through our proxy (never the wallet's provider). */
async function receiptOf(hash: Hex) {
  return publicClient().waitForTransactionReceipt({ hash, timeout: 180_000, pollingInterval: 3_000 });
}

/** Two decimals of a rate, rounded: what a person compares. */
const rate2 = (units: bigint) => {
  const cents = (units + 5_000n) / 10_000n;
  return `${(cents / 100n).toLocaleString("en-US")}.${(cents % 100n).toString().padStart(2, "0")}`;
};

/**
 * The "Buy shielded ZEC" page (section 5.7 of the v4 design): a quote relayed from the maker, an
 * Ironwood address pasted or scanned, the salt and commitment made here and kept here, the user's
 * own wallet approving the USDC and calling `open`, then the order followed on Base: open → paid →
 * proven → settled, or a refund after the deadline. One primary button, named after what is needed next.
 */
export function BuyZec() {
  const mounted = useMounted();
  const amountId = useId();
  const inviteId = useId();
  // The mainnet beta's maker quotes invitees only (sent as a header, never kept by this page).
  const [invite, setInvite] = useState("");
  const [input, setInput] = useState("");
  const [ua, setUa] = useState("");
  const [quote, setQuote] = useState<SignedSwapQuote | null>(null);
  const [wallet, setWallet] = useState<Address | null>(null);
  const [orders, setOrders] = useState<StoredSwapOrder[]>([]);
  // The destination is salted once per quote, so a retried purchase commits to the same one.
  const [dest, setDest] = useState<PreparedDestination | null>(null);
  const [stage, setStage] = useState<string | null>(null);
  const [opened, setOpened] = useState<StoredSwapOrder | null>(null);
  const reload = useCallback(() => setOrders(store().list()), []);
  useEffect(() => reload(), [reload]);

  const swap = config.swap;
  const token = config.usdc;
  const name = loanTokenName();
  const usable = swap !== null && !/^0x0{40}$/i.test(token);
  let usdc: bigint | null = null;
  let inputError: string | null = null;
  if (input.trim()) {
    try {
      usdc = parseUsdc(input);
      if (usdc === 0n) inputError = "Enter an amount above zero.";
    } catch (e) {
      inputError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  // DEMO: the replay's wallet speaks for itself in the progress lines.
  const as = (live: string, demo: string) => (REPLAY ? `DEMO: ${demo}` : live);

  const connect = useAction(async () => {
    const w = injectedWallet();
    if (!w) throw new WalletError("No browser wallet found. Install one that supports Base, then reload this page.");
    const a = await connectWallet(w);
    await ensureChain(w, config.chainId, DEPLOYMENT.chainName);
    setWallet(a);
    return a;
  });
  const getQuote = useAction(async (amount: bigint) => {
    const q = await api.getSwapQuote({ usdc: amount, taker: wallet ?? undefined, invite: DEPLOYMENT.invites ? invite : undefined });
    const problem = quoteProblem(q, { swap, taker: wallet, nowSeconds: nowSeconds() });
    if (problem) throw new Error(problem);
    // The swap contract checks the maker's signature at `open`: a quote that fails it is not shown.
    if (!(await quoteSignedByMaker(publicClient(), q))) throw new Error("The maker's signature on this quote does not check out: it is not used.");
    setQuote(q);
    setDest(null);
    return q;
  });
  const buy = useAction(async () => {
    const w = injectedWallet();
    if (!w || !wallet) throw new WalletError("Connect your wallet first.");
    if (!quote || !swap) throw new Error("Get a quote first.");
    const problem = quoteProblem(quote, { swap, taker: wallet, nowSeconds: nowSeconds() });
    if (problem) throw new Error(problem);
    await ensureChain(w, config.chainId, DEPLOYMENT.chainName);
    const d = dest ?? prepareDestination(ua, DEPLOYMENT.zcashNetwork);
    setDest(d);
    const c = publicClient();
    setStage(`Reading your ${name} allowance…`);
    const allowance = await c.readContract({ address: token, abi: erc20AllowanceAbi, functionName: "allowance", args: [wallet, swap] });
    const calls = buyCalls({ swap, usdcToken: token, quote, commitment: d.commitment, allowance });
    const amount = `${formatUsdc(quote.quote.usdc)} ${name}`;
    for (const [i, call] of calls.entries()) {
      const last = i === calls.length - 1;
      if (last) {
        setStage("Checking the order on Base before the wallet signs it…");
        try {
          await c.call({ account: wallet, to: call.to, data: call.data });
        } catch (e) {
          throw new Error(`The swap contract would refuse this order (${describeError(e)}). Nothing was sent.`);
        }
      }
      setStage(
        last
          ? as("Confirm the order in your wallet…", "the replay's wallet opens the order…")
          : as(`Approve exactly ${amount} in your wallet…`, `the replay's wallet approves exactly ${amount}…`),
      );
      const hash = await sendFromWallet(w, { from: wallet, to: call.to, data: call.data });
      setStage(last ? "Waiting for the order on Base…" : "Waiting for the approval on Base…");
      const receipt = await receiptOf(hash);
      if (receipt.status !== "success") throw new Error(last ? "The order reverted on Base." : "The approval reverted on Base.");
      if (last) {
        const opened = openedOrder(receipt.logs, swap);
        if (!opened) throw new Error("The order is on Base, but its OrderOpened event was not found.");
        const order: StoredSwapOrder = {
          v: 1,
          orderId: opened.orderId,
          swap,
          chainId: config.chainId,
          maker: quote.quote.maker,
          taker: wallet,
          zatoshi: quote.quote.zatoshi.toString(),
          usdc: quote.quote.usdc.toString(),
          payDeadline: opened.payDeadline,
          openedBlock: receipt.blockNumber.toString(),
          scannedTo: receipt.blockNumber.toString(),
          openTx: hash,
          receiver: d.ua,
          salt: d.saltHex,
          commitment: d.commitment,
          destinationSent: false,
          proven: false,
          settled: false,
          refunded: false,
          createdAt: Date.now(),
        };
        // Kept before the maker hears of it: the receiver and the salt must survive a failed hand-off.
        store().save(order);
        setStage("Telling the maker where to pay (off-chain, through our backend)…");
        try {
          await api.postSwapDestination(order.orderId, d.ua, d.saltHex);
          store().save({ ...order, destinationSent: true });
        } catch {
          // The order's card offers to send it again.
        }
        setOpened(order);
        reload();
      }
    }
    setStage(null);
    setQuote(null);
    setDest(null);
    setInput("");
    return true;
  });

  if (!mounted) return <p className="text-sm text-muted">Loading…</p>;
  const recorded = REPLAY ? formatUsdc(BigInt(recording().swap.usdc)) : null;
  const demoAddress = REPLAY ? replayDemoAddress() : null;
  const addressOk = Boolean(ua.trim()) && destinationError(ua) === null;
  // The one primary button: what is needed next, then the purchase.
  const next: { label: string; target?: string; run?: () => void; disabled?: boolean } = !usdc || inputError
    ? { label: "Enter an amount", disabled: true }
    : !quote
      ? { label: "Get the maker's quote", target: "swap-quote", run: () => void getQuote.run(usdc!) }
      : !addressOk
        ? { label: "Add your Ironwood address", disabled: true }
        : !wallet
          ? { label: REPLAY ? "Connect the demo wallet (DEMO)" : "Connect your wallet", target: "connect-wallet", run: () => void connect.run() }
          : { label: `Buy ${formatTaz(quote.quote.zatoshi)} (locks ${formatUsdc(quote.quote.usdc)} ${name})`, target: "buy", run: () => void buy.run() };
  const busy = getQuote.busy || connect.busy || buy.busy;

  const howItWorks = [
    { icon: <UsdcMark size={22} />, title: `Your ${name} waits on ${DEPLOYMENT.chainName}`, body: "Your wallet opens the order: the tokens sit in the swap contract, not with anyone." },
    { icon: <ZecMark size={22} />, title: "The maker pays you on Zcash", body: "A shielded payment to your Ironwood address, from the maker's own ZEC." },
    { icon: <ProofSeal size="sm" className="!size-[22px]" />, title: "Proven on Base in zero knowledge", body: "A proof that the payment is in a Zcash block reaches Base. No custodian, no signer, no wrapped ZEC." },
    { icon: <IconCheck size={20} strokeWidth={2.4} className="text-ok" />, title: "Settled, or refunded", body: `Only then is the ${name} released to the maker. If it does not pay and prove in time, you take it back.` },
  ];

  return (
    <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,30rem)_minmax(0,1fr)] [&>*]:min-w-0">
      <div className="space-y-4">
        {!usable ? (
          <Notice tone="warn" title="The swap is not configured in this build">
            <p>This deployment has no swap contract pinned (NEXT_PUBLIC_SWAP), or no USDC token.</p>
          </Notice>
        ) : opened ? (
          <Card lead title="Order opened">
            <div className="space-y-4">
              <p className="text-[15px] text-soft">
                {formatUsdc(BigInt(opened.usdc))} {name} locked on Base. The maker now pays {formatTaz(BigInt(opened.zatoshi))} to your
                Ironwood address; your {name} is released only when that payment is proven on Base.
              </p>
              <Hint>Follow it beside this card. If the maker does not pay and prove by {formatUtc(opened.payDeadline)}, you take it back.</Hint>
              <Button variant="ghost" onClick={() => setOpened(null)}>
                Buy more
              </Button>
            </div>
          </Card>
        ) : (
          <Card lead>
            <div className="space-y-4">
              <h2 className="text-[19px] font-extrabold font-stretch-116%">Swap</h2>
              <div className="rounded-[20px] bg-bg/80 p-4 shadow-[inset_0_0_0_1px_var(--color-control)] focus-within:shadow-[inset_0_0_0_2px_var(--color-text)]">
                <label htmlFor={amountId} className="text-[13px] font-semibold text-muted">
                  You pay
                </label>
                <div className="mt-1.5 flex items-center gap-3">
                  <input
                    id={amountId}
                    inputMode="decimal"
                    autoComplete="off"
                    value={input}
                    onChange={(e) => {
                      setInput(e.target.value);
                      setQuote(null);
                    }}
                    placeholder="0.00"
                    aria-invalid={inputError ? true : undefined}
                    className="w-full min-w-0 bg-transparent text-[36px] leading-none font-bold tabular-nums text-text outline-none placeholder:text-faint"
                  />
                  <span className="inline-flex flex-none items-center gap-2 text-[16px] font-bold">
                    <UsdcMark size={28} />
                    {name}
                  </span>
                </div>
                {inputError ? <p className="mt-2 text-sm text-danger">{inputError}</p> : null}
                {recorded !== null && input !== recorded ? (
                  <Button
                    variant="demo"
                    size="sm"
                    className="mt-3"
                    data-replay="swap-use-amount"
                    onClick={() => {
                      setInput(recorded);
                      setQuote(null);
                    }}
                  >
                    DEMO: use the run&apos;s {recorded}
                  </Button>
                ) : null}
              </div>

              <div aria-hidden="true" className="relative z-[1] -my-6 flex justify-center">
                <span className="inline-flex size-10 items-center justify-center rounded-xl bg-surface-3 text-soft shadow-[0_0_0_4px_var(--color-surface),inset_0_0_0_1px_var(--color-line)]">
                  <IconArrowDown size={18} />
                </span>
              </div>

              <div className="rounded-[20px] bg-bg/80 p-4 shadow-[inset_0_0_0_1px_var(--color-border)]">
                <p className="text-[13px] font-semibold text-muted">You receive, shielded</p>
                <div className="mt-1.5 flex items-center gap-3">
                  <span className={quote ? "w-full min-w-0 truncate text-[36px] leading-none font-bold tabular-nums text-text" : "w-full min-w-0 truncate text-[36px] leading-none font-bold tabular-nums text-faint"}>
                    {quote ? formatZec(quote.quote.zatoshi) : "0.00"}
                  </span>
                  <span className="inline-flex flex-none items-center gap-2 text-[16px] font-bold">
                    <ZecMark size={28} />
                    {TICKER}
                  </span>
                </div>
                {quote ? (
                  <div className="mt-2 text-[13px] text-muted">
                    <p className="font-semibold text-soft tabular-nums">
                      1 {TICKER} = {rate2((quote.quote.usdc * 100_000_000n) / quote.quote.zatoshi)} {name}
                    </p>
                    <p className="flex flex-wrap items-center gap-x-2">
                      Quote valid until {formatUtc(quote.quote.quoteDeadline)}
                      <button
                        type="button"
                        onClick={() => usdc && void getQuote.run(usdc)}
                        className="inline-flex min-h-8 items-center gap-1 font-semibold text-text"
                      >
                        <IconRefresh size={14} />
                        Refresh
                      </button>
                    </p>
                  </div>
                ) : (
                  <p className="mt-2 text-[12.5px] text-muted">Paid to your Ironwood address on Zcash, proven on Base before the maker is paid</p>
                )}
              </div>
              {quote ? (
                <p className="text-[12.5px] text-muted">
                  The maker <Mono>{short(quote.quote.maker)}</Mono> has {Math.round(quote.quote.payWindow / 3600)} h to pay and prove, or you take your{" "}
                  {name} back.
                </p>
              ) : null}

              {DEPLOYMENT.invites ? (
                <div className="space-y-1">
                  <label htmlFor={inviteId} className="block text-sm font-semibold text-soft">
                    Invite code
                  </label>
                  <input
                    id={inviteId}
                    autoComplete="off"
                    spellCheck={false}
                    value={invite}
                    onChange={(e) => {
                      setInvite(e.target.value);
                      setQuote(null);
                    }}
                    className="field max-w-xs"
                  />
                  <p className="text-xs text-muted">The mainnet beta&apos;s maker quotes invitees only, and binds each quote to your wallet: connect it first.</p>
                </div>
              ) : null}
              <ActionError error={getQuote.error} refusal={getQuote.refusal} title="No quote" />

              {quote ? (
                <div className="space-y-2">
                  <IronwoodAddressInput
                    value={ua}
                    onChange={(v) => {
                      setUa(v);
                      setDest(null);
                    }}
                    label="Your Ironwood address (unified address)"
                    hint="Use a fresh address from an Ironwood-capable wallet. Only its Ironwood receiver is paid."
                  />
                  {/* DEMO: the replay has no camera to point; it offers the demo wallet's address instead. */}
                  {REPLAY ? null : <QrScan onAddress={(a) => (setUa(a), setDest(null))} />}
                  {demoAddress && ua !== demoAddress ? (
                    <Button
                      variant="demo"
                      size="sm"
                      data-replay="swap-demo-address"
                      onClick={() => {
                        setUa(demoAddress);
                        setDest(null);
                      }}
                    >
                      DEMO: use the demo wallet&apos;s Ironwood address
                    </Button>
                  ) : null}
                </div>
              ) : null}

              {wallet ? (
                <p className="flex flex-wrap items-center gap-2 text-[13px] text-soft">
                  <IconWallet size={15} />
                  <Mono>{short(wallet)}</Mono>
                  <span className="text-muted">· your wallet, visible on Base when you open the order</span>
                </p>
              ) : null}
              <Button
                size="lg"
                className="w-full"
                data-replay={next.target}
                onClick={next.run}
                busy={busy}
                disabled={next.disabled || !next.run}
              >
                {next.label}
                {busy || next.disabled ? null : <IconArrowRight size={18} />}
              </Button>
              {stage ? (
                <p className="flex items-center gap-2 text-sm text-soft" aria-live="polite">
                  <Spinner />
                  {stage}
                </p>
              ) : null}
              <ActionError error={connect.error} refusal={connect.refusal} title="The wallet did not connect" />
              <ActionError error={buy.error} refusal={buy.refusal} title="No order opened" />
            </div>
          </Card>
        )}
        <Hint>Selling ZEC for {name} works on the same contract, from the command line for now.</Hint>
      </div>

      <div className="space-y-6">
        {orders.length > 0 ? (
          <section aria-labelledby="orders" className="space-y-4">
            <h2 id="orders" className="text-[19px] font-extrabold font-stretch-116%">
              Your orders <span className="text-[14px] font-semibold text-muted">· kept in this browser</span>
            </h2>
            {orders.map((o) => (
              <OrderCard key={o.orderId} order={o} wallet={wallet} onChanged={reload} />
            ))}
          </section>
        ) : null}
        <Card title="How the swap works" icon={<IconShield size={18} />}>
          <ol className="space-y-4">
            {howItWorks.map((s, i) => (
              <li key={s.title} className="grid grid-cols-[auto_minmax(0,1fr)] gap-3.5">
                <span className="relative inline-flex size-10 items-center justify-center rounded-xl bg-bg shadow-[inset_0_0_0_1px_var(--color-border)]">
                  {s.icon}
                  <span className="absolute -top-1.5 -right-1.5 inline-flex size-5 items-center justify-center rounded-full bg-surface-3 font-mono text-[10.5px] text-soft">
                    {i + 1}
                  </span>
                </span>
                <span>
                  <span className="block font-bold">{s.title}</span>
                  <span className="block text-[13.5px] text-muted">{s.body}</span>
                </span>
              </li>
            ))}
          </ol>
          <p className="mt-5 text-[13.5px] text-soft">
            <strong className="text-text">{SWAP_PRIVACY_SENTENCE}</strong> Your wallet&apos;s address opens the order on Base; your Ironwood
            address and the salt hiding it stay in this browser and go to the maker only, never to Base.
          </p>
        </Card>
      </div>
    </div>
  );
}

const STATUS: Record<SwapStatus["kind"], { tone: "info" | "ok" | "warn" | "neutral" | "proof"; badge: string; text: string }> = {
  open: { tone: "info", badge: "Waiting for payment", text: "Waiting for the maker's payment on Zcash." },
  paid: { tone: "info", badge: "Paid on Zcash", text: "Paid on Zcash (the maker says): its proof is on its way to Base." },
  abandoned: { tone: "warn", badge: "Won't be paid", text: "The maker says it will not pay this order: take your tokens back once the deadline passes." },
  proven: { tone: "proof", badge: "Proven on Base", text: "Proven on Base: released to the maker once the Zcash header is deep enough." },
  settled: { tone: "ok", badge: "Settled", text: "Settled: you hold the shielded ZEC, the maker the tokens, with no custodian in between." },
  refundable: { tone: "warn", badge: "Refundable", text: "The deadline passed without a proof: take your tokens back." },
  refunded: { tone: "neutral", badge: "Refunded", text: "Refunded: the tokens are back in your wallet." },
};

/** One order, as a crossing: its status from Base (and the maker's word for "paid"), the hand-off, the refund. */
function OrderCard({ order, wallet, onChanged }: { order: StoredSwapOrder; wallet: Address | null; onChanged: () => void }) {
  const [maker, setMaker] = useState<MakerOrderStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const final = order.settled || order.refunded;
  useEffect(() => {
    if (final) return;
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // One read at a time: the next one is scheduled when this one is done.
    const tick = async () => {
      try {
        const next = await followOrder(publicClient(), order);
        if (stop) return;
        if (next.scannedTo !== order.scannedTo || next.proven !== order.proven || next.settled !== order.settled || next.refunded !== order.refunded) {
          store().save(next);
          onChanged();
        }
        setError(null);
      } catch (e) {
        if (!stop) setError(describeError(e));
      }
      try {
        const s = await api.getSwapOrder(order.orderId);
        if (!stop) setMaker(s);
      } catch {
        // The maker's word is optional: Base decides.
      }
      // DEMO: the replay's steps come seconds apart.
      if (!stop) timer = setTimeout(() => void tick(), REPLAY ? 2_500 : 20_000);
    };
    void tick();
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  }, [order, final, onChanged]);
  const resend = useAction(async () => {
    await api.postSwapDestination(order.orderId, order.receiver, order.salt);
    store().save({ ...order, destinationSent: true });
    onChanged();
    return true;
  });
  const refund = useAction(async () => {
    const w = injectedWallet();
    if (!w || !wallet) throw new WalletError("Connect the wallet that opened the order.");
    if (getAddress(wallet) !== order.taker) throw new WalletError(`Connect the wallet that opened the order (${short(order.taker)}).`);
    await ensureChain(w, order.chainId, DEPLOYMENT.chainName);
    const call = refundCall(order.swap, order.orderId);
    const receipt = await receiptOf(await sendFromWallet(w, { from: wallet, to: call.to, data: call.data }));
    if (receipt.status !== "success") throw new Error("The refund reverted on Base.");
    store().save({ ...order, refunded: true });
    onChanged();
    return true;
  });
  const s = swapStatus(order, maker, nowSeconds());
  const st = STATUS[s.kind];
  const name = loanTokenName();
  const paid = s.kind === "paid" || s.kind === "proven" || s.kind === "settled";
  const proven = s.kind === "proven" || s.kind === "settled";
  const zcash: TrackerRow[] = [
    {
      key: "pay",
      label: `The maker pays ${formatTaz(BigInt(order.zatoshi))} to your Ironwood address`,
      note: "the order id in its memo",
      state: paid ? "done" : s.kind === "open" ? "now" : "todo",
    },
  ];
  const base: TrackerRow[] = [
    { key: "locked", label: `${formatUsdc(BigInt(order.usdc))} ${name} locked in ZecuredSwap`, state: "done" },
    { key: "proven", label: "Payment proven on Base", proof: true, state: proven ? "done" : s.kind === "paid" ? "now" : "todo" },
    {
      key: "released",
      label: order.refunded ? "Refunded to your wallet" : "Released to the maker",
      note: s.kind === "refundable" ? "or refunded to you: the deadline passed" : final ? undefined : `unless the maker misses its deadline, ${formatUtc(order.payDeadline)}`,
      state: final ? "done" : s.kind === "proven" ? "now" : "todo",
    },
  ];
  // Hand the destination again only when it never reached the maker, or the maker still waits for it.
  const resendable = !final && (!order.destinationSent || maker === "awaiting_destination");
  return (
    <article className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-[16px] font-bold">
          {formatTaz(BigInt(order.zatoshi))} for {formatUsdc(BigInt(order.usdc))} {name}
        </h3>
        <Badge tone={st.tone}>{st.badge}</Badge>
      </div>
      <SeamTracker
        title={`Order ${short(order.orderId, 8, 6)}`}
        zcash={zcash}
        base={base}
        seam={proven ? "proven" : s.kind === "paid" ? "proving" : "idle"}
        provingLabel="Proving the payment"
        footer={
          <>
            <span className="block">
              <strong className="font-semibold text-ink">Base sees</strong> your wallet and the order.
            </span>
            <span className="block">
              <strong className="font-semibold text-ink">Never</strong> where the ZEC goes.
            </span>
          </>
        }
      />
      <p className="text-[13.5px] text-soft">{st.text}</p>
      <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[12.5px] text-muted">
        <span>
          Order <Mono>{short(order.orderId, 10, 8)}</Mono>
        </span>
        <span>
          · paid to <Mono>{short(order.receiver, 12, 8)}</Mono>
        </span>
        <span>· destination {order.destinationSent ? "handed to the maker" : "not yet handed to the maker"}</span>
      </p>
      <div className="flex flex-wrap gap-2">
        {resendable ? (
          <Button variant={order.destinationSent ? "ghost" : "secondary"} size="sm" onClick={() => void resend.run()} busy={resend.busy}>
            {order.destinationSent ? "Send the destination again" : "Send the destination to the maker"}
          </Button>
        ) : null}
        {s.kind === "refundable" ? (
          <Button variant="danger" size="sm" onClick={() => void refund.run()} busy={refund.busy}>
            Take my {name} back
          </Button>
        ) : null}
        <CopyButton value={JSON.stringify(order, null, 2)} label="Copy order details (includes your address and salt)" />
        {final ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              store().remove(order.orderId);
              onChanged();
            }}
          >
            Forget this order
          </Button>
        ) : null}
      </div>
      {error ? <p className="text-xs text-warn">Base could not be read: {error}</p> : null}
      <ActionError error={resend.error} refusal={resend.refusal} title="Not sent" />
      <ActionError error={refund.error} refusal={refund.refusal} title="No refund" />
    </article>
  );
}
