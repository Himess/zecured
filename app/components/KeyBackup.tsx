"use client";

import { useId, useState, type ReactNode } from "react";
import { notifyLoansChanged, useLoans } from "@/lib/hooks";
import { backupFileName, exportLoan, loanStore, short, verifyBackup, type StoredLoan } from "@/lib/keys";
import { REPLAY } from "@/lib/replay/flag";
import { CopyButton } from "./client";
import { IconArrowRight, IconDownload, IconKey, IconUpload } from "./icons";
import { Button, cx, Disclosure, Notice } from "./ui";

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

/** One numbered row of the backup: its number, what to do, and the controls that do it. */
function Row({ n, done, title, children }: { n: number; done?: boolean; title: string; children: ReactNode }) {
  return (
    <li className="grid gap-3 border-t border-hairline py-4 first:border-t-0 sm:grid-cols-[13rem_minmax(0,1fr)] sm:items-center">
      <p className="flex items-center gap-2.5 font-semibold">
        <span
          aria-hidden="true"
          className={cx(
            "inline-flex h-[22px] min-w-[30px] items-center justify-center rounded-full font-mono text-[11.5px]",
            done ? "bg-surface-3 text-ok" : "text-soft shadow-[inset_0_0_0_1px_var(--color-line)]",
          )}
        >
          0{n}
        </span>
        {title}
      </p>
      <div className="min-w-0">{children}</div>
    </li>
  );
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
    <div className="space-y-5">
      <p className="max-w-2xl text-[15px] text-soft">
        A fresh owner key was made in this browser for this loan: it controls the loan account and signs every request,
        and it is never sent anywhere. Clear this browser&apos;s data without a backup, and you lose the loan and the ZEC in
        its vault. Anyone with the backup can act on the loan: keep it offline.
      </p>
      {REPLAY && !verified ? (
        <Notice tone="demo" title="DEMO: this key is a dummy">
          <p>
            The replay&apos;s key controls nothing, so skip this step. In the live app it is the only key to the loan: you
            save it, then prove it restores.
          </p>
          <Button variant="demo" data-replay="skip-backup" onClick={() => onConfirmed?.(false)}>
            Skip the backup (demo key)
            <IconArrowRight size={17} />
          </Button>
        </Notice>
      ) : null}
      {!storageOk ? (
        <Notice tone="danger" title="Local storage is unavailable in this browser">
          <p>The key cannot be kept after you close this tab. Download the backup now.</p>
        </Notice>
      ) : null}

      {/* The key itself, as the thing being saved. */}
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2 rounded-2xl bg-bg/70 px-4 py-3.5 shadow-[inset_0_0_0_1px_var(--color-border)]">
        <span className="flex items-center gap-2.5 font-semibold">
          <span className="inline-flex size-8 items-center justify-center rounded-xl bg-surface-3 text-soft">
            <IconKey size={17} />
          </span>
          Owner key
        </span>
        <span aria-hidden="true" className="font-mono text-[13px] tracking-[0.2em] text-muted">
          ••••••••••••••
        </span>
        <span className="text-[13px] text-muted">
          account <span className="font-mono text-soft">{short(loan.account)}</span>
        </span>
        <span className="text-[12.5px] text-faint sm:ml-auto">only in this browser</span>
      </div>

      <ol>
        <Row n={1} title="Save it">
          <div className="flex flex-wrap gap-2">
            <Button variant="secondary" onClick={() => downloadBackup(loan)}>
              <IconDownload size={16} />
              Download backup (.json)
            </Button>
            <CopyButton value={exportLoan(loan)} label="Copy" size="md" />
          </div>
        </Row>
        <Row n={2} done={verified} title="Prove it restores">
          <div className="space-y-2">
            <label htmlFor={fileId} className="sr-only">
              The saved backup file
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
              className="block text-sm text-muted file:mr-3 file:min-h-11 file:rounded-full file:border-0 file:bg-surface-2 file:px-5 file:font-semibold file:text-text file:shadow-[inset_0_0_0_1px_var(--color-control)] hover:file:bg-surface-3"
            />
            <Disclosure summary="Or paste it instead">
              <label htmlFor={pasteId} className="sr-only">
                The backup you copied
              </label>
              <textarea
                id={pasteId}
                value={paste}
                onChange={(e) => setPaste(e.target.value)}
                rows={3}
                spellCheck={false}
                autoComplete="off"
                className="field font-mono text-xs"
              />
              <Button variant="secondary" size="sm" onClick={() => check(paste)} disabled={!paste.trim()}>
                <IconUpload size={15} />
                Check the pasted backup
              </Button>
            </Disclosure>
            <p className="text-[12.5px] text-muted">Read in this browser only. It must restore this very loan: a backup never restored is only a hope.</p>
          </div>
        </Row>
      </ol>
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
      <div className="flex flex-wrap items-center gap-2 border-t border-hairline pt-5">
        <Button size="lg" onClick={() => onConfirmed?.(true)} disabled={!verified}>
          Continue to the deposit
          <IconArrowRight size={18} />
        </Button>
        {!verified && !REPLAY ? (
          <Button variant="ghost" onClick={() => onConfirmed?.(false)}>
            Skip for now (risky)
          </Button>
        ) : null}
      </div>
    </div>
  );
}
