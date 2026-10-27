"use client";

import { useId, useState } from "react";
import type { Hex } from "viem";
import { prepareDestination, UnifiedAddressError } from "@/lib/commitment";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { notifyLoansChanged, useAction, useDemoStatus } from "@/lib/hooks";
import { loanStore, short, type StoredLoan } from "@/lib/keys";
import { submitDestination } from "@/lib/owner";
import { ActionError } from "./ActionError";
import { Badge, Button, Mono, Notice } from "./ui";

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
  hint = "Use a fresh address from an Ironwood-capable wallet. Only its Ironwood receiver is used.",
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
      <label htmlFor={id} className="block text-[14px] font-semibold text-soft">
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
        className="field font-mono text-sm break-all"
      />
      <p id={`${id}-hint`} className={error ? "text-sm text-danger" : "text-xs text-muted"}>
        {error ??
          (other ? "Valid. This address has other receivers too; only its Ironwood receiver will be paid." : hint)}
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
  const submit = useAction(async (address: string) => {
    const { commitment } = await submitDestination(loan, vaultId, address);
    loanStore().update(loan.id, { destinationCommitment: commitment });
    notifyLoansChanged();
    return commitment;
  });
  // P-1 (DEMO): a demo vault's ZEC may go back to the demo wallet when the user has no address of their own.
  const demo = useDemoStatus();
  const demoReturn = loan.demo && config.demoPath && demo.data?.enabled ? demo.data.returnAddress : null;

  const demoBox = demoReturn ? (
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-dashed border-demo/45 bg-demo/[0.04] p-4">
      <div className="min-w-0 flex-1 basis-[16rem] text-sm">
        <p className="flex flex-wrap items-center gap-2 font-semibold">
          <Badge tone="demo">DEMO</Badge> No Ironwood address of your own?
        </p>
        <p className="mt-1 text-muted">The demo wallet funded this vault: your ZEC can go back to it.</p>
      </div>
      <Button
        data-replay="demo-destination"
        onClick={async () => {
          const c = await submit.run(demoReturn);
          if (c) onDone(c);
        }}
        busy={submit.busy}
      >
        Send my ZEC back to the demo wallet
      </Button>
    </div>
  ) : null;

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">
        Before anything happens on Base, the signers check and record where your ZEC will go. Base only ever sees a salted
        commitment to this address; the salt stays with you and the signers.
      </p>
      {demoBox}
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
      <IronwoodAddressInput
        value={ua}
        onChange={setUa}
        label={demoReturn ? "Or a fresh Ironwood address of your own (unified address)" : "Ironwood destination (unified address)"}
      />
      <Button
        variant={demoReturn ? "secondary" : "primary"}
        onClick={async () => {
          const c = await submit.run(ua);
          if (c) onDone(c);
        }}
        busy={submit.busy}
        disabled={!ua.trim() || destinationError(ua) !== null}
      >
        Check this address with the signers
      </Button>
      <ActionError error={submit.error} refusal={submit.refusal} title="The destination was not accepted" />
    </div>
  );
}
