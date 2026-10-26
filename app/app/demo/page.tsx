import type { Metadata } from "next";
import { DemoPanel } from "@/components/DemoPanel";
import { IconSigners } from "@/components/icons";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Demo panel" };

export default function DemoPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconSigners size={16} /> Operators only
        </>
      }
      title="Demo control panel"
      lead="Signer node health and modes, the threshold demo and the MOCK oracle price. Protected by the admin token."
    >
      <DemoPanel />
    </Page>
  );
}
