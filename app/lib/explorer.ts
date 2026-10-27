/**
 * Where the app links a Base or a Zcash transaction. A live build follows its deployment profile (lib/deployment.ts
 * `baseExplorerTx`, `zcashExplorerTx`: the judge testnet links, the mainnet beta does not); a replay build (DEMO)
 * links its recording's real transactions, and nothing while its recording is the placeholder, whose hashes are dummies.
 */
import { DEPLOYMENT } from "./deployment";
import { REPLAY } from "./replay/flag";
import { recording } from "./replay/recording";

export function baseTxUrl(hash: string): string | null {
  if (REPLAY) {
    const r = recording();
    return r.placeholder ? null : `${r.explorers.base}${hash}`;
  }
  return DEPLOYMENT.baseExplorerTx ? `${DEPLOYMENT.baseExplorerTx}${hash}` : null;
}

/** A Zcash transaction's link, its id in display order (as `zecured-wallet balance` prints it). */
export function zcashTxUrl(txid: string): string | null {
  if (REPLAY) {
    const r = recording();
    return r.placeholder || !r.explorers.zcash ? null : `${r.explorers.zcash}${txid}`;
  }
  return DEPLOYMENT.zcashExplorerTx ? `${DEPLOYMENT.zcashExplorerTx}${txid}` : null;
}

/** DEMO: the replay plays sample data, whose hashes are dummies: the app names no transaction then. */
export function sampleTransactions(): boolean {
  return REPLAY && recording().placeholder;
}
