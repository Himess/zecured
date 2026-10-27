import { Notice } from "./ui";

/**
 * Leak L7 (docs/THREAT_MODEL.md): what not to do once a payout reaches the user's Ironwood
 * address. `onBase`: the amount appears in a public Base event (release, partial release,
 * close); refunds have none. `bought`: ZEC bought in the swap, whose amount is public next to
 * the buyer's wallet (L26).
 */
export function PayoutNotice({ onBase = true, bought = false }: { onBase?: boolean; bought?: boolean }) {
  return (
    <Notice tone="warn" title={bought ? "Now that the ZEC is yours" : "When the ZEC arrives"}>
      <p>
        {bought ? "The amount you bought is public on Base, next to your wallet. " : onBase ? "The amount paid to you is public on Base. " : null}
        Do not deshield it or send that same amount on right away: that links this {bought ? "purchase" : "payout"} to where the
        money goes. Wait, and move different amounts.
      </p>
    </Notice>
  );
}
