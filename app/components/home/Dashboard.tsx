"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { LoanTokenName, RealAndMockList, useMounted } from "@/components/client";
import { IconActivity, IconArrowRight, IconFileCheck, IconPlus, IconShield, IconVault } from "@/components/icons";
import { LoanCard } from "@/components/LoanCard";
import { rate, TokenAmount, usdc, zec } from "@/components/loan";
import { MorphoMark, PairMarks, UsdcMark, ZecMark, ZzecMark } from "@/components/marks";
import { buttonClass, Card, cx, EmptyState, Meter, SectionTitle, Skeleton, Stat } from "@/components/ui";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useLoans, useProtocol, useStatusReport } from "@/lib/hooks";
import { priceLineName } from "@/lib/labels";
import { formatUsdPrice, WAD } from "@/lib/morpho";
import { bondNumbers, marketNumbers } from "@/lib/public";
import { REPLAY } from "@/lib/replay/flag";

const TESTNET = DEPLOYMENT.kind === "testnet";

/** The home page below its hero: this browser's loans, the market, the protocol in the open, and the trust it asks. */
export function Dashboard() {
  const mounted = useMounted();
  const { loans, ready } = useLoans();
  const protocol = useProtocol(60_000);
  const status = useStatusReport(60_000);
  const s = protocol.data;
  const m = s ? marketNumbers(s.market, s.borrowRate) : null;
  const bonds = s?.bonds ? bondNumbers(s.bonds, s.lockedZat, s.price) : null;
  const online = status.data ? status.data.nodes.filter((n) => n.mode === "normal").length : null;
  // Without Base or the backend, the figures say so instead of loading forever.
  const unread = Boolean(protocol.error) || !config.contractsDeployed;
  const value = (v: ReactNode | null) => v ?? (unread ? "—" : <Skeleton className="h-7 w-24" />);

  return (
    <div className="space-y-14">
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
              title={REPLAY ? "Your replayed loan appears here" : "No loans in this browser yet"}
              action={
                REPLAY ? null : (
                  <Link href="/borrow" className={buttonClass("secondary")}>
                    Open your first vault
                    <IconArrowRight size={16} />
                  </Link>
                )
              }
            >
              {REPLAY ? (
                "Once you open its vault, with the replay's first step above."
              ) : (
                <>
                  Lock a standard amount of shielded ZEC in a fresh threshold vault, then borrow <LoanTokenName /> against it from a
                  brand-new account. Already have a loan? <Link href="/loans">Import its key backup</Link>.
                </>
              )}
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
          action={<MorphoMark size={28} title="Morpho" />}
        >
          {s && m ? (
            <div className="space-y-4">
              <dl className="grid grid-cols-2 gap-3">
                <div>
                  <dt className="text-[12.5px] text-muted">Borrow rate</dt>
                  <dd className="text-[22px] font-bold tabular-nums font-stretch-110%">{rate(m.borrowApyWad)}</dd>
                </div>
                <div>
                  <dt className="text-[12.5px] text-muted">Lenders earn</dt>
                  <dd className="text-[22px] font-bold tabular-nums font-stretch-110%">{rate(m.supplyApyWad)}</dd>
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
                  <span className="tabular-nums">{rate(m.utilizationWad)}</span>
                </p>
                <Meter value={Number((m.utilizationWad * 1000n) / WAD) / 1000} tone="info" className="pb-1" />
              </div>
              <div className="flex items-start justify-between gap-3 rounded-xl bg-bg/70 px-3.5 py-2.5 text-[13.5px]">
                <span className="flex items-center gap-2 whitespace-nowrap text-muted">
                  <ZecMark size={16} /> ZEC price
                </span>
                <span className="text-right font-semibold tabular-nums">
                  {formatUsdPrice(s.price)}
                  <span className="block text-[12px] font-normal text-muted">{priceLineName()}</span>
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

      <section aria-labelledby="numbers" className="space-y-5">
        <SectionTitle
          id="numbers"
          title="In the open"
          sub={
            REPLAY
              ? "Sample public numbers, as Base would show them: nothing about any vault or user."
              : "Public numbers only, read from Base through our proxy: nothing about any vault or user."
          }
          action={
            <span className="flex gap-4 text-[14px] font-semibold">
              <Link href="/status" className="text-soft">
                Status
              </Link>
              <Link href="/evidence" className="text-soft">
                Evidence
              </Link>
            </span>
          }
        />
        <dl className="grid gap-x-8 gap-y-5 sm:grid-cols-2 lg:grid-cols-4">
          <Stat label="Shielded ZEC in vaults" value={value(s ? <TokenAmount token="TAZ" value={zec(s.lockedZat)} size="md" /> : null)} hint="backing zZEC and unpaid payouts" />
          <Stat icon={<IconVault size={15} />} label="Open vaults" value={value(s ? s.openVaults.toString() : null)} hint="each 3-of-5 FROST, Ironwood only" />
          <Stat
            icon={<IconShield size={15} />}
            label="Signers online"
            value={online !== null ? `${online} of 5` : status.error ? "—" : <Skeleton className="h-7 w-24" />}
            hint="three must agree on every payout"
          />
          <Stat label="Signers' bonds" value={value(bonds ? <TokenAmount token="USDC" value={usdc(bonds.sum)} size="md" /> : null)} hint="slashed on a provable theft" />
        </dl>
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
          <RealAndMockList verifierIsMock={status.data ? (status.data.deployment?.verifierIsMock ?? null) : status.error ? null : undefined} />
        </Card>
      </section>
    </div>
  );
}
