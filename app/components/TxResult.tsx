import type { SentUserOp } from "@/lib/account";
import { CopyButton } from "./client";
import { Mono, Notice } from "./ui";

/**
 * Result of a sponsored UserOp. Deliberately no block-explorer link: opening an explorer from
 * this device would tell it which account you care about.
 */
export function TxResult({ result, what }: { result: SentUserOp; what: string }) {
  return (
    <Notice tone={result.success ? "ok" : "danger"} title={result.success ? `${what}: included on Base` : `${what}: reverted on Base`} live>
      <p className="flex flex-wrap items-center gap-2">
        Transaction <Mono>{result.transactionHash}</Mono>
        <CopyButton value={result.transactionHash} />
      </p>
      <p className="text-xs">
        Looking this transaction up on a public block explorer from this device would tell the explorer you are interested
        in this account. Use a different network or device if that matters to you.
      </p>
    </Notice>
  );
}
