"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { LoanTokenName, RealAndMockList, useMounted } from "@/components/client";
import { IconActivity, IconArrowRight, IconEye, IconEyeOff, IconFileCheck, IconPlus, IconShield, IconSigners, IconVault } from "@/components/icons";
import { LoanCard } from "@/components/LoanCard";
import { TokenAmount, usdc, zec } from "@/components/loan";
import { MorphoMark, PairMarks, UsdcMark, ZecMark, ZzecMark } from "@/components/marks";
import { ProofSeal } from "@/components/seam";
import { buttonClass, Card, cx, EmptyState, Meter, SectionTitle, Skeleton } from "@/components/ui";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useLoans, useProtocol, useStatusReport } from "@/lib/hooks";
import { useReplay } from "@/lib/replay";
import { priceSourceName } from "@/lib/labels";
import { formatUsdPrice, WAD } from "@/lib/morpho";
import { formatWadPercent } from "@/lib/preliq";
import { bondNumbers, marketNumbers } from "@/lib/public";
import { SWAP_PRIVACY_SENTENCE } from "@/lib/v4";

const TESTNET = DEPLOYMENT.kind === "testnet";

/** The home page below its hero: this browser's loans, the market, the protocol's public numbers, how it works. */
export function Dashboard() {
  const mounted = useMounted();
  const replay = useReplay();
  const { loans, ready } = useLoans();
  const protocol = useProtocol(60_000);
  const status = useStatusReport(60_000);
  const s = protocol.data;
  const m = s ? marketNumbers(s.market, s.borrowRate) : null;
  const bonds = s?.bonds ? bondNumbers(s.bonds, s.lockedZat, s.price) : null;
  const online = status.data ? status.data.nodes.filter((n) => n.mode === "normal").length : null;
  // Without Base or the backend, the figures say so instead of loading forever.
  const unread = Boolean(protocol.error) || !config.contractsDeployed;

  return (
    <div className="space-y-14">
      {replay.active && replay.recording ? (
        <section aria-labelledby="replay-h" className="panel panel-lead grid items-center gap-6 p-6 sm:p-8 md:grid-cols-[minmax(0,1fr)_auto]">
          <div className="space-y-2">
            <p className="flex items-center gap-2 text-[13px] font-semibold text-warn">
              <span aria-hidden="true" className="dot-live size-2 rounded-full bg-warn" />
              DEMO · the judges&apos; replay
            </p>
            <h2 id="replay-h" className="text-[length:clamp(21px,2vw,26px)] leading-[1.1] font-extrabold font-stretch-118%">
              Replay the live run, in this app, step by step
            </h2>
            <p className="max-w-2xl text-[15px] text-soft">
              Open a vault, pay it with demo TAZ, watch its zero-knowledge proof land on Base, borrow, close from collateral and buy
              shielded ZEC. Your browser plays the recorded run; the dock at the bottom links every step to its real transaction.
            </p>
          </div>
          {replay.next ? (
            <Link href={replay.next.href} className={buttonClass("primary", undefined, "lg")}>
              {replay.index < 0 ? "Start" : "Continue"}: {replay.next.title}
              <IconArrowRight size={18} />
            </Link>
          ) : (
            <button
              type="button"
              onClick={() => {
                replay.reset();
                window.location.assign(`${process.env.NEXT_PUBLIC_BASE_PATH ?? ""}/`);
              }}
              className={buttonClass("secondary", undefined, "lg")}
            >
              Replay it again
            </button>
          )}
        </section>
      ) : null}
      <section aria-labelledby="loans" className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]">
        <div className="min-w-0 space-y-4">
          <SectionTitle
            id="loans"
            title="Your loans"
            sub="Kept in this browser only: each loan has its own key and its own account on Base."
            action={
              <Link href="/borrow" className={buttonClass("secondary", undefined, "sm")}>
                <IconPlus size={15} />
                New loan
              </Link>
            }
          />
          {!mounted || !ready ? (
            <div className="panel space-y-3 p-6">
              <Skeleton className="h-7 w-48" />
              <Skeleton className="h-4 w-72" />
            </div>
          ) : loans.length === 0 ? (
            <EmptyState
              icon={<IconVault size={22} />}
              title="No loans in this browser yet"
              action={
                <Link href="/borrow" className={buttonClass("primary")}>
                  Open your first vault
                  <IconArrowRight size={16} />
                </Link>
              }
            >
              Lock a standard amount of shielded ZEC in a fresh threshold vault, then borrow <LoanTokenName /> against it from a
              brand-new account. Already have a loan? <Link href="/loans">Import its key backup</Link>.
            </EmptyState>
          ) : (
            <ul className="space-y-3">
              {loans.map((l) => (
                <LoanCard key={l.id} loan={l} />
              ))}
            </ul>
          )}
        </div>

        <Card
          lead
          title={
            <span className="flex items-center gap-3">
              <PairMarks a={<ZzecMark size={30} />} b={<UsdcMark size={30} />} size={30} />
              <span>
                zZEC / <LoanTokenName />
                <span className="block text-[13px] font-semibold text-muted font-stretch-100%">Morpho Blue market</span>
              </span>
            </span>
          }
          action={<MorphoMark size={30} title="Morpho" />}
        >
          {s && m ? (
            <div className="space-y-4">
              <dl className="grid grid-cols-2 gap-3">
                <div>
                  <dt className="text-[12.5px] text-muted">Borrow rate</dt>
                  <dd className="text-[22px] font-bold tabular-nums font-stretch-112%">{formatWadPercent(m.borrowApyWad)}</dd>
                </div>
                <div>
                  <dt className="text-[12.5px] text-muted">Lenders earn</dt>
                  <dd className="text-[22px] font-bold tabular-nums text-ok font-stretch-112%">{formatWadPercent(m.supplyApyWad)}</dd>
                </div>
                <div>
                  <dt className="text-[12.5px] text-muted">Supplied</dt>
                  <dd className="text-[15px] font-semibold tabular-nums">{usdc(m.suppliedUsdc)}</dd>
                </div>
                <div>
                  <dt className="text-[12.5px] text-muted">Borrowed</dt>
                  <dd className="text-[15px] font-semibold tabular-nums">{usdc(m.borrowedUsdc)}</dd>
                </div>
              </dl>
              <div>
                <p className="flex justify-between text-[12.5px] text-muted">
                  <span>Utilization</span>
                  <span className="tabular-nums">{formatWadPercent(m.utilizationWad)}</span>
                </p>
                <Meter value={Number((m.utilizationWad * 1000n) / WAD) / 1000} tone="info" className="pb-1" />
              </div>
              <div className="flex items-center justify-between gap-3 rounded-xl bg-bg/70 px-3.5 py-2.5 text-[13.5px]">
                <span className="flex items-center gap-2 text-muted">
                  <ZecMark size={16} /> ZEC price
                </span>
                <span className="font-semibold tabular-nums">
                  {formatUsdPrice(s.price)} <span className="text-[12px] font-normal text-muted">· {priceSourceName()}</span>
                </span>
              </div>
              <p className="text-[12.5px] text-muted">Liquidation above 62.5 % loan-to-value; the app borrows up to 40 %.</p>
              <Link href="/market" className={cx(buttonClass("ghost", "w-full", "sm"), "text-soft")}>
                Open the market
                <IconArrowRight size={15} />
              </Link>
            </div>
          ) : protocol.error || !config.contractsDeployed ? (
            <p className="text-sm text-muted">The market&apos;s numbers appear once Base can be read through the backend.</p>
          ) : (
            <div className="space-y-3">
              <Skeleton className="h-8 w-40" />
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </div>
          )}
        </Card>
      </section>

      <section aria-labelledby="numbers" className="space-y-4">
        <SectionTitle
          id="numbers"
          title="The protocol, in the open"
          sub="Public numbers only, read from Base through our proxy: nothing about any vault or user."
          action={
            <Link href="/status" className="text-[14px] font-semibold text-soft">
              System status
            </Link>
          }
        />
        <dl className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <Figure
            icon={<ZecMark size={18} />}
            label="Shielded ZEC in vaults"
            value={s ? <TokenAmount token="TAZ" value={zec(s.lockedZat)} size="md" /> : unread ? "—" : null}
            hint="Backing zZEC and unpaid claims"
          />
          <Figure icon={<IconVault size={17} />} label="Open vaults" value={s ? s.openVaults.toString() : unread ? "—" : null} hint="Each one 3-of-5 FROST, Ironwood only" />
          <Figure
            icon={<IconSigners size={17} />}
            label="Signers online"
            value={online !== null ? `${online} of 5` : status.error ? "—" : null}
            hint="Three must agree on every payout"
          />
          <Figure
            icon={<IconShield size={17} />}
            label="Signers' bonds"
            value={bonds ? <TokenAmount token="USDC" value={usdc(bonds.sum)} size="md" /> : unread ? "—" : null}
            hint="Slashed on a provable theft"
          />
        </dl>
      </section>

      <section aria-labelledby="how" className="space-y-4">
        <SectionTitle id="how" title="How a loan works" sub="Three steps; the ZEC never leaves Zcash's shielded pool." />
        <ol className="grid gap-3 md:grid-cols-3">
          <Step
            n={1}
            icon={<ZecMark size={26} />}
            title="Deposit from Ironwood"
            body="Pay a fresh 3-of-5 threshold vault a standard amount, by QR code. Nothing crosses a pool, so no turnstile shows the amount."
          />
          <Step
            n={2}
            icon={<ProofSeal size="sm" className="!size-[26px]" />}
            title="Proven on Base"
            body="A zero-knowledge proof that your deposit is in a Zcash block reaches Base. zZEC is minted on it: no signer vouches for it."
          />
          <Step
            n={3}
            icon={<UsdcMark size={26} />}
            title={
              <>
                Borrow <LoanTokenName />
              </>
            }
            body="A brand-new, gasless account borrows on Morpho Blue. Exit by repaying, or close from collateral: the rest returns to a new shielded address."
          />
        </ol>
      </section>

      <section aria-labelledby="swap" className="panel grid items-center gap-6 p-6 sm:p-8 md:grid-cols-[minmax(0,1fr)_auto]">
        <div className="space-y-2">
          <p className="flex items-center gap-2 text-[13px] font-semibold text-muted">
            <UsdcMark size={18} />
            <IconArrowRight size={14} />
            <ZecMark size={18} />
            Zecured Swap
          </p>
          <h2 id="swap" className="text-[length:clamp(21px,2vw,26px)] leading-[1.1] font-extrabold font-stretch-118%">
            Buy shielded ZEC with USDC, no custodian
          </h2>
          <p className="max-w-2xl text-[15px] text-soft">
            Your USDC waits in a contract on Base; a market maker pays your Ironwood address and proves the payment on Base in
            zero knowledge before it is paid. {SWAP_PRIVACY_SENTENCE}
          </p>
        </div>
        <Link href="/buy" className={buttonClass("secondary")}>
          Buy shielded ZEC
          <IconArrowRight size={16} />
        </Link>
      </section>

      <section aria-labelledby="world" className="space-y-4">
        <SectionTitle
          id="world"
          title="What does the world see?"
          action={
            <Link href="/world" className="text-[14px] font-semibold text-soft">
              See it with live data
            </Link>
          }
        />
        <ul className="grid gap-3 md:grid-cols-3">
          <Eyes
            who="Everyone"
            sees="A fresh account with no history, a standard-size position, salted commitments."
            not="Who you are, your wallet, your balance, where the ZEC goes."
          />
          <Eyes who="The signers" sees="Your vault's balance, its timing and the destinations you give for payouts." not="Your identity or any ZEC outside the vault." />
          <Eyes who="You" sees="Everything about your loan, and the vault itself through its viewing key." not="Third parties' payouts (sent without an outgoing viewing key)." />
        </ul>
      </section>

      <section aria-labelledby="trust" className="grid gap-4 lg:grid-cols-2">
        <Card title={<span id="trust">Trust, stated plainly</span>} icon={<IconShield size={18} />}>
          <ul className="space-y-2.5 text-[14.5px] text-soft">
            <li>Base never takes the signers&apos; word: deposits, payouts and misbehaviour are all proven.</li>
            <li>Two signers can&apos;t move anything; if three collude, the theft is provable and their bonds pay the victims.</li>
            <li>If the signers stop paying a claim, their bonds pay it instead, once a signer has confirmed its destination.</li>
            <li>
              {TESTNET
                ? "During the hackathon all five signers are run by the team: the bonds show the mechanism, not independent economic security."
                : "During the beta all five signers are run by the team, and caps are kept small."}
            </li>
          </ul>
          <div className="mt-5 flex flex-wrap gap-2">
            <Link href="/evidence" className={buttonClass("secondary", undefined, "sm")}>
              <IconFileCheck size={15} />
              Evidence
            </Link>
            <Link href="/status" className={buttonClass("ghost", undefined, "sm")}>
              <IconActivity size={15} />
              Status
            </Link>
          </div>
        </Card>
        <Card title="What is real and what is a mock" icon={<IconFileCheck size={18} />}>
          <RealAndMockList />
        </Card>
      </section>
    </div>
  );
}

