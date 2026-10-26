import type { Metadata } from "next";
import Link from "next/link";
import { Denominations } from "@/components/client";
import { IconArrowRight, IconShield } from "@/components/icons";
import { ZecMark } from "@/components/marks";
import { buttonClass, Card, Notice, Page } from "@/components/ui";

export const metadata: Metadata = { title: "Prepare your ZEC" };

export default function PreparePage() {
  return (
    <Page
      eyebrow={
        <>
          <ZecMark size={18} /> Before you borrow
        </>
      }
      title="Prepare your ZEC: Ironwood only"
      lead="Zecured only accepts deposits that stay inside Zcash's Ironwood pool. A few minutes of preparation keep your loan unlinkable to your wallet."
    >
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="min-w-0 space-y-6">
          <Card title="Why only Ironwood" icon={<IconShield size={18} />}>
            <div className="prose-calm">
              <p>
                Zcash has several pools: transparent, Sapling, Orchard and, since NU6.3, <strong>Ironwood</strong>. Inside a
                shielded pool, senders, recipients and amounts are encrypted. But whenever value <strong>moves between
                pools</strong>, it passes a turnstile (ZIP 209) and the <strong>amount becomes public</strong>.
              </p>
              <p>
                A deposit from Sapling, Orchard or a transparent address would therefore show a public movement of exactly
                the vault&apos;s amount (<Denominations />) at the exact time your vault is funded. Anyone could match it to the loan that appears on Base a
                few minutes later. An <strong>Ironwood → Ironwood</strong> payment crosses no turnstile and reveals nothing.
              </p>
            </div>
          </Card>

          <Card title="Get your ZEC into Ironwood, then wait">
            <ol className="list-decimal space-y-3 pl-5 text-sm text-muted">
              <li>
                <strong className="text-text">Migrate early.</strong> Use your wallet&apos;s Orchard → Ironwood migration
                (ZIP 318) if it has one, or send to your own Ironwood address. This crossing is public, so do it well before
                you borrow.
              </li>
              <li>
                <strong className="text-text">Migrate an amount that matches no vault size.</strong> Move, say, 0.12 TAZ
                rather than exactly <Denominations />, so the public crossing does not match a loan.
              </li>
              <li>
                <strong className="text-text">Wait before depositing.</strong> Hours or days between the migration and the
                deposit make timing correlation much harder. Low testnet volume makes this matter even more.
              </li>
              <li>
                <strong className="text-text">Deposit in one Ironwood → Ironwood payment</strong> of exactly the vault&apos;s
                amount, before its expiry height, using the QR code the app shows.
              </li>
            </ol>
          </Card>

          <Card title="What counts as a valid deposit">
            <ul className="list-disc space-y-2 pl-5 text-sm text-muted">
              <li>The whole deposit transaction touches only Ironwood: no transparent inputs or outputs, no Sapling spends or outputs, no Orchard-pool actions.</li>
              <li>
                Its value is exactly the vault&apos;s denomination (<Denominations /> on testnet).
              </li>
              <li>It is mined at or before the vault&apos;s expiry height, and it is the first such note the vault receives.</li>
            </ul>
            <div className="mt-4">
              <Notice tone="info" title="Everything else comes back as a refund">
                <p>
                  Wrong amounts, late payments, pool-crossing deposits and extra payments never back a loan. A deposit that
                  reaches the vault is proven on Base either way: as collateral, or as a Refund claim, which you give an
                  Ironwood destination on the vault&apos;s refund page and the signers pay back minus the network fee. Pay
                  exactly as shown and you never need it.
                </p>
              </Notice>
            </div>
          </Card>

          <Card title="When ZEC comes back to you">
            <ul className="list-disc space-y-2 pl-5 text-sm text-muted">
              <li>Payouts go only to the Ironwood (Orchard-protocol) receiver of a unified address. Use a fresh address from your wallet.</li>
              <li>On Base, the destination appears only as a salted commitment; the salt never leaves you and the signers.</li>
              <li>Do not move the same amount out of the shielded pool right after a payout: that would link the two.</li>
            </ul>
          </Card>
        </div>

        <aside className="space-y-4">
          <Notice tone="warn" title="Testnet only">
            <p>Use testnet ZEC (TAZ). Testnet volume is low, so the anonymity set is small.</p>
          </Notice>
          <Card title="Ready?">
            <p className="mb-4 text-sm text-muted">Your Ironwood notes should cover the amount plus the network fee.</p>
            <Link href="/borrow" className={buttonClass("primary", "w-full")}>
              Start a loan
              <IconArrowRight size={16} />
            </Link>
          </Card>
        </aside>
      </div>
    </Page>
  );
}
