import type { Metadata } from "next";
import { BorrowWizard } from "@/components/borrow/BorrowWizard";
import { Denominations, LoanTokenName } from "@/components/client";
import { Page } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";

export const metadata: Metadata = { title: "Borrow" };

export default function BorrowPage() {
  return (
    <Page
      title="Borrow against shielded ZEC"
      lead={
        <>
          Lock <Denominations /> in a fresh threshold vault, then borrow <LoanTokenName /> on {DEPLOYMENT.chainName} from a
          fresh, gasless account.
        </>
      }
    >
      <BorrowWizard />
    </Page>
  );
}
