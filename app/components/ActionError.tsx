"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import type { ReactNode } from "react";
import { useReplay } from "@/lib/replay";
import { showOnArrival, spotlight } from "./replay/spotlight";
import { Notice } from "./ui";

const page = (p: string) => p.split("#")[0]!.replace(/\/+$/, "") || "/";

/**
 * An action's error, as a callout: a failure in red; in the replay, when the recorded run did not take that action
 * here (`refusal`), a DEMO note instead, with the replay's next step and the way to it.
 */
export function ActionError({
  error,
  refusal = false,
  title,
  children,
}: {
  error: string | null;
  refusal?: boolean;
  title: ReactNode;
  children?: ReactNode;
}) {
  const r = useReplay();
  const pathname = usePathname();
  if (!error) return null;
  if (refusal && r.active) {
    const next = r.next?.status === "next" ? r.next : null;
    return (
      <Notice tone="demo" title="Not part of the recorded run" live>
        <p>{error}</p>
        {next ? (
          <p>
            Next in the replay: <strong className="font-semibold text-text">{next.title}</strong>.{" "}
            {page(next.href) === page(pathname) ? (
              <button type="button" onClick={() => spotlight(next.id)} className="font-semibold text-demo underline underline-offset-4">
                Show me
              </button>
            ) : (
              <Link href={next.href} onClick={() => showOnArrival(next.id)} className="font-semibold text-demo">
                Go there
              </Link>
            )}
          </p>
        ) : null}
      </Notice>
    );
  }
  return (
    <Notice tone="danger" title={title} live>
      <p>{error}</p>
      {children}
    </Notice>
  );
}
