"use client";

import Link from "next/link";
import type { Address } from "viem";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, MissingVaultId, useMounted } from "@/components/client";
import { IconArrowDown, IconEye, IconKey, IconLayers, IconPlus, IconShield, IconSwap } from "@/components/icons";
import { HealthRunway, healthOf, rate as ratePct, SplitBar, TokenAmount, usd, usdc, zec } from "@/components/loan";
import { UsdcMark, ZecMark } from "@/components/marks";
import { PayoutNotice } from "@/components/PayoutNotice";
import { ProofTrail } from "@/components/ProofTrail";
import { ProofSeal } from "@/components/seam";
import { Tabs, type TabItem } from "@/components/Tabs";
import { Badge, buttonClass, Card, Disclosure, KeyValue, Mono, Notice, Skeleton } from "@/components/ui";
import type { Claim } from "@/lib/api";
import { formatUtc } from "@/lib/calls";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { REPLAY } from "@/lib/replay/flag";
import { useLoanByVault, usePosition, useProtocol, useUrlFragment, useVault, useVaultLedger } from "@/lib/hooks";
import { short } from "@/lib/keys";
import { loanTokenName, priceSourceName } from "@/lib/labels";
import { collateralValue, formatUsdPrice, maxBorrowAtTargetLtv, WAD } from "@/lib/morpho";
import { marketNumbers } from "@/lib/public";
import { vaultHref, vaultIdFromFragment } from "@/lib/routes";
import { CLAIM_KIND_LABEL, feeClaim, refundsToClaim } from "@/lib/v3";
import { formatTaz } from "@/lib/zcash";
import { STATUS_LABEL } from "../status";
import { Exits, PendingClose, SendUsdcForm } from "./Exits";
import { Deleverage } from "./Deleverage";
import { ClaimsCard, PendingActivationCard, PendingRefundsCard, RefundsCard, TheftNotice } from "./Ledger";
import { PartialRelease } from "./PartialRelease";
import { ProtectionCard } from "./Protection";

const ZERO_ACCOUNT = "0x0000000000000000000000000000000000000000";

const COORDINATOR_STATUS: Record<Claim["status"], { tone: "ok" | "warn" | "neutral" | "info"; text: string }> = {
  pending: { tone: "warn", text: "Pending" },
  paid: { tone: "ok", text: "Paid" },
  writtenOff: { tone: "neutral", text: "Written off" },
  compensated: { tone: "info", text: "Compensated" },
  disputed: { tone: "warn", text: "Disputed" },
};

/** The coordinator's word on a claim, shown while Base cannot be read: v4's delivery badges first. */
function coordinatorBadge(c: Claim): { tone: "ok" | "warn" | "neutral" | "info" | "proof"; text: string } {
  // OFF's `proven`: final (its payout proven, or its confirmation undisputed through the window).
  if (c.proven) return { tone: "proof", text: "Final on Base" };
  if (c.disputableUntil) return { tone: "info", text: `Signers confirmed: disputable until ${formatUtc(c.disputableUntil)}` };
  return COORDINATOR_STATUS[c.status];
}

/** What Base says about the vault, as a badge, when it adds to the signers' status. */
const BASE_STATUS: Partial<Record<string, { tone: "info" | "neutral" | "warn"; text: string }>> = {
  Pending: { tone: "info", text: "Proven, waiting to be activated" },
  Cancelled: { tone: "neutral", text: "Refunds only" },
};

