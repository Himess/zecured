"use client";

import { useId, useState } from "react";
import { getAddress, isAddress } from "viem";
import { loanAccount, type SentUserOp } from "@/lib/account";
import { borrowCalls, describeRevert, setPreLiquidationCalls } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction, usePreLiquidation } from "@/lib/hooks";
import type { StoredLoan } from "@/lib/keys";
import { loanTokenName, priceSourceName } from "@/lib/labels";
import { PRE_LIQUIDATION_PRESETS, termsActive, termsOf, type PreLiquidationPreset } from "@/lib/preliq";
import { ProtectionPresets } from "./position/Protection";
import {
  formatBps,
  formatUsdc,
  formatUsdPrice,
  formatWad,
  healthFactor,
  liquidationPrice,
  ltvBps,
  maxBorrowAtTargetLtv,
  parseUsdc,
  TARGET_LTV_BPS,
} from "@/lib/morpho";
import { Button, Notice } from "./ui";
import { TxResult } from "./TxResult";

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

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">
        Price {formatUsdPrice(snapshot.price)} per ZEC ({priceSourceName()}). You can borrow up to{" "}
        <strong className="text-text">
          {formatUsdc(available)} {loanTokenName()}
        </strong>{" "}
        more ({formatBps(TARGET_LTV_BPS)} LTV; the market liquidates above 62.5 %).
      </p>
      <div className="space-y-2">
        <label htmlFor={amountId} className="block text-sm font-medium">
          Amount (USDC)
        </label>
        <div className="flex gap-2">
          <input
            id={amountId}
            inputMode="decimal"
            autoComplete="off"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            placeholder="0.00"
            aria-invalid={amountError ? true : undefined}
            className="w-full rounded-lg border border-border bg-bg px-3 py-2 tabular-nums text-text focus:border-muted"
          />
          <Button variant="secondary" onClick={() => setAmount(formatUsdc(available))} disabled={available === 0n}>
            Max
          </Button>
        </div>
        {amountError ? <p className="text-sm text-danger">{amountError}</p> : null}
      </div>

      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Where should the USDC go?</legend>
        <label className="flex items-start gap-3 text-sm">
          <input type="radio" name={receiverId} checked={!toOther} onChange={() => setToOther(false)} className="mt-1" />
          <span>
            Keep it in the loan account <span className="text-muted">(recommended; you can send it later)</span>
          </span>
        </label>
        <label className="flex items-start gap-3 text-sm">
          <input type="radio" name={receiverId} checked={toOther} onChange={() => setToOther(true)} className="mt-1" />
          <span>Send it to another Base address</span>
        </label>
        {toOther ? (
          <div className="space-y-2 pl-7">
            <input
              aria-label="Receiver address"
              value={receiver}
              onChange={(e) => setReceiver(e.target.value)}
              placeholder="0x…"
              spellCheck={false}
              autoComplete="off"
              className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-sm text-text focus:border-muted"
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

      {after !== null ? (
        <p className="text-sm text-muted">
          After this borrow: debt {formatUsdc(after)} {loanTokenName()}, LTV {formatBps(ltvBps(collateral, snapshot.price, after) ?? 0n)},
          health factor {formatWad(healthFactor(collateral, snapshot.price, after, config.lltv) ?? 0n)}, liquidation below{" "}
          {formatUsdPrice(liquidationPrice(collateral, after, config.lltv) ?? 0n)} per ZEC.
        </p>
      ) : null}

      {canProtect ? (
        <div className="space-y-3 rounded-2xl bg-bg p-4">
          <label htmlFor={protectId} className="flex items-start gap-3 text-sm">
            <input id={protectId} type="checkbox" checked={protect} onChange={(e) => setProtect(e.target.checked)} className="mt-1" />
            <span>
              <span className="font-medium">Protect my loan</span>{" "}
              <span className="text-muted">
                (optional): agree to be partly repaid early for a small incentive instead of waiting for a full liquidation.
              </span>
            </span>
          </label>
          {protect ? <ProtectionPresets preset={preset} onPreset={setPreset} debt={after ?? snapshot.debt} /> : null}
        </div>
      ) : null}

      <Button
        onClick={() => assets && to && borrow.run(assets, to, withProtection)}
        busy={borrow.busy}
        disabled={!assets || Boolean(amountError) || !to}
      >
        {snapshot.accountDeployed ? (withProtection ? "Borrow and protect" : "Borrow") : "Create account and borrow"}
      </Button>
      {!snapshot.accountDeployed ? (
        <p className="text-xs text-muted">
          Your first transaction deploys the loan account and authorizes the Zecured Registry and Settlement Adapter on
          Morpho. Gas is sponsored: the account never holds ETH.
        </p>
      ) : null}
      {borrow.error ? (
        <Notice tone="danger" title="The borrow did not go through" live>
          <p>{borrow.error}</p>
        </Notice>
      ) : null}
      {borrow.result ? <TxResult result={borrow.result} what="Borrowed" /> : null}
    </div>
  );
}
