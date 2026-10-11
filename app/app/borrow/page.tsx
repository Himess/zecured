import type { Metadata } from "next";
import { BorrowWizard } from "@/components/borrow/BorrowWizard";
import { Denominations } from "@/components/client";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Borrow" };

export default function BorrowPage() {
  return (
    <Page
      title="Borrow against shielded ZEC"
      lead={
        <>
          Lock <Denominations /> in a fresh threshold vault, then borrow MockUSDC on Base Sepolia from a fresh, gasless
          account.
        </>
      }
    >
      <BorrowWizard />
    </Page>
  );
}
