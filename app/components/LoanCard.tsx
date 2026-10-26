"use client";

import Link from "next/link";
import { usePosition, useVault } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { vaultHref } from "@/lib/routes";
import { IconArrowRight } from "./icons";
import { HealthRing, healthOf, TokenAmount, usdc, zec } from "./loan";
import { STATUS_LABEL } from "./status";
import { Badge, buttonClass, cx, Skeleton } from "./ui";

/**
 * One loan of this browser, live: its vault's status (the signers' word), its collateral and debt on Base, and its
 * health. Requests for several loans are staggered (hooks.ts `createStagger`), so the backend does not get this
 * browser's vault ids as one burst.
 */
export function LoanCard({ loan, compact = false }: { loan: StoredLoan; compact?: boolean }) {
  const vault = useVault(loan.vaultId, 30_000, { staggered: true });
  const active = vault.data?.status === "active" || vault.data?.status === "closing";
  const position = usePosition(loan.vaultId && active ? loan.account : undefined, 60_000);
  const p = position.data;
  const status = vault.data ? STATUS_LABEL[vault.data.status] : null;
  const health = p ? healthOf(p) : null;
  // A loan that is over (closed, expired, cancelled) shows what it locked; a live one what is locked now.
  const over = vault.data ? ["closed", "expired", "cancelled"].includes(vault.data.status) : false;
  const collateral = over ? BigInt(loan.amount) : (p?.position.collateral ?? vault.data?.outstanding ?? BigInt(loan.amount));
  const opened = Boolean(loan.vaultId);
  const href = opened && (active || over) ? vaultHref("position", loan.vaultId!) : "/borrow";

  return (
    <li className="panel flex flex-wrap items-center gap-x-6 gap-y-4 px-5 py-4 sm:px-6">
      <div className="min-w-0 flex-1 space-y-1.5">
        <div className="flex flex-wrap items-center gap-2">
          <TokenAmount token="TAZ" value={zec(collateral)} size="lg" />
          {status ? (
            <Badge tone={status.tone} dot>
              {status.text}
            </Badge>
          ) : opened ? (
            <Skeleton className="h-6 w-24" />
          ) : (
            <Badge tone="warn">Vault not opened</Badge>
          )}
          {loan.demo ? <Badge tone="neutral">Demo TAZ</Badge> : null}
        </div>
        <p className="text-[13px] text-muted">
          Account <span className="font-mono text-soft">{short(loan.account)}</span>
          {loan.vaultId ? (
            <>
              {" · "}vault <span className="font-mono text-soft">{short(loan.vaultId, 8, 6)}</span>
            </>
          ) : null}
        </p>
      </div>
      {!over && (!compact || p) ? (
        <div className="flex items-center gap-5">
          <div className="space-y-0.5 text-right">
            <p className="text-[12px] font-semibold text-muted">Debt</p>
            {p ? <TokenAmount token="USDC" value={usdc(p.debt)} size="md" /> : <p className="text-[15px] font-bold text-faint">—</p>}
          </div>
          <HealthRing health={health} size={64} />
        </div>
      ) : null}
      <Link href={href} prefetch={false} className={cx(buttonClass(active || over ? "secondary" : "primary", undefined, "sm"), "gap-1.5")}>
        {active ? "Open" : over ? "View" : opened ? "Continue the deposit" : "Open the vault"}
        <IconArrowRight size={15} />
      </Link>
    </li>
  );
}
