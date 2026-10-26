import type { Metadata } from "next";
import { MarketView } from "@/components/public/MarketView";
import { MorphoMark } from "@/components/marks";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Market" };

export default function MarketPage() {
  return (
    <Page
      eyebrow={
        <>
          <MorphoMark size={18} /> Morpho Blue
        </>
      }
      title="Market"
      lead="The lending market behind every loan: how much is lent and borrowed, at what rates, the shielded ZEC behind it, and the signers' bonds that compensate a theft."
    >
      <MarketView />
    </Page>
  );
}
