import Link from "next/link";
import { LoanTokenName } from "@/components/client";
import { Dashboard } from "@/components/home/Dashboard";
import { SeamFlow } from "@/components/home/SeamFlow";
import { IconArrowRight, IconSwap } from "@/components/icons";
import { buttonClass } from "@/components/ui";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY } from "@/lib/replay/flag";

const TESTNET = DEPLOYMENT.kind === "testnet";

export default function Home() {
  return (
    <div className="space-y-14">
      <section aria-labelledby="hero" className="grid items-center gap-10 lg:grid-cols-[minmax(0,1fr)_minmax(0,25rem)]">
        <div className="space-y-6">
          <p className="inline-flex items-center gap-2 rounded-full bg-surface px-3 py-1 text-[13px] font-semibold text-soft shadow-[inset_0_0_0_1px_var(--color-border)]">
            <span aria-hidden="true" className={`dot-live size-2 rounded-full ${REPLAY ? "bg-warn" : "bg-ok"}`} />
            {REPLAY ? "A replay of the live run on Zcash testnet and Base Sepolia" : TESTNET ? "Live on Zcash testnet and Base Sepolia" : "Zecured mainnet beta"}
          </p>
          <h1
            id="hero"
            className="max-w-[13em] text-[length:clamp(38px,5.2vw,68px)] leading-[0.98] font-extrabold tracking-[-0.018em] text-balance font-stretch-125%"
          >
            Borrow against shielded ZEC. <span className="text-muted">Sell nothing. Reveal nothing.</span>
          </h1>
          <p className="max-w-[38rem] text-[17px] text-soft">
            Your ZEC stays in Zcash&apos;s shielded Ironwood pool, in a vault no single party controls. A brand-new account on
            Base borrows <LoanTokenName /> against it on Morpho Blue, and nothing on Base points back to you.
          </p>
          <div className="flex flex-wrap gap-3">
            <Link href="/borrow" className={buttonClass("primary", undefined, "lg")}>
              {TESTNET ? "Borrow on testnet" : "Borrow (invite only)"}
              <IconArrowRight size={18} />
            </Link>
            <Link href="/buy" className={buttonClass("secondary", undefined, "lg")}>
              <IconSwap size={18} />
              Buy shielded ZEC
            </Link>
            <Link href="/prepare" className={buttonClass("ghost", undefined, "lg")}>
              Prepare your ZEC first
            </Link>
          </div>
        </div>
        <SeamFlow />
      </section>
      <Dashboard />
    </div>
  );
}
