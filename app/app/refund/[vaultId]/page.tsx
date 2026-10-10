import type { Metadata } from "next";
import { RefundView } from "@/components/RefundView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Refunds" };

export default async function RefundPage({ params }: { params: Promise<{ vaultId: string }> }) {
  const { vaultId } = await params;
  return (
    <Page
      title="Refund a non-conforming deposit"
      lead="Deposits that did not follow the rules are never attested. Ask for them back, to an Ironwood address."
    >
      <RefundView vaultId={vaultId} />
    </Page>
  );
}
