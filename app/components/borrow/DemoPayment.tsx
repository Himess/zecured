"use client";

import { ActionError } from "@/components/ActionError";
import { CopyButton } from "@/components/client";
import { IconBolt, IconCheck, IconExternal } from "@/components/icons";
import { Badge, Button } from "@/components/ui";
import { api, apiErrorCode } from "@/lib/api";
import { config } from "@/lib/config";
import { demoRefusal, solveDemoStamp } from "@/lib/demo";
import { sampleTransactions, zcashTxUrl } from "@/lib/explorer";
import { notifyLoansChanged, useAction, useDemoStatus } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import { REPLAY } from "@/lib/replay/flag";
import { formatTaz } from "@/lib/zcash";

/**
 * P-1 (DEMO, the testnet deployment only): "Pay it with demo TAZ". The backend's demo wallet pays the vault its
 * full amount, once per vault, after a small proof of work in this browser. The vault is described exactly as all
 * five signers vouched for it when it opened (C-4), so the demo wallet pays only a vault the signers control.
 * Shown only in a build with the demo path and while the backend's demo wallet is on: a slim dashed row, its
 * button the step's one primary.
 */
export function DemoPayment({ loan, onPaid }: { loan: StoredLoan; onPaid?: () => void }) {
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
      onPaid?.();
      return r;
    } catch (e) {
      const why = demoRefusal(apiErrorCode(e));
      if (why) throw new Error(why);
      throw e;
    }
  });

  if (!config.demoPath || !demo.data?.enabled) return null;
  const amount = formatTaz(BigInt(loan.amount));
  const txUrl = pay.result ? zcashTxUrl(pay.result.txid) : null;
  return (
    <div className="space-y-3 rounded-2xl border border-dashed border-demo/45 bg-demo/[0.04] p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-x-5 gap-y-3">
        <div className="min-w-0 flex-1 basis-[18rem]">
          <p className="flex flex-wrap items-center gap-2 text-[15px] font-bold">
            <Badge tone="demo">DEMO</Badge>
            No testnet ZEC at hand?
          </p>
          <p className="mt-1 text-[13.5px] text-muted">
            {REPLAY
              ? `The demo wallet pays this vault ${amount} from its own Ironwood notes. In the live app that is a real Zcash testnet payment, proven on Base like any deposit; here the replay plays it.`
              : `The demo wallet pays this vault ${amount} from its own Ironwood notes, once per vault: a real Zcash testnet payment, proven on Base like any deposit. When you exit, your ZEC goes back to the demo wallet unless you give an Ironwood address of your own.`}
          </p>
        </div>
        {pay.result ? null : (
          <Button size="lg" data-replay="demo-pay" onClick={() => void pay.run()} busy={pay.busy}>
            {pay.busy ? null : <IconBolt size={17} />}
            {pay.busy ? "Paying…" : "Pay it with demo TAZ"}
          </Button>
        )}
      </div>
      {pay.result ? (
        <p role="status" className="flex flex-wrap items-center gap-2 text-[13.5px] text-soft">
          <IconCheck size={16} strokeWidth={2.6} className="text-ok" />
          The demo wallet paid {formatTaz(pay.result.amount)}.
          {sampleTransactions() ? (
            <span className="text-muted">Zcash transaction · sample data</span>
          ) : (
            <>
              <span className="text-muted">
                Zcash transaction{" "}
                {txUrl ? (
                  <a
                    href={txUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    referrerPolicy="no-referrer"
                    className="inline-flex items-center gap-1 font-mono text-soft hover:underline"
                    title="A public block explorer learns which transaction you look at."
                  >
                    {short(pay.result.txid, 8, 6)}
                    <IconExternal size={12} />
                    <span className="sr-only">(a Zcash block explorer, opens a new tab: a third-party site that learns which transaction you look at)</span>
                  </a>
                ) : (
                  <span className="font-mono text-soft">{short(pay.result.txid, 8, 6)}</span>
                )}
              </span>
              <CopyButton value={pay.result.txid} />
            </>
          )}
          {REPLAY ? null : <span className="basis-full text-muted">Its confirmations on Zcash take a few blocks; this page follows them.</span>}
        </p>
      ) : null}
      {pay.busy ? (
        <p className="text-xs text-muted">
          {REPLAY
            ? "The replay plays the demo wallet's payment."
            : "A small proof of work in this browser, then the demo wallet builds and proves the payment (up to a minute)…"}
        </p>
      ) : null}
      <ActionError error={pay.error} refusal={pay.refusal} title="DEMO: no demo payment" />
      {demo.data.lowBalance ? <p className="text-xs text-warn">The demo wallet runs low; demo payments may fail.</p> : null}
    </div>
  );
}
