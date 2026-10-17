"use client";

import { useId, useState } from "react";
import { notifyLoansChanged, useLoans } from "@/lib/hooks";
import { backupFileName, exportLoan, loanStore, verifyBackup, type StoredLoan } from "@/lib/keys";
import { CopyButton } from "./client";
import { Button, Notice } from "./ui";

/** Saves a loan backup as a local file (Blob URL: no network). */
export function downloadBackup(loan: StoredLoan): void {
  const blob = new Blob([exportLoan(loan)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = backupFileName();
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * Asks the user to export the per-loan owner key, then to prove the backup works by restoring it
 * (P-8): the saved file, chosen again or pasted, must restore this very loan. `onConfirmed(true)`
 * runs once it does, `onConfirmed(false)` if they explicitly skip.
 */
export function KeyBackup({ loan, onConfirmed }: { loan: StoredLoan; onConfirmed?: (backedUp: boolean) => void }) {
  const fileId = useId();
  const pasteId = useId();
  const [verified, setVerified] = useState(Boolean(loan.backupVerified));
  const [problem, setProblem] = useState<string | null>(null);
  const [paste, setPaste] = useState("");
  const { storageAvailable: storageOk } = useLoans();

  const check = (text: string) => {
    const r = verifyBackup(text, loan);
    if (r.ok) {
      setVerified(true);
      setProblem(null);
      loanStore().update(loan.id, { backedUp: true, backupVerified: true });
      notifyLoansChanged();
    } else {
      setVerified(false);
      setProblem(`Not verified: ${r.reason}.`);
    }
  };

  return (
    <div className="space-y-4">
      <Notice tone="warn" title="This key is the only way to control your loan">
        <p>
          A fresh owner key was created in this browser for this loan. It controls the loan account and signs every
          request. It is stored only in this browser&apos;s local storage and is never sent anywhere. If you clear your
          browser data without a backup, you lose control of the loan and of the ZEC in its vault.
        </p>
        <p>Anyone who gets the backup file can act on the loan. Keep it offline.</p>
      </Notice>
      {!storageOk ? (
        <Notice tone="danger" title="Local storage is unavailable in this browser">
          <p>The key cannot be kept after you close this tab. Download the backup now.</p>
        </Notice>
      ) : null}
      <div className="space-y-2">
        <h3 className="font-semibold">1. Save the backup</h3>
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" onClick={() => downloadBackup(loan)}>
            Download backup (.json)
          </Button>
          <CopyButton value={exportLoan(loan)} label="Copy backup to clipboard" size="md" />
        </div>
      </div>
      <div className="space-y-2">
        <h3 className="font-semibold">2. Check that it restores</h3>
        <p className="text-sm text-muted">
          Choose the file you just saved (or paste what you copied). It is read in this browser only, and must restore this
          very loan: a backup that was never restored is only a hope.
        </p>
        <label htmlFor={fileId} className="sr-only">
          Saved backup file
        </label>
        <input
          id={fileId}
          type="file"
          accept="application/json,.json"
          onChange={async (e) => {
            const f = e.target.files?.[0];
            if (f) check(await f.text());
            e.target.value = "";
          }}
          className="block text-sm text-muted file:mr-3 file:rounded-md file:border file:border-border file:bg-surface-2 file:px-3 file:py-2 file:text-text"
        />
        <label htmlFor={pasteId} className="block text-sm font-medium">
          Or paste it
        </label>
        <textarea
          id={pasteId}
          value={paste}
          onChange={(e) => setPaste(e.target.value)}
          rows={3}
          spellCheck={false}
          autoComplete="off"
          className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-xs text-text focus:border-accent"
        />
        <Button variant="ghost" onClick={() => check(paste)} disabled={!paste.trim()}>
          Check the pasted backup
        </Button>
        {verified ? (
          <Notice tone="ok" title="Backup verified" live>
            <p>The saved backup restores this loan&apos;s key and vault.</p>
          </Notice>
        ) : null}
        {problem ? (
          <Notice tone="danger" title="Backup not verified" live>
            <p>{problem}</p>
          </Notice>
        ) : null}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button onClick={() => onConfirmed?.(true)} disabled={!verified}>
          Continue
        </Button>
        {!verified ? (
          <Button variant="ghost" onClick={() => onConfirmed?.(false)}>
            Skip for now (risky)
          </Button>
        ) : null}
      </div>
    </div>
  );
}
