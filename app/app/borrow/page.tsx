import type { Metadata } from "next";
import { BorrowWizard } from "@/components/borrow/BorrowWizard";
import Link from "next/link";
import { IconInfo } from "@/components/icons";
import { buttonClass, cx, Page } from "@/components/ui";

export const metadata: Metadata = { title: "Borrow" };

export default function BorrowPage() {
  return (
    <Page
      actions={
        <Link href="/prepare" className={cx(buttonClass("ghost", undefined, "sm"), "max-sm:-ml-3.5")}>
          <IconInfo size={15} />
          How to prepare your ZEC
        </Link>
      }
      title="Borrow against shielded ZEC"
    >
      <BorrowWizard />
    </Page>
  );
}
