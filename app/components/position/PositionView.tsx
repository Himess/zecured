"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import type { Address } from "viem";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, MissingVaultId, useMounted } from "@/components/client";
import { IconArrowDown, IconEye, IconKey, IconLayers, IconPlus, IconShield, IconSwap, IconVault } from "@/components/icons";
import { HealthRing, healthOf, TokenAmount, usdc, zec } from "@/components/loan";
import { BaseMark, MorphoMark, UsdcMark, ZecMark } from "@/components/marks";
import { PayoutNotice } from "@/components/PayoutNotice";
import { ProofTrail } from "@/components/ProofTrail";
import { Tabs, type TabItem } from "@/components/Tabs";
import { Badge, buttonClass, Card, KeyValue, Meter, Mono, Notice, Skeleton } from "@/components/ui";
import type { Claim } from "@/lib/api";
import { formatUtc } from "@/lib/calls";
import { config } from "@/lib/config";
import { DEPLOYMENT } from "@/lib/deployment";
import { useLoanByVault, usePosition, useUrlFragment, useVault, useVaultLedger } from "@/lib/hooks";
import { short } from "@/lib/keys";
import { loanTokenName, priceSourceName } from "@/lib/labels";
import { collateralValue, formatBps, formatUsdPrice, maxBorrowAtTargetLtv, WAD } from "@/lib/morpho";
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
function coordinatorBadge(c: Claim): { tone: "ok" | "warn" | "neutral" | "info"; text: string } {
  // OFF's `proven`: final (its payout proven, or its confirmation undisputed through the window).
  if (c.proven) return { tone: "ok", text: "Final on Base ✓" };
  if (c.disputableUntil) return { tone: "info", text: `Signers confirmed — disputable until ${formatUtc(c.disputableUntil)}` };
  return COORDINATOR_STATUS[c.status];
}

/** What Base says about the vault, as a badge, when it adds to the signers' status. */
const BASE_STATUS: Partial<Record<string, { tone: "info" | "neutral" | "warn"; text: string }>> = {
  Pending: { tone: "info", text: "Proven, waiting to be activated" },
  Cancelled: { tone: "neutral", text: "Refunds only" },
};

