import { Dashboard } from "@/components/home/Dashboard";
import { HeroActions } from "@/components/home/HeroActions";
import { SeamFlow } from "@/components/home/SeamFlow";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY } from "@/lib/replay/flag";

const TESTNET = DEPLOYMENT.kind === "testnet";

export default function Home() {
  return (
    <div className="space-y-14">
      <section aria-labelledby="hero" className="grid items-center gap-10 lg:grid-cols-[minmax(0,1fr)_minmax(0,25rem)]">
        <div className="space-y-6">
          <p className="text-[13px] font-semibold text-muted">
            {REPLAY ? "ZECATHON 2026 · the app, as a guided replay" : TESTNET ? "Zcash testnet and Base Sepolia" : "Zecured mainnet beta"}
          </p>
          <h1
            id="hero"
            className="max-w-[12em] text-[length:clamp(38px,5vw,64px)] leading-[0.98] font-extrabold tracking-[-0.02em] text-balance font-stretch-125%"
          >
            Borrow against shielded ZEC. Only the <span className="text-accent">proof</span> crosses.
          </h1>
          <p className="max-w-[38rem] text-[17px] text-soft">
            Borrow USDC on Base against ZEC that never leaves Ironwood, Zcash&apos;s newest shielded pool, or swap USDC into shielded
            ZEC. Base sees zero-knowledge proofs, never your Zcash address, transaction or balance.
          </p>
          <HeroActions />
        </div>
        <SeamFlow />
      </section>
      <Dashboard />
    </div>
  );
}
