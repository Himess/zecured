"use client";

import Link from "next/link";
import { useEffect, useState, type ReactNode } from "react";
import { isHex, type Hex } from "viem";
import { useMounted } from "./client";
import { STATUS_LABEL } from "./status";
import { Badge, Card, cx, Mono, Notice } from "./ui";
import { describeError } from "@/lib/api";
import { readMarket, readPrice, readRecentRegistryEvents, type RegistryEvent } from "@/lib/chain";
import { config } from "@/lib/config";
import { useLoans, useVault } from "@/lib/hooks";
import { short, type StoredLoan } from "@/lib/keys";
import { formatUsdc, formatUsdPrice, type MarketState } from "@/lib/morpho";
import { formatTaz } from "@/lib/zcash";

interface PublicData {
  market: MarketState | null;
  price: bigint | null;
  events: RegistryEvent[];
  error: string | null;
  loading: boolean;
}

function usePublicBase(): PublicData {
  const [data, setData] = useState<PublicData>({ market: null, price: null, events: [], error: null, loading: config.contractsDeployed });
  useEffect(() => {
    if (!config.contractsDeployed) return;
    let stop = false;
    Promise.all([readMarket(), readPrice(), readRecentRegistryEvents()])
      .then(([market, price, events]) => !stop && setData({ market, price, events, error: null, loading: false }))
      .catch((e: unknown) => !stop && setData((d) => ({ ...d, error: describeError(e), loading: false })));
    return () => {
      stop = true;
    };
  }, []);
  return data;
}

function Column({ title, tone, children }: { title: string; tone: "everyone" | "signers" | "you"; children: ReactNode }) {
  const accent = { everyone: "border-t-info", signers: "border-t-warn", you: "border-t-ok" }[tone];
  return (
    <section aria-label={title} className={cx("space-y-4 rounded-xl border border-border border-t-4 bg-surface p-4 sm:p-5", accent)}>
      <h2 className="text-lg font-semibold">{title}</h2>
      {children}
    </section>
  );
}

