import type { Metadata } from "next";
import { StatusView } from "@/components/public/StatusView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Status" };

export default function StatusPage() {
  return (
    <Page
      title="Status"
      lead="What the signer network and the contracts are doing right now: the signers, the Zcash header relay, the proof queue, the coverage cap, the bonds and the anonymity sets. Public data only; nothing about any vault or user."
    >
      <StatusView />
    </Page>
  );
}
