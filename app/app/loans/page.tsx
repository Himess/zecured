import type { Metadata } from "next";
import { LoansView } from "@/components/LoansView";
import { IconLoans } from "@/components/icons";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Your loans" };

export default function LoansPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconLoans size={16} /> Kept in this browser only
        </>
      }
      title="Your loans"
      lead="Loan keys kept in this browser. Export a backup of every key: clearing the browser's data deletes them."
    >
      <LoansView />
    </Page>
  );
}
