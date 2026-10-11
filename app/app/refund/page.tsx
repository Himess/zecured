import type { Metadata } from "next";
import { RefundView } from "@/components/RefundView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Refunds" };

/** `/refund#<vaultId>`: the vault id stays in the fragment, out of the host's logs (M-6). */
export default function RefundPage() {
  return (
    <Page
      title="Refund a non-conforming deposit"
      lead="Deposits that did not follow the rules are never attested. Ask for them back, to an Ironwood address."
    >
      <RefundView />
    </Page>
  );
}
