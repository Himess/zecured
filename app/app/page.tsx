import Link from "next/link";
import { LoanTokenName, RealAndMockList } from "@/components/client";
import { buttonClass, Card, cx } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import { SWAP_PRIVACY_SENTENCE } from "@/lib/v4";

const TESTNET = DEPLOYMENT.kind === "testnet";

const SECTION_HEADING = "text-[length:clamp(26px,2.6vw,34px)] leading-[1.05] font-extrabold tracking-[-0.01em] font-stretch-125%";

/** One row of a list on the dark ground: rounded, no rules (the design's ladder), not a grid of cards. */
const ROW = "grid gap-x-10 gap-y-2 rounded-[18px] bg-surface px-5 py-5 sm:px-7 sm:py-6 md:items-baseline";

const steps = [
  {
    title: "Deposit from Ironwood",
    body: "Pay a fresh 3-of-5 FROST threshold vault from your Ironwood notes, by QR code, for a standard amount. The ZEC never crosses a pool, so no turnstile reveals the amount.",
  },
  {
    title: "Borrow USDC",
    body: "Once a zero-knowledge proof that your deposit is in a Zcash block reaches Base, a non-transferable collateral receipt (zZEC) is minted straight into a Morpho Blue position of a brand-new, gasless smart account. No signer vouches for it. Borrow up to 40 % loan-to-value.",
  },
  {
    title: "Exit without unshielding",
    body: "Repay and release, or close from collateral: a market maker repays the debt and takes ZEC from the vault, and the rest returns to a new Ironwood address. Every payout's delivery is proven on Base.",
  },
];

const world = [
  {
    who: "Everyone",
    sees: "A fresh account with no history, a standard-size position, salted commitments. On a swap, the Base account that buys.",
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
        <p className="text-[15px] font-medium text-muted">{TESTNET ? "Zecured on testnet" : "Zecured mainnet beta"}</p>
        <h1
          id="hero"
          className="max-w-[15em] text-[length:clamp(36px,5vw,64px)] leading-[0.98] font-extrabold tracking-[-0.015em] text-balance font-stretch-125%"
        >
          Borrow against shielded ZEC. Sell nothing. Reveal nothing.
        </h1>
        <p className="max-w-2xl text-lg text-soft">
          Using ZEC in DeFi today usually means taking it out of the shielded pool. Zecured doesn&apos;t: your ZEC stays in
          Zcash&apos;s shielded Ironwood pool, in a vault no single party controls, and a fresh account on Base borrows{" "}
          <LoanTokenName /> against it. Nothing on Base points back to you.
        </p>
        <div className="flex flex-wrap gap-3">
          <Link href="/borrow" className={buttonClass("primary", "no-underline")}>
            {TESTNET ? "Borrow on testnet" : "Borrow (invite only)"}
          </Link>
          <Link href="/buy" className={buttonClass("secondary", "no-underline")}>
            Buy shielded ZEC
          </Link>
          <Link href="/prepare" className={buttonClass("ghost", "no-underline")}>
            Prepare your ZEC first
          </Link>
        </div>
      </section>

      <section aria-labelledby="how" className="space-y-6">
        <h2 id="how" className={SECTION_HEADING}>
          How it works
        </h2>
        <ol className="space-y-2.5">
          {steps.map((s, i) => (
            <li key={s.title} className={cx(ROW, "md:grid-cols-[minmax(0,19rem)_minmax(0,1fr)]")}>
              <h3 className="flex items-baseline gap-3 text-xl leading-tight font-extrabold font-stretch-118%">
                <span className="text-[15px] font-semibold text-muted tabular-nums font-stretch-100%">{i + 1}</span>
                {s.title}
              </h3>
              <p className="text-[15px] text-soft">{s.body}</p>
            </li>
          ))}
        </ol>
        <Card title="Swap USDC into shielded ZEC">
          <p className="text-[15px] text-soft">
            The same Zcash light client on Base lets anyone buy shielded ZEC with no custodian: your USDC waits in a contract on
            Base, a market maker pays your Ironwood address, and proves the payment on Base in zero knowledge before it is paid.{" "}
            {SWAP_PRIVACY_SENTENCE} <Link href="/buy">Buy shielded ZEC</Link>.
          </p>
        </Card>
      </section>

      <section aria-labelledby="world" className="space-y-6">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <h2 id="world" className={SECTION_HEADING}>
            What does the world see?
          </h2>
          <Link href="/world" className="text-[15px] font-medium">
            See it with live data
          </Link>
        </div>
        <ul className="space-y-2.5">
          {world.map((w) => (
            <li key={w.who} className={cx(ROW, "md:grid-cols-[minmax(0,12rem)_minmax(0,1fr)_minmax(0,1fr)]")}>
              <h3 className="text-xl leading-tight font-extrabold font-stretch-118%">{w.who}</h3>
              <p className="text-[15px]">
                <span className="font-semibold text-ok">Sees: </span>
                <span className="text-soft">{w.sees}</span>
              </p>
              <p className="text-[15px]">
                <span className="font-semibold text-danger">Does not see: </span>
                <span className="text-soft">{w.not}</span>
              </p>
            </li>
          ))}
        </ul>
      </section>

      <section aria-labelledby="trust" className="grid gap-4 md:grid-cols-2">
        <Card title={<span id="trust">Trust, stated plainly</span>}>
          <ul className="list-disc space-y-2 pl-5 text-sm text-soft">
            <li>Base never takes the signers&apos; word: deposits are proven, payouts are proven, misbehaviour is proven.</li>
            <li>Two signers can&apos;t move anything; if three collude, the theft is provable and their bonds pay the victims.</li>
            <li>If the signers stop paying a claim, their bonds pay it instead, once a signer has confirmed its destination.</li>
            <li>You can fetch your vault&apos;s viewing key and verify it independently.</li>
            <li>
              {TESTNET
                ? "During the hackathon all five signers are run by the team: the bonds show the mechanism, not independent economic security."
                : "During the beta all five signers are run by the team, and caps are kept small."}
            </li>
          </ul>
          <p className="mt-4 text-sm text-muted">
            See the <Link href="/status">system status</Link>, the <Link href="/evidence">evidence of real transactions</Link> and{" "}
            <Link href="/market">the market</Link>.
          </p>
        </Card>
        <Card title="What is real and what is a mock">
          <RealAndMockList />
        </Card>
      </section>
    </div>
  );
}
