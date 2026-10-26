import type { Metadata } from "next";
import { BorrowWizard } from "@/components/borrow/BorrowWizard";
import Link from "next/link";
import { Denominations, LoanTokenName } from "@/components/client";
import { IconInfo, IconVault } from "@/components/icons";
import { buttonClass, Page } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";

export const metadata: Metadata = { title: "Borrow" };

export default function BorrowPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconVault size={16} /> New loan
        </>
      }
      actions={
        <Link href="/prepare" className={buttonClass("ghost", undefined, "sm")}>
          <IconInfo size={15} />
          How to prepare your ZEC
        </Link>
      }
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