function Figure({ icon, label, value, hint }: { icon: ReactNode; label: string; value: ReactNode | null; hint: string }) {
  return (
    <div className="panel px-5 py-4">
      <dt className="flex items-center gap-2 text-[13px] font-semibold text-muted">
        {icon}
        {label}
      </dt>
      <dd className="mt-2 min-h-8 text-[22px] font-bold tabular-nums font-stretch-112%">{value ?? <Skeleton className="h-7 w-28" />}</dd>
      <dd className="mt-1 text-xs text-muted">{hint}</dd>
    </div>
  );
}

function Step({ n, icon, title, body }: { n: number; icon: ReactNode; title: ReactNode; body: string }) {
  return (
    <li className="panel space-y-3 p-6">
      <div className="flex items-center justify-between">
        <span className="inline-flex size-11 items-center justify-center rounded-2xl bg-bg shadow-[inset_0_0_0_1px_var(--color-border)]">{icon}</span>
        <span className="text-[13px] font-bold text-faint tabular-nums">0{n}</span>
      </div>
      <h3 className="text-[18px] leading-tight font-extrabold font-stretch-118%">{title}</h3>
      <p className="text-[14px] text-soft">{body}</p>
    </li>
  );
}

function Eyes({ who, sees, not }: { who: string; sees: string; not: string }) {
  return (
    <li className="panel space-y-3 p-5">
      <h3 className="text-[17px] font-extrabold font-stretch-118%">{who}</h3>
      <p className="flex gap-2.5 text-[14px]">
        <IconEye size={17} className="mt-0.5 text-ok" />
        <span>
          <span className="font-semibold text-ok">Sees </span>
          <span className="text-soft">{sees}</span>
        </span>
      </p>
      <p className="flex gap-2.5 text-[14px]">
        <IconEyeOff size={17} className="mt-0.5 text-danger" />
        <span>
          <span className="font-semibold text-danger">Never sees </span>
          <span className="text-soft">{not}</span>
        </span>
      </p>
    </li>
  );
}

