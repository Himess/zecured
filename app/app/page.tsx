import Link from "next/link";
import { buttonClass, Card } from "@/components/ui";

const steps = [
  {
    title: "Deposit from Ironwood",
    body: "Pay a fresh 3-of-5 FROST threshold vault from your Ironwood notes, by QR code, for a standard amount. The ZEC never crosses a pool, so no turnstile reveals the amount.",
  },
  {
    title: "Borrow USDC",
    body: "Once the signers see your deposit, a non-transferable receipt (zZEC) is minted straight into a Morpho Blue position of a brand-new, gasless smart account. Borrow MockUSDC up to 40 % loan-to-value.",
  },
  {
    title: "Exit without unshielding",
    body: "Repay and release, or close from collateral: a market maker repays the debt and takes ZEC from the vault, and the rest returns to a new Ironwood address. No outside funds touch your account.",
  },
];

const world = [
  {
    who: "Everyone",
    sees: "A fresh account with no history, a standard-size position, salted commitments.",
    not: "Who you are, your wallet, your balance, where the ZEC goes.",
  },
  {
    who: "Signer network",
    sees: "Your vault's balance, its timing and the destinations you reveal for payouts.",
    not: "Your identity or any ZEC outside the vault.",
  },
  {
    who: "You",
    sees: "Everything about your loan, and your vault itself through its viewing key.",
    not: "Third-party payouts (sent without an outgoing viewing key).",
  },
];

export default function Home() {
  return (
    <div className="space-y-16">
      <section className="space-y-6 pt-4" aria-labelledby="hero">
        <p className="text-sm font-medium uppercase tracking-widest text-accent">Zecured · testnet</p>
        <h1 id="hero" className="max-w-3xl text-4xl font-semibold leading-tight tracking-tight sm:text-5xl">
          Borrow against shielded ZEC. Sell nothing. Reveal nothing.
        </h1>
        <p className="max-w-2xl text-lg text-muted">
          Your ZEC stays in Zcash&apos;s shielded Ironwood pool, in a vault no single party controls. A fresh account on
          Base borrows USDC against it, and nothing on Base points back to you.
        </p>
        <div className="flex flex-wrap gap-3">
          <Link href="/borrow" className={buttonClass("primary", "no-underline")}>
            Borrow on testnet
          </Link>
          <Link href="/prepare" className={buttonClass("secondary", "no-underline")}>
            Prepare your ZEC first
          </Link>
        </div>
      </section>

      <section aria-labelledby="how" className="space-y-6">
        <h2 id="how" className="text-xl font-semibold">
          How it works
        </h2>
        <ol className="grid gap-4 md:grid-cols-3">
          {steps.map((s, i) => (
            <li key={s.title} className="rounded-xl border border-border bg-surface p-5">
              <p className="font-mono text-sm text-accent">0{i + 1}</p>
              <h3 className="mt-2 font-semibold">{s.title}</h3>
              <p className="mt-2 text-sm text-muted">{s.body}</p>
            </li>
          ))}
        </ol>
      </section>

      <section aria-labelledby="world" className="space-y-6">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <h2 id="world" className="text-xl font-semibold">
            What does the world see?
          </h2>
          <Link href="/world" className="text-sm">
            See it with live data →
          </Link>
        </div>
        <div className="grid gap-4 md:grid-cols-3">
          {world.map((w) => (
            <Card key={w.who} as="article" title={w.who}>
              <p className="text-sm">
                <span className="text-ok">Sees: </span>
                <span className="text-muted">{w.sees}</span>
              </p>
              <p className="mt-2 text-sm">
                <span className="text-danger">Does not see: </span>
                <span className="text-muted">{w.not}</span>
              </p>
            </Card>
          ))}
        </div>
      </section>

      <section aria-labelledby="trust" className="grid gap-4 md:grid-cols-2">
        <Card title={<span id="trust">Trust, stated plainly</span>}>
          <ul className="list-disc space-y-2 pl-5 text-sm text-muted">
            <li>Any payout needs 3 of 5 signer nodes, and each checks Base, the exact Zcash outputs and the vault&apos;s books.</li>
            <li>One or two compromised signers cannot move funds. Three colluding signers could: during the hackathon all five are run by us.</li>
            <li>You can fetch your vault&apos;s viewing key and verify it independently.</li>
          </ul>
        </Card>
        <Card title="What is real and what is a mock">
          <ul className="list-disc space-y-2 pl-5 text-sm text-muted">
            <li>Real: shielded Ironwood transactions on Zcash testnet, 3-of-5 FROST vaults, the Morpho Blue market on Base Sepolia.</li>
            <li>Mock: the ZEC price (MockZecOracle, set by hand for the demo), the USDC (MockUSDC), and signer independence.</li>
          </ul>
        </Card>
      </section>
    </div>
  );
}
