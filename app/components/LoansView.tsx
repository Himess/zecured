"use client";

import Link from "next/link";
import { useId, useState } from "react";
import { useMounted } from "./client";
import { IconArrowRight, IconDownload, IconKey, IconUpload, IconVault } from "./icons";
import { downloadBackup } from "./KeyBackup";
import { TokenAmount } from "./loan";
import { STATUS_LABEL } from "./status";
import { Badge, Button, buttonClass, Card, EmptyState, Mono, Notice } from "./ui";
import { notifyLoansChanged, useLoans, useVault } from "@/lib/hooks";
import { importLoan, loanStore, short, type StoredLoan } from "@/lib/keys";
import { vaultHref } from "@/lib/routes";
import { formatZec } from "@/lib/zcash";

export function LoansView() {
  const mounted = useMounted();
  const { loans, ready, storageAvailable } = useLoans();
  const [confirmWipe, setConfirmWipe] = useState(false);

  if (!mounted || !ready) return <p className="text-sm text-muted">Reading this browser&apos;s storage…</p>;

  return (
    <div className="space-y-6">
      {!storageAvailable ? (
        <Notice tone="danger" title="Local storage is unavailable">
          <p>This browser blocks local storage, so loan keys cannot be kept here. Import a backup each time you need it.</p>
        </Notice>
      ) : null}
      {loans.length === 0 ? (
        <EmptyState
          icon={<IconVault size={22} />}
          title="No loan keys in this browser"
          action={
            <Link href="/borrow" className={buttonClass("primary")}>
              Start a loan
              <IconArrowRight size={16} />
            </Link>
          }
        >
          Each loan has its own key, kept in this browser only. Start a loan, or import a backup below.
        </EmptyState>
      ) : (
        <ul className="space-y-4">
          {loans.map((l) => (
            <LoanRow key={l.id} loan={l} />
          ))}
        </ul>
      )}

      <ImportCard />

      {loans.length > 0 ? (
        <Card title="Forget every loan key on this device">
          <p className="mb-3 text-sm text-muted">
            Removes all owner keys from this browser. Without a backup you lose control of those loans.
          </p>
          {!confirmWipe ? (
            <Button variant="danger" onClick={() => setConfirmWipe(true)}>
              Forget all keys…
            </Button>
          ) : (
            <div className="flex flex-wrap gap-2">
              <Button
                variant="danger"
                onClick={() => {
                  loanStore().wipe();
                  notifyLoansChanged();
                  setConfirmWipe(false);
                }}
              >
                Yes, forget {loans.length} key{loans.length === 1 ? "" : "s"}
              </Button>
              <Button variant="ghost" onClick={() => setConfirmWipe(false)}>
                Cancel
              </Button>
            </div>
          )}
        </Card>
      ) : null}
    </div>
  );
}

function LoanRow({ loan }: { loan: StoredLoan }) {
  // Staggered: the backend should not receive every vault of this browser in one burst (L-11).
  const vault = useVault(loan.vaultId, 0, { staggered: true });
  const [confirmForget, setConfirmForget] = useState(false);
  const status = vault.data ? STATUS_LABEL[vault.data.status] : null;
  return (
    <li className="panel p-5 sm:p-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="space-y-1.5 text-sm">
          <p className="flex flex-wrap items-center gap-2.5">
            <TokenAmount token="TAZ" value={formatZec(BigInt(loan.amount))} size="lg" />
            {status ? (
              <Badge tone={status.tone} dot>
                {status.text}
              </Badge>
            ) : loan.vaultId ? null : (
              <Badge tone="warn">Vault not opened</Badge>
            )}
          </p>
          <p className="text-muted">
            Account <Mono>{short(loan.account)}</Mono>
            {loan.vaultId ? (
              <>
                , vault <Mono>{short(loan.vaultId, 10, 6)}</Mono>
              </>
            ) : null}
          </p>
          <p className="text-xs text-faint">
            Created {new Date(loan.createdAt).toLocaleString()}.{" "}
            {loan.backupVerified ? (
              "Backup verified."
            ) : loan.backedUp ? (
              <span className="text-warn">Backed up, not verified by a restore.</span>
            ) : (
              <span className="text-warn">Not backed up.</span>
            )}
            {loan.demo ? " DEMO: paid by the demo wallet." : null}
          </p>
          {vault.error && loan.vaultId ? <p className="text-xs text-warn">Status unavailable: {vault.error}</p> : null}
        </div>
        <div className="flex flex-wrap gap-2">
          {loan.vaultId ? (
            <Link href={vaultHref("position", loan.vaultId)} prefetch={false} className={buttonClass("primary", undefined, "sm")}>
              Open
              <IconArrowRight size={15} />
            </Link>
          ) : (
            <Link href="/borrow" className={buttonClass("primary", undefined, "sm")}>
              Continue
              <IconArrowRight size={15} />
            </Link>
          )}
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              downloadBackup(loan);
              loanStore().update(loan.id, { backedUp: true });
              notifyLoansChanged();
            }}
          >
            <IconDownload size={15} />
            Export key
          </Button>
          {!confirmForget ? (
            <Button variant="ghost" size="sm" onClick={() => setConfirmForget(true)}>
              Forget…
            </Button>
          ) : (
            <Button
              variant="danger"
              size="sm"
              onClick={() => {
                loanStore().remove(loan.id);
                notifyLoansChanged();
              }}
            >
              Confirm: forget this key
            </Button>
          )}
        </div>
      </div>
    </li>
  );
}

function ImportCard() {
  const textId = useId();
  const fileId = useId();
  const [text, setText] = useState("");
  const [message, setMessage] = useState<{ tone: "ok" | "danger"; text: string } | null>(null);

  const doImport = (raw: string) => {
    try {
      const loan = importLoan(raw);
      const store = loanStore();
      const existed = Boolean(store.get(loan.id));
      if (!store.save(loan)) throw new Error(store.lastError ?? "could not save to local storage");
      notifyLoansChanged();
      setText("");
      setMessage({ tone: "ok", text: `${existed ? "Updated" : "Imported"} the loan for account ${short(loan.account)}.` });
    } catch (e) {
      setMessage({ tone: "danger", text: e instanceof Error ? e.message : "Import failed" });
    }
  };

  return (
    <Card title="Import a loan backup" icon={<IconKey size={18} />}>
      <p className="mb-3 text-sm text-muted">
        Paste the JSON backup or pick the file. It is read locally and never uploaded. The app checks that the key matches
        the loan&apos;s account before saving it.
      </p>
      <div className="space-y-3">
        <label htmlFor={fileId} className="block text-sm font-medium">
          Backup file
        </label>
        <input
          id={fileId}
          type="file"
          accept="application/json,.json"
          onChange={async (e) => {
            const f = e.target.files?.[0];
            if (f) doImport(await f.text());
            e.target.value = "";
          }}
          className="block text-sm text-muted file:mr-3 file:rounded-full file:border-0 file:bg-surface-3 file:px-4 file:py-2 file:font-semibold file:text-text hover:file:bg-line"
        />
        <label htmlFor={textId} className="block text-sm font-medium">
          Or paste it
        </label>
        <textarea
          id={textId}
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={4}
          spellCheck={false}
          autoComplete="off"
          className="field font-mono text-xs"
        />
        <Button variant="secondary" onClick={() => doImport(text)} disabled={!text.trim()}>
          <IconUpload size={16} />
          Import
        </Button>
        {message ? (
          <Notice tone={message.tone} title={message.tone === "ok" ? "Imported" : "Import failed"} live>
            <p>{message.text}</p>
          </Notice>
        ) : null}
      </div>
    </Card>
  );
}
