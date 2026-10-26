"use client";

import Link from "next/link";
import { usePosition, useVault } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { vaultHref } from "@/lib/routes";
import { IconArrowRight } from "./icons";
import { fallBeforeLiquidation, fallPercent, HealthRunway, healthOf, TokenAmount, usdc, zec } from "./loan";
import { statusOf } from "./status";
import { Badge, buttonClass, cx, Skeleton } from "./ui";

/**
 * One loan of this browser, live, in a row: its collateral and the vault's status (the signers' word), its debt on
 * Base, and its health. Requests for several loans are staggered (hooks.ts `createStagger`), so the backend does
 * not get this browser's vault ids as one burst.
 */
export function LoanCard({ loan, compact = false }: { loan: StoredLoan; compact?: boolean }) {
  const vault = useVault(loan.vaultId, 30_000, { staggered: true });
  const active = vault.data?.status === "active" || vault.data?.status === "closing";
  const position = usePosition(loan.vaultId && active ? loan.account : undefined, 60_000);
  const p = position.data;
  const status = vault.data ? statusOf(vault.data) : null;
  const health = p ? healthOf(p) : null;
  const fall = health ? fallBeforeLiquidation(health) : null;
  // A loan that is over (closed, expired, cancelled) shows what it locked; a live one what is locked now.
  const over = vault.data ? ["closed", "expired", "cancelled"].includes(vault.data.status) : false;
  const collateral = over ? BigInt(loan.amount) : (p?.position.collateral ?? vault.data?.outstanding ?? BigInt(loan.amount));
  const opened = Boolean(loan.vaultId);
  const href = opened && (active || over) ? vaultHref("position", loan.vaultId!) : "/borrow";
  const showFigures = !over && (!compact || p);

  return (
    <li
      className={cx(
        "panel grid items-center gap-x-6 gap-y-3 px-5 py-4 sm:px-6",
        showFigures ? "md:grid-cols-[minmax(0,1.3fr)_minmax(0,0.8fr)_minmax(0,1.1fr)_auto]" : "md:grid-cols-[minmax(0,1fr)_auto]",
      )}
    >
      <div className="min-w-0 space-y-1.5">
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
          {loan.demo ? <Badge tone="demo">Demo TAZ</Badge> : null}
        </div>
        <p className="text-[12.5px] text-muted">
          shielded · account <span className="font-mono text-soft">{short(loan.account)}</span>
          {loan.vaultId ? (
            <>
              {" · "}vault <span className="font-mono text-soft">{short(loan.vaultId, 8, 6)}</span>
            </>
          ) : null}
        </p>
      </div>
      {showFigures ? (
        <>
          <div className="space-y-1">
            <p className="text-[12px] font-semibold text-muted">Debt</p>
            {p ? <TokenAmount token="USDC" value={usdc(p.debt)} size="md" /> : <p className="text-[15px] font-bold text-faint">—</p>}
          </div>
          <div className="space-y-1">
            <p className="text-[12px] font-semibold text-muted">Health</p>
            {health ? (
              <>
                <HealthRunway health={health} size="s" />
                <p className="text-[12px] text-muted">
                  {fall === null ? "no debt" : fall === 0n ? "liquidatable now" : `ZEC can fall ${fallPercent(fall)} %`}
                </p>
              </>
            ) : (
              <p className="text-[15px] font-bold text-faint">—</p>
            )}
          </div>
        </>
      ) : null}
      <Link href={href} prefetch={false} className={cx(buttonClass(active || over ? "secondary" : "primary", undefined, "sm"), "gap-1.5 justify-self-start md:justify-self-end")}>
        {active ? "Open" : over ? "View" : opened ? "Continue the deposit" : "Open the vault"}
        <IconArrowRight size={15} />
      </Link>
    </li>
  );
}
