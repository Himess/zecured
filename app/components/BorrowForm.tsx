"use client";

import { useId, useState } from "react";
import { getAddress, isAddress } from "viem";
import { loanAccount, type SentUserOp } from "@/lib/account";
import { borrowCalls } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction } from "@/lib/hooks";
import type { StoredLoan } from "@/lib/keys";
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

/** Borrow MockUSDC against the position, up to 40 % LTV, in one sponsored UserOp. */
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
  const [amount, setAmount] = useState("");
  const [toOther, setToOther] = useState(false);
  const [receiver, setReceiver] = useState("");
  const borrow = useAction(async (assets: bigint, to: `0x${string}`) => {
    if (!snapshot) throw new Error("Position not loaded");
    const r = await loanAccount(loan.ownerKey).send(
      borrowCalls({
        account: loan.account,
        assets,
        receiver: to,
        registryAuthorized: snapshot.registryAuthorized,
        adapterAuthorized: snapshot.adapterAuthorized,
      }),
    );
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

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">
        Price {formatUsdPrice(snapshot.price)} per ZEC (MockZecOracle). You can borrow up to{" "}
        <strong className="text-text">{formatUsdc(available)} USDC</strong> more ({formatBps(TARGET_LTV_BPS)} LTV; the market
        liquidates above 62.5 %).
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
            className="w-full rounded-lg border border-border bg-bg px-3 py-2 tabular-nums text-text focus:border-accent"
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
              className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-sm text-text focus:border-accent"
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
          After this borrow: debt {formatUsdc(after)} USDC, LTV {formatBps(ltvBps(collateral, snapshot.price, after) ?? 0n)},
          health factor {formatWad(healthFactor(collateral, snapshot.price, after, config.lltv) ?? 0n)}, liquidation below{" "}
          {formatUsdPrice(liquidationPrice(collateral, after, config.lltv) ?? 0n)} per ZEC.
        </p>
      ) : null}

      <Button
        onClick={() => assets && to && borrow.run(assets, to)}
        busy={borrow.busy}
        disabled={!assets || Boolean(amountError) || !to}
      >
        {snapshot.accountDeployed ? "Borrow" : "Create account and borrow"}
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
