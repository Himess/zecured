import type { Metadata } from "next";
import { MarketView } from "@/components/public/MarketView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Market" };

export default function MarketPage() {
  return (
    <Page
      title="Market"
      lead="The lending market behind every loan: how much is lent and borrowed, at what rates, the shielded ZEC behind it, and the signers' bonds that compensate a theft."
    >
      <MarketView />
    </Page>
  );
}
