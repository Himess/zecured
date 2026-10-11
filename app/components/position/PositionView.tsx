"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import type { Address, Hex } from "viem";
import { BorrowForm } from "@/components/BorrowForm";
import { CopyButton, MissingVaultId, useMounted } from "@/components/client";
import { PayoutNotice } from "@/components/PayoutNotice";
import { Badge, buttonClass, Card, KeyValue, Mono, Notice, Stat } from "@/components/ui";
import { describeError, type Claim, type Vault } from "@/lib/api";
import { readVault, type OnchainVault } from "@/lib/chain";
import { config } from "@/lib/config";
import { useLoanByVault, usePosition, useUrlFragment, useVault } from "@/lib/hooks";
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
import { formatTaz } from "@/lib/zcash";
import { STATUS_LABEL } from "../status";
import { Exits, PendingClose, SendUsdcForm } from "./Exits";
import { PartialRelease } from "./PartialRelease";

const CLAIM_LABEL: Record<Claim["kind"], string> = {
  Release: "Release to you",
  Liquidation: "Liquidation (paid to the liquidator)",
  CloseUser: "Close: your remainder",
  CloseMarketMaker: "Close: market maker's share",
  Unknown: "Payout",
};

/** Reads the vault's account from the Registry when this browser has no key for it. */
function useOnchainVault(vaultId: Hex | undefined, enabled: boolean) {
  const [data, setData] = useState<OnchainVault | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!vaultId || !enabled || !config.contractsDeployed) return;
    let stop = false;
    readVault(vaultId)
      .then((v) => !stop && setData(v))
      .catch((e: unknown) => !stop && setError(describeError(e)));
    return () => {
      stop = true;
    };
  }, [vaultId, enabled]);
  return { data, error };
}

/** The vault comes from the URL fragment (`/position#0x…`), never the path (M-6). */
export function PositionView() {
  const mounted = useMounted();
  const fragment = useUrlFragment();
  const vaultId = (fragment && vaultIdFromFragment(fragment)) || undefined;
  const { loan, ready } = useLoanByVault(vaultId);
  const vault = useVault(vaultId, 15_000);
  const onchain = useOnchainVault(vaultId, ready && !loan);
  const zeroAccount = "0x0000000000000000000000000000000000000000";
  const account: Address | undefined =
    loan?.account ?? (onchain.data && onchain.data.account !== zeroAccount ? onchain.data.account : undefined);
  const position = usePosition(account, 30_000);

  if (fragment === undefined) return <p className="text-sm text-muted">Loading…</p>;
  if (!vaultId) return <MissingVaultId fragment={fragment} />;

  const v = vault.data;
  const p = position.data;
  const status = v ? STATUS_LABEL[v.status] : null;
  const collateral = p?.position.collateral ?? v?.outstanding ?? v?.amount;
  const debt = p?.debt;
  const hf = p && debt !== undefined ? healthFactor(p.position.collateral, p.price, debt, config.lltv) : null;
  const lp = p && debt ? liquidationPrice(p.position.collateral, debt, config.lltv) : null;
  const ltv = p && debt !== undefined ? ltvBps(p.position.collateral, p.price, debt) : null;
  const canAct = Boolean(loan) && v?.status === "active" && Boolean(p) && (p?.position.collateral ?? 0n) > 0n;
  const liquidations = v?.claims.filter((c) => c.kind === "Liquidation") ?? [];
  const borrowRoom = p ? maxBorrowAtTargetLtv(p.position.collateral, p.price) > p.debt : false;
  const refresh = () => {
    void vault.refresh();
    void position.refresh();
  };

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm text-muted">Vault</span>
        <Mono className="text-sm">{vaultId}</Mono>
        <CopyButton value={vaultId} label="Copy id" />
        {status ? <Badge tone={status.tone}>{status.text}</Badge> : null}
      </div>
      <nav aria-label="Loan pages" className="flex flex-wrap gap-2 text-sm">
        <Link href={vaultHref("verify", vaultId)} prefetch={false} className={buttonClass("secondary", "no-underline")}>
          Verify vault (viewing key)
        </Link>
        <Link href={vaultHref("refund", vaultId)} prefetch={false} className={buttonClass("secondary", "no-underline")}>
          Refunds{v && v.nonConforming.length ? ` (${v.nonConforming.length})` : ""}
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
      {onchain.error ? (
        <Notice tone="warn" title="Registry unavailable" live>
          <p>{onchain.error}</p>
        </Notice>
      ) : null}

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
          <Stat label="ZEC price" value={p ? formatUsdPrice(p.price) : "—"} hint="MockZecOracle (mock)" />
          <Stat label="Loan-to-value" value={ltv !== null && ltv !== undefined ? formatBps(ltv) : "—"} hint="max 62.5 %" />
        </dl>
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

      {v && v.status !== "active" && v.status !== "closing" && v.status !== "closed" && loan ? (
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

      {v && v.claims.length > 0 ? (
        <Card title="Zcash payouts">
          <ul className="divide-y divide-border text-sm">
            {v.claims.map((c) => (
              <li key={`${c.claimId}-${c.kind}`} className="flex flex-wrap items-center justify-between gap-2 py-2">
                <span>{CLAIM_LABEL[c.kind]}</span>
                <span className="flex items-center gap-3">
                  <span className="tabular-nums">{formatTaz(c.amount)}</span>
                  <Badge tone={c.status === "paid" ? "ok" : "warn"}>{c.status === "paid" ? "Paid" : "Pending"}</Badge>
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
          ) : (
            <Card title="Exit">
              <Exits loan={loan} vault={v} snapshot={p} onChanged={refresh} />
            </Card>
          )}
          {!loan.pendingClose ? (
            <Card title="Withdraw part of your ZEC">
              <PartialRelease loan={loan} vault={v} snapshot={p} onChanged={refresh} />
            </Card>
          ) : null}
          {borrowRoom && !loan.pendingClose ? (
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