/** One of the summary's figures: a label above a large value and a line of context. */
function Figure({ label, children, hint }: { label: ReactNode; children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="min-w-0 space-y-1.5">
      <p className="text-[13px] font-semibold text-muted">{label}</p>
      <div>{children}</div>
      {hint ? <p className="text-[12.5px] text-muted">{hint}</p> : null}
    </div>
  );
}

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

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;

  const v = vault.data;
  const p = position.data;
  const status = v ? STATUS_LABEL[v.status] : null;
  const baseStatus = l ? BASE_STATUS[l.status] : undefined;
  const collateral = p?.position.collateral ?? v?.outstanding ?? v?.amount;
  const debt = p?.debt;
  const health = p ? healthOf(p) : null;
  // T-04: a theft case freezes the vault's exits; an executed one leaves its claims to the compensator.
  const frozen = Boolean(l && (l.frozen || l.compromised));
  const canAct = Boolean(loan) && v?.status === "active" && Boolean(p) && (p?.position.collateral ?? 0n) > 0n;
  const liquidations = l
    ? l.claims.filter((c) => c.kind === "Liquidation")
    : (v?.claims.filter((c) => c.kind === "Liquidation") ?? []);
  const borrowRoom = p ? maxBorrowAtTargetLtv(p.position.collateral, p.price) > p.debt : false;
  const toClaim = l ? refundsToClaim(l).length : 0;
  const fee = l ? feeClaim(l) : undefined;
  const refresh = () => {
    void vault.refresh();
    void position.refresh();
    void ledger.refresh();
  };

  // The ways to act on the loan, as tabs: exits first while there is debt.
  const tabs: TabItem[] = [];
  if (canAct && loan && v && p) {
    if (loan.pendingClose) {
      tabs.push({
        id: "exit",
        label: "Close from collateral",
        icon: <IconSwap size={16} />,
        content: <PendingClose loan={loan} vault={v} onChanged={refresh} />,
      });
    } else if (!frozen) {
      tabs.push({
        id: "exit",
        label: p.position.borrowShares > 0n ? "Close or repay" : "Release",
        icon: <IconSwap size={16} />,
        content: <Exits loan={loan} vault={v} snapshot={p} onChanged={refresh} />,
      });
    }
    if (borrowRoom && !loan.pendingClose && !frozen) {
      tabs.push({ id: "borrow", label: "Borrow more", icon: <IconPlus size={16} />, content: <BorrowForm loan={loan} snapshot={p} onBorrowed={refresh} /> });
    }
    if (!loan.pendingClose && !frozen) {
      tabs.push({
        id: "withdraw",
        label: "Withdraw part",
        icon: <IconArrowDown size={16} />,
        content: <PartialRelease loan={loan} vault={v} snapshot={p} onChanged={refresh} />,
      });
    }
    if (p.debt > 0n && !loan.pendingClose && !frozen) {
      tabs.push({
        id: "deleverage",
        label: "Reduce debt",
        icon: <IconLayers size={16} />,
        content: <Deleverage loan={loan} vault={v} snapshot={p} onChanged={refresh} />,
      });
    }
    if (!frozen) {
      tabs.push({ id: "protect", label: "Protect", icon: <IconShield size={16} />, content: <ProtectionCard loan={loan} snapshot={p} onChanged={refresh} /> });
    }
    if (p.usdcBalance > 0n) {
      tabs.push({
        id: "send",
        label: `Send ${loanTokenName()}`,
        icon: <UsdcMark size={16} />,
        content: <SendUsdcForm loan={loan} snapshot={p} onChanged={refresh} />,
      });
    }
  }
  const ltvPct = health?.ltv !== null && health?.ltv !== undefined ? Number(health.ltv) / 100 : 0;

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2.5">
          <span className="inline-flex min-h-9 items-center gap-2 rounded-full bg-surface px-3.5 text-[13px] text-muted shadow-[inset_0_0_0_1px_var(--color-border)]">
            <IconVault size={15} />
            Vault <Mono className="text-[12.5px]">{short(vaultId, 10, 8)}</Mono>
          </span>
          <CopyButton value={vaultId} label="Copy id" />
          {status ? (
            <Badge tone={status.tone} dot>
              {status.text}
            </Badge>
          ) : null}
          {baseStatus ? <Badge tone={baseStatus.tone}>{baseStatus.text}</Badge> : null}
        </div>
        <nav aria-label="Loan pages" className="flex flex-wrap gap-2">
          <Link href={vaultHref("verify", vaultId)} prefetch={false} className={buttonClass("secondary", undefined, "sm")}>
            <IconKey size={15} />
            Verify with the viewing key
          </Link>
          <Link href={vaultHref("refund", vaultId)} prefetch={false} className={buttonClass("ghost", undefined, "sm")}>
            Refunds{toClaim ? ` (${toClaim} to claim)` : ""}
          </Link>
          <Link href="/world" className={buttonClass("ghost", undefined, "sm")}>
            <IconEye size={15} />
            What the world sees
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

      <section aria-labelledby="summary" className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,20rem)]">
        <h2 id="summary" className="sr-only">
          Summary
        </h2>
        <div className="panel panel-lead space-y-6 p-[clamp(20px,2.6vw,30px)]">
          <div className="grid gap-6 sm:grid-cols-2">
            <Figure
              label={
                <span className="flex items-center gap-2">
                  Collateral <span className="font-normal text-faint">· shielded on Zcash</span>
                </span>
              }
              hint={p ? `≈ $${usdc(collateralValue(p.position.collateral, p.price))} at ${formatUsdPrice(p.price)} per ZEC` : "in the vault"}
            >
              {collateral !== undefined ? <TokenAmount token="TAZ" value={zec(collateral)} size="xl" /> : <Skeleton className="h-9 w-40" />}
            </Figure>
            <Figure
              label={
                <span className="flex items-center gap-2">
                  Debt <span className="font-normal text-faint">· on Morpho Blue</span>
                </span>
              }
              hint="Including accrued interest"
            >
              {debt !== undefined ? <TokenAmount token="USDC" value={usdc(debt)} unit={loanTokenName()} size="xl" /> : config.contractsDeployed ? <Skeleton className="h-9 w-40" /> : <span className="text-[28px] font-bold text-faint">—</span>}
            </Figure>
          </div>
          <div>
            <p className="flex justify-between text-[13px]">
              <span className="font-semibold text-muted">Loan-to-value</span>
              <span className="font-semibold tabular-nums">{health?.ltv !== null && health?.ltv !== undefined ? formatBps(health.ltv) : "—"}</span>
            </p>
            <Meter
              value={ltvPct}
              max={70}
              tone={health ? (health.tone === "muted" ? "ok" : health.tone) : "ok"}
              markers={[
                { at: 40, label: "40 % target", tone: "muted" },
                { at: 62.5, label: "62.5 % liquidation", tone: "danger" },
              ]}
            />
          </div>
          <dl className="grid gap-3 border-t border-hairline pt-4 text-[13.5px] sm:grid-cols-3">
            <div>
              <dt className="text-muted">Liquidation below</dt>
              <dd className="font-semibold tabular-nums">{health?.liquidationPrice ? `${formatUsdPrice(health.liquidationPrice)} per ZEC` : "—"}</dd>
            </div>
            <div>
              <dt className="text-muted">ZEC price</dt>
              <dd className="font-semibold tabular-nums">
                {p ? formatUsdPrice(p.price) : "—"} <span className="font-normal text-muted">· {priceSourceName()}</span>
              </dd>
            </div>
            <div>
              <dt className="flex items-center gap-1.5 text-muted">
                <MorphoMark size={14} /> Market
              </dt>
              <dd className="font-semibold">zZEC / {loanTokenName()} · LLTV 62.5 %</dd>
            </div>
          </dl>
          {fee ? (
            <p className="text-xs text-muted">
              Opening fee: {formatTaz(fee.amount)} of the deposit, owed to the signers&apos; treasury (a Fee claim, paid with the
              vault&apos;s first payout); the collateral is the deposit less the fee.
            </p>
          ) : null}
          {!config.contractsDeployed ? <p className="text-xs text-muted">Base figures appear once the contracts are deployed.</p> : null}
        </div>
        <div className="panel flex flex-col items-center justify-center gap-3 p-6 text-center">
          <HealthRing health={health} size={168} />
          <p className="max-w-[16rem] text-[13px] text-muted">
            {health && health.hf !== null
              ? health.hf < WAD
                ? "Liquidatable: a liquidator can repay debt and take ZEC from the vault."
                : "Liquidation starts below a health factor of 1.00."
              : "No debt: nothing can be liquidated."}
          </p>
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
            {formatTaz(liquidations.reduce((s, c) => s + c.amount, 0n))} in shielded ZEC is paid to whoever repaid part of the
            debt: the market maker of a deleverage you signed, a pre-liquidation within the terms you set, or a liquidator. The
            rest is still yours: release it once no debt remains, or close or repay.
          </p>
        </Notice>
      ) : null}

      {v && v.status !== "active" && v.status !== "closing" && v.status !== "closed" && loan && !l?.pending && l?.status !== "Cancelled" ? (
        <Notice tone="info" title="This loan is not active yet">
          <p>
            Status: {status?.text}. <Link href="/borrow">Continue the deposit</Link> from the borrow page.
          </p>
        </Notice>
      ) : null}

      {tabs.length > 0 ? (
        <Card lead title="Manage your loan">
          <Tabs items={tabs} initial="exit" label="Ways to act on the loan" />
        </Card>
      ) : null}

      {l ? <PendingRefundsCard ledger={l} /> : null}
      {l ? <RefundsCard ledger={l} loan={loan} onChanged={refresh} /> : null}

      {l ? (
        <ClaimsCard ledger={l} loan={loan} vault={v} onChanged={refresh} />
      ) : v && v.claims.length > 0 ? (
        <Card title="Zcash payouts" icon={<ZecMark size={18} />}>
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

      {config.contractsDeployed ? <ProofTrail vaultId={vaultId} account={account} since={loan?.createdAt} /> : null}

      {account ? (
        <Card title={`Loan account on ${DEPLOYMENT.chainName}`} icon={<BaseMark size={18} />}>
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
              ["USDC in the account", p ? `${usdc(p.usdcBalance)} ${loanTokenName()}` : "—"],
              [
                "Morpho authorizations",
                p ? `Registry ${p.registryAuthorized ? "✓" : "✗"}, Settlement Adapter ${p.adapterAuthorized ? "✓" : "✗"}` : "—",
              ],
              ["Gas", "Sponsored by our paymaster: the account never holds ETH"],
            ]}
          />
        </Card>
      ) : null}
    </div>
  );
}
