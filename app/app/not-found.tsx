import type { Metadata } from "next";
import Link from "next/link";
import { Page } from "@/components/ui";

export const metadata: Metadata = { title: "Page not found" };

export default function NotFound() {
  return (
    <Page title="Page not found" lead="This page does not exist.">
      <p>
        <Link href="/">Back to the start</Link>
      </p>
    </Page>
  );
}
