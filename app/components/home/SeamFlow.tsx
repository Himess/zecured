/**
 * The loan in one picture, as the site tells it: shielded ZEC in a threshold vault on Zcash (dark, round, its
 * notes encrypted), a zero-knowledge proof crossing the seam (the gold π), and on Base (light, square) a collateral
 * receipt in a Morpho market and USDC in a brand-new account. Server-safe and decorative except for its words.
 */
import { LoanTokenName } from "../client";
import { MorphoMark, UsdcMark, ZecMark, ZzecMark } from "../marks";
import { ProofSeal } from "../seam";

export function SeamFlow() {
  return (
    <figure className="overflow-hidden rounded-t-[22px] rounded-b-[6px] shadow-[0_0_0_1px_var(--color-border),0_30px_60px_-36px_rgb(0_0_0/0.9)]">
      <figcaption className="sr-only">
        Your ZEC stays in a 3-of-5 threshold vault in Zcash&apos;s Ironwood pool; a zero-knowledge proof of the deposit
        crosses to Base, where a collateral receipt backs a Morpho Blue loan to a brand-new account.
      </figcaption>
      <div className="relative bg-[radial-gradient(120%_90%_at_20%_0%,#123224_0%,#0a1a12_45%,#060d09_100%)] px-6 pt-6 pb-12">
        <p className="flex items-center gap-2 text-[12.5px] font-semibold text-pool-soft">
          <span aria-hidden="true" className="size-2 rounded-full bg-pool-soft blur-[1px]" />
          Zcash · Ironwood pool
        </p>
        <div className="mt-4 flex items-center gap-4">
          <span className="relative inline-flex size-14 items-center justify-center rounded-full bg-[#0b1711] shadow-[inset_0_0_0_1px_rgb(127_209_180/0.35),0_0_40px_-6px_rgb(244_183_40/0.45)]">
            <ZecMark size={38} />
          </span>
          <div className="min-w-0">
            <p className="text-[17px] font-extrabold font-stretch-118%">Your ZEC, still shielded</p>
            <p className="text-[13px] text-muted">In a 3-of-5 FROST vault no single signer controls</p>
          </div>
        </div>
        <div aria-hidden="true" className="mt-4 flex flex-wrap gap-1.5">
          {["7f3a9c", "e10b44", "92d1f0", "4c7e2a", "b8051d", "0de96f"].map((n) => (
            <span key={n} className="cipher rounded-md bg-pool/15 px-2 py-0.5 font-mono text-[11px] text-pool-soft">
              {n}
            </span>
          ))}
        </div>
      </div>
      <div aria-hidden="true" className="relative z-[2] h-0">
        <ProofSeal className="absolute top-[-30px] left-[22px]" />
        <span className="absolute top-[-13px] left-24 inline-flex min-h-[26px] items-center rounded-full bg-accent px-3 text-[12.5px] font-bold whitespace-nowrap text-on-accent">
          Zero-knowledge proof
        </span>
      </div>
      <div className="paper px-6 pt-11 pb-6">
        <p className="flex items-center gap-2 text-[12.5px] font-semibold text-ink-muted">
          <span aria-hidden="true" className="size-2 bg-base" />
          Base · in the open
        </p>
        <ul className="mt-4 space-y-3">
          <li className="flex items-center gap-3">
            <ZzecMark size={34} />
            <div className="min-w-0 flex-1">
              <p className="text-[15px] font-bold">zZEC collateral receipt</p>
              <p className="text-[12.5px] text-ink-muted">Minted on the proof, straight into Morpho</p>
            </div>
            <MorphoMark size={28} />
          </li>
          <li className="flex items-center gap-3 border-t border-rule pt-3">
            <UsdcMark size={34} />
            <div className="min-w-0 flex-1">
              <p className="text-[15px] font-bold">
                <LoanTokenName /> to a brand-new account
              </p>
              <p className="text-[12.5px] text-ink-muted">Gas sponsored: it never held ETH</p>
            </div>
          </li>
        </ul>
      </div>
    </figure>
  );
}
