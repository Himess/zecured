import type { Metadata } from "next";
import { WorldView } from "@/components/WorldView";
import { IconEye } from "@/components/icons";
import { Page } from "@/components/ui";
import { sampleTransactions } from "@/lib/explorer";
import { REPLAY } from "@/lib/replay/flag";

export const metadata: Metadata = { title: "What does the world see?" };

export default function WorldPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconEye size={16} /> Privacy
        </>
      }
      title="What does the world see?"
      lead={
        REPLAY
          ? `The same loan, through three pairs of eyes: what Base shows everyone, what the signers see, and what only this browser knows. In the replay, ${sampleTransactions() ? "with sample data" : "the loan of our live run of 27 October"}.`
          : "The same loan, through three pairs of eyes. Live data where it exists: public Base events through our proxy, the signer network's view of your vaults, and what only this browser knows."
      }
    >
      <WorldView />
    </Page>
  );
}
