import type { Metadata } from "next";
import { EvidenceView } from "@/components/public/EvidenceView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Evidence" };

export default function EvidencePage() {
  return (
    <Page
      title="Evidence"
      lead="The deployment's manifest and the real transactions of every flow, on Zcash and on Base, with the leak scan that searches Base for Zcash identifiers."
    >
      <EvidenceView />
    </Page>
  );
}
