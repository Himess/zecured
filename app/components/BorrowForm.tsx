"use client";

import { useId, useState } from "react";
import { getAddress, isAddress } from "viem";
import { loanAccount, type SentUserOp } from "@/lib/account";
import { borrowCalls, describeRevert, setPreLiquidationCalls } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction, usePreLiquidation } from "@/lib/hooks";
import { REPLAY } from "@/lib/replay/flag";
import { recording } from "@/lib/replay/recording";
import type { StoredLoan } from "@/lib/keys";
import { loanTokenName, priceSourceName } from "@/lib/labels";
import { PRE_LIQUIDATION_PRESETS, termsActive, termsOf, type PreLiquidationPreset } from "@/lib/preliq";
import { ProtectionPresets } from "./position/Protection";
import {
  collateralValue,
  formatBps,
  formatUsdc,
  formatUsdPrice,
  maxBorrowAtTargetLtv,
  parseUsdc,
  TARGET_LTV_BPS,
} from "@/lib/morpho";
import { IconArrowRight, IconShield } from "./icons";
import { HealthRing, healthOf, ltvText } from "./loan";
import { UsdcMark, ZecMark } from "./marks";
import { Button, cx, Meter, Notice } from "./ui";
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
 * pre-liquidation terms.
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
  const [toOther, setToOther] = useState(false);
  const [receiver, setReceiver] = useState("");
  // v4 pre-liquidation: offered while the account has no terms in force (null data: the Adapter has none).
  const terms = usePreLiquidation(loan.account);
  const [protect, setProtect] = useState(false);
  const [preset, setPreset] = useState<PreLiquidationPreset>(PRE_LIQUIDATION_PRESETS[1]!);
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
    return r;
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

  const cap = maxBorrowAtTargetLtv(collateral, snapshot.price);
  const available = cap > snapshot.debt ? cap - snapshot.debt : 0n;
  let assets: bigint | null = null;
  let amountError: string | null = null;
  if (amount.trim()) {
    try {
      assets = parseUsdc(amount);
      if (assets === 0n) amountError = "Enter an amount above zero.";
      else if (assets > available) amountError = `At most ${formatUsdc(available)} USDC keeps you at or below ${formatBps(TARGET_LTV_BPS)} LTV.`;
    } catch (e) {
      amountError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  const receiverError =
    toOther && receiver.trim() && !isAddress(receiver.trim(), { strict: false }) ? "Not a Base (EVM) address." : null;
  const to = toOther ? (receiver.trim() && !receiverError ? getAddress(receiver.trim()) : null) : loan.account;
  const after = assets && !amountError ? snapshot.debt + assets : null;
  const nowSeconds = BigInt(snapshot.blockTimestamp);
  const canProtect = terms.data !== null && terms.data !== undefined && !termsActive(terms.data, nowSeconds);
  const withProtection = protect && canProtect;
  // The preview: the position as it would be after this borrow, or as it is.
  const preview = healthOf({ ...snapshot, debt: after ?? snapshot.debt });
  // The slider runs in cents of the loan token, up to what is available.
  const maxCents = Number(available / 10_000n);
  const cents = assets !== null && !amountError ? Math.min(maxCents, Number(assets / 10_000n)) : 0;
  const ltvNow = preview.ltv !== null ? Number(preview.ltv) / 100 : 0;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,19rem)]">
      <div className="min-w-0 space-y-5">
        <div className="rounded-[22px] bg-bg/80 p-5 shadow-[inset_0_0_0_1px_var(--color-border)] focus-within:shadow-[inset_0_0_0_1px_var(--color-soft)]">
          <div className="flex flex-wrap items-center justify-between gap-2 text-[13px]">
            <label htmlFor={amountId} className="font-semibold text-soft">
              You borrow
            </label>
            <span className="text-muted">
              Available <strong className="text-soft tabular-nums">{formatUsdc(available)}</strong> {loanTokenName()}
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
              className="w-full min-w-0 bg-transparent text-[clamp(30px,4vw,40px)] leading-none font-bold tabular-nums text-text outline-none placeholder:text-faint focus-visible:shadow-none focus-visible:outline-none"
            />
            <span className="inline-flex flex-none items-center gap-2 rounded-full bg-surface-2 py-1.5 pr-3.5 pl-1.5 text-[15px] font-bold shadow-[inset_0_0_0_1px_var(--color-line)]">
              <UsdcMark size={26} />
              {loanTokenName()}
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
          <div className="mt-2 flex flex-wrap gap-2">
            {PICKS.map((p) => (
              <button
                key={p.label}
                type="button"
                disabled={available === 0n}
                onClick={() => setAmount(formatUsdc((available * p.share) / 100n))}
                className="min-h-8 rounded-full bg-surface-2 px-3 text-[12.5px] font-semibold text-soft shadow-[inset_0_0_0_1px_var(--color-border)] hover:bg-surface-3 hover:text-text disabled:opacity-50"
              >
                {p.label}
              </button>
            ))}
          </div>
          {amountError ? (
            <p id={`${amountId}-error`} className="mt-3 text-sm text-danger">
              {amountError}
            </p>
          ) : null}
          {REPLAY ? (
            <p className="mt-3 flex flex-wrap items-center gap-2 text-[13px] text-muted">
              <span className="font-bold text-warn">DEMO</span> The live run borrowed{" "}
              {formatUsdc(BigInt(recording().loan.borrow.assets))} {loanTokenName()}: the replay borrows exactly that.
              <button
                type="button"
                onClick={() => setAmount(formatUsdc(BigInt(recording().loan.borrow.assets)))}
                className="font-semibold text-text underline underline-offset-4"
              >
                Use it
              </button>
            </p>
          ) : null}
        </div>

        <fieldset className="space-y-2">
          <legend className="mb-2 text-[14px] font-semibold text-soft">Where should the USDC go?</legend>
          <div className="grid gap-2 sm:grid-cols-2">
            {[
              { other: false, title: "Keep it in the loan account", body: "Recommended: you can send it later" },
              { other: true, title: "Another Base address", body: "Appears in Morpho's public Borrow event" },
            ].map((o) => (
              <label
                key={String(o.other)}
                className={cx(
                  "flex cursor-pointer items-start gap-3 rounded-2xl px-4 py-3 text-[14px]",
                  toOther === o.other ? "bg-surface-3 shadow-[inset_0_0_0_1.5px_var(--color-soft)]" : "bg-bg/70 shadow-[inset_0_0_0_1px_var(--color-border)]",
                )}
              >
                <input type="radio" name={receiverId} checked={toOther === o.other} onChange={() => setToOther(o.other)} className="mt-1" />
                <span>
                  <span className="block font-semibold">{o.title}</span>
                  <span className="block text-[12.5px] text-muted">{o.body}</span>
                </span>
              </label>
            ))}
          </div>
          {toOther ? (
            <div className="space-y-2">
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
              <Notice tone="warn" title="Think before you send">
                <p>
                  Sending borrowed USDC straight to an exchange account (KYC) links the loan to your identity: the receiver
                  appears in Morpho&apos;s public Borrow event. Prefer an address with no link to you.
                </p>
              </Notice>
            </div>
          ) : null}
        </fieldset>

        {canProtect ? (
          <div className="space-y-3 rounded-2xl bg-bg/70 p-4 shadow-[inset_0_0_0_1px_var(--color-border)]">
            <label htmlFor={protectId} className="flex cursor-pointer items-start gap-3 text-[14px]">
              <input id={protectId} type="checkbox" checked={protect} onChange={(e) => setProtect(e.target.checked)} className="mt-1 size-4" />
              <span>
                <span className="flex items-center gap-2 font-semibold">
                  <IconShield size={16} className="text-ok" />
                  Protect my loan
                </span>
                <span className="text-muted">
                  Optional: agree to be partly repaid early for a small incentive instead of waiting for a full liquidation.
                </span>
              </span>
            </label>
            {protect ? <ProtectionPresets preset={preset} onPreset={setPreset} debt={after ?? snapshot.debt} /> : null}
          </div>
        ) : null}

        <div className="space-y-2">
          <Button
            size="lg"
            className="w-full sm:w-auto"
            onClick={() => assets && to && borrow.run(assets, to, withProtection)}
            busy={borrow.busy}
            disabled={!assets || Boolean(amountError) || !to}
          >
            {snapshot.accountDeployed
              ? withProtection
                ? `Borrow ${assets && !amountError ? formatUsdc(assets) : ""} and protect`
                : `Borrow ${assets && !amountError ? `${formatUsdc(assets)} ${loanTokenName()}` : ""}`
              : "Create account and borrow"}
            {borrow.busy ? null : <IconArrowRight size={18} />}
          </Button>
          {!snapshot.accountDeployed ? (
            <p className="text-xs text-muted">
              Your first transaction deploys the loan account and authorizes the Zecured Registry and Settlement Adapter on
              Morpho. Gas is sponsored: the account never holds ETH.
            </p>
          ) : null}
        </div>
        {borrow.error ? (
          <Notice tone="danger" title="The borrow did not go through" live>
            <p>{borrow.error}</p>
          </Notice>
        ) : null}
        {borrow.result ? <TxResult result={borrow.result} what="Borrowed" /> : null}
      </div>

      <aside aria-label={after !== null ? "After this borrow" : "Your position"} className="space-y-4 rounded-[22px] bg-bg/60 p-5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        <p className="text-[13px] font-semibold text-muted">{after !== null ? "After this borrow" : "Your position now"}</p>
        <div className="flex justify-center">
          <HealthRing health={preview} size={140} />
        </div>
        <dl className="space-y-2.5 text-[14px]">
          <div className="flex items-center justify-between gap-3">
            <dt className="flex items-center gap-2 text-muted">
              <ZecMark size={15} /> Collateral
            </dt>
            <dd className="font-semibold tabular-nums">≈ ${formatUsdc(collateralValue(collateral, snapshot.price))}</dd>
          </div>
          <div className="flex items-center justify-between gap-3">
            <dt className="flex items-center gap-2 text-muted">
              <UsdcMark size={15} /> Debt
            </dt>
            <dd className="font-semibold tabular-nums">{formatUsdc(after ?? snapshot.debt)}</dd>
          </div>
          <div className="flex items-center justify-between gap-3">
            <dt className="text-muted">Loan-to-value</dt>
            <dd className="font-semibold tabular-nums">{ltvText(preview)}</dd>
          </div>
        </dl>
        <Meter
          value={ltvNow}
          max={70}
          tone={preview.tone === "muted" ? "ok" : preview.tone}
          markers={[
            { at: 40, label: "40 % target", tone: "muted" },
            { at: 62.5, label: "62.5 %", tone: "danger" },
          ]}
        />
        <dl className="space-y-2 border-t border-hairline pt-3 text-[13px]">
          <div className="flex justify-between gap-3">
            <dt className="text-muted">Liquidation below</dt>
            <dd className="font-semibold tabular-nums">{preview.liquidationPrice ? formatUsdPrice(preview.liquidationPrice) : "—"}</dd>
          </div>
          <div className="flex justify-between gap-3">
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
