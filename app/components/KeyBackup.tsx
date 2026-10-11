"use client";

import { useId, useState } from "react";
import { notifyLoansChanged, useLoans } from "@/lib/hooks";
import { backupFileName, exportLoan, loanStore, type StoredLoan } from "@/lib/keys";
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
 * Asks the user to export the per-loan owner key. `onConfirmed(true)` runs once they confirm they
 * saved it, `onConfirmed(false)` if they explicitly skip.
 */
export function KeyBackup({ loan, onConfirmed }: { loan: StoredLoan; onConfirmed?: (backedUp: boolean) => void }) {
  const [saved, setSaved] = useState(Boolean(loan.backedUp));
  const checkboxId = useId();
  const { storageAvailable: storageOk } = useLoans();

  const confirm = () => {
    loanStore().update(loan.id, { backedUp: true });
    notifyLoansChanged();
    onConfirmed?.(true);
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
      <div className="flex flex-wrap gap-2">
        <Button variant="secondary" onClick={() => downloadBackup(loan)}>
          Download backup (.json)
        </Button>
        <CopyButton value={exportLoan(loan)} label="Copy backup to clipboard" size="md" />
      </div>
      <div className="flex items-start gap-3">
        <input
          id={checkboxId}
          type="checkbox"
          checked={saved}
          onChange={(e) => setSaved(e.target.checked)}
          className="mt-1 h-4 w-4 accent-[var(--color-accent)]"
        />
        <label htmlFor={checkboxId} className="text-sm text-muted">
          I saved the backup somewhere safe and understand it contains a private key.
        </label>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button onClick={confirm} disabled={!saved}>
          Continue
        </Button>
        {!saved ? (
          <Button variant="ghost" onClick={() => onConfirmed?.(false)}>
            Skip for now (risky)
          </Button>
        ) : null}
      </div>
    </div>
  );
}
