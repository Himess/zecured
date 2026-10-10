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

// ------------------------------------------------------------------ polling

function usePolling(fn: () => Promise<void>, intervalMs: number, enabled: boolean) {
  const fnRef = useRef(fn);
  fnRef.current = fn;
  useEffect(() => {
    if (!enabled) return;
    let stop = false;
    let first = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      // The first load always runs (a page opened in a background tab must still load);
      // later polls pause while the tab is hidden and resume on visibilitychange.
      if (first || typeof document === "undefined" || document.visibilityState !== "hidden") {
        first = false;
        await fnRef.current();
      }
      if (!stop && intervalMs > 0) timer = setTimeout(tick, intervalMs);
    };
    void tick();
    const onVisible = () => {
      if (document.visibilityState === "visible") void fnRef.current();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stop = true;
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [intervalMs, enabled]);
}

export interface Remote<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

/** GET /vaults/:id, polled. */
export function useVault(vaultId: Hex | undefined, intervalMs = 10_000): Remote<Vault> {
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
  usePolling(refresh, intervalMs, Boolean(vaultId));
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
