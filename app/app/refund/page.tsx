import type { Metadata } from "next";
import { RefundView } from "@/components/RefundView";
import { IconRefresh } from "@/components/icons";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Refunds" };

/** `/refund#<vaultId>`: the vault id stays in the fragment, out of the host's logs (M-6). */
export default function RefundPage() {
  return (
    <Page
      eyebrow={
        <>
          <IconRefresh size={16} /> Deposits that cannot back a loan
        </>
      }
      title="Refunds"
      lead="A deposit that cannot back a loan comes back as a Refund claim on Base. Give it an Ironwood destination here."
    >
      <RefundView />
    </Page>
  );
}