/** The vault comes from the URL fragment (`/position#0x…`), never the path (M-6). */
export function PositionView() {
  const mounted = useMounted();
  const fragment = useUrlFragment();
  const vaultId = (fragment && vaultIdFromFragment(fragment)) || undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 15_000);
  const ledger = useVaultLedger(vaultId, 30_000);
  const l = ledger.data;
  const account: Address | undefined = loan?.account ?? (l && l.account !== ZERO_ACCOUNT ? l.account : undefined);
  const position = usePosition(account, 30_000);
  const protocol = useProtocol(60_000);

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;

  const v = vault.data;
  const p = position.data;
  const status = v ? STATUS_LABEL[v.status] : null;
  const baseStatus = l ? BASE_STATUS[l.status] : undefined;
  const collateral = p?.position.collateral ?? v?.outstanding ?? v?.amount;
  const debt = p?.debt;
  const health = p ? healthOf(p) : null;
  const token = loanTokenName();
  const rate = protocol.data ? marketNumbers(protocol.data.market, protocol.data.borrowRate).borrowApyWad : null;
  // T-04: a theft case freezes the vault's exits; an executed one leaves its claims to the compensator.
  const frozen = Boolean(l && (l.frozen || l.compromised));
  const canAct = Boolean(loan) && v?.status === "active" && Boolean(p) && (p?.position.collateral ?? 0n) > 0n;
  const closed = v?.status === "closing" || v?.status === "closed";
  const liquidations = l ? l.claims.filter((c) => c.kind === "Liquidation") : (v?.claims.filter((c) => c.kind === "Liquidation") ?? []);
  const borrowRoom = p ? maxBorrowAtTargetLtv(p.position.collateral, p.price) > p.debt : false;
  const toClaim = l ? refundsToClaim(l).length : 0;
  const fee = l ? feeClaim(l) : undefined;
  const refresh = () => {
    void vault.refresh();
    void position.refresh();
    void ledger.refresh();
  };

  // The ways to act on the loan, as tabs: the way out first, set apart from the adjustments.
  const tabs: TabItem[] = [];
  if (canAct && loan && v && p) {
    if (loan.pendingClose) {
      tabs.push({ id: "exit", label: "Close from collateral", icon: <IconSwap size={16} />, divider: true, content: <PendingClose loan={loan} vault={v} onChanged={refresh} /> });
    } else if (!frozen) {
      tabs.push({
        id: "exit",
        label: p.position.borrowShares > 0n ? "Close or repay" : "Release",
        icon: <IconSwap size={16} />,
        divider: true,
        content: <Exits loan={loan} vault={v} snapshot={p} onChanged={refresh} />,
      });
    }
    if (borrowRoom && !loan.pendingClose && !frozen) {
      tabs.push({ id: "borrow", label: "Borrow more", icon: <IconPlus size={16} />, content: <BorrowForm loan={loan} snapshot={p} onBorrowed={refresh} /> });
    }
    if (!loan.pendingClose && !frozen) {
      tabs.push({ id: "withdraw", label: "Withdraw part", icon: <IconArrowDown size={16} />, content: <PartialRelease loan={loan} vault={v} snapshot={p} onChanged={refresh} /> });
    }
    if (p.debt > 0n && !loan.pendingClose && !frozen) {
      tabs.push({ id: "deleverage", label: "Reduce debt", icon: <IconLayers size={16} />, content: <Deleverage loan={loan} vault={v} snapshot={p} onChanged={refresh} /> });
    }
    if (!frozen) {
      tabs.push({ id: "protect", label: "Protect", icon: <IconShield size={16} />, content: <ProtectionCard loan={loan} snapshot={p} onChanged={refresh} /> });
    }
    if (p.usdcBalance > 0n) {
      tabs.push({ id: "send", label: `Send ${token}`, icon: <UsdcMark size={16} />, content: <SendUsdcForm loan={loan} snapshot={p} onChanged={refresh} /> });
    }
  }
  // After a close from collateral: what went where, from the vault's claims on Base.
  const claims = l?.claims ?? [];
  const toUser = claims.find((c) => c.kind === "CloseUser");
  const toMaker = claims.find((c) => c.kind === "CloseMarketMaker");

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <span className="inline-flex min-h-9 items-center gap-1.5 rounded-full bg-surface pr-1 pl-3.5 text-[13px] text-muted shadow-[inset_0_0_0_1px_var(--color-border)]">
            Vault <Mono className="text-[12.5px]">{short(vaultId, 10, 8)}</Mono>
            <CopyButton value={vaultId} label="Copy" className="!min-h-7 !bg-transparent !shadow-none" />
          </span>
          {status ? (
            <Badge tone={status.tone} dot>
              {status.text}
            </Badge>
          ) : null}
          {baseStatus ? <Badge tone={baseStatus.tone}>{baseStatus.text}</Badge> : null}
        </div>
        <nav aria-label="Loan pages" className="flex flex-wrap items-center gap-2">
          {/* DEMO: the replay never publishes a viewing key; the page says so. */}
          <Link href={vaultHref("verify", vaultId)} prefetch={false} className={buttonClass(REPLAY ? "ghost" : "secondary", undefined, "sm")}>
            <IconKey size={15} />
            {REPLAY ? "Viewing key (live app)" : "Verify with the viewing key"}
          </Link>
          <Link href="/world" className={buttonClass("ghost", undefined, "sm")}>
            <IconEye size={15} />
            What the world sees
          </Link>
          <Link href={vaultHref("refund", vaultId)} prefetch={false} className={buttonClass("ghost", undefined, "sm")}>
            Refunds{toClaim ? ` (${toClaim} to claim)` : ""}
          </Link>
        </nav>
      </div>

      {mounted && ready && !loan ? (
        <Notice tone="info" title="View only">
          <p>
            This browser does not hold the owner key for this vault, so you can only view it. To act on it,{" "}
            <Link href="/loans">import its key backup</Link>.
          </p>
        </Notice>
      ) : null}
      {vault.error ? (
        <Notice tone="warn" title="Vault status unavailable" live>
          <p>{vault.error}</p>
        </Notice>
      ) : null}
      {position.error ? (
        <Notice tone="warn" title="Base position unavailable" live>
          <p>{position.error}</p>
        </Notice>
      ) : null}
      {ledger.error ? (
        <Notice tone="warn" title="Registry unavailable" live>
          <p>{ledger.error}</p>
        </Notice>
      ) : null}

      {l ? <TheftNotice ledger={l} /> : null}
      {l?.pending ? <PendingActivationCard ledger={l} loan={loan} onChanged={refresh} /> : null}

      {/* The loan as the mark opened out: the shielded collateral on the dark Zcash ground, the public debt on Base's
          light one, the proof between them. */}
      <section aria-labelledby="summary" className="panel overflow-hidden">
        <h2 id="summary" className="sr-only">
          Summary
        </h2>
        <div className="relative grid md:grid-cols-2">
          <div className="space-y-2 bg-[radial-gradient(120%_120%_at_0%_0%,#123224_0%,#0a1a12_50%,#070d0a_100%)] px-6 pt-6 pb-7 sm:px-7">
            <p className="flex items-center gap-2 text-[13px] font-semibold text-muted">
              <ZecMark size={15} /> Collateral <span className="font-normal text-faint">· shielded on Zcash</span>
            </p>
            {collateral !== undefined ? <TokenAmount token="TAZ" value={zec(collateral)} size="xl" /> : <Skeleton className="h-9 w-40" />}
            <p className="text-[13px] text-muted">
              {closed
                ? "All paid out: see the payouts below"
                : p
                  ? `≈ ${usd(collateralValue(p.position.collateral, p.price))} at ${formatUsdPrice(p.price)} (${priceSourceName()})`
                  : "in the vault"}
            </p>
            <div aria-hidden="true" className="flex flex-wrap gap-1.5 pt-1">
              {["7f3a9c", "e10b44", "92d1f0", "4c7e2a"].map((n) => (
                <span key={n} className="cipher rounded-md bg-pool/15 px-2 py-0.5 font-mono text-[11px] text-pool-soft">
                  {n}
                </span>
              ))}
            </div>
          </div>
          <div className="paper space-y-2 px-6 pt-6 pb-7 sm:px-7 md:pl-10">
            <p className="flex items-center gap-2 text-[13px] font-semibold text-ink-muted">
              <span aria-hidden="true" className="size-2 bg-base" /> Debt <span className="font-normal text-ink-faint">· in the open on Morpho Blue</span>
            </p>
            {debt !== undefined ? (
              <TokenAmount token="USDC" value={usdc(debt)} size="xl" paper />
            ) : config.contractsDeployed ? (
              <Skeleton className="h-9 w-40" />
            ) : (
              <span className="text-[28px] font-bold text-ink-faint">—</span>
            )}
            <p className="text-[13px] text-ink-muted">
              {rate !== null ? `${ratePct(rate)} a year, accruing every block` : "including accrued interest"}
              {account ? (
                <>
                  {" · "}account <span className="font-mono">{short(account)}</span>, gas sponsored
                </>
              ) : null}
            </p>
          </div>
          <span aria-hidden="true" className="absolute top-1/2 left-1/2 hidden -translate-x-1/2 -translate-y-1/2 md:block">
            <ProofSeal size="sm" className="!size-[34px]" />
          </span>
        </div>
        <div className="border-t border-hairline px-6 py-5 sm:px-7">
          {closed ? (
            <p className="text-[14px] text-muted">Closed: no collateral and no debt are left, so nothing can be liquidated.</p>
          ) : (
            <HealthRunway health={health} size="l" />
          )}
          {fee ? (
            <p className="mt-3 text-[12px] text-muted">
              Collateral is the deposit less its opening fee ({formatTaz(fee.amount)}, paid to the signers from the vault&apos;s first payout).
            </p>
          ) : null}
          {!config.contractsDeployed ? <p className="mt-3 text-xs text-muted">Base figures appear once the contracts are deployed.</p> : null}
        </div>
      </section>

      {health && health.hf !== null && health.hf < WAD ? (
        <Notice tone="danger" title="This position can be liquidated">
          <p>A liquidator can repay debt and take ZEC from the vault. Repay or close from collateral to avoid it.</p>
        </Notice>
      ) : null}
      {liquidations.length > 0 ? (
        <Notice tone="warn" title="Part of the collateral went to repay debt">
          <p>
            {formatTaz(liquidations.reduce((s, c) => s + c.amount, 0n))} in shielded ZEC is paid to whoever repaid part of the debt:
            the market maker of a deleverage you signed, a pre-liquidation within the terms you set, or a liquidator. The rest is
            still yours: release it once no debt remains, or close or repay.
          </p>
        </Notice>
      ) : null}

      {v && v.status !== "active" && !closed && loan && !l?.pending && l?.status !== "Cancelled" ? (
        <Notice tone="info" title="This loan is not active yet">
          <p>
            Status: {status?.text}. <Link href="/borrow">Continue the deposit</Link> from the borrow page.
          </p>
        </Notice>
      ) : null}

      {tabs.length > 0 ? (
        <Card lead title="What next?" className="scroll-mt-24" as="section">
          <div id="manage">
            <Tabs items={tabs} initial="exit" label="Ways to act on the loan" />
          </div>
        </Card>
      ) : null}

      {closed && toUser && toMaker ? (
        <Card lead title="Loan closed from collateral">
          <div className="space-y-4">
            <p className="text-[15px] text-soft">
              The market maker repaid your debt on Morpho and takes {formatTaz(toMaker.amount)} from the vault.{" "}
              {p && p.usdcBalance > 0n ? `You keep the ${usdc(p.usdcBalance)} ${token} in your loan account; ` : ""}
              {formatTaz(toUser.amount)} {toUser.status === "Paid" ? "was paid to" : "is on its way to"} your new Ironwood address.
            </p>
            <SplitBar mm={toMaker.amount} user={toUser.amount} userTo="to a new Ironwood address" />
            <div className="flex flex-wrap gap-2">
              <a href="#payouts" className={buttonClass("secondary", undefined, "sm")}>
                See the payouts
              </a>
              <Link href="/world" className={buttonClass("ghost", undefined, "sm")}>
                What the world saw
              </Link>
            </div>
            {loan && p && p.usdcBalance > 0n ? (
              <Disclosure summary={`Send the ${token} on`}>
                <SendUsdcForm loan={loan} snapshot={p} onChanged={refresh} />
              </Disclosure>
            ) : null}
          </div>
        </Card>
      ) : null}

      {l ? <PendingRefundsCard ledger={l} /> : null}
      {l ? <RefundsCard ledger={l} loan={loan} onChanged={refresh} /> : null}

      <div id="payouts" className="scroll-mt-24">
        {l ? (
          <ClaimsCard ledger={l} loan={loan} vault={v} onChanged={refresh} />
        ) : v && v.claims.length > 0 ? (
          <Card title="Payouts on Zcash" icon={<ZecMark size={18} />}>
            <ul className="divide-y divide-hairline text-sm">
              {v.claims.map((c) => (
                <li key={`${c.claimId}-${c.kind}`} className="flex flex-wrap items-center justify-between gap-2 py-2.5">
                  <span>{CLAIM_KIND_LABEL[c.kind]}</span>
                  <span className="flex items-center gap-3">
                    <span className="tabular-nums">{formatTaz(c.amount)}</span>
                    <Badge tone={coordinatorBadge(c).tone}>{coordinatorBadge(c).text}</Badge>
                  </span>
                </li>
              ))}
            </ul>
            {v.payoutInFlight ? <p className="mt-2 text-xs text-muted">A payout transaction is being confirmed on Zcash.</p> : null}
            {v.claims.some((c) => c.kind === "Release" || c.kind === "CloseUser") ? (
              <div className="mt-4">
                <PayoutNotice />
              </div>
            ) : null}
          </Card>
        ) : null}
      </div>

      {config.contractsDeployed ? <ProofTrail vaultId={vaultId} account={account} since={loan?.createdAt} /> : null}

      {account ? (
        <Disclosure
          summary={
            <>
              Loan account on {DEPLOYMENT.chainName}
              <span className="font-mono text-[12.5px] font-normal text-muted">{short(account)}</span>
            </>
          }
        >
          <KeyValue
            rows={[
              [
                "Smart account",
                <span key="a" className="flex flex-wrap items-center gap-2">
                  <Mono>{account}</Mono>
                  <CopyButton value={account} />
                </span>,
              ],
              ["Deployed", p ? (p.accountDeployed ? "Yes" : "Not yet (deployed by its first transaction)") : "—"],
              [`${token} in the account`, p ? `${usdc(p.usdcBalance)} ${token}` : "—"],
              ["ETH in the account", "None, ever: its gas is sponsored by our paymaster"],
              ["Morpho authorizations", p ? `Registry ${p.registryAuthorized ? "yes" : "no"}, Settlement Adapter ${p.adapterAuthorized ? "yes" : "no"}` : "—"],
            ]}
          />
        </Disclosure>
      ) : null}
    </div>
  );
}

