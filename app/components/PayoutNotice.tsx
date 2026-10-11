import { Notice } from "./ui";

/**
 * Leak L7 (docs/THREAT_MODEL.md): what not to do once a payout reaches the user's Ironwood
 * address. `onBase`: the amount appears in a public Base event (release, partial release,
 * close); refunds have none.
 */
export function PayoutNotice({ onBase = true }: { onBase?: boolean }) {
  return (
    <Notice tone="warn" title="When the ZEC arrives">
      <p>
        {onBase ? "The amount paid to you is public on Base. " : null}
        Do not deshield it or send that same amount on right away: that links this payout to where the money goes. Wait,
        and move different amounts.
      </p>
    </Notice>
  );
}
