"use client";

import Link from "next/link";
import { MissingVaultId, useMounted } from "./client";
import { PendingActivationCard, PendingRefundsCard, RefundsCard } from "./position/Ledger";
import { buttonClass, Card, Notice } from "./ui";
import { config } from "@/lib/config";
import { useLoanByVault, useUrlFragment, useVault, useVaultLedger } from "@/lib/hooks";
import { vaultHref, vaultIdFromFragment } from "@/lib/routes";
import { formatPeriod } from "@/lib/v3";
import { formatTaz } from "@/lib/zcash";

/**
 * The vault's refunds (T-02): every deposit that cannot back a loan lands on Base as a Refund
 * claim, whose Ironwood destination the vault's account gives here; and a pending activation can
 * be turned into one after a long pause (T-03). There is no refund outside Base. The vault comes
 * from the URL fragment (`/refund#0x…`), never the path (M-6).
 */
export function RefundView() {
  const mounted = useMounted();
  const fragment = useUrlFragment();
  const vaultId = (fragment && vaultIdFromFragment(fragment)) || undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 20_000);
  const ledger = useVaultLedger(vaultId, 20_000);

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;
  const l = ledger.data;
  const others = vault.data?.nonConforming ?? [];
  const refunds = l?.claims.filter((c) => c.kind === "Refund") ?? [];
  const refresh = () => {
    void vault.refresh();
    void ledger.refresh();
  };

  return (
    <div className="space-y-6">
      <Notice tone="info" title="How deposits come back">
        <p>
          Only one exact Ironwood payment of the vault&apos;s amount, before its deadline, backs a loan. Any other deposit
          that reaches the vault (a second payment, a deposit while minting is halted, or one you had proven as a refund
          rather than wait for room under the coverage cap) is proven on Base as a refund: pending for a few Zcash blocks,
          then a Refund claim. You give it an Ironwood destination here, and the signers pay it, minus the network fee,
          through a commitment on Base like every payout.
          {config.v3
            ? ` A refund left without a destination for ${formatPeriod(config.v3.refundAbandonPeriod)} can be written off; you can still revive it later by giving one.`
            : null}
        </p>
      </Notice>
      {mounted && ready && !loan ? (
        <Notice tone="info" title="View only">
          <p>
            A refund&apos;s destination is given with the loan&apos;s owner key. <Link href="/loans">Import the backup</Link> to
            give one.
          </p>
        </Notice>
      ) : null}
      {ledger.error ? (
        <Notice tone="warn" title="Registry unavailable" live>
          <p>{ledger.error}</p>
        </Notice>
      ) : null}
      {!config.contractsDeployed ? (
        <Notice tone="info" title="Refunds are on Base">
          <p>They appear here once the app is configured for the Base contracts.</p>
        </Notice>
      ) : null}
      {ledger.loading ? <p className="text-sm text-muted">Reading the vault on Base…</p> : null}
      {l?.pending ? <PendingActivationCard ledger={l} loan={loan} onChanged={refresh} /> : null}
      {l ? <PendingRefundsCard ledger={l} /> : null}
      {l ? <RefundsCard ledger={l} loan={loan} onChanged={refresh} /> : null}
      {l && refunds.length === 0 && (l.pendingRefunds?.length ?? 0) === 0 ? (
        <Card>
          <p className="text-sm text-muted">This vault has no refunds on Base.</p>
        </Card>
      ) : null}
      {others.length > 0 ? (
        <Notice tone="warn" title="Other payments to this vault">
          <p>
            The signers' scan found {others.length} other payment(s) to this vault ({others.map((n) => formatTaz(n.amount)).join(", ")})
            that cannot back the loan. Such a payment comes back only as a Refund claim proven on Base; it then appears above.
          </p>
        </Notice>
      ) : null}
      <Link href={vaultHref("position", vaultId)} prefetch={false} className={buttonClass("ghost", "no-underline")}>
        Back to the loan
      </Link>
    </div>
  );
}
