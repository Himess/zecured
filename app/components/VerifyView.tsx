"use client";

import Link from "next/link";
import { useState } from "react";
import { ActionError } from "./ActionError";
import { CopyButton, MissingVaultId, useMounted } from "./client";
import { Button, Card, Notice } from "./ui";
import type { Vault } from "@/lib/api";
import { config } from "@/lib/config";
import { useAction, useLoanByVault, useUrlFragment, useVault } from "@/lib/hooks";
import { fetchViewingKey, type ViewingKey } from "@/lib/owner";
import { vaultIdFromFragment } from "@/lib/routes";

/**
 * Where a scan of the vault starts: its birthday (the Zcash tip when it was opened), from
 * GET /vaults/:id, or derived from the deposit deadline (expiry = birthday + vaultExpiryBlocks).
 */
function scanStart(vault: Vault | null, expiry: number | undefined): number | undefined {
  if (vault?.birthdayHeight !== undefined) return vault.birthdayHeight;
  if (expiry !== undefined && config.vaultExpiryBlocks !== null) return Math.max(0, expiry - config.vaultExpiryBlocks);
  return undefined;
}

/** The vault comes from the URL fragment (`/verify#0x…`), never the path (M-6). */
export function VerifyView() {
  const mounted = useMounted();
  const fragment = useUrlFragment();
  const vaultId = (fragment && vaultIdFromFragment(fragment)) || undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 0);
  const [key, setKey] = useState<ViewingKey | null>(null);
  const request = useAction(async () => {
    if (!loan || !vaultId) throw new Error("No owner key for this vault in this browser");
    // Rejected unless it is the key every signer signed for when the vault opened (C-4).
    setKey(await fetchViewingKey(loan, vaultId));
    return true;
  });

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;
  const expiry = vault.data?.expiryHeight ?? loan?.expiryHeight;
  const from = scanStart(vault.data, expiry);
  const command = `zecured-wallet watch --ufvk-file ufvk.txt --from ${from ?? "<birthday height>"} --tier3`;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
      <div className="min-w-0 space-y-6">
        <Card title="Your vault's viewing key">
          <p className="mb-4 text-sm text-muted">
            The signers give the vault&apos;s unified full viewing key (UFVK) to its owner only: your request is signed with
            this loan&apos;s owner key. With it you can watch the vault on Zcash yourself, without trusting us. It shows this
            vault&apos;s notes and spends only, nothing about your own wallet.
          </p>
          {mounted && ready && !loan ? (
            <Notice tone="info" title="No owner key for this vault in this browser">
              <p>
                <Link href="/loans">Import the loan backup</Link> to request the viewing key.
              </p>
            </Notice>
          ) : null}
          {!key ? (
            <Button onClick={() => request.run()} busy={request.busy} disabled={!loan}>
              Request the viewing key
            </Button>
          ) : (
            <div className="space-y-3">
              {key.checked ? (
                <Notice tone="ok" title="Signed by all 5 signers, checked in this browser">
                  <p>This is the viewing key every signer signed for when the vault was opened.</p>
                </Notice>
              ) : (
                <Notice tone="warn" title="This key could not be checked">
                  <p>
                    This loan was opened before the signers signed vault keys, so nothing here shows that this is the key of
                    the vault they control.
                  </p>
                </Notice>
              )}
              <label htmlFor="ufvk" className="block text-sm font-medium">
                Unified full viewing key
              </label>
              <textarea
                id="ufvk"
                readOnly
                value={key.ufvk}
                rows={5}
                spellCheck={false}
                className="field font-mono text-xs break-all"
              />
              <div className="flex flex-wrap gap-2">
                <CopyButton value={key.ufvk} label="Copy viewing key" size="md" />
                <Button variant="ghost" onClick={() => setKey(null)}>
                  Hide
                </Button>
              </div>
            </div>
          )}
          {request.error ? (
            <div className="mt-4">
              <ActionError error={request.error} refusal={request.refusal} title="No viewing key shown" />
            </div>
          ) : null}
        </Card>

        <Card title="How to verify">
          <ol className="list-decimal space-y-3 pl-5 text-sm text-muted">
            <li>
              With the Zecured CLI wallet in this repository (it verifies the note commitment tree against the server):
              <pre className="mt-2 overflow-x-auto rounded-lg border border-border bg-bg p-3 text-xs text-text">{command}</pre>
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <CopyButton value={command} label="Copy command" />
              </div>
              <p className="mt-2">
                {from !== undefined
                  ? `--from ${from} is the vault's birthday: the Zcash height when it was opened, so nothing earlier concerns it.`
                  : "--from is the vault's birthday: the Zcash height when it was opened."}{" "}
                Save the key as ufvk.txt where you run the command, and delete the file afterwards.
              </p>
            </li>
            <li>Or import the key as a view-only account in an Ironwood-capable wallet that accepts a UFVK.</li>
            <li>
              You should see your deposit note of exactly the vault&apos;s amount, any other payment to the vault (it comes back
              only as a refund proven on Base), and later the vault&apos;s spends when you exit.
            </li>
          </ol>
        </Card>
      </div>
      <aside className="min-w-0 space-y-4">
        <Notice tone="warn" title="Treat it as sensitive">
          <p>
            Anyone holding this key sees the vault&apos;s whole history: the deposit, the balance and your payouts. It is
            shown once and never stored by the app. Do not paste it into websites or share it.
          </p>
        </Notice>
        <Notice tone="info" title="What it does not show">
          <p>
            Payouts to third parties (a liquidator or a market maker) are created without an outgoing viewing key, so the
            vault key shows that ZEC left the vault but not where it went. Your own payouts remain visible to you. Nothing
            about your wallet or other vaults is in it.
          </p>
        </Notice>
      </aside>
    </div>
  );
}
