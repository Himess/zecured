import type { Metadata } from "next";
import { LoansView } from "@/components/LoansView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "My loans" };

export default function LoansPage() {
  return (
    <Page
      title="My loans"
      lead="Loan keys kept in this browser. Export a backup of every key: clearing the browser's data deletes them."
    >
      <LoansView />
    </Page>
  );
}
