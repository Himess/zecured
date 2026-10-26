import type { SentUserOp } from "@/lib/account";
import { baseTxUrl } from "@/lib/explorer";
import { CopyButton } from "./client";
import { IconExternal } from "./icons";
import { Mono, Notice } from "./ui";

/**
 * Result of a sponsored UserOp. On the judge testnet the transaction links to the block explorer, so anyone can see
 * it on Base; the mainnet beta shows the hash only (lib/deployment.ts `baseExplorerTx`): opening an explorer from
 * this device would tell it which account you care about.
 */
export function TxResult({ result, what }: { result: SentUserOp; what: string }) {
  const explorer = baseTxUrl(result.transactionHash);
  return (
    <Notice tone={result.success ? "ok" : "danger"} title={result.success ? `${what}: included on Base` : `${what}: reverted on Base`} live>
      <p className="flex flex-wrap items-center gap-2">
        Transaction <Mono>{result.transactionHash}</Mono>
        <CopyButton value={result.transactionHash} />
        {explorer ? (
          <a
            href={explorer}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-[13px] font-semibold text-info no-underline hover:underline"
          >
            View on Basescan
            <IconExternal size={13} />
            <span className="sr-only">(opens a new tab)</span>
          </a>
        ) : null}
      </p>
      <p className="text-xs">
        Looking this transaction up on a public block explorer from this device tells the explorer you are interested in this
        account. Use a different network or device if that matters to you.
      </p>
    </Notice>
  );
}
