import type { ReactNode } from "react";
import type { SentUserOp } from "@/lib/account";
import { baseTxUrl, sampleTransactions } from "@/lib/explorer";
import { short } from "@/lib/keys";
import { CopyButton } from "./client";
import { IconCheck, IconExternal } from "./icons";
import { Notice } from "./ui";

/**
 * The receipt of a sponsored UserOp: what happened (an amount, when there is one), that the gas was sponsored, and
 * its transaction on Base. On the testnet the transaction links to the block explorer, so anyone can see it; the
 * mainnet beta shows the hash only (lib/deployment.ts `baseExplorerTx`): opening an explorer from this device would
 * tell it which account you care about. A replay of sample data names no transaction.
 */
export function TxResult({ result, what, amount, children }: { result: SentUserOp; what: string; amount?: ReactNode; children?: ReactNode }) {
  if (!result.success) {
    return (
      <Notice tone="danger" title={`${what}: reverted on Base`} live>
        <p>Nothing changed. Transaction {short(result.transactionHash, 10, 8)}.</p>
      </Notice>
    );
  }
  const explorer = baseTxUrl(result.transactionHash);
  const sample = sampleTransactions();
  return (
    <div role="status" className="animate-rise space-y-3 rounded-[20px] bg-ok/[0.05] p-5 shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-ok)_32%,transparent)]">
      <p className="flex items-center gap-2 text-[15px] font-bold">
        <span className="inline-flex size-6 items-center justify-center rounded-full bg-ok/20 text-ok">
          <IconCheck size={14} strokeWidth={2.8} />
        </span>
        {what}
      </p>
      {amount ? <div className="animate-pop">{amount}</div> : null}
      {children ? <div className="text-[14px] text-soft">{children}</div> : null}
      <p className="flex flex-wrap items-center gap-x-3 gap-y-2 text-[12.5px] text-muted">
        <span className="inline-flex min-h-6 items-center rounded-full px-2.5 font-semibold text-soft shadow-[inset_0_0_0_1px_var(--color-line)]">
          Gas sponsored
        </span>
        {sample ? (
          <span>Transaction on Base · sample data</span>
        ) : (
          <>
            <span>
              Transaction <span className="font-mono text-soft">{short(result.transactionHash, 10, 8)}</span>
            </span>
            <CopyButton value={result.transactionHash} />
            {explorer ? (
              <a
                href={explorer}
                target="_blank"
                rel="noopener noreferrer"
                referrerPolicy="no-referrer"
                className="inline-flex items-center gap-1 font-semibold text-info no-underline hover:underline"
                title="A public block explorer learns which transaction you look at."
              >
                Basescan
                <IconExternal size={13} />
                <span className="sr-only">(opens a new tab, a third-party site that learns which transaction you look at)</span>
              </a>
            ) : null}
          </>
        )}
      </p>
    </div>
  );
}
