"use client";

import Link from "next/link";
import { useState } from "react";
import { isHex, type Hex } from "viem";
import { CopyButton, useMounted } from "./client";
import { Button, Card, Notice } from "./ui";
import { useAction, useLoanByVault, useVault } from "@/lib/hooks";
import { fetchViewingKey } from "@/lib/owner";

/** Blocks before the deposit deadline to start scanning from (well before the vault existed). */
const SCAN_MARGIN_BLOCKS = 1_000;

export function VerifyView({ vaultId: raw }: { vaultId: string }) {
  const mounted = useMounted();
  const valid = isHex(raw) && raw.length === 66;
  const vaultId = valid ? (raw.toLowerCase() as Hex) : undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 0);
  const [ufvk, setUfvk] = useState<string | null>(null);
  const request = useAction(async () => {
    if (!loan || !vaultId) throw new Error("No owner key for this vault in this browser");
    const key = await fetchViewingKey(loan, vaultId);
    setUfvk(key);
    return true;
  });

  if (!valid) {
    return (
      <Notice tone="danger" title="Not a vault id">
        <p>A vault id is 0x followed by 64 hexadecimal characters.</p>
      </Notice>
    );
  }
  const expiry = vault.data?.expiryHeight ?? loan?.expiryHeight;
  const from = expiry !== undefined ? Math.max(0, expiry - SCAN_MARGIN_BLOCKS) : undefined;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="min-w-0 space-y-6">
        <Card title="Your vault's viewing key">
          <p className="mb-4 text-sm text-muted">
            The signers give the vault&apos;s unified full viewing key (UFVK) to its owner only: your request is signed with
            this loan&apos;s owner key. With it you can watch the vault on Zcash yourself, without trusting us.
          </p>
          {mounted && ready && !loan ? (
            <Notice tone="info" title="No owner key for this vault in this browser">
              <p>
                <Link href="/loans">Import the loan backup</Link> to request the viewing key.
              </p>
            </Notice>
          ) : null}
          {!ufvk ? (
            <Button onClick={() => request.run()} busy={request.busy} disabled={!loan}>
              Request the viewing key
            </Button>
          ) : (
            <div className="space-y-3">
              <label htmlFor="ufvk" className="block text-sm font-medium">
                Unified full viewing key
              </label>
              <textarea
                id="ufvk"
                readOnly
                value={ufvk}
                rows={5}
                spellCheck={false}
                className="w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-xs break-all text-text"
              />
              <div className="flex flex-wrap gap-2">
                <CopyButton value={ufvk} label="Copy viewing key" size="md" />
                <Button variant="ghost" onClick={() => setUfvk(null)}>
                  Hide
                </Button>
              </div>
            </div>
          )}
          {request.error ? (
            <div className="mt-4">
              <Notice tone="danger" title="The signers did not return the key" live>
                <p>{request.error}</p>
              </Notice>
            </div>
          ) : null}
        </Card>

        <Card title="How to verify">
          <ol className="list-decimal space-y-3 pl-5 text-sm text-muted">
            <li>
              With the Zecured CLI wallet in this repository (it verifies the note commitment tree against the server):
              <pre className="mt-2 overflow-x-auto rounded-lg border border-border bg-bg p-3 text-xs text-text">
                {`zecured-wallet watch --ufvk <UFVK> --from ${from ?? "<height before you opened the vault>"}`}
              </pre>
            </li>
            <li>Or import the key as a view-only account in an Ironwood-capable wallet that accepts a UFVK.</li>
            <li>
              You should see your deposit note of exactly the vault&apos;s amount, any non-conforming notes (refundable), and
              later the vault&apos;s spends when you exit.
            </li>
          </ol>
        </Card>
      </div>
      <aside className="min-w-0 space-y-4">
        <Notice tone="warn" title="Keep it private">
          <p>
            Anyone with this key sees the vault&apos;s full history. It is shown once and never stored by the app. Do not
            paste it into websites.
          </p>
        </Notice>
        <Notice tone="info" title="What it does not show">
          <p>
            Payouts to third parties (a liquidator or a market maker) are encrypted without an outgoing viewing key, so the
            vault key cannot reveal where their ZEC went. Your own payouts remain visible to you.
          </p>
        </Notice>
      </aside>
    </div>
  );
}
