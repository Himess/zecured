import type { Metadata } from "next";
import { StatusView } from "@/components/public/StatusView";
import { IconActivity } from "@/components/icons";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Status" };

export default function StatusPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconActivity size={16} /> Live, public data only
        </>
      }
      title="Status"
      lead="The signers, the Zcash header relay on Base, the proof queue, the coverage cap and the bonds, right now. Public data only."
    >
      <StatusView />
    </Page>
  );
}
