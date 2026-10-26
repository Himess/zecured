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
      lead="No custodian: your USDC waits on Base until the maker proves, in zero knowledge, that it paid your Ironwood address."
    >
      <BuyZec />
    </Page>
  );
}
