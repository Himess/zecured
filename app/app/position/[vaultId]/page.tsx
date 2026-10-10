import type { Metadata } from "next";
import { PositionView } from "@/components/position/PositionView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Position" };

export default async function PositionPage({ params }: { params: Promise<{ vaultId: string }> }) {
  const { vaultId } = await params;
  return (
    <Page
      title="Your loan"
      lead="Collateral, debt and health, read from Base through our proxy, and the ways out: close from collateral, repay, or release."
    >
      <PositionView vaultId={vaultId} />
    </Page>
  );
}
