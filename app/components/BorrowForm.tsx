"use client";

import { useId, useState } from "react";
import { getAddress, isAddress } from "viem";
import { loanAccount, type SentUserOp } from "@/lib/account";
import { borrowCalls, describeRevert, setPreLiquidationCalls } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction, usePreLiquidation } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { REPLAY } from "@/lib/replay/flag";
import { recording } from "@/lib/replay/recording";
import { loanTokenName, priceSourceName } from "@/lib/labels";
import { PRE_LIQUIDATION_PRESETS, termsActive, termsOf, type PreLiquidationPreset } from "@/lib/preliq";
import { ProtectionPresets } from "./position/Protection";
import { collateralValue, formatBps, formatUsdc, formatUsdPrice, maxBorrowAtTargetLtv, parseUsdc, TARGET_LTV_BPS } from "@/lib/morpho";
import { ActionError } from "./ActionError";
import { IconArrowRight, IconShield } from "./icons";
import { HealthRunway, healthOf, TokenAmount, usd, usdc } from "./loan";
import { UsdcMark, ZzecMark } from "./marks";
import { Button, cx, Disclosure, Hint, Notice } from "./ui";
import { TxResult } from "./TxResult";

/** The quick picks under the amount: shares of what can still be borrowed at the target LTV. */
const PICKS = [
  { label: "25 %", share: 25n },
  { label: "50 %", share: 50n },
  { label: "75 %", share: 75n },
  { label: "Max", share: 100n },
] as const;

/**
 * Borrow the market's loan token against the position, up to 40 % LTV, in one sponsored UserOp;
 * with "Protect my loan" (section 4 of the v4 design), the same UserOp sets the account's
 * pre-liquidation terms. The amount leads; where it goes and the protection are one line each until opened.
 */
