"use client";

import { useId, useState } from "react";
import type { Hex } from "viem";
import { DestinationStep } from "@/components/DestinationForm";
import { TxResult } from "@/components/TxResult";
import { Button, cx, KeyValue, Mono, Notice } from "@/components/ui";
import { loanAccount, type SentUserOp } from "@/lib/account";
import type { Vault } from "@/lib/api";
import { describeRevert, partialReleaseCalls } from "@/lib/calls";
import { readReleaseClaim, type PositionSnapshot, type ReleaseClaim } from "@/lib/chain";
import { config } from "@/lib/config";
import { notifyLoansChanged, useAction } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import {
  collateralValue,
  formatBps,
  formatUsdc,
  formatUsdPrice,
  formatWad,
  healthFactor,
  liquidationPrice,
  ltvAfterRelease,
  MAX_WITHDRAW_LTV_BPS,
  maxPartialRelease,
  MIN_RELEASE,
  partialReleaseRejection,
} from "@/lib/morpho";
import { formatTaz, formatZec, parseZec } from "@/lib/zcash";

/**
 * D14: withdraw part of the collateral while the loan stays open. Same pattern as the exits:
 * the signers pre-validate the Ironwood destination first (DestinationStep), then one
 * sponsored UserOp authorizes the Registry if needed and calls `requestPartialRelease`.
 */
