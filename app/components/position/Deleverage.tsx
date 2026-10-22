"use client";

import { useId, useState } from "react";
import { TxResult } from "@/components/TxResult";
import { Button, KeyValue, Mono, Notice } from "@/components/ui";
import { loanAccount } from "@/lib/account";
import { api, type DeleverageQuote, type Vault } from "@/lib/api";
import { invalidateNonceCalls } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { deleveragePreview } from "@/lib/deleverage";
import { CLOSE_TTL_SECONDS, deadlineIn, randomNonce128, type DeleverageRequestMessage } from "@/lib/eip712";
import { useAction } from "@/lib/hooks";
import type { StoredLoan } from "@/lib/keys";
import { loanTokenName } from "@/lib/labels";
import { formatBps, formatUsdc, formatUsdPrice, formatWad, ltvBps, parseUsdc } from "@/lib/morpho";
import { formatTaz } from "@/lib/zcash";

/**
 * P-7, "Reduce debt with collateral" (S18): a market maker repays the amount asked for and takes
 * collateral worth it plus its margin, through `SettlementAdapter.deleverageWithCollateral`; the
 * vault stays active. A quote first, then the full preview (debt repaid, collateral spent, the loan
 * after), then a DeleverageRequest signed by the account (ERC-1271) for that executor, margin and at
 * most the quoted collateral plus 1 %.
 */