export function BorrowForm({
  loan,
  snapshot,
  onBorrowed,
}: {
  loan: StoredLoan;
  snapshot: PositionSnapshot | null;
  onBorrowed?: (r: SentUserOp) => void;
}) {
  const amountId = useId();
  const sliderId = useId();
  const receiverId = useId();
  const protectId = useId();
  const [amount, setAmount] = useState("");
  const [choosing, setChoosing] = useState(false);
  const [toOther, setToOther] = useState(false);
  const [receiver, setReceiver] = useState("");
  // v4 pre-liquidation: offered while the account has no terms in force (null data: the Adapter has none).
  const terms = usePreLiquidation(loan.account);
  const [protect, setProtect] = useState(false);
  const [preset, setPreset] = useState<PreLiquidationPreset>(PRE_LIQUIDATION_PRESETS[1]!);
  // The receipt names what this borrow did: a first one also deployed the account.
  const [first, setFirst] = useState(false);
  const borrow = useAction(async (assets: bigint, to: `0x${string}`, withProtection: boolean) => {
    if (!snapshot) throw new Error("Position not loaded");
    const calls = borrowCalls({
      account: loan.account,
      assets,
      receiver: to,
      registryAuthorized: snapshot.registryAuthorized,
      adapterAuthorized: snapshot.adapterAuthorized,
    });
    if (withProtection) calls.push(...setPreLiquidationCalls({ terms: termsOf(preset, BigInt(snapshot.blockTimestamp)) }));
    setFirst(!snapshot.accountDeployed);
    let r: SentUserOp;
    try {
      r = await loanAccount(loan.ownerKey).send(calls);
    } catch (e) {
      const why = describeRevert(e);
      if (why) throw new Error(why);
      throw e;
    }
    void terms.refresh();
    // The amount is borrowed: what is left to borrow starts from zero again.
    if (r.success) setAmount("");
    onBorrowed?.(r);
    return { ...r, assets, to };
  });

  if (!config.contractsDeployed) {
    return (
      <Notice tone="info" title="Borrowing opens once the Base contracts are deployed">
        <p>The app is running with placeholder contract addresses. Off-chain steps (opening a vault, the deposit) work already.</p>
      </Notice>
    );
  }
  if (!snapshot) return <p className="text-sm text-muted">Loading your position…</p>;

  const collateral = snapshot.position.collateral;
  if (collateral === 0n) {
    return (
      <Notice tone="info" title="No collateral yet">
        <p>Your position has no zZEC collateral yet. It appears once your deposit is proven and activated on Base.</p>
      </Notice>
    );
  }

  const token = loanTokenName();
  const cap = maxBorrowAtTargetLtv(collateral, snapshot.price);
  const available = cap > snapshot.debt ? cap - snapshot.debt : 0n;
  let assets: bigint | null = null;
  let amountError: string | null = null;
  if (amount.trim()) {
    try {
      assets = parseUsdc(amount);
      if (assets === 0n) amountError = "Enter an amount above zero.";
      else if (assets > available) amountError = `At most ${formatUsdc(available)} ${token} keeps you at or below ${formatBps(TARGET_LTV_BPS)} LTV.`;
    } catch (e) {
      amountError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  const receiverError = toOther && receiver.trim() && !isAddress(receiver.trim(), { strict: false }) ? "Not a Base (EVM) address." : null;
  const to = toOther ? (receiver.trim() && !receiverError ? getAddress(receiver.trim()) : null) : loan.account;
  const after = assets && !amountError ? snapshot.debt + assets : null;
  const nowSeconds = BigInt(snapshot.blockTimestamp);
  const canProtect = terms.data !== null && terms.data !== undefined && !termsActive(terms.data, nowSeconds);
  // DEMO: the recorded run borrowed without protection, so the replay offers none.
  const offerProtection = canProtect && !(REPLAY && !recording().loan.borrow.protected);
  const withProtection = protect && offerProtection;
  // The position as it is, and as it would be after this borrow.
  const now = healthOf(snapshot);
  const preview = after !== null ? healthOf({ ...snapshot, debt: after }) : null;
  // The slider runs in cents of the loan token, up to what is available.
  const maxCents = Number(available / 10_000n);
  const cents = assets !== null && !amountError ? Math.min(maxCents, Number(assets / 10_000n)) : 0;
  const recorded = REPLAY ? BigInt(recording().loan.borrow.assets) : null;
  const result = borrow.result;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,20rem)]">
      <div className="min-w-0 space-y-4">
        {result ? (
          <TxResult
            result={result}
            what={`Borrowed ${formatUsdc(result.assets)} ${token}`}
            amount={<TokenAmount token="USDC" value={`+${usdc(result.assets)}`} size="xl" />}
          >
            {result.to.toLowerCase() === loan.account.toLowerCase() ? (
              <>
                In your loan account <span className="font-mono text-[13px]">{short(loan.account)}</span>.{" "}
                {first ? "One transaction deployed it, authorized Zecured on Morpho and borrowed; it has never held ETH." : null}
              </>
            ) : (
              <>
                Sent to <span className="font-mono text-[13px]">{short(result.to)}</span>.
              </>
            )}
          </TxResult>
        ) : null}
        <div className="rounded-[22px] bg-bg/80 p-5 shadow-[inset_0_0_0_1px_var(--color-control)] focus-within:shadow-[inset_0_0_0_2px_var(--color-text)]">
          <div className="flex flex-wrap items-center justify-between gap-2 text-[13px]">
            <label htmlFor={amountId} className="font-semibold text-soft">
              You borrow
            </label>
            <span className="text-muted">
              Available <strong className="font-semibold text-soft tabular-nums">{usdc(available)}</strong>{" "}
              <button
                type="button"
                disabled={available === 0n}
                onClick={() => setAmount(formatUsdc(available))}
                className="ml-1 min-h-8 rounded-full px-2 font-semibold text-text underline underline-offset-4 disabled:opacity-50"
              >
                Max
              </button>
            </span>
          </div>
          <div className="mt-2 flex items-center gap-3">
            <input
              id={amountId}
              inputMode="decimal"
              autoComplete="off"
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              placeholder="0.00"
              aria-invalid={amountError ? true : undefined}
              aria-describedby={amountError ? `${amountId}-error` : undefined}
              className="w-full min-w-0 bg-transparent text-[clamp(34px,4vw,46px)] leading-none font-bold tabular-nums text-text outline-none placeholder:text-faint"
            />
            <span className="inline-flex flex-none items-center gap-2 text-[16px] font-bold">
              <UsdcMark size={28} />
              {token}
            </span>
          </div>
          <label htmlFor={sliderId} className="sr-only">
            Amount as a share of what is available
          </label>
          <input
            id={sliderId}
            type="range"
            min={0}
            max={Math.max(1, maxCents)}
            step={1}
            value={cents}
            disabled={maxCents === 0}
            onChange={(e) => setAmount((Number(e.target.value) / 100).toFixed(2))}
            className="mt-4 w-full"
          />
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {PICKS.map((p) => {
              const value = (available * p.share) / 100n;
              const active = assets !== null && assets === value && value > 0n;
              return (
                <button
                  key={p.label}
                  type="button"
                  disabled={available === 0n}
                  aria-pressed={active}
                  onClick={() => setAmount(formatUsdc(value))}
                  className={cx(
                    "min-h-9 rounded-full px-3.5 text-[13px] font-semibold disabled:opacity-50",
                    active ? "bg-text text-bg" : "bg-surface-2 text-soft shadow-[inset_0_0_0_1px_var(--color-border)] hover:bg-surface-3 hover:text-text",
                  )}
                >
                  {p.label}
                </button>
              );
            })}
            {recorded !== null ? (
              <Button variant="demo" size="sm" data-replay="use-recorded-borrow" onClick={() => setAmount(formatUsdc(recorded))}>
                DEMO: use the run&apos;s {formatUsdc(recorded)}
              </Button>
            ) : null}
          </div>
          {amountError ? (
            <p id={`${amountId}-error`} className="mt-3 text-sm text-danger">
              {amountError}
            </p>
          ) : null}
        </div>

        {choosing ? (
          <fieldset className="space-y-2 rounded-2xl bg-bg/50 p-4 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
            <legend className="sr-only">Where should the {token} go?</legend>
            <p className="text-[14px] font-semibold text-soft">Send it to</p>
            {[
              { other: false, title: "This loan's account", body: "Recommended: you can send it on later" },
              { other: true, title: "Another Base address", body: "It appears in Morpho's public Borrow event" },
            ].map((o) => (
              <label key={String(o.other)} className="flex cursor-pointer items-start gap-3 rounded-xl px-2 py-1.5 text-[14px]">
                <input type="radio" name={receiverId} checked={toOther === o.other} onChange={() => setToOther(o.other)} className="mt-1" />
                <span>
                  <span className="font-semibold">{o.title}</span>
                  <span className="block text-[12.5px] text-muted">{o.body}</span>
                </span>
              </label>
            ))}
            {toOther ? (
              <div className="space-y-2 pt-1">
                <input
                  aria-label="Receiver address"
                  value={receiver}
                  onChange={(e) => setReceiver(e.target.value)}
                  placeholder="0x…"
                  spellCheck={false}
                  autoComplete="off"
                  className="field font-mono text-sm"
                />
                {receiverError ? <p className="text-sm text-danger">{receiverError}</p> : null}
                <Hint>
                  Sending borrowed {token} straight to an exchange account (KYC) links the loan to your identity: the receiver
                  appears in Morpho&apos;s public Borrow event. Prefer an address with no link to you.
                </Hint>
              </div>
            ) : null}
          </fieldset>
        ) : (
          <p className="flex flex-wrap items-center gap-x-2 text-[14px] text-muted">
            To <span className="font-semibold text-soft">this loan&apos;s account</span>
            <span className="font-mono text-[12.5px]">{short(loan.account)}</span>
            <button type="button" onClick={() => setChoosing(true)} className="min-h-8 font-semibold text-text underline underline-offset-4">
              Change
            </button>
          </p>
        )}

        {offerProtection ? (
          <Disclosure
            summary={
              <>
                <IconShield size={16} className="text-ok" />
                Protect my loan <span className="font-normal text-muted">(optional)</span>
              </>
            }
          >
            <label htmlFor={protectId} className="flex cursor-pointer items-start gap-3 text-[14px]">
              <input id={protectId} type="checkbox" checked={protect} onChange={(e) => setProtect(e.target.checked)} className="mt-1 size-4" />
              <span className="text-muted">Agree to be partly repaid early, for a small incentive, instead of waiting for a full liquidation.</span>
            </label>
            {protect ? <ProtectionPresets preset={preset} onPreset={setPreset} debt={after ?? snapshot.debt} /> : null}
          </Disclosure>
        ) : null}

        <div className="space-y-2">
          <Button
            size="lg"
            data-replay="borrow"
            className="w-full sm:w-auto"
            onClick={() => assets && to && borrow.run(assets, to, withProtection)}
            busy={borrow.busy}
            disabled={!assets || Boolean(amountError) || !to}
          >
            {borrow.busy
              ? "Borrowing…"
              : `${snapshot.accountDeployed ? "Borrow" : "Create account and borrow"} ${assets && !amountError ? `${formatUsdc(assets)} ${token}` : token}${withProtection ? ", protected" : ""}`}
            {borrow.busy ? null : <IconArrowRight size={18} />}
          </Button>
          {!snapshot.accountDeployed ? (
            <Hint>One sponsored transaction deploys the account, authorizes Zecured on Morpho and borrows. It never holds ETH.</Hint>
          ) : null}
        </div>
        <ActionError error={borrow.error} refusal={borrow.refusal} title="The borrow did not go through" />
      </div>

      <aside aria-label={preview ? "After this borrow" : "Your position"} className="space-y-4 rounded-[22px] bg-bg/60 p-5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        <p className="text-[13px] font-semibold text-muted">{preview ? "After this borrow" : "Your position now"}</p>
        <HealthRunway health={now} preview={preview} size="m" />
        <dl className="space-y-2.5 border-t border-hairline pt-3 text-[13.5px]">
          <div className="flex items-center justify-between gap-3">
            <dt className="flex items-center gap-2 text-muted">
              <ZzecMark size={15} /> Collateral
            </dt>
            <dd className="text-right font-semibold tabular-nums">
              {usd(collateralValue(collateral, snapshot.price))}
            </dd>
          </div>
          <div className="flex items-center justify-between gap-3">
            <dt className="flex items-center gap-2 text-muted">
              <UsdcMark size={15} /> Debt
            </dt>
            <dd className="text-right font-semibold tabular-nums">
              {after !== null && after !== snapshot.debt ? (
                <>
                  <span className="font-normal text-muted">{usdc(snapshot.debt)} → </span>
                  {usdc(after)}
                </>
              ) : (
                usdc(snapshot.debt)
              )}{" "}
              <span className="font-normal text-muted">{token}</span>
            </dd>
          </div>
          <div className="flex items-start justify-between gap-3">
            <dt className="text-muted">ZEC price</dt>
            <dd className="text-right font-semibold tabular-nums">
              {formatUsdPrice(snapshot.price)}
              <span className="block text-[11.5px] font-normal text-muted">{priceSourceName()}</span>
            </dd>
          </div>
        </dl>
      </aside>
    </div>
  );
}