export function PartialRelease({
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
  const amountId = useId();
  const [input, setInput] = useState("");
  const [commitment, setCommitment] = useState<Hex | null>(null);
  const collateral = snapshot.position.collateral;
  const { debt, price } = snapshot;
  const max = maxPartialRelease({ collateral, debt, price, lltv: config.lltv });

  const release = useAction(async (amount: bigint, dest: Hex): Promise<{ sent: SentUserOp; claim: ReleaseClaim | null }> => {
    let sent: SentUserOp;
    try {
      sent = await loanAccount(loan.ownerKey).send(
        partialReleaseCalls({ vaultId: vault.vaultId, amount, commitment: dest, registryAuthorized: snapshot.registryAuthorized }),
      );
    } catch (e) {
      const why = describeRevert(e);
      if (why) throw new Error(why);
      throw e;
    }
    let claim: ReleaseClaim | null = null;
    if (sent.success) {
      // This destination is used: the next payout asks for a fresh one.
      loanStore().update(loan.id, { destinationCommitment: undefined });
      notifyLoansChanged();
      setInput("");
      setCommitment(null);
      claim = await readReleaseClaim(vault.vaultId, sent.transactionHash).catch(() => null);
    }
    onChanged();
    return { sent, claim };
  });

  let amount: bigint | null = null;
  let amountError: string | null = null;
  if (input.trim()) {
    try {
      amount = parseZec(input);
      const rejection = partialReleaseRejection({ collateral, debt, price, lltv: config.lltv, amount });
      if (rejection === "ReleaseAmountTooSmall") amountError = `Withdraw at least ${formatTaz(MIN_RELEASE)}.`;
      else if (rejection === "ReleaseAmountNotPartial") amountError = "That is all of your collateral: use an exit above to take everything.";
      else if (rejection === "ReleaseRemainderTooSmall") amountError = `At least ${formatTaz(MIN_RELEASE)} must stay in the vault.`;
      else if (rejection) amountError = `That would leave the loan above ${formatBps(MAX_WITHDRAW_LTV_BPS)} LTV. At most ${formatTaz(max)}.`;
      else if (amount > max) amountError = `At most ${formatTaz(max)}: a small margin covers interest accruing until the withdrawal is included.`;
    } catch (e) {
      amountError = e instanceof Error ? e.message : "Invalid amount";
    }
  }
  const valid = amount !== null && !amountError;

  if (max === 0n && !release.result) {
    return (
      <Notice tone="info" title="Nothing can be withdrawn right now">
        <p>
          {collateral < 2n * MIN_RELEASE
            ? `A partial withdrawal takes at least ${formatTaz(MIN_RELEASE)} and leaves at least as much in the vault; this vault holds too little.`
            : `With the current debt, any withdrawal would leave the loan above ${formatBps(MAX_WITHDRAW_LTV_BPS)} LTV. Less debt or a higher ZEC price leaves room.`}
        </p>
      </Notice>
    );
  }

  const remaining = valid ? collateral - amount! : null;
  const ltvAfter = valid ? ltvAfterRelease({ collateral, debt, price, amount: amount! }) : null;
  const hfAfter = remaining !== null ? healthFactor(remaining, price, debt, config.lltv) : null;
  const lpAfter = remaining !== null ? liquidationPrice(remaining, debt, config.lltv) : null;

  return (
    <div className="space-y-5">
      <p className="text-sm text-muted">
        Take part of your ZEC back while the loan stays open. The loan must stay at or below{" "}
        {formatBps(MAX_WITHDRAW_LTV_BPS)} LTV afterwards (the market liquidates above 62.5 %), and at least{" "}
        {formatTaz(MIN_RELEASE)} stays in the vault.
      </p>
      <ol className="space-y-5">
        <li className="space-y-3">
          <h3 className="font-semibold">1. How much?</h3>
          <div className="space-y-2">
            <label htmlFor={amountId} className="block text-sm font-medium">
              Amount (TAZ)
            </label>
            <div className="flex gap-2">
              <input
                id={amountId}
                inputMode="decimal"
                autoComplete="off"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                placeholder={formatZec(max)}
                aria-invalid={amountError ? true : undefined}
                aria-describedby={`${amountId}-hint`}
                className="w-full rounded-lg border border-border bg-bg px-3 py-2 tabular-nums text-text focus:border-accent"
              />
              <Button variant="secondary" onClick={() => setInput(formatZec(max))}>
                Max
              </Button>
            </div>
            <p id={`${amountId}-hint`} className={amountError ? "text-sm text-danger" : "text-xs text-muted"} aria-live="polite">
              {amountError ??
                `Up to ${formatTaz(max)} of ${formatTaz(collateral)}, minus the Zcash network fee when it is paid out.`}
            </p>
          </div>
          {remaining !== null ? (
            <div aria-live="polite">
              <KeyValue
                rows={[
                  ["Collateral after", `${formatTaz(remaining)} (≈ ${formatUsdc(collateralValue(remaining, price))} USDC)`],
                  ["Loan-to-value after", `${ltvAfter !== null ? formatBps(ltvAfter) : "—"} (limit ${formatBps(MAX_WITHDRAW_LTV_BPS)})`],
                  ["Health factor after", hfAfter === null ? "∞ (no debt)" : formatWad(hfAfter)],
                  ["Liquidation price after", lpAfter ? `${formatUsdPrice(lpAfter)} per ZEC` : "—"],
                ]}
              />
            </div>
          ) : null}
        </li>
        <li className="space-y-3">
          <h3 className="font-semibold">2. Where should it go?</h3>
          {commitment ? (
            <Notice tone="ok" title="Destination validated by the signers">
              <p>
                Commitment <Mono>{short(commitment, 10, 8)}</Mono>.{" "}
                <button type="button" className="underline" onClick={() => setCommitment(null)}>
                  Change
                </button>
              </p>
            </Notice>
          ) : (
            <DestinationStep loan={loan} vaultId={vault.vaultId} onDone={setCommitment} />
          )}
        </li>
        <li className="space-y-3">
          <h3 className={cx("font-semibold", !(valid && commitment) && "text-faint")}>3. Withdraw</h3>
          <Button onClick={() => valid && commitment && release.run(amount!, commitment)} busy={release.busy} disabled={!valid || !commitment}>
            {valid ? `Withdraw ${formatTaz(amount!)} (sponsored)` : "Withdraw (sponsored)"}
          </Button>
          {!snapshot.registryAuthorized ? (
            <p className="text-xs text-muted">
              The same transaction first authorizes the Zecured Registry on Morpho: it withdraws the collateral for you.
            </p>
          ) : null}
          {release.error ? (
            <Notice tone="danger" title="The withdrawal did not go through" live>
              <p>{release.error}</p>
            </Notice>
          ) : null}
          {release.result ? (
            <div className="space-y-3">
              <TxResult result={release.result.sent} what="Partial release requested" />
              {release.result.sent.success ? <ClaimStatus claim={release.result.claim} vault={vault} /> : null}
            </div>
          ) : null}
        </li>
      </ol>
    </div>
  );
}

/** The Release claim the withdrawal created, as the signers see it (GET /vaults/:id, polled). */
function ClaimStatus({ claim, vault }: { claim: ReleaseClaim | null; vault: Vault }) {
  if (!claim) {
    return <p className="text-sm text-muted">The new Release claim appears under Zcash payouts once the signers record it.</p>;
  }
  const seen = vault.claims.find((c) => c.claimId.toLowerCase() === claim.claimId.toLowerCase());
  const status = !seen
    ? "Waiting for the signers to record it from Base"
    : seen.status === "paid"
      ? "Paid to your Ironwood destination"
      : "Pending: the signers pay it in their payout queue";
  return (
    <KeyValue
      rows={[
        ["Release claim", <Mono key="c">{short(claim.claimId, 10, 8)}</Mono>],
        ["Amount", `${formatTaz(claim.amount)} minus the Zcash network fee`],
        [
          "Payout",
          <span key="s" role="status">
            {status}
          </span>,
        ],
      ]}
    />
  );
}
