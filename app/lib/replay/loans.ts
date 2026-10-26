/**
 * The replay's loan in this browser (DEMO). The recorded loan's owner key is never published, so
 * the replay's loan carries a dummy key (lib/replay/demo-keys.ts) with the recorded owner and
 * account: lib/keys.ts accepts that pair, in a replay build only, in place of deriving the owner
 * and its Safe from the key. The replay keeps the app's storage in this tab's sessionStorage,
 * under `zecured:replay:` keys, never in localStorage: closing the tab forgets it.
 */
import { getAddress, isAddress, type Address, type Hex } from "viem";
import type { KeyValueStorage, StoredLoan } from "../keys";
import { REPLAY_OWNER_KEY } from "./demo-keys";
import { recording } from "./recording";
import { REPLAY_STORAGE_PREFIX, sessionStore } from "./world";

/** `createLoan` in a replay build: the recorded loan, with the dummy key. */
export function createReplayLoan(amount: bigint, now: number): StoredLoan {
  const l = recording().loan;
  if (amount !== BigInt(l.amount)) throw new Error("This replay opens only the recorded run's vault size.");
  return {
    v: 1,
    id: l.requestId,
    ownerKey: REPLAY_OWNER_KEY,
    owner: getAddress(l.owner),
    account: getAddress(l.account),
    amount: l.amount,
    requestId: l.requestId,
    createdAt: now,
  };
}

/** `parseLoan`'s owner and account in a replay build: only the replay's own loan is accepted. */
export function replayLoanIdentity(ownerKey: Hex, raw: Record<string, unknown>): { owner: Address; account: Address } {
  const l = recording().loan;
  if (ownerKey.toLowerCase() !== REPLAY_OWNER_KEY.toLowerCase()) {
    throw new Error("this replay holds only its own demo loan; a real loan's backup belongs in the live app");
  }
  const owner = getAddress(l.owner);
  const account = getAddress(l.account);
  if (typeof raw.owner === "string" && isAddress(raw.owner, { strict: false }) && getAddress(raw.owner) !== owner) {
    throw new Error("owner address does not match the replay's loan");
  }
  if (typeof raw.account === "string" && isAddress(raw.account, { strict: false }) && getAddress(raw.account) !== account) {
    throw new Error("account address does not match the replay's loan");
  }
  return { owner, account };
}

/** `browserStorage()` in a replay build: sessionStorage, with every `zecured:` key moved under `zecured:replay:`. */
export function replayStorage(): KeyValueStorage | null {
  const s = sessionStore();
  if (!s) return null;
  try {
    const probe = `${REPLAY_STORAGE_PREFIX}probe`;
    s.setItem(probe, "1");
    s.removeItem(probe);
  } catch {
    return null;
  }
  const key = (k: string) => `${REPLAY_STORAGE_PREFIX}${k.startsWith("zecured:") ? k.slice("zecured:".length) : k}`;
  return {
    getItem: (k) => s.getItem(key(k)),
    setItem: (k, v) => s.setItem(key(k), v),
    removeItem: (k) => s.removeItem(key(k)),
  };
}
