/**
 * React hooks shared by the client components. Polling pauses while the tab is hidden and
 * only ever talks to our backend (API and RPC proxy).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import type { Address, Hex } from "viem";
import { api, describeError, type Vault } from "./api";
import { readPosition, type PositionSnapshot } from "./chain";
import { config } from "./config";
import { loanStore, type StoredLoan } from "./keys";
import { storedVault, verifyVaultOpened, type VaultCheck } from "./signers";

// ------------------------------------------------------------------ local loans

const listeners = new Set<() => void>();

/** Call after writing to the loan store so every mounted view re-reads it. */
export function notifyLoansChanged(): void {
  for (const l of listeners) l();
}

export interface LoansState {
  loans: StoredLoan[];
  /** False until the first client-side read (localStorage is not available on the server). */
  ready: boolean;
  /** False when localStorage is unavailable (privacy mode, blocked storage). */
  storageAvailable: boolean;
}

export function useLoans(): LoansState {
  const [state, setState] = useState<LoansState>({ loans: [], ready: false, storageAvailable: true });
  useEffect(() => {
    const read = () => {
      const store = loanStore();
      setState({ loans: store.list(), ready: true, storageAvailable: store.available });
    };
    read();
    listeners.add(read);
    const onStorage = (e: StorageEvent) => {
      if (e.key === null || e.key.startsWith("zecured:")) read();
    };
    window.addEventListener("storage", onStorage);
    return () => {
      listeners.delete(read);
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return state;
}

export function useLoanByVault(vaultId: string | undefined): { loan: StoredLoan | null; ready: boolean } {
  const { loans, ready } = useLoans();
  const v = vaultId?.toLowerCase();
  return { loan: (v && loans.find((l) => l.vaultId?.toLowerCase() === v)) || null, ready };
}

// ------------------------------------------------------------------ the signers' word on a vault

export type VaultProofState =
  /** The loan has no vault yet. */
  | { state: "none" }
  | { state: "checking" }
  /** Every signer vouches for the stored address and tag key hash (C-4). */
  | { state: "vouched"; check: VaultCheck }
  /** Opened before vaults were signed, or under `VaultOpened` v1: its address cannot be checked. */
  | { state: "legacy" }
  /** The signatures do not check out, or this build cannot check them: never pay it. */
  | { state: "refused"; message: string };

/**
 * Checks a stored loan's vault against the signers' `VaultOpened` signatures, again on every
 * change: a stored or imported record is not trusted on its own (lib/signers.ts).
 */
export function useVaultProof(loan: StoredLoan | null): VaultProofState {
  const [state, setState] = useState<VaultProofState>({ state: "checking" });
  const loanRef = useRef(loan);
  loanRef.current = loan;
  const key = loan
    ? [
        loan.vaultId,
        loan.vaultAddress,
        loan.expiryHeight,
        loan.tagKeyHash,
        loan.amount,
        loan.owner,
        loan.account,
        ...(loan.vaultSignatures ?? []).map((s) => `${s.index}:${s.signature}`),
      ].join("|")
    : "";
  useEffect(() => {
    const current = loanRef.current;
    if (!current?.vaultId || !current.vaultAddress) {
      setState({ state: "none" });
      return;
    }
    const stored = storedVault(current);
    if (!stored) {
      setState({ state: "legacy" });
      return;
    }
    let live = true;
    setState({ state: "checking" });
    verifyVaultOpened(stored.fields, stored.signatures).then(
      (check) => {
        if (live) setState({ state: "vouched", check });
      },
      (e: unknown) => {
        if (live) setState({ state: "refused", message: describeError(e) });
      },
    );
    return () => {
      live = false;
    };
  }, [key]);
  return state;
}

// ------------------------------------------------------------------ URL fragment

/**
 * The URL fragment without its "#" ("" when there is none); undefined until mounted, since the
 * server never sees it. Vault pages read their vault id here (lib/routes.ts, M-6). Follows
 * hashchange and popstate; in-app links to a vault page always change the path, so they mount
 * the page afresh.
 */
export function useUrlFragment(): string | undefined {
  const [fragment, setFragment] = useState<string | undefined>(undefined);
  useEffect(() => {
    const read = () => setFragment(window.location.hash.replace(/^#/, ""));
    read();
    window.addEventListener("hashchange", read);
    window.addEventListener("popstate", read);
    return () => {
      window.removeEventListener("hashchange", read);
      window.removeEventListener("popstate", read);
    };
  }, []);
  return fragment;
}

// ------------------------------------------------------------------ polling

/**
 * Spaces out requests that would otherwise leave together, such as one GET /vaults/:id per
 * loan this browser holds (security review L-11): each call returns how long to wait so that
 * its request goes out at least `minGapMs` after the previous one, plus a random jitter. The
 * backend then does not get a browser's vault ids as one burst. (It still sees one IP for all
 * of them; this only removes the trivial link.)
 */
export function createStagger(minGapMs = 1_500, jitterMs = 2_500, now = () => Date.now(), random = Math.random) {
  let next = 0;
  return (): number => {
    const t = now();
    const at = Math.max(t, next) + Math.floor(random() * jitterMs);
    next = at + minGapMs;
    return at - t;
  };
}

const staggerDelay = createStagger();

function usePolling(fn: () => Promise<void>, intervalMs: number, enabled: boolean, staggered = false) {
  const fnRef = useRef(fn);
  fnRef.current = fn;
  useEffect(() => {
    if (!enabled) return;
    let stop = false;
    let first = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let visibleTimer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      // The first load always runs (a page opened in a background tab must still load);
      // later polls pause while the tab is hidden and resume on visibilitychange.
      if (first || typeof document === "undefined" || document.visibilityState !== "hidden") {
        first = false;
        await fnRef.current();
      }
      if (!stop && intervalMs > 0) timer = setTimeout(tick, staggered ? intervalMs + staggerDelay() : intervalMs);
    };
    if (staggered) timer = setTimeout(tick, staggerDelay());
    else void tick();
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      if (!staggered) {
        void fnRef.current();
        return;
      }
      clearTimeout(visibleTimer);
      visibleTimer = setTimeout(() => void fnRef.current(), staggerDelay());
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stop = true;
      clearTimeout(timer);
      clearTimeout(visibleTimer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [intervalMs, enabled, staggered]);
}

export interface Remote<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

/**
 * GET /vaults/:id, polled. `staggered`: for lists of this browser's vaults, each request waits
 * for its own randomized slot (see `createStagger`) instead of leaving with the others.
 */
export function useVault(vaultId: Hex | undefined, intervalMs = 10_000, { staggered = false } = {}): Remote<Vault> {
  const [data, setData] = useState<Vault | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(Boolean(vaultId));
  const refresh = useCallback(async () => {
    if (!vaultId) return;
    try {
      const v = await api.getVault(vaultId);
      setData(v);
      setError(null);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setLoading(false);
    }
  }, [vaultId]);
  usePolling(refresh, intervalMs, Boolean(vaultId), staggered);
  return { data, error, loading, refresh };
}

/** Morpho position + market + oracle through the RPC proxy, polled. */
export function usePosition(account: Address | undefined, intervalMs = 30_000): Remote<PositionSnapshot> {
  const enabled = Boolean(account) && config.contractsDeployed;
  const [data, setData] = useState<PositionSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(enabled);
  const refresh = useCallback(async () => {
    if (!account || !config.contractsDeployed) return;
    try {
      setData(await readPosition(account));
      setError(null);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setLoading(false);
    }
  }, [account]);
  usePolling(refresh, intervalMs, enabled);
  return { data, error, loading, refresh };
}

// ------------------------------------------------------------------ actions

export interface Action<A extends unknown[], R> {
  run: (...args: A) => Promise<R | undefined>;
  busy: boolean;
  error: string | null;
  result: R | undefined;
  reset: () => void;
}

/** Wraps an async user action: busy flag, error message, last result; never throws. */
export function useAction<A extends unknown[], R>(fn: (...args: A) => Promise<R>): Action<A, R> {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<R | undefined>(undefined);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const run = useCallback(async (...args: A) => {
    setBusy(true);
    setError(null);
    try {
      const r = await fnRef.current(...args);
      setResult(r);
      return r;
    } catch (e) {
      setError(describeError(e));
      return undefined;
    } finally {
      setBusy(false);
    }
  }, []);
  const reset = useCallback(() => {
    setError(null);
    setResult(undefined);
  }, []);
  return { run, busy, error, result, reset };
}
