import type { Metadata } from "next";
import { PositionView } from "@/components/position/PositionView";
import { IconVault } from "@/components/icons";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Position" };

/** `/position#<vaultId>`: the vault id stays in the fragment, out of the host's logs (M-6). */
export default function PositionPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconVault size={16} /> Loan
        </>
      }
      title="Your loan"
      lead="Collateral, debt and health, read from Base through our proxy, and the ways out: close from collateral, repay, release, or withdraw part of your ZEC."
    >
      <PositionView />
    </Page>
  );
}
