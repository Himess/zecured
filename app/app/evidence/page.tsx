import type { Metadata } from "next";
import { EvidenceView } from "@/components/public/EvidenceView";
import { IconFileCheck } from "@/components/icons";
import { Page } from "@/components/ui";
import { REPLAY } from "@/lib/replay/flag";

export const metadata: Metadata = { title: "Evidence" };

export default function EvidencePage() {
  return (
    <Page
      eyebrow={
        <>
          <IconFileCheck size={16} /> Don&apos;t trust, verify
        </>
      }
      title="Evidence"
      lead={
        REPLAY
          ? "The evidence file's shape: the deployment's manifest, every flow's transactions on Zcash and on Base, and the leak scan."
          : "The deployment's manifest and the real transactions of every flow, on Zcash and on Base, with the leak scan that searches Base for Zcash identifiers."
      }
    >
      <EvidenceView />
    </Page>
  );
}
