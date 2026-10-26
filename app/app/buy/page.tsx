import type { Metadata } from "next";
import { BuyZec } from "@/components/swap/BuyZec";
import { IconArrowRight } from "@/components/icons";
import { UsdcMark, ZecMark } from "@/components/marks";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Buy shielded ZEC" };

export default function BuyPage() {
  return (
    <Page
      eyebrow={
        <>
          <UsdcMark size={18} />
          <IconArrowRight size={14} />
          <ZecMark size={18} />
          Zecured Swap
        </>
      }
      title="Buy shielded ZEC"
      lead="Swap USDC into shielded ZEC with no custodian: the maker pays your Ironwood address and proves it on Base before your USDC is released."
    >
      <BuyZec />
    </Page>
  );
}
