import type { Metadata } from "next";
import { VerifyView } from "@/components/VerifyView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Verify your vault" };

export default async function VerifyPage({ params }: { params: Promise<{ vaultId: string }> }) {
  const { vaultId } = await params;
  return (
    <Page title="Verify your vault" lead="Don't trust, verify: watch the vault on Zcash with its viewing key.">
      <VerifyView vaultId={vaultId} />
    </Page>
  );
}
