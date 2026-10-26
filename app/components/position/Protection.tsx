"use client";

import { useId, useState } from "react";
import { TxResult } from "@/components/TxResult";
import { Badge, Button, cx, KeyValue, Notice } from "@/components/ui";
import { loanAccount } from "@/lib/account";
import { clearPreLiquidationCalls, describeRevert, formatUtc, setPreLiquidationCalls, type Call } from "@/lib/calls";
import type { PositionSnapshot } from "@/lib/chain";
import { config } from "@/lib/config";
import { useAction, usePreLiquidation } from "@/lib/hooks";
import type { StoredLoan } from "@/lib/keys";
import { formatBps, formatUsdc, ltvBps } from "@/lib/morpho";
import {
  formatIncentive,
  formatWadPercent,
  PRE_LIQUIDATION_PRESETS,
  preLiquidationNow,
  previewSentence,
  termsActive,
  termsOf,
  type PreLiquidationNow,
  type PreLiquidationPreset,
} from "@/lib/preliq";
import { formatTaz } from "@/lib/zcash";

/** The "Now" row: below the band, in it (with today's numbers), or past the LLTV. */
function nowSentence(p: PreLiquidationNow): string {
  switch (p.state) {
    case "none":
      return "No debt: nothing to pre-liquidate.";
    case "below":
      return "Below the band: nobody can pre-liquidate.";
    case "above":
      return "Above the market's liquidation LTV: a full liquidation applies.";
    case "band":
      return `In the band: one pre-liquidation may repay up to ${formatUsdc(p.maxRepayAssets)} USDC (${formatWadPercent(p.closeFactor)} of the debt) and take about ${formatTaz(p.seizedAtMax)} of your collateral for it (a ${formatIncentive(p.incentive)} incentive).`;
  }
}

async function send(loan: StoredLoan, calls: Call[]) {
  try {
    return await loanAccount(loan.ownerKey).send(calls);
  } catch (e) {
    const why = describeRevert(e);
    if (why) throw new Error(why);
    throw e;
  }
}

/** The presets as a radio group, with the preview sentence for `debt` (loan-token units, after the borrow if any). */
export function ProtectionPresets({
  preset,
  onPreset,
  debt,
}: {
  preset: PreLiquidationPreset;
  onPreset: (p: PreLiquidationPreset) => void;
  debt: bigint | null;
}) {
  const group = useId();
  return (
    <div className="space-y-3">
      <fieldset className="grid gap-2 sm:grid-cols-3">
        <legend className="sr-only">When pre-liquidation may start</legend>
        {PRE_LIQUIDATION_PRESETS.map((p) => (
          <label
            key={p.id}
            className={cx(
              "flex cursor-pointer items-start gap-2 rounded-lg border p-3 text-sm",
              preset.id === p.id ? "border-soft bg-surface-2" : "border-border hover:border-faint",
            )}
          >
            <input type="radio" name={group} checked={preset.id === p.id} onChange={() => onPreset(p)} className="mt-1" />
            <span>
              <span className="block font-medium">{p.label}</span>
              <span className="block text-xs text-muted">
                {formatIncentive(p.preLif1)} to {formatIncentive(p.preLif2)} incentive, {formatWadPercent(p.preLcf1)} to{" "}
                {formatWadPercent(p.preLcf2)} of the debt
              </span>
            </span>
          </label>
        ))}
      </fieldset>
      <p className="text-sm text-muted">{previewSentence(preset, config.lltv, debt, formatUsdc)}</p>
    </div>
  );
}

/**
 * Section 4 of the v4 design, "Protect my loan": the account's consent to pre-liquidation, its
 * terms, where the loan's LTV stands in the band, and the cancel. One sponsored UserOp each.
 */