function SeesList({ sees, hidden }: { sees: string[]; hidden: string[] }) {
  return (
    <div className="space-y-3 text-sm">
      <div>
        <p className="font-medium text-ok">Sees</p>
        <ul className="mt-1 list-disc space-y-1 pl-5 text-muted">
          {sees.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ul>
      </div>
      <div>
        <p className="font-medium text-danger">Cannot see</p>
        <ul className="mt-1 list-disc space-y-1 pl-5 text-muted">
          {hidden.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ul>
      </div>
    </div>
  );
}

function fieldValue(event: RegistryEvent, key: string, value: string): ReactNode {
  if (/amount/i.test(key) && /^\d+$/.test(value)) return formatTaz(BigInt(value));
  if (/^0x[0-9a-fA-F]{40,}$/.test(value)) return <Mono>{short(value, 8, 6)}</Mono>;
  return value;
}

export function WorldView() {
  const mounted = useMounted();
  const { loans } = useLoans();
  const base = usePublicBase();
  const [inspect, setInspect] = useState("");
  const inspectId = isHex(inspect.trim()) && inspect.trim().length === 66 ? (inspect.trim().toLowerCase() as Hex) : undefined;
  const withVault = loans.filter((l) => l.vaultId);

  return (
    <div className="space-y-6">
      <div className="grid gap-4 lg:grid-cols-3">
        <Column title="Everyone" tone="everyone">
          <SeesList
            sees={[
              "Zcash: Ironwood → Ironwood transactions, with senders, recipients and amounts encrypted. No pool crossing.",
              "Base: a fresh smart account with no history and no ETH (gas is sponsored).",
              "A standard-size zZEC position, the USDC borrowed, interest, closes and liquidations.",
              "Recipients only as salted commitments keccak256(salt ‖ receiver).",
            ]}
            hidden={[
              "Who owns the account.",
              "Your Zcash wallet, balance or history.",
              "Any Zcash address, transaction id or note on Base.",
            ]}
          />
          <div className="space-y-3 border-t border-border pt-4">
            <p className="text-sm font-medium">Live from Base Sepolia</p>
            {!config.contractsDeployed ? (
              <p className="text-sm text-muted">Appears once the contracts are deployed.</p>
            ) : base.loading ? (
              <p className="text-sm text-muted">Reading public chain data…</p>
            ) : base.error ? (
              <p className="text-sm text-warn">Unavailable: {base.error}</p>
            ) : (
              <>
                <dl className="grid grid-cols-2 gap-2 text-sm">
                  <dt className="text-muted">Supplied</dt>
                  <dd className="text-right tabular-nums">{base.market ? `${formatUsdc(base.market.totalSupplyAssets)} USDC` : "—"}</dd>
                  <dt className="text-muted">Borrowed</dt>
                  <dd className="text-right tabular-nums">{base.market ? `${formatUsdc(base.market.totalBorrowAssets)} USDC` : "—"}</dd>
                  <dt className="text-muted">ZEC price (mock)</dt>
                  <dd className="text-right tabular-nums">{base.price ? formatUsdPrice(base.price) : "—"}</dd>
                </dl>
                <p className="text-sm font-medium">Recent Zecured events</p>
                {base.events.length === 0 ? (
                  <p className="text-sm text-muted">None in the last ~10,000 blocks.</p>
                ) : (
                  <ul className="space-y-2 text-xs">
                    {base.events.slice(0, 8).map((e) => (
                      <li key={`${e.transactionHash}-${e.name}-${e.vaultId}`} className="rounded-md border border-border p-2">
                        <p className="font-medium text-text">{e.name}</p>
                        <p className="text-muted">
                          vault <Mono>{short(e.vaultId, 8, 6)}</Mono>
                        </p>
                        {Object.entries(e.fields).map(([k, v]) => (
                          <p key={k} className="text-muted">
                            {k}: {fieldValue(e, k, v)}
                          </p>
                        ))}
                      </li>
                    ))}
                  </ul>
                )}
              </>
            )}
          </div>
        </Column>

        <Column title="Signer network" tone="signers">
          <SeesList
            sees={[
              "The vault's notes, balance and timing (they hold its viewing key).",
              "The Ironwood destinations revealed for payouts (receiver and salt).",
              "Payments to a vault that cannot back its loan, until they are refunded.",
            ]}
            hidden={[
              "Your identity, wallet or any ZEC outside the vault.",
              "Your IP address: our backend sees it and logs no IPs, addresses, salts or viewing keys; the app's host sees page loads, never a vault id (it stays in the URL fragment).",
            ]}
          />
          <div className="space-y-3 border-t border-border pt-4">
            <p className="text-sm font-medium">What they hold about your vaults</p>
            {!mounted ? null : withVault.length === 0 ? (
              <p className="text-sm text-muted">No vault keys in this browser.</p>
            ) : (
              withVault.map((l) => <SignerView key={l.id} vaultId={l.vaultId!} staggered />)
            )}
            <label className="block text-sm">
              <span className="text-muted">Inspect a vault id</span>
              <input
                value={inspect}
                onChange={(e) => setInspect(e.target.value)}
                placeholder="0x…"
                spellCheck={false}
                autoComplete="off"
                className="mt-1 w-full rounded-lg border border-border bg-bg px-3 py-2 font-mono text-xs text-text focus:border-accent"
              />
            </label>
            {inspectId ? <SignerView vaultId={inspectId} /> : null}
          </div>
        </Column>

        <Column title="You" tone="you">
          <SeesList
            sees={[
              "Everything about your loan: the owner key in this browser, the account, the vault address.",
              "Your vault itself, through its viewing key.",
            ]}
            hidden={["Where third-party payouts went: they are encrypted without an outgoing viewing key."]}
          />
          <div className="space-y-3 border-t border-border pt-4">
            <p className="text-sm font-medium">Held only in this browser</p>
            {!mounted ? null : loans.length === 0 ? (
              <p className="text-sm text-muted">
                Nothing yet. <Link href="/borrow">Start a loan</Link>.
              </p>
            ) : (
              loans.map((l) => <YouView key={l.id} loan={l} />)
            )}
          </div>
        </Column>
      </div>
      <Notice tone="info" title="Deliberately public">
        <p>
          Like any DeFi position, the loan size, the collateral denomination, interest, closes and liquidations are visible on
          Base, unlinked to you. Testnet volume is low, which keeps the crowd you hide in small.
        </p>
      </Notice>
    </div>
  );
}

/** `staggered` for this browser's own vaults: not all requested in one burst (L-11). */
function SignerView({ vaultId, staggered = false }: { vaultId: Hex; staggered?: boolean }) {
  const vault = useVault(vaultId, 0, { staggered });
  const v = vault.data;
  return (
    <div className="rounded-md border border-border p-3 text-xs">
      <p className="text-muted">
        vault <Mono>{short(vaultId, 8, 6)}</Mono>
      </p>
      {vault.error ? <p className="text-warn">Unavailable: {vault.error}</p> : null}
      {v ? (
        <ul className="mt-1 space-y-0.5 text-muted">
          <li>
            Status <Badge tone={STATUS_LABEL[v.status].tone}>{STATUS_LABEL[v.status].text}</Badge>
          </li>
          <li>Amount {formatTaz(v.amount)}</li>
          {v.outstanding !== undefined ? <li>Outstanding {formatTaz(v.outstanding)}</li> : null}
          <li>Deposit deadline: block #{v.expiryHeight.toLocaleString("en-US")}</li>
          <li>Other payments (not collateral): {v.nonConforming.length}</li>
          <li>
            Payout claims: {v.claims.length} ({v.claims.filter((c) => c.status === "paid").length} paid)
          </li>
        </ul>
      ) : null}
    </div>
  );
}

function YouView({ loan }: { loan: StoredLoan }) {
  return (
    <div className="rounded-md border border-border p-3 text-xs text-muted">
      <p>
        Owner key: <span className="text-text">held here (secret)</span>, owner <Mono>{short(loan.owner)}</Mono>
      </p>
      <p>
        Account <Mono>{short(loan.account)}</Mono>
      </p>
      {loan.vaultAddress ? (
        <p>
          Vault address <Mono>{short(loan.vaultAddress, 12, 8)}</Mono>
        </p>
      ) : null}
      {loan.destinationCommitment ? (
        <p>
          Destination commitment <Mono>{short(loan.destinationCommitment, 8, 6)}</Mono> (the address and salt are not kept)
        </p>
      ) : null}
    </div>
  );
}
