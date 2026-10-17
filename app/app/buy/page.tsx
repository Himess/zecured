import type { Metadata } from "next";
import { BuyZec } from "@/components/swap/BuyZec";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Buy shielded ZEC" };

export default function BuyPage() {
  return (
    <Page
      title="Buy shielded ZEC"
      lead="Swap USDC into shielded ZEC with no custodian: the maker pays your Ironwood address and proves it on Base before your USDC is released."
    >
      <BuyZec />
    </Page>
  );
}
