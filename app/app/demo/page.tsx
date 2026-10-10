import type { Metadata } from "next";
import { DemoPanel } from "@/components/DemoPanel";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Demo panel" };

export default function DemoPage() {
  return (
    <Page title="Demo control panel" lead="Signer node health and modes, and the threshold demo. Protected by the admin token.">
      <DemoPanel />
    </Page>
  );
}
