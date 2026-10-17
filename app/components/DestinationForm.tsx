"use client";

import { useId, useState } from "react";
import type { Hex } from "viem";
import { prepareDestination, UnifiedAddressError } from "@/lib/commitment";
import { DEPLOYMENT } from "@/lib/deployment";
import { notifyLoansChanged, useAction } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import { submitDestination } from "@/lib/owner";
import { Button, Mono, Notice } from "./ui";

/** Validates a destination as the user types; returns an error message or null. */
export function destinationError(ua: string): string | null {
  if (!ua.trim()) return null;
  try {
    prepareDestination(ua, DEPLOYMENT.zcashNetwork);
    return null;
  } catch (e) {
    return e instanceof UnifiedAddressError ? `Not usable: ${e.message}.` : "Not a valid unified address.";
  }
}

/** The address input shared by every exit and by refunds. */
export function IronwoodAddressInput({
  value,
  onChange,
  label = "Ironwood destination (unified address)",
  hint = "Use a fresh address from an Ironwood-capable wallet. Only its Ironwood (Orchard-protocol) receiver is used.",
}: {
  value: string;
  onChange: (v: string) => void;
  label?: string;
  hint?: string;
}) {
  const id = useId();
  const error = destinationError(value);
  let other = false;
  if (value.trim() && !error) {
    try {
      other = prepareDestination(value, DEPLOYMENT.zcashNetwork).hasOtherReceivers;
    } catch {
      other = false;
    }
  }
  return (
    <div className="space-y-2">
      <label htmlFor={id} className="block text-sm font-medium">
        {label}
      </label>
      <textarea
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        rows={3}
        spellCheck={false}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        placeholder={DEPLOYMENT.zcashNetwork === "test" ? "utest1…" : "u1…"}
        aria-invalid={error ? true : undefined}
        aria-describedby={`${id}-hint`}
        className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-sm break-all text-text placeholder:text-faint focus:border-accent"
      />
      <p id={`${id}-hint`} className={error ? "text-sm text-danger" : "text-xs text-muted"} aria-live="polite">
        {error ??
          (other ? "Valid. This address has other receivers too; only its Ironwood (Orchard-protocol) receiver will be paid." : hint)}
      </p>
    </div>
  );
}

/**
 * Exit step 1: ask where the ZEC goes, salt it, sign `Destination` and have the signers
 * pre-validate it (POST /vaults/:id/destination) before any Base transaction burns zZEC.
 */
export function DestinationStep({
  loan,
  vaultId,
  onDone,
}: {
  loan: StoredLoan;
  vaultId: Hex;
  onDone: (commitment: Hex) => void;
}) {
  const [ua, setUa] = useState("");
  const submit = useAction(async () => {
    const { commitment } = await submitDestination(loan, vaultId, ua);
    loanStore().update(loan.id, { destinationCommitment: commitment });
    notifyLoansChanged();
    return commitment;
  });

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">
        Before anything happens on Base, the signers check and record where your ZEC will go. Base only ever sees a salted
        commitment to this address; the salt stays with you and the signers.
      </p>
      {loan.destinationCommitment ? (
        <Notice tone="info" title="You already validated a destination for this loan">
          <p>
            Commitment <Mono>{short(loan.destinationCommitment, 10, 8)}</Mono>. Reuse it if an earlier attempt was
            interrupted, or enter a new address below.
          </p>
          <Button variant="secondary" onClick={() => onDone(loan.destinationCommitment!)}>
            Use the validated destination
          </Button>
        </Notice>
      ) : null}
      <IronwoodAddressInput value={ua} onChange={setUa} />
      <Button
        onClick={async () => {
          const c = await submit.run();
          if (c) onDone(c);
        }}
        busy={submit.busy}
        disabled={!ua.trim() || destinationError(ua) !== null}
      >
        Validate destination with the signers
      </Button>
      {submit.error ? (
        <Notice tone="danger" title="The destination was not accepted" live>
          <p>{submit.error}</p>
        </Notice>
      ) : null}
    </div>
  );
}
