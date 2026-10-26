"use client";

import { useCallback, useEffect, useId, useState } from "react";
import { getAddress, type Address, type Hex } from "viem";
import { CopyButton, useMounted } from "@/components/client";
import { IronwoodAddressInput, destinationError } from "@/components/DestinationForm";
import { IconArrowDown, IconCheck, IconRefresh, IconShield, IconWallet } from "@/components/icons";
import { UsdcMark, ZecMark } from "@/components/marks";
import { ProofSeal } from "@/components/seam";
import { Badge, Button, Card, cx, KeyValue, Mono, Notice, Spinner, Steps } from "@/components/ui";
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
import {
  buyCalls,
  followOrder,
  openedOrder,
  quoteProblem,
  quoteSignedByMaker,
  refundCall,
  SWAP_STEPS,
  SwapStore,
  swapStatus,
  type StoredSwapOrder,
  type SwapStatus,
} from "@/lib/swap";
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

/**
 * The "Buy shielded ZEC" page (section 5.7 of the v4 design): a quote relayed from the maker, an
 * Ironwood address pasted or scanned, the salt and commitment made here and kept here, the user's
 * own wallet approving the USDC and calling `open`, then the order followed on Base: open → paid →
 * proven → settled, or a refund after the deadline.
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
  const reload = useCallback(() => setOrders(store().list()), []);
  useEffect(() => reload(), [reload]);

  const swap = config.swap;
  const token = config.usdc;
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
    setStage("Reading your USDC allowance…");
    const allowance = await c.readContract({ address: token, abi: erc20AllowanceAbi, functionName: "allowance", args: [wallet, swap] });
    const calls = buyCalls({ swap, usdcToken: token, quote, commitment: d.commitment, allowance });
    for (const [i, call] of calls.entries()) {
      const last = i === calls.length - 1;
      if (last) {
        setStage("Checking the order on Base before your wallet signs it…");
        try {
          await c.call({ account: wallet, to: call.to, data: call.data });
        } catch (e) {
          throw new Error(`The swap contract would refuse this order (${describeError(e)}). Nothing was sent.`);
        }
      }
      setStage(last ? "Confirm the order in your wallet…" : "Approve exactly this USDC in your wallet…");
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
  const steps = ["Amount", "Ironwood address", "Your wallet", "Open the order"];
  const step = !quote ? 0 : !ua.trim() || destinationError(ua) ? 1 : !wallet ? 2 : 3;
  const howItWorks = [
    { icon: <UsdcMark size={22} />, title: `Your USDC waits on ${DEPLOYMENT.chainName}`, body: "Your wallet opens the order: the USDC sits in the swap contract, not with anyone." },
    { icon: <ZecMark size={22} />, title: "The maker pays you on Zcash", body: "A shielded payment to your Ironwood address, from the maker's own ZEC." },
    {
      icon: <ProofSeal size="sm" className="!size-[22px]" />,
      title: "Proven on Base in zero knowledge",
      body: "A proof that the payment is in a Zcash block reaches Base. No custodian, no signer, no wrapped ZEC.",
    },
    {
      icon: <IconCheck size={20} strokeWidth={2.4} className="text-ok" />,
      title: "Settled, or refunded",
      body: "Only then is the USDC released to the maker. If it does not pay and prove in time, you take your USDC back.",
    },
  ];

  return (
    <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,32rem)_minmax(0,1fr)] [&>*]:min-w-0">
      <div className="space-y-4">
        {!usable ? (
          <Notice tone="warn" title="The swap is not configured in this build">
            <p>This deployment has no swap contract pinned (NEXT_PUBLIC_SWAP), or no USDC token.</p>
          </Notice>
        ) : (
          <Card lead>
            <div className="space-y-4">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <h2 className="text-[20px] font-extrabold font-stretch-118%">Swap</h2>
                <Badge tone="neutral">
                  Step {step + 1} of 4 · {steps[step]}
                </Badge>
              </div>

              <div className="rounded-[20px] bg-bg/80 p-4 shadow-[inset_0_0_0_1px_var(--color-border)] focus-within:shadow-[inset_0_0_0_1px_var(--color-soft)]">
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
                    placeholder="25.00"
                    aria-invalid={inputError ? true : undefined}
                    className="w-full min-w-0 bg-transparent text-[34px] leading-none font-bold tabular-nums text-text outline-none placeholder:text-faint focus-visible:shadow-none focus-visible:outline-none"
                  />
                  <span className="inline-flex flex-none items-center gap-2 rounded-full bg-surface-2 py-1.5 pr-3.5 pl-1.5 text-[15px] font-bold shadow-[inset_0_0_0_1px_var(--color-line)]">
                    <UsdcMark size={26} />
                    {loanTokenName()}
                  </span>
                </div>
                <p className="mt-2 text-[12.5px] text-muted">Locked in the swap contract until the maker proves its payment</p>
                {inputError ? <p className="mt-2 text-sm text-danger">{inputError}</p> : null}
                {REPLAY ? (
                  <p className="mt-2 flex flex-wrap items-center gap-2 text-[13px] text-muted">
                    <span className="font-bold text-warn">DEMO</span> The live run spent {formatUsdc(BigInt(recording().swap.usdc))} {loanTokenName()}.
                    <button
                      type="button"
                      onClick={() => {
                        setInput(formatUsdc(BigInt(recording().swap.usdc)));
                        setQuote(null);
                      }}
                      className="font-semibold text-text underline underline-offset-4"
                    >
                      Use it
                    </button>
                  </p>
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
                  <span className={cx("w-full min-w-0 truncate text-[34px] leading-none font-bold tabular-nums", quote ? "text-text" : "text-faint")}>
                    {quote ? formatZec(quote.quote.zatoshi) : "0"}
                  </span>
                  <span className="inline-flex flex-none items-center gap-2 rounded-full bg-surface-2 py-1.5 pr-3.5 pl-1.5 text-[15px] font-bold shadow-[inset_0_0_0_1px_var(--color-line)]">
                    <ZecMark size={26} />
                    {TICKER}
                  </span>
                </div>
                <p className="mt-2 text-[12.5px] text-muted">Paid to your Ironwood address on Zcash, proven on Base before the maker is paid</p>
              </div>

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

              <Button
                variant={quote ? "ghost" : "secondary"}
                className="w-full"
                onClick={() => usdc && void getQuote.run(usdc)}
                busy={getQuote.busy}
                disabled={!usdc || Boolean(inputError)}
              >
                {getQuote.busy ? null : <IconRefresh size={16} />}
                {quote ? "Refresh the quote" : "Get the maker's quote"}
              </Button>
              {getQuote.error ? (
                <Notice tone="danger" title="No quote" live>
                  <p>{getQuote.error}</p>
                </Notice>
              ) : null}
              {quote ? (
                <dl className="space-y-2 rounded-2xl bg-bg/60 px-4 py-3 text-[13px] shadow-[inset_0_0_0_1px_var(--color-hairline)]">
                  <div className="flex justify-between gap-3">
                    <dt className="text-muted">Rate</dt>
                    <dd className="font-semibold tabular-nums">
                      1 {TICKER} = {formatUsdc((quote.quote.usdc * 100_000_000n) / quote.quote.zatoshi)} {loanTokenName()}
                    </dd>
                  </div>
                  <div className="flex justify-between gap-3">
                    <dt className="text-muted">The maker has</dt>
                    <dd className="text-right font-semibold">{Math.round(quote.quote.payWindow / 3600)} h to pay and prove, or you take the USDC back</dd>
                  </div>
                  <div className="flex justify-between gap-3">
                    <dt className="text-muted">Maker</dt>
                    <dd>
                      <Mono>{short(quote.quote.maker)}</Mono>
                    </dd>
                  </div>
                  <div className="flex justify-between gap-3">
                    <dt className="text-muted">Quote valid until</dt>
                    <dd className="font-semibold">{formatUtc(quote.quote.quoteDeadline)}</dd>
                  </div>
                </dl>
              ) : null}

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
                <QrScan onAddress={(a) => (setUa(a), setDest(null))} />
                {REPLAY && replayDemoAddress() ? (
                  <button
                    type="button"
                    onClick={() => {
                      setUa(replayDemoAddress()!);
                      setDest(null);
                    }}
                    className="text-[13px] font-semibold text-soft underline underline-offset-4"
                  >
                    DEMO: use the demo wallet&apos;s Ironwood address
                  </button>
                ) : null}
              </div>

              <div className="flex flex-wrap items-center gap-3">
                {wallet ? (
                  <span className="inline-flex min-h-9 items-center gap-2 rounded-full bg-surface-2 px-3.5 text-[13px] text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
                    <IconWallet size={15} />
                    <Mono>{short(wallet)}</Mono>
                    <span className="text-muted">· visible on Base when you open the order</span>
                  </span>
                ) : (
                  <Button variant="secondary" onClick={() => void connect.run()} busy={connect.busy}>
                    {connect.busy ? null : <IconWallet size={16} />}
                    Connect your wallet
                  </Button>
                )}
                {connect.error ? <span className="text-sm text-danger">{connect.error}</span> : null}
              </div>
              <Button
                size="lg"
                className="w-full"
                onClick={() => void buy.run()}
                busy={buy.busy}
                disabled={!quote || !wallet || !ua.trim() || destinationError(ua) !== null}
              >
                Buy {quote ? formatTaz(quote.quote.zatoshi) : "shielded ZEC"} with my wallet
              </Button>
              {stage ? (
                <p className="flex items-center gap-2 text-sm text-soft" aria-live="polite">
                  <Spinner />
                  {stage}
                </p>
              ) : null}
              {buy.error ? (
                <Notice tone="danger" title="No order opened" live>
                  <p>{buy.error}</p>
                </Notice>
              ) : null}
              {buy.result ? (
                <Notice tone="ok" title="Order opened" live>
                  <p>The maker now pays your Ironwood address; follow the order under Your orders.</p>
                </Notice>
              ) : null}
            </div>
          </Card>
        )}
        <p className="text-xs text-muted">Selling ZEC for USDC works on the same contract, from the command line for now.</p>
      </div>

      <div className="space-y-6">
        {orders.length > 0 ? (
          <section aria-labelledby="orders" className="space-y-4">
            <h2 id="orders" className="text-[20px] font-extrabold font-stretch-118%">
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
                  <span className="absolute -top-1.5 -right-1.5 inline-flex size-5 items-center justify-center rounded-full bg-surface-3 text-[11px] font-bold text-soft">
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
          <p className="mt-5 rounded-2xl bg-bg/60 p-4 text-[13.5px] text-soft shadow-[inset_0_0_0_1px_var(--color-hairline)]">
            <strong className="text-text">{SWAP_PRIVACY_SENTENCE}</strong> Your wallet&apos;s address opens the order on Base; your
            Ironwood address and the salt hiding it stay in this browser and go to the maker only, never to Base.
          </p>
        </Card>
      </div>
    </div>
  );
}

const STATUS_TEXT: Record<SwapStatus["kind"], { tone: "info" | "ok" | "warn" | "neutral"; text: string }> = {
  open: { tone: "info", text: "Open: waiting for the maker's payment" },
  paid: { tone: "info", text: "Paid on Zcash (the maker says): its proof is on its way to Base" },
  abandoned: { tone: "warn", text: "The maker says it will not pay this order: take your USDC back once the deadline passes" },
  proven: { tone: "ok", text: "Proven on Base: the USDC goes to the maker once the Zcash header is deep enough" },
  settled: { tone: "ok", text: "Settled: you have the ZEC, the maker the USDC" },
  refundable: { tone: "warn", text: "The deadline passed without a proof: take your USDC back" },
  refunded: { tone: "neutral", text: "Refunded: the USDC is back in your wallet" },
};

/** One order: its status from Base (and the maker's word for "paid"), the destination's hand-off, the refund. */
function OrderCard({ order, wallet, onChanged }: { order: StoredSwapOrder; wallet: Address | null; onChanged: () => void }) {
  const [maker, setMaker] = useState<MakerOrderStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const final = order.settled || order.refunded;
  useEffect(() => {
    if (final) return;
    let stop = false;
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
      api
        .getSwapOrder(order.orderId)
        .then((s) => !stop && setMaker(s))
        .catch(() => undefined);
    };
    void tick();
    // DEMO: the replay's steps come seconds apart.
    const timer = setInterval(() => void tick(), REPLAY ? 2_500 : 20_000);
    return () => {
      stop = true;
      clearInterval(timer);
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
  const label = STATUS_TEXT[s.kind];
  const stepIndex = s.kind === "settled" ? 4 : Math.max(0, SWAP_STEPS.indexOf(s.kind as (typeof SWAP_STEPS)[number]));
  return (
    <Card
      as="article"
      title={`${formatTaz(BigInt(order.zatoshi))} for ${formatUsdc(BigInt(order.usdc))} ${loanTokenName()}`}
      action={<Badge tone={label.tone}>{s.kind}</Badge>}
    >
      <div className="space-y-3 text-sm">
        {s.kind !== "refundable" && s.kind !== "refunded" ? <Steps steps={["Open", "Paid", "Proven", "Settled"]} current={stepIndex} /> : null}
        <p className="text-muted">{label.text}.</p>
        <KeyValue
          rows={[
            ["Order", <Mono key="o">{short(order.orderId, 10, 8)}</Mono>],
            ["Pay deadline", formatUtc(order.payDeadline)],
            ["Paid to", <Mono key="r">{short(order.receiver, 12, 8)}</Mono>],
            ["Destination", order.destinationSent ? "handed to the maker" : "not yet handed to the maker"],
          ]}
        />
        <div className="flex flex-wrap gap-2">
          {!final ? (
            <Button variant={order.destinationSent ? "ghost" : "secondary"} onClick={() => void resend.run()} busy={resend.busy}>
              {order.destinationSent ? "Send the destination again" : "Send the destination to the maker"}
            </Button>
          ) : null}
          {s.kind === "refundable" ? (
            <Button variant="danger" onClick={() => void refund.run()} busy={refund.busy}>
              Take my USDC back
            </Button>
          ) : null}
          <CopyButton
            value={JSON.stringify(order, null, 2)}
            label="Copy the order (it contains your Ironwood address and its salt)"
          />
          {final ? (
            <Button
              variant="ghost"
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
        {resend.error ? <p className="text-xs text-danger">{resend.error}</p> : null}
        {refund.error ? <p className="text-xs text-danger">{refund.error}</p> : null}
      </div>
    </Card>
  );
}
