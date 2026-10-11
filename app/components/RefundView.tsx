"use client";

import Link from "next/link";
import { useState } from "react";
import type { Hex } from "viem";
import { MissingVaultId, useMounted } from "./client";
import { destinationError, IronwoodAddressInput } from "./DestinationForm";
import { PayoutNotice } from "./PayoutNotice";
import { Badge, Button, Card, Mono, Notice } from "./ui";
import type { NonConformingNote } from "@/lib/api";
import { useAction, useLoanByVault, useUrlFragment, useVault } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { requestRefund } from "@/lib/owner";
import { vaultIdFromFragment } from "@/lib/routes";
import { formatTaz } from "@/lib/zcash";

/** The vault comes from the URL fragment (`/refund#0x…`), never the path (M-6). */
export function RefundView() {
  const mounted = useMounted();
  const fragment = useUrlFragment();
  const vaultId = (fragment && vaultIdFromFragment(fragment)) || undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 20_000);

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;
  const notes = vault.data?.nonConforming ?? [];

  return (
    <div className="space-y-6">
      <Notice tone="info" title="Which deposits are refundable">
        <p>
          Any note the vault received that was not attested: a wrong amount, a payment after the expiry height, a deposit
          that crossed pools (transparent, Sapling or Orchard parts), or an extra payment after the first valid one. The
          valid deposit itself is never refunded this way; it leaves the vault only through an exit.
        </p>
      </Notice>
      {mounted && ready && !loan ? (
        <Notice tone="info" title="View only">
          <p>
            Refunds are signed with the loan&apos;s owner key. <Link href="/loans">Import the backup</Link> to request one.
          </p>
        </Notice>
      ) : null}
      {vault.error ? (
        <Notice tone="warn" title="Vault status unavailable" live>
          <p>{vault.error}</p>
        </Notice>
      ) : null}
      {vault.loading ? <p className="text-sm text-muted">Loading the vault…</p> : null}
      {vault.data && notes.length === 0 ? (
        <Card>
          <p className="text-sm text-muted">This vault holds no refundable notes.</p>
        </Card>
      ) : null}
      {notes.map((n) => (
        <RefundCard key={n.noteId} note={n} vaultId={vaultId} loan={loan} />
      ))}
    </div>
  );
}

function RefundCard({ note, vaultId, loan }: { note: NonConformingNote; vaultId: Hex; loan: StoredLoan | null }) {
  const [ua, setUa] = useState("");
  const refund = useAction(async () => {
    if (!loan) throw new Error("No owner key for this vault in this browser");
    const accepted = await requestRefund(loan, vaultId, note.noteId, ua);
    if (!accepted) throw new Error("The signers did not accept the refund request.");
    return accepted;
  });
  return (
    <Card
      title={`Refundable note: ${formatTaz(note.amount)}`}
      action={refund.result ? <Badge tone="ok">Accepted</Badge> : <Badge tone="warn">Not attested</Badge>}
    >
      <p className="mb-4 text-xs text-muted">
        Note id <Mono>{short(note.noteId, 10, 8)}</Mono>. The refund pays the note minus the Zcash network fee.
      </p>
      {refund.result ? (
        <div className="space-y-3">
          <Notice tone="ok" title="Refund accepted" live>
            <p>The signers will pay it to your Ironwood address in the payout queue. It can take a few blocks.</p>
          </Notice>
          <PayoutNotice onBase={false} />
        </div>
      ) : (
        <div className="space-y-3">
          <IronwoodAddressInput value={ua} onChange={setUa} label="Refund to (unified address)" />
          <Button
            onClick={() => refund.run()}
            busy={refund.busy}
            disabled={!loan || !ua.trim() || destinationError(ua) !== null}
          >
            Request refund
          </Button>
          {refund.error ? (
            <Notice tone="danger" title="Refund not accepted" live>
              <p>{refund.error}</p>
            </Notice>
          ) : null}
        </div>
      )}
    </Card>
  );
}
