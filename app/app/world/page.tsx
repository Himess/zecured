import type { Metadata } from "next";
import { WorldView } from "@/components/WorldView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "What does the world see?" };

export default function WorldPage() {
  return (
    <Page
      title="What does the world see?"
      lead="The same loan, through three pairs of eyes. Live data where it exists: public Base events through our proxy, the signer network's view of your vaults, and what only this browser knows."
    >
      <WorldView />
    </Page>
  );
}
