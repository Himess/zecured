"use client";

import Link from "next/link";
import type { Address } from "viem";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, MissingVaultId, useMounted } from "@/components/client";
import { PayoutNotice } from "@/components/PayoutNotice";
import { Badge, buttonClass, Card, KeyValue, Mono, Notice, Stat } from "@/components/ui";
import type { Claim } from "@/lib/api";
import { config } from "@/lib/config";
import { useLoanByVault, usePosition, useUrlFragment, useVault, useVaultLedger } from "@/lib/hooks";
import { priceSourceName } from "@/lib/labels";
import {
  collateralValue,
  formatBps,
  formatUsdc,
  formatUsdPrice,
  formatWad,
  healthFactor,
  liquidationPrice,
  ltvBps,
  maxBorrowAtTargetLtv,
  WAD,
} from "@/lib/morpho";
import { vaultHref, vaultIdFromFragment } from "@/lib/routes";
import { CLAIM_KIND_LABEL, feeClaim, refundsToClaim } from "@/lib/v3";
import { formatTaz } from "@/lib/zcash";
import { STATUS_LABEL } from "../status";
import { Exits, PendingClose, SendUsdcForm } from "./Exits";
import { Deleverage } from "./Deleverage";
import { ClaimsCard, PendingActivationCard, RefundsCard, TheftNotice } from "./Ledger";
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

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;

  const v = vault.data;
  const p = position.data;
  const status = v ? STATUS_LABEL[v.status] : null;
  const baseStatus = l ? BASE_STATUS[l.status] : undefined;
  const collateral = p?.position.collateral ?? v?.outstanding ?? v?.amount;
  const debt = p?.debt;
  const hf = p && debt !== undefined ? healthFactor(p.position.collateral, p.price, debt, config.lltv) : null;
  const lp = p && debt ? liquidationPrice(p.position.collateral, debt, config.lltv) : null;
  const ltv = p && debt !== undefined ? ltvBps(p.position.collateral, p.price, debt) : null;
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

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm text-muted">Vault</span>
        <Mono className="text-sm">{vaultId}</Mono>
        <CopyButton value={vaultId} label="Copy id" />
        {status ? <Badge tone={status.tone}>{status.text}</Badge> : null}
        {baseStatus ? <Badge tone={baseStatus.tone}>{baseStatus.text}</Badge> : null}
      </div>
      <nav aria-label="Loan pages" className="flex flex-wrap gap-2 text-sm">
        <Link href={vaultHref("verify", vaultId)} prefetch={false} className={buttonClass("secondary", "no-underline")}>
          Verify vault (viewing key)
        </Link>
        <Link href={vaultHref("refund", vaultId)} prefetch={false} className={buttonClass("secondary", "no-underline")}>
          Refunds{toClaim ? ` (${toClaim} to claim)` : ""}
        </Link>
        <Link href="/world" className={buttonClass("ghost", "no-underline")}>
          What the world sees
        </Link>
      </nav>

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

      <section aria-labelledby="summary">
        <h2 id="summary" className="sr-only">
          Summary
        </h2>
        <dl className="grid grid-cols-2 gap-3 md:grid-cols-3">
          <Stat
            label="Collateral"
            value={collateral !== undefined ? formatTaz(collateral) : "—"}
            hint={p ? `≈ ${formatUsdc(collateralValue(p.position.collateral, p.price))} USDC` : "in the vault"}
          />
          <Stat label="Debt" value={debt !== undefined ? `${formatUsdc(debt)} USDC` : "—"} hint="incl. accrued interest" />
          <Stat
            label="Health factor"
            value={p ? (hf === null ? "∞" : formatWad(hf)) : "—"}
            hint={hf !== null && hf < WAD ? "liquidatable" : "liquidation below 1.00"}
          />
          <Stat label="Liquidation price" value={lp ? formatUsdPrice(lp) : "—"} hint="ZEC/USD" />
          <Stat label="ZEC price" value={p ? formatUsdPrice(p.price) : "—"} hint={priceSourceName()} />
          <Stat label="Loan-to-value" value={ltv !== null && ltv !== undefined ? formatBps(ltv) : "—"} hint="max 62.5 %" />
        </dl>
        {fee ? (
          <p className="mt-2 text-xs text-muted">
            Opening fee: {formatTaz(fee.amount)} of the deposit, owed to the signers&apos; treasury (a Fee claim, paid with the
            vault&apos;s first payout); the collateral is the deposit less the fee.
          </p>
        ) : null}
        {!config.contractsDeployed ? (
          <p className="mt-2 text-xs text-muted">Base figures appear once the contracts are deployed.</p>
        ) : null}
      </section>

      {hf !== null && hf < WAD ? (
        <Notice tone="danger" title="This position can be liquidated">
          <p>A liquidator can repay debt and take ZEC from the vault. Repay or close from collateral to avoid it.</p>
        </Notice>
      ) : null}
      {liquidations.length > 0 ? (
        <Notice tone="warn" title="Part of the collateral was liquidated">
          <p>
            A liquidator repaid debt and received {formatTaz(liquidations.reduce((s, c) => s + c.amount, 0n))} in shielded
            ZEC. The rest is still yours: release it once no debt remains, or close or repay.
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

      {account ? (
        <Card title="Loan account (Base Sepolia)">
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
              ["USDC in the account", p ? `${formatUsdc(p.usdcBalance)} USDC` : "—"],
              [
                "Morpho authorizations",
                p ? `Registry ${p.registryAuthorized ? "✓" : "✗"} · Settlement Adapter ${p.adapterAuthorized ? "✓" : "✗"}` : "—",
              ],
            ]}
          />
        </Card>
      ) : null}

      {l ? <RefundsCard ledger={l} loan={loan} onChanged={refresh} /> : null}

      {l ? (
        <ClaimsCard ledger={l} loan={loan} vault={v} onChanged={refresh} />
      ) : v && v.claims.length > 0 ? (
        <Card title="Zcash payouts">
          <ul className="divide-y divide-border text-sm">
            {v.claims.map((c) => (
              <li key={`${c.claimId}-${c.kind}`} className="flex flex-wrap items-center justify-between gap-2 py-2">
                <span>{CLAIM_KIND_LABEL[c.kind]}</span>
                <span className="flex items-center gap-3">
                  <span className="tabular-nums">{formatTaz(c.amount)}</span>
                  <Badge tone={COORDINATOR_STATUS[c.status].tone}>{COORDINATOR_STATUS[c.status].text}</Badge>
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

      {canAct && loan && v && p ? (
        <>
          {loan.pendingClose ? (
            <Card title="Close from collateral">
              <PendingClose loan={loan} vault={v} onChanged={refresh} />
            </Card>
          ) : frozen ? null : (
            <Card title="Exit">
              <Exits loan={loan} vault={v} snapshot={p} onChanged={refresh} />
            </Card>
          )}
          {!loan.pendingClose && !frozen ? (
            <Card title="Withdraw part of your ZEC">
              <PartialRelease loan={loan} vault={v} snapshot={p} onChanged={refresh} />
            </Card>
          ) : null}
          {p.debt > 0n && !loan.pendingClose && !frozen ? (
            <Card title="Reduce debt with collateral">
              <Deleverage loan={loan} vault={v} snapshot={p} onChanged={refresh} />
            </Card>
          ) : null}
          {!frozen ? (
            <Card title="Protect my loan">
              <ProtectionCard loan={loan} snapshot={p} onChanged={refresh} />
            </Card>
          ) : null}
          {borrowRoom && !loan.pendingClose && !frozen ? (
            <Card title="Borrow more">
              <BorrowForm loan={loan} snapshot={p} onBorrowed={refresh} />
            </Card>
          ) : null}
          {p.usdcBalance > 0n ? (
            <Card title="Send USDC from the loan account">
              <SendUsdcForm loan={loan} snapshot={p} onChanged={refresh} />
            </Card>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