export function ProtectionCard({ loan, snapshot, onChanged }: { loan: StoredLoan; snapshot: PositionSnapshot; onChanged: () => void }) {
  const terms = usePreLiquidation(loan.account);
  const [preset, setPreset] = useState<PreLiquidationPreset>(PRE_LIQUIDATION_PRESETS[1]!);
  const nowSeconds = BigInt(snapshot.blockTimestamp);
  const protect = useAction(async () => {
    const r = await send(loan, setPreLiquidationCalls({ terms: termsOf(preset, nowSeconds) }));
    void terms.refresh();
    onChanged();
    return r;
  });
  const cancel = useAction(async () => {
    const r = await send(loan, clearPreLiquidationCalls());
    void terms.refresh();
    onChanged();
    return r;
  });
  const [confirmCancel, setConfirmCancel] = useState(false);

  // A Settlement Adapter without pre-liquidation (v3): nothing to offer.
  if (terms.data === null && !terms.loading && !terms.error) return null;
  const t = terms.data ?? null;
  const active = t !== null && termsActive(t, nowSeconds) ? t : null;
  const ltv = ltvBps(snapshot.position.collateral, snapshot.price, snapshot.debt);
  // Where the loan stands now, as the Adapter would decide a pre-liquidation (the market accrued to the latest block).
  const position = active ? preLiquidationNow(active, config.lltv, snapshot.position, snapshot.market, snapshot.price) : null;

  return (
    <div className="space-y-4">
      {terms.error ? <p className="text-sm text-warn">The Settlement Adapter could not be read: {terms.error}</p> : null}
      {active ? (
        <>
          <p className="flex flex-wrap items-center gap-2 text-sm">
            <Badge tone="ok">Protected</Badge>
            <span className="text-muted">{previewSentence(active, config.lltv, snapshot.debt, formatUsdc)}</span>
          </p>
          <KeyValue
            rows={[
              ["Starts above", formatWadPercent(active.preLltv)],
              ["Your loan-to-value now", ltv === null ? "—" : formatBps(ltv)],
              ["Now", position ? nowSentence(position) : "—"],
              ["Terms valid until", formatUtc(active.expiry)],
            ]}
          />
          {!confirmCancel ? (
            <Button variant="ghost" onClick={() => setConfirmCancel(true)}>
              Cancel the protection…
            </Button>
          ) : (
            <Notice tone="warn" title="Cancel the protection?">
              <p>Without it, nothing happens until the loan reaches 62.5 % LTV, where a full liquidation takes a 12.7 % incentive.</p>
              <div className="flex flex-wrap gap-2">
                <Button variant="danger" onClick={() => void cancel.run()} busy={cancel.busy}>
                  Cancel the protection (sponsored)
                </Button>
                <Button variant="ghost" onClick={() => setConfirmCancel(false)}>
                  Keep it
                </Button>
              </div>
            </Notice>
          )}
        </>
      ) : (
        <>
          <p className="text-sm text-muted">
            Agree once to be partly repaid early: if the loan-to-value climbs into the band you choose, anyone may repay a part of
            the debt for a small incentive, paid in your vault&apos;s shielded ZEC, which brings the loan back down long before a
            full liquidation. You can cancel any time.
            {t && t.preLltv !== 0n ? " Your earlier terms have expired." : ""}
          </p>
          <ProtectionPresets preset={preset} onPreset={setPreset} debt={snapshot.debt} />
          <Button onClick={() => void protect.run()} busy={protect.busy}>
            Protect my loan (sponsored)
          </Button>
        </>
      )}
      {protect.error ? (
        <Notice tone="danger" title="The protection was not set" live>
          <p>{protect.error}</p>
        </Notice>
      ) : null}
      {protect.result ? <TxResult result={protect.result} what="Protection set" /> : null}
      {cancel.error ? (
        <Notice tone="danger" title="The protection was not cancelled" live>
          <p>{cancel.error}</p>
        </Notice>
      ) : null}
      {cancel.result ? <TxResult result={cancel.result} what="Protection cancelled" /> : null}
    </div>
  );
}
