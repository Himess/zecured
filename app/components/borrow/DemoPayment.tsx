"use client";

import { Badge, Button, Mono, Notice } from "@/components/ui";
import { CopyButton } from "@/components/client";
import { api, apiErrorCode } from "@/lib/api";
import { config } from "@/lib/config";
import { demoRefusal, solveDemoStamp } from "@/lib/demo";
import { notifyLoansChanged, useAction, useDemoStatus } from "@/lib/hooks";
import { loanStore, type StoredLoan } from "@/lib/keys";
import { formatTaz } from "@/lib/zcash";

/**
 * P-1 (DEMO, judge deployment only): "Try with demo TAZ". The backend's demo wallet pays the vault
 * its full amount, once per vault, after a small proof of work in this browser. The vault is
 * described exactly as all five signers vouched for it when it opened (C-4), so the demo wallet
 * pays only a vault the signers control. Shown only in a build with the demo path and while the
 * backend's demo wallet is on.
 */
export function DemoPayment({ loan }: { loan: StoredLoan }) {
  const demo = useDemoStatus();
  const pay = useAction(async () => {
    const status = demo.data;
    if (!status?.enabled) throw new Error("The demo wallet is off on this deployment.");
    if (!loan.vaultId || !loan.vaultAddress || loan.expiryHeight === undefined || !loan.tagKeyHash || !loan.vaultSignatures) {
      throw new Error("This loan's vault was opened before vaults were signed: the demo wallet does not pay it.");
    }
    const stamp = await solveDemoStamp(loan.vaultId, status.stampBits);
    try {
      const r = await api.demoPay(loan.vaultId, {
        owner: loan.owner,
        account: loan.account,
        amount: BigInt(loan.amount),
        expiryHeight: loan.expiryHeight,
        address: loan.vaultAddress,
        tagKeyHash: loan.tagKeyHash,
        signatures: loan.vaultSignatures,
        stamp,
      });
      loanStore().update(loan.id, { demo: true });
      notifyLoansChanged();
      return r;
    } catch (e) {
      const why = demoRefusal(apiErrorCode(e));
      if (why) throw new Error(why);
      throw e;
    }
  });

  if (!config.demoPath || !demo.data?.enabled) return null;
  return (
    <div className="space-y-3 rounded-lg border border-dashed border-warn/60 p-4">
      <p className="flex flex-wrap items-center gap-2 text-sm font-medium">
        <Badge tone="warn">DEMO</Badge> No testnet ZEC at hand?
      </p>
      <p className="text-sm text-muted">
        The demo wallet can pay this vault its {formatTaz(BigInt(loan.amount))} from its own Ironwood notes, once per vault. It
        is a real Zcash testnet payment, proven on Base like any deposit. When you exit, your ZEC goes back to the demo
        wallet unless you give an Ironwood address of your own.
      </p>
      {pay.result ? (
        <Notice tone="ok" title="DEMO: the demo wallet paid your vault" live>
          <p>
            {formatTaz(pay.result.amount)} sent from the demo wallet. Zcash transaction <Mono>{pay.result.txid}</Mono>{" "}
            <CopyButton value={pay.result.txid} />
          </p>
          <p className="text-xs">The deposit is proven on Base once it has its confirmations; this page follows it.</p>
        </Notice>
      ) : (
        <Button variant="secondary" onClick={() => void pay.run()} busy={pay.busy}>
          Try with demo TAZ (DEMO)
        </Button>
      )}
      {pay.busy ? <p className="text-xs text-muted">Computing a small proof of work, then the demo wallet builds and proves the payment (up to a minute)…</p> : null}
      {pay.error ? (
        <Notice tone="danger" title="DEMO: no demo payment" live>
          <p>{pay.error}</p>
        </Notice>
      ) : null}
      {demo.data.lowBalance ? <p className="text-xs text-warn">The demo wallet runs low; demo payments may fail.</p> : null}
    </div>
  );
}