export function Deleverage({
  loan,
  vault,
  snapshot,
  onChanged,
}: {
  loan: StoredLoan;
  vault: Vault;
  snapshot: PositionSnapshot;
  onChanged: () => void;
}) {
  const inputId = useId();
  const [input, setInput] = useState("");
  const [quote, setQuote] = useState<DeleverageQuote | null>(null);
  const [signed, setSigned] = useState<DeleverageRequestMessage | null>(null);
  let repay: bigint | null = null;
  let inputError: string | null = null;
  if (input.trim()) {
    try {
      repay = parseUsdc(input);
      if (repay === 0n) inputError = "Enter an amount above zero.";
      else if (repay >= snapshot.debt) inputError = "To repay everything from collateral, close from collateral instead.";
    } catch (e) {
      inputError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  const getQuote = useAction(async (amount: bigint) => {
    const q = await api.getDeleverageQuote(vault.vaultId, amount);
    setQuote(q);
    return q;
  });
  const preview =
    quote !== null
      ? deleveragePreview({ collateral: snapshot.position.collateral, debt: snapshot.debt, price: snapshot.price, lltv: config.lltv, quote })
      : null;
  const submit = useAction(async () => {
    if (!quote || !preview) throw new Error("Get a quote first.");
    const req: DeleverageRequestMessage = {
      vaultId: vault.vaultId,
      account: loan.account,
      executor: quote.executor,
      marginBps: quote.marginBps,
      repayAssets: quote.repayAssets,
      maxCollateral: preview.maxCollateral,
      nonce: randomNonce128(),
      deadline: deadlineIn(CLOSE_TTL_SECONDS),
    };
    const signature = await loanAccount(loan.ownerKey).signDeleverageRequest(req);
    setSigned(req);
    const tx = await api.submitDeleverage(req, signature);
    setSigned(null);
    setQuote(null);
    setInput("");
    onChanged();
    return { tx };
  });
  const cancel = useAction(async (nonce: bigint) => {
    const r = await loanAccount(loan.ownerKey).send(invalidateNonceCalls({ nonce }));
    if (r.success) setSigned(null);
    return r;
  });
  const ltvNow = ltvBps(snapshot.position.collateral, snapshot.price, snapshot.debt);
  const nowSeconds = Math.floor(Date.now() / 1000);
  const expired = quote ? quote.expiresAt <= nowSeconds : false;
  const token = loanTokenName();

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">
        Lower your debt without bringing {token}: a market maker repays the part you choose and takes ZEC from your vault worth
        it plus its margin, paid out on Zcash to its own shielded address. Your loan stays open with less debt and less
        collateral.
      </p>
      {!snapshot.accountDeployed ? (
        <Notice tone="info" title="Your account is not deployed yet">
          <p>The request is signed by the account itself (ERC-1271): borrow first.</p>
        </Notice>
      ) : !snapshot.adapterAuthorized ? (
        <Notice tone="info" title="The Settlement Adapter is not authorized yet">
          <p>Authorize it from the close-from-collateral step first: it withdraws the collateral when the request executes.</p>
        </Notice>
      ) : null}
      <div className="space-y-1">
        <label htmlFor={inputId} className="block text-sm font-medium">
          Debt to repay ({token})
        </label>
        <div className="flex gap-2">
          <input
            id={inputId}
            inputMode="decimal"
            autoComplete="off"
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              setQuote(null);
            }}
            placeholder={formatUsdc(snapshot.debt / 2n)}
            aria-invalid={inputError ? true : undefined}
            className="w-full rounded-lg border border-border bg-bg px-3 py-2 tabular-nums text-text focus:border-muted"
          />
          <Button variant="secondary" onClick={() => repay && void getQuote.run(repay)} busy={getQuote.busy} disabled={!repay || Boolean(inputError)}>
            {quote ? "Refresh quote" : "Get a quote"}
          </Button>
        </div>
        {inputError ? <p className="text-sm text-danger">{inputError}</p> : null}
      </div>
      {getQuote.error ? (
        <Notice tone="danger" title="No quote" live>
          <p>{getQuote.error}</p>
        </Notice>
      ) : null}
      {quote && preview ? (
        <div className="space-y-3">
          <KeyValue
            rows={[
              ["Debt repaid", `${formatUsdc(preview.repaid)} ${token}`],
              ["Collateral spent", `${formatTaz(preview.collateralSpent)} (margin ${quote.marginBps / 100} %; at most ${formatTaz(preview.maxCollateral)})`],
              ["Debt after", `${formatUsdc(preview.debtAfter)} ${token}`],
              ["Collateral after", formatTaz(preview.collateralAfter)],
              [
                "Loan-to-value",
                `${ltvNow === null ? "—" : formatBps(ltvNow)} → ${preview.ltvAfterBps === null ? "—" : formatBps(preview.ltvAfterBps)}`,
              ],
              ["Health factor after", preview.healthAfter === null ? "∞ (no debt left)" : formatWad(preview.healthAfter)],
              ["Liquidation price after", preview.liquidationPriceAfter === null ? "—" : `${formatUsdPrice(preview.liquidationPriceAfter)} per ZEC`],
              ["Executor", <Mono key="e">{quote.executor}</Mono>],
              ["Quote valid until", new Date(quote.expiresAt * 1000).toLocaleTimeString()],
            ]}
          />
          <p className="text-xs text-muted">
            Signing authorizes only this executor, this margin, this repayment and at most the collateral shown, for about 10
            minutes. The collateral the market maker takes appears on Base as its Liquidation claim.
          </p>
          <Button
            onClick={() => void submit.run()}
            busy={submit.busy}
            disabled={expired || !snapshot.accountDeployed || !snapshot.adapterAuthorized}
          >
            Sign and reduce my debt
          </Button>
          {expired ? <p className="text-sm text-warn">This quote expired. Refresh it.</p> : null}
        </div>
      ) : null}
      {submit.error ? (
        <Notice tone="danger" title="The debt was not reduced" live>
          <p>{submit.error}</p>
          {signed ? (
            <>
              <p>The signed request stays valid until it expires. Cancel it if you no longer want it executed.</p>
              <Button variant="danger" onClick={() => void cancel.run(signed.nonce)} busy={cancel.busy}>
                Cancel the signed request (sponsored)
              </Button>
            </>
          ) : null}
        </Notice>
      ) : null}
      {submit.result ? (
        <Notice tone="ok" title="Debt reduced" live>
          <p>
            {submit.result.tx ? (
              <>
                The market maker&apos;s transaction: <Mono>{submit.result.tx}</Mono>
              </>
            ) : (
              "Executed by the market maker."
            )}
          </p>
        </Notice>
      ) : null}
      {cancel.result ? <TxResult result={cancel.result} what="Request cancelled" /> : null}
      {cancel.error ? <p className="text-sm text-danger">{cancel.error}</p> : null}
    </div>
  );
}
