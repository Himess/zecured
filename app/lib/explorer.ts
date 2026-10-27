/**
 * Where the app links a Base or a Zcash transaction or address. A live build follows its deployment profile
 * (lib/deployment.ts `baseExplorerTx`, `zcashExplorerTx`: the judge testnet links, the mainnet beta does not); a replay
 * build (DEMO) links its recording's real transactions, and nothing while its recording is the placeholder, whose
 * hashes are dummies. An explorer's address page sits beside its `/tx/` page.
 */
import { DEPLOYMENT } from "./deployment";
import { REPLAY } from "./replay/flag";
import { recording } from "./replay/recording";

/** The explorers' `/tx/` prefixes this build links to, or null where it links none. */
function txPrefixes(): { base: string | null; zcash: string | null } {
  if (REPLAY) {
    const r = recording();
    return r.placeholder ? { base: null, zcash: null } : { base: r.explorers.base, zcash: r.explorers.zcash };
  }
  return { base: DEPLOYMENT.baseExplorerTx, zcash: DEPLOYMENT.zcashExplorerTx };
}

function addressPrefix(txPrefix: string | null): string | null {
  return txPrefix && /\/tx\/$/.test(txPrefix) ? txPrefix.replace(/\/tx\/$/, "/address/") : null;
}

export function baseTxUrl(hash: string): string | null {
  const p = txPrefixes().base;
  return p ? `${p}${hash}` : null;
}

/** An account's or a contract's page on the Base explorer. */
export function baseAddressUrl(address: string): string | null {
  const p = addressPrefix(txPrefixes().base);
  return p ? `${p}${address}` : null;
}

/** A Zcash transaction's link, its id in display order (as `zecured-wallet balance` prints it). */
export function zcashTxUrl(txid: string): string | null {
  const p = txPrefixes().zcash;
  return p ? `${p}${txid}` : null;
}

/** A unified address's page on the Zcash explorer: its receivers, never a shielded balance or history. */
export function zcashAddressUrl(address: string): string | null {
  const p = addressPrefix(txPrefixes().zcash);
  return p ? `${p}${address}` : null;
}

/** A commit of this repository on GitHub. */
export function commitUrl(sha: string): string | null {
  return /^[0-9a-f]{7,40}$/i.test(sha) ? `https://github.com/Himess/zecured/commit/${sha}` : null;
}

/** DEMO: the replay plays sample data, whose hashes are dummies: the app names no transaction then. */
export function sampleTransactions(): boolean {
  return REPLAY && recording().placeholder;
}
