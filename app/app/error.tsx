"use client";

import { Button, Notice } from "@/components/ui";

/** Route error boundary. Deliberately no logging or reporting: errors can contain addresses. */
export default function ErrorPage({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <div className="space-y-4">
      <Notice tone="danger" title="Something went wrong on this page" live>
        <p>{error.message || "Unexpected error"}</p>
      </Notice>
      <Button onClick={reset}>Try again</Button>
    </div>
  );
}
