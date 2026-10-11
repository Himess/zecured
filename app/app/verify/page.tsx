import type { Metadata } from "next";
import { VerifyView } from "@/components/VerifyView";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Verify your vault" };

/** `/verify#<vaultId>`: the vault id stays in the fragment, out of the host's logs (M-6). */
export default function VerifyPage() {
  return (
    <Page title="Verify your vault" lead="Don't trust, verify: watch the vault on Zcash with its viewing key.">
      <VerifyView />
    </Page>
  );
}
